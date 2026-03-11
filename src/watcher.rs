use crossbeam_channel::{bounded, Receiver};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pdfium_render::prelude::Pdfium;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::error::ProcessorError;
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
        warn!("Failed to save processed pair ({},{}): {}", lower, higher, e);
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
        .filter_map(|p| {
            match extract_counter(&p) {
                Ok(c) => {
                    debug!("Startup scan: found file with counter {}: {}", c, p.display());
                    Some((c, p))
                }
                Err(e) => {
                    debug!("Startup scan: failed to extract counter from {}: {}", p.display(), e);
                    None
                }
            }
        })
        .collect();
    
    info!("Found {} scan files in startup", counter_map.len());
    
    // Sort counters for deterministic processing
    let mut sorted_counters: Vec<u32> = counter_map.keys().copied().collect();
    sorted_counters.sort_unstable();
    
    let mut processed_in_startup = HashSet::new();
    
    for counter in sorted_counters {
        // Skip if already processed in this startup or already paired
        if processed_in_startup.contains(&counter) || !counter_map.contains_key(&counter) {
            continue;
        }
        
        // Try to find a pairing partner: counter±1
        let partner_counter = if counter_map.contains_key(&(counter + 1)) {
            Some(counter + 1)
        } else if counter > 0 && counter_map.contains_key(&(counter - 1)) {
            Some(counter - 1)
        } else {
            None
        };
        
        match partner_counter {
            Some(partner) => {
                // Check if this pair was already processed before
                let pair_key = if counter < partner {
                    (counter, partner)
                } else {
                    (partner, counter)
                };
                
                if processed_pairs.contains(&pair_key) {
                    info!(
                        "Startup: skipping already processed pair ({}, {}) - files will be ignored",
                        pair_key.0, pair_key.1
                    );
                    counter_map.remove(&counter);
                    counter_map.remove(&partner);
                    processed_in_startup.insert(counter);
                    processed_in_startup.insert(partner);
                    continue;
                }
                
                // We have a new pair to process
                let file_a = counter_map.remove(&counter).unwrap();
                let file_b = counter_map.remove(&partner).unwrap();
                processed_in_startup.insert(counter);
                processed_in_startup.insert(partner);
                
                let (file_a, file_b, counter_a, counter_b) = if counter < partner {
                    (file_a, file_b, counter, partner)
                } else {
                    (file_b, file_a, partner, counter)
                };
                
                info!(
                    "Startup: processing complete pair ({}, {}) - {} + {}",
                    counter_a,
                    counter_b,
                    file_a.file_name().unwrap_or_default().to_string_lossy(),
                    file_b.file_name().unwrap_or_default().to_string_lossy()
                );
                
                // Process the pair in a spawned thread
                let config_clone = Arc::clone(&config);
                
                thread::spawn(move || {
                    info!("⏳ Startup pair ({}, {}) - beginning processing...", counter_a, counter_b);
                    
                    // Set TESSDATA_PREFIX to the tessdata directory itself.
                    // config.tessdata_path already points to the tessdata folder
                    // (e.g. /usr/share/tessdata), so use it directly — NOT .parent(),
                    // which would give /usr/share and cause "eng.traineddata not found".
                    std::env::set_var("TESSDATA_PREFIX", config_clone.tessdata_path.to_string_lossy().as_ref());
                    info!("  [Init] TESSDATA_PREFIX = {}", config_clone.tessdata_path.display());
                    
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
                                "✓ Startup pair ({}, {}) → {} (student: {})",
                                counter_a, counter_b,
                                result.output_path.file_name().unwrap_or_default().to_string_lossy(),
                                result.student_number,
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
            None => {
                // No partner found yet - add to pending so watcher can match it later
                let file_path = counter_map.remove(&counter).unwrap();
                let mut pending_map = pending.lock().unwrap();
                pending_map.insert(counter, file_path.clone());
                info!(
                    "Startup: incomplete pair ({}) waiting for partner in pending - {}",
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

/// Main event loop: receive file paths, match pairs, spawn processing threads.
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

        debug!("Watcher event: file counter={} path={}", counter, path.display());

        let pair = {
            let mut pending_map = pending.lock().unwrap();
            let processed = processed_pairs.lock().unwrap();

            // Try to find a partner: look for counter-1 OR counter+1.
            // IMPORTANT: use checked subtraction — counter is u32 and could be 0,
            // causing a panic (debug) or wrap-around (release) without the guard.
            let partner_result: Option<(u32, PathBuf, PathBuf)> =
                if counter > 0 {
                    if let Some(partner_path) = pending_map.remove(&(counter - 1)) {
                        // Current file has the HIGHER counter → it is file_B; partner is file_A
                        Some((counter - 1, partner_path, path.clone()))
                    } else if let Some(partner_path) = pending_map.remove(&(counter + 1)) {
                        // Current file has the LOWER counter → it is file_A; partner is file_B
                        Some((counter + 1, path.clone(), partner_path))
                    } else {
                        None
                    }
                } else {
                    // counter == 0: can only have a partner at counter+1
                    pending_map.remove(&(counter + 1))
                        .map(|partner_path| (counter + 1, path.clone(), partner_path))
                };

            match partner_result {
                Some((partner, file_a, file_b)) => {
                    // pair_key is always (lower, higher)
                    let pair_key = if counter < partner {
                        (counter, partner)
                    } else {
                        (partner, counter)
                    };

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
                        "Watcher: waiting for pair of {} (counter={}) - looking for counter {} or {}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        counter,
                        if counter > 0 { counter - 1 } else { 0 },
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
                info!("⏳ Watcher pair ({}, {}) - beginning processing...", counter_a, counter_b);
                
                // Set TESSDATA_PREFIX to the tessdata directory itself.
                // config.tessdata_path already points to the tessdata folder
                // (e.g. /usr/share/tessdata), so use it directly — NOT .parent().
                std::env::set_var("TESSDATA_PREFIX", config.tessdata_path.to_string_lossy().as_ref());
                info!("  [Init] TESSDATA_PREFIX = {}", config.tessdata_path.display());
                
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
                        processed_pairs.lock().unwrap().insert((counter_a, counter_b));
                        info!(
                            "✓ Processed pair ({}, {}) → {} (student: {})",
                            counter_a,
                            counter_b,
                            result
                                .output_path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy(),
                            result.student_number,
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
