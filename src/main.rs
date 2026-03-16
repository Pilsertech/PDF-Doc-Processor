mod config;
mod error;
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
    #[serde(default)]
    page_order: PageOrderToml,
}

#[derive(Debug, Deserialize)]
struct GeneralConfig {
    watch_dir: PathBuf,
    output_dir: PathBuf,
    dpi: u32,
    pdfium_lib: PathBuf,
    #[serde(default = "default_jpeg_quality")]
    jpeg_quality: u8,
    #[serde(default)]
    library_path: Option<String>,
}

fn default_jpeg_quality() -> u8 {
    70
}

#[derive(Debug, Deserialize)]
struct RoiConfigToml {
    y_start: f32,
    y_end: f32,
    x_start: f32,
    x_end: f32,
}

#[derive(Debug, Deserialize)]
struct PageOrderToml {
    #[serde(default = "default_a_right")]
    page1: String,
    #[serde(default = "default_b_right")]
    page2: String,
    #[serde(default = "default_b_left")]
    page3: String,
    #[serde(default = "default_a_left")]
    page4: String,
}

fn default_a_right() -> String {
    "A_right".into()
}
fn default_b_right() -> String {
    "B_right".into()
}
fn default_b_left() -> String {
    "B_left".into()
}
fn default_a_left() -> String {
    "A_left".into()
}

impl Default for PageOrderToml {
    fn default() -> Self {
        Self {
            page1: default_a_right(),
            page2: default_b_right(),
            page3: default_b_left(),
            page4: default_a_left(),
        }
    }
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

    if let Some(library_path) = &toml.general.library_path {
        let library_path_buf = PathBuf::from(library_path);
        let library_dir = if library_path_buf.is_absolute() {
            library_path_buf
        } else {
            let exe_path = std::env::current_exe()?;
            let exe_dir = exe_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
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
        pdfium_lib_path: toml.general.pdfium_lib,
        jpeg_quality: toml.general.jpeg_quality,
        roi: crate::config::RoiConfig {
            x_start_frac: toml.roi.x_start,
            x_end_frac: toml.roi.x_end,
            y_start_frac: toml.roi.y_start,
            y_end_frac: toml.roi.y_end,
        },
        page_order: crate::config::PageOrderConfig {
            page1: toml.page_order.page1,
            page2: toml.page_order.page2,
            page3: toml.page_order.page3,
            page4: toml.page_order.page4,
        },
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
