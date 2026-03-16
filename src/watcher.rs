use crossbeam_channel::{bounded, Receiver};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::processor::process_pair;
use crate::splitter::init_pdfium;
use crate::utils::{extract_counter, is_scan_file};

/// State shared between the notify callback and the processing thread.
/// Maps file counter → full path of the pending (unmatched) file.
type PendingMap = Arc<Mutex<HashMap<u32, PathBuf>>>;

const PROCESSED_FILE: &str = "processed_pairs.txt";

fn get_record_path(config: &Config) -> PathBuf {
    config.watch_dir.join(PROCESSED_FILE)
}

/// Load processed pairs. Format: each line is "LOWER,HIGHER" (two counters forming a pair)
fn load_processed_pairs(config: &Config) -> HashSet<(u32, u32)> {
    let record_path = get_record_path(config);
    if !record_path.exists() {
        return HashSet::new();
    }

    let content = match fs::read_to_string(&record_path) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to read processed pairs file: {}", e);
            return HashSet::new();
        }
    };

    content
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.trim().split(',').collect();
            if parts.len() == 2 {
                let a = parts[0].trim().parse::<u32>().ok()?;
                let b = parts[1].trim().parse::<u32>().ok()?;
                // Normalize to (lower, higher)
                Some((a.min(b), a.max(b)))
            } else {
                None
            }
        })
        .collect()
}

/// Save a processed pair. Format: "LOWER,HIGHER"
fn save_processed_pair(config: &Config, counter_a: u32, counter_b: u32) {
    let record_path = get_record_path(config);
    let lower = counter_a.min(counter_b);
    let higher = counter_a.max(counter_b);

    if let Err(e) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&record_path)
        .and_then(|mut f| {
            use std::io::Write;
            writeln!(f, "{},{}", lower, higher)
        })
    {
        warn!(
            "Failed to save processed pair ({},{}): {}",
            lower, higher, e
        );
    }
}

fn scan_and_process_existing(config: &Arc<Config>, pending: &PendingMap) {
    let watch_dir = &config.watch_dir;
    if !watch_dir.exists() {
        return;
    }

    let processed_pairs = load_processed_pairs(config);
    info!("Found {} previously processed pairs", processed_pairs.len());

    let entries = match fs::read_dir(watch_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to read watch directory: {}", e);
            return;
        }
    };

    // Build a map of counter → full path for all scan files
    let mut counter_map: HashMap<u32, PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_scan_file(p))
        .filter_map(|p| match extract_counter(&p) {
            Ok(c) => {
                debug!(
                    "Startup scan: found file with counter {}: {}",
                    c,
                    p.display()
                );
                Some((c, p))
            }
            Err(e) => {
                debug!(
                    "Startup scan: failed to extract counter from {}: {}",
                    p.display(),
                    e
                );
                None
            }
        })
        .collect();

    let total_files = counter_map.len();
    info!("Found {} scan files in startup", total_files);

    // Check if total file count is even (required for pairing)
    if total_files % 2 != 0 {
        error!(
            "Total number of files ({}) is odd, cannot process in complete pairs. Stopping.",
            total_files
        );
        return;
    }

    info!(
        "Total file count is even ({}). Proceeding with pair processing.",
        total_files
    );

    // Sort counters for deterministic processing
    let mut sorted_counters: Vec<u32> = counter_map.keys().copied().collect();
    sorted_counters.sort_unstable();

    let mut processed_in_startup = HashSet::new();

    // Process files in pairs from the top: (0,1), (2,3), (4,5), etc.
    for chunk in sorted_counters.chunks(2) {
        if chunk.len() < 2 {
            // Should not happen due to even check above, but handle just in case
            let counter = chunk[0];
            let file_path = counter_map.get(&counter).unwrap();
            warn!(
                "Startup: unpaired file (counter {}) - {}",
                counter,
                file_path.file_name().unwrap_or_default().to_string_lossy()
            );
            continue;
        }

        let counter_a = chunk[0];
        let counter_b = chunk[1];

        // Skip if already processed in this startup or already paired
        if processed_in_startup.contains(&counter_a) || processed_in_startup.contains(&counter_b) {
            continue;
        }

        // Check if this pair was already processed before
        let pair_key = (counter_a.min(counter_b), counter_a.max(counter_b));

        if processed_pairs.contains(&pair_key) {
            info!(
                "Startup: skipping already processed pair ({}, {}) - files will be ignored",
                pair_key.0, pair_key.1
            );
            processed_in_startup.insert(counter_a);
            processed_in_startup.insert(counter_b);
            continue;
        }

        // Get full file paths
        let file_a = counter_map.get(&counter_a).cloned();
        let file_b = counter_map.get(&counter_b).cloned();

        let (file_a, file_b) = match (file_a, file_b) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };

        processed_in_startup.insert(counter_a);
        processed_in_startup.insert(counter_b);

        info!(
            "Startup: processing pair ({}, {}) - {} + {}",
            counter_a,
            counter_b,
            file_a.file_name().unwrap_or_default().to_string_lossy(),
            file_b.file_name().unwrap_or_default().to_string_lossy()
        );

        // Process the pair in a spawned thread
        let config_clone = Arc::clone(&config);

        thread::spawn(move || {
            info!(
                "⏳ Startup pair ({}, {}) - beginning processing...",
                counter_a, counter_b
            );

            info!("  [Init] Loading PDFium library...");

            let pdfium = match init_pdfium(&config_clone.pdfium_lib_path) {
                Ok(p) => {
                    info!("  [Init] ✓ PDFium loaded");
                    p
                }
                Err(e) => {
                    error!("  [Init] ✗ Failed to init pdfium: {}", e);
                    return;
                }
            };

            match process_pair(&file_a, &file_b, &config_clone, &pdfium) {
                Ok(result) => {
                    save_processed_pair(&config_clone, counter_a, counter_b);
                    info!(
                        "✓ Startup pair ({}, {}) → {}",
                        counter_a,
                        counter_b,
                        result
                            .output_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                }
                Err(e) => {
                    error!(
                        "✗ Startup pair ({}, {}) failed: {}",
                        counter_a, counter_b, e
                    );
                }
            }
        });
    }

    // Add any remaining unprocessed files to pending (for watcher to handle later)
    for counter in &sorted_counters {
        if !processed_in_startup.contains(counter) {
            if let Some(file_path) = counter_map.get(counter) {
                let mut pending_map = pending.lock().unwrap();
                pending_map.insert(*counter, file_path.clone());
                info!(
                    "Startup: file (counter {}) added to pending - {}",
                    counter,
                    file_path.file_name().unwrap_or_default().to_string_lossy()
                );
            }
        }
    }
}

/// Start the file-watching daemon. Blocks until Ctrl-C.
///
/// Architecture:
///   - notify watcher thread → crossbeam channel → processor thread pool
///   - Pair matching happens in a single Mutex-protected HashMap (no races)
///   - Each matched pair is processed in its own thread (non-blocking)
pub fn run_daemon(config: Config) -> anyhow::Result<()> {
    let config = Arc::new(config);

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

    // Channel: notify → our handler
    let (tx, rx) = bounded::<PathBuf>(64);

    // Start the notify watcher
    let watch_dir = config.watch_dir.clone();
    // Bind to _watcher — the leading underscore is intentional.
    // The notify watcher stops delivering events as soon as it is dropped,
    // so we must keep it alive for the entire daemon lifetime.
    // `run_event_loop` blocks forever, so a plain `drop(watcher)` after it
    // would never execute, leaving the watcher alive only by accident.
    // Naming it `_watcher` makes the intent explicit and silences the compiler.
    let _watcher = start_watcher(watch_dir, tx)?;

    // Scan for existing files and process any complete pairs
    scan_and_process_existing(&config, &pending);

    info!("Daemon running. Press Ctrl-C to stop.");

    // Process events from the channel (blocks until channel is closed)
    run_event_loop(rx, pending, config);
    Ok(())
}

/// Start a notify watcher that sends new PDF paths to the channel.
fn start_watcher(
    watch_dir: PathBuf,
    tx: crossbeam_channel::Sender<PathBuf>,
) -> anyhow::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        match res {
            Ok(event) => {
                // Only care about file creation events
                if !matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
                ) {
                    return;
                }

                for path in event.paths {
                    if !is_scan_file(&path) {
                        continue;
                    }
                    debug!("Detected new file: {}", path.display());

                    // Small delay to ensure the file is fully flushed by the scanner
                    // (some scanners create the file then write to it)
                    thread::sleep(Duration::from_millis(500));

                    if let Err(e) = tx.send(path) {
                        error!("Channel send error: {e}");
                    }
                }
            }
            Err(e) => error!("Watcher error: {e}"),
        }
    })?;

    watcher.watch(&watch_dir, RecursiveMode::NonRecursive)?;
    info!("Watching directory: {}", watch_dir.display());

    Ok(watcher)
}

/// Main event loop: receive file paths, pair sequentially (first file waits for second file), spawn processing threads.
fn run_event_loop(rx: Receiver<PathBuf>, pending: PendingMap, config: Arc<Config>) {
    let processed_pairs = Arc::new(Mutex::new(load_processed_pairs(&config)));

    for path in rx {
        let counter = match extract_counter(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Skipping file ({}): {}", path.display(), e);
                continue;
            }
        };

        debug!(
            "Watcher event: file counter={} path={}",
            counter,
            path.display()
        );

        let pair = {
            let mut pending_map = pending.lock().unwrap();
            let processed = processed_pairs.lock().unwrap();

            // Simple queue-based pairing: if there's a waiting file, pair with it
            // No checking of counter numbers - just pair any two files together
            if let Some((waiting_counter, waiting_path)) =
                pending_map.iter().next().map(|(k, v)| (*k, v.clone()))
            {
                pending_map.remove(&waiting_counter);

                // Pair: file_a is the waiting file, file_b is the new file
                let file_a = waiting_path;
                let file_b = path.clone();
                let counter_a = waiting_counter;
                let counter_b = counter;

                let pair_key = (counter_a.min(counter_b), counter_a.max(counter_b));

                if processed.contains(&pair_key) {
                    info!(
                        "Watcher: pair ({}, {}) already processed, ignoring - {} + {}",
                        pair_key.0,
                        pair_key.1,
                        file_a.file_name().unwrap_or_default().to_string_lossy(),
                        file_b.file_name().unwrap_or_default().to_string_lossy()
                    );
                    None
                } else {
                    info!(
                        "Watcher: matched pair ({}, {}) - {} + {}",
                        pair_key.0,
                        pair_key.1,
                        file_a.file_name().unwrap_or_default().to_string_lossy(),
                        file_b.file_name().unwrap_or_default().to_string_lossy()
                    );
                    Some((file_a, file_b, pair_key.0, pair_key.1))
                }
            } else {
                // No file waiting - add to pending
                pending_map.insert(counter, path.clone());
                info!(
                    "Watcher: waiting for pair of {} (counter={})",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    counter
                );
                None
            }
        };

        debug!(
            "Watcher event: file counter={} path={}",
            counter,
            path.display()
        );

        let pair = {
            let mut pending_map = pending.lock().unwrap();
            let processed = processed_pairs.lock().unwrap();

            // Sequential pairing: first look for counter-1 (previous file), then counter+1
            let partner_result: Option<(u32, PathBuf, PathBuf)> =
                if counter > 0 && pending_map.contains_key(&(counter - 1)) {
                    // Current file is the HIGHER counter → it's file_B; partner (counter-1) is file_A
                    let partner_path = pending_map.remove(&(counter - 1)).unwrap();
                    Some((counter - 1, partner_path, path.clone()))
                } else if let Some(partner_path) = pending_map.remove(&(counter + 1)) {
                    // Current file is the LOWER counter → it's file_A; partner (counter+1) is file_B
                    Some((counter + 1, path.clone(), partner_path))
                } else {
                    None
                };

            match partner_result {
                Some((partner, file_a, file_b)) => {
                    // pair_key is always (lower, higher)
                    let pair_key = (partner.min(counter), partner.max(counter));

                    if processed.contains(&pair_key) {
                        info!(
                            "Watcher: pair ({}, {}) already processed, ignoring - {} + {}",
                            pair_key.0,
                            pair_key.1,
                            file_a.file_name().unwrap_or_default().to_string_lossy(),
                            file_b.file_name().unwrap_or_default().to_string_lossy()
                        );
                        None
                    } else {
                        info!(
                            "Watcher: matched pair ({}, {}) - {} + {}",
                            pair_key.0,
                            pair_key.1,
                            file_a.file_name().unwrap_or_default().to_string_lossy(),
                            file_b.file_name().unwrap_or_default().to_string_lossy()
                        );
                        Some((file_a, file_b, pair_key.0, pair_key.1))
                    }
                }
                None => {
                    // No match yet — park in pending map
                    pending_map.insert(counter, path.clone());
                    info!(
                        "Watcher: waiting for pair of {} (counter={}) - looking for counter {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        counter,
                        counter + 1
                    );
                    None
                }
            }
        };

        // If we have a complete pair, process it in a new thread
        if let Some((file_a, file_b, counter_a, counter_b)) = pair {
            let config = Arc::clone(&config);
            let processed_pairs = Arc::clone(&processed_pairs);

            thread::spawn(move || {
                info!(
                    "⏳ Watcher pair ({}, {}) - beginning processing...",
                    counter_a, counter_b
                );

                info!("  [Init] Loading PDFium library...");

                let pdfium = match init_pdfium(&config.pdfium_lib_path) {
                    Ok(p) => {
                        info!("  [Init] ✓ PDFium loaded");
                        p
                    }
                    Err(e) => {
                        error!("  [Init] ✗ Failed to init pdfium: {}", e);
                        return;
                    }
                };

                match process_pair(&file_a, &file_b, &config, &pdfium) {
                    Ok(result) => {
                        save_processed_pair(&config, counter_a, counter_b);
                        processed_pairs
                            .lock()
                            .unwrap()
                            .insert((counter_a, counter_b));
                        info!(
                            "✓ Processed pair ({}, {}) → {}",
                            counter_a,
                            counter_b,
                            result
                                .output_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                        );
                    }
                    Err(e) => {
                        error!(
                            "✗ Pair ({}, {}) failed: ({} + {}): {}",
                            counter_a,
                            counter_b,
                            file_a.file_name().unwrap_or_default().to_string_lossy(),
                            file_b.file_name().unwrap_or_default().to_string_lossy(),
                            e,
                        );
                    }
                }
            });
        }
    }
}
