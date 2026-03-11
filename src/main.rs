mod config;
mod error;
mod ocr;
mod processor;
mod splitter;
mod utils;
mod watcher;

use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

use crate::config::Config;
use crate::watcher::run_daemon;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ConfigToml {
    general: GeneralConfig,
    roi: RoiConfigToml,
    debug: DebugConfig,
    #[serde(default)]
    environment: EnvironmentConfig,
}

#[derive(Debug, Deserialize)]
struct GeneralConfig {
    watch_dir: PathBuf,
    output_dir: PathBuf,
    dpi: u32,
    tessdata: PathBuf,
    pdfium_lib: PathBuf,
    #[serde(default)]
    library_path: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct EnvironmentConfig {
    #[serde(default)]
    tessdata_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RoiConfigToml {
    y_start: f32,
    y_end: f32,
    x_start: f32,
    x_end: f32,
}

#[derive(Debug, Deserialize)]
struct DebugConfig {
    debug_roi: bool,
}

fn main() -> anyhow::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(false)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // Catch panics in threads to ensure they're logged
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            eprintln!("THREAD PANIC: {}", s);
            tracing::error!("THREAD PANIC: {}", s);
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            eprintln!("THREAD PANIC: {}", s);
            tracing::error!("THREAD PANIC: {}", s);
        }
        if let Some(loc) = panic_info.location() {
            eprintln!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
            tracing::error!("  at {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
        default_panic(panic_info);
    }));

    let config_path = std::env::current_exe()?
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("config.toml");

    info!("Loading config from: {}", config_path.display());

    let config_content = std::fs::read_to_string(&config_path)?;
    let toml: ConfigToml = toml::from_str(&config_content)?;

    // Set TESSDATA_PREFIX for Tesseract
    if let Some(tessdata_prefix) = &toml.environment.tessdata_prefix {
        std::env::set_var("TESSDATA_PREFIX", tessdata_prefix);
        info!("Set TESSDATA_PREFIX to: {}", tessdata_prefix);
    }

    if let Some(library_path) = &toml.general.library_path {
        let library_path_buf = PathBuf::from(library_path);
        let library_dir = if library_path_buf.is_absolute() {
            library_path_buf
        } else {
            let exe_path = std::env::current_exe()?;
            let exe_dir = exe_path.parent().unwrap_or_else(|| std::path::Path::new("."));
            exe_dir.join(&library_path_buf)
        };
        if library_dir.exists() {
            std::env::set_var("LD_LIBRARY_PATH", library_dir.to_string_lossy().as_ref());
            info!("Set LD_LIBRARY_PATH to: {}", library_dir.display());
        }
    }

    let watch_dir = toml.general.watch_dir;
    let output_dir = toml.general.output_dir;

    if !watch_dir.exists() {
        std::fs::create_dir_all(&watch_dir)?;
        info!("Created watch directory: {}", watch_dir.display());
    }
    if !output_dir.exists() {
        std::fs::create_dir_all(&output_dir)?;
        info!("Created output directory: {}", output_dir.display());
    }

    let config = Config {
        watch_dir,
        output_dir,
        dpi: toml.general.dpi,
        tessdata_path: toml.general.tessdata,
        pdfium_lib_path: toml.general.pdfium_lib,
        roi: crate::config::RoiConfig {
            x_start_frac: toml.roi.x_start,
            x_end_frac: toml.roi.x_end,
            y_start_frac: toml.roi.y_start,
            y_end_frac: toml.roi.y_end,
        },
        debug_roi: toml.debug.debug_roi,
    };

    info!("Scanner Processor starting...");
    info!("Watching:  {}", config.watch_dir.display());
    info!("Output:    {}", config.output_dir.display());
    info!("DPI:       {}", config.dpi);
    info!(
        "ROI:       y=[{:.1}%–{:.1}%]  x=[{:.1}%–{:.1}%]",
        config.roi.y_start_frac * 100.0,
        config.roi.y_end_frac * 100.0,
        config.roi.x_start_frac * 100.0,
        config.roi.x_end_frac * 100.0,
    );

    run_daemon(config)
}
