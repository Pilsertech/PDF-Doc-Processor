use std::path::PathBuf;

/// ROI (Region of Interest) as fractions of the A4 page dimensions.
/// These fractions are DPI-independent and were calibrated from the
/// Nyunga Foundation Bursary Application Form sample:
///   - APPLICATION NO. box sits at ~y=16%–23%, x=8%–92% of the right A4 half
#[derive(Debug, Clone)]
pub struct RoiConfig {
    /// Left edge of ROI (0.0 = page left, 1.0 = page right)
    pub x_start_frac: f32,
    /// Right edge of ROI
    pub x_end_frac: f32,
    /// Top edge of ROI (0.0 = page top, 1.0 = page bottom)
    pub y_start_frac: f32,
    /// Bottom edge of ROI
    pub y_end_frac: f32,
}

impl RoiConfig {
    /// Compute pixel coordinates for a page of given dimensions
    pub fn to_pixels(&self, page_width: u32, page_height: u32) -> RoiPixels {
        RoiPixels {
            x1: (page_width as f32 * self.x_start_frac) as u32,
            x2: (page_width as f32 * self.x_end_frac) as u32,
            y1: (page_height as f32 * self.y_start_frac) as u32,
            y2: (page_height as f32 * self.y_end_frac) as u32,
        }
    }
}

/// Pixel coordinates of the OCR region of interest
#[derive(Debug, Clone, Copy)]
pub struct RoiPixels {
    pub x1: u32,
    pub x2: u32,
    pub y1: u32,
    pub y2: u32,
}

impl RoiPixels {
    pub fn width(&self) -> u32 {
        self.x2 - self.x1
    }
    pub fn height(&self) -> u32 {
        self.y2 - self.y1
    }
}

/// Top-level application configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory to watch for incoming PDFs
    pub watch_dir: PathBuf,

    /// Directory to write output PDFs
    pub output_dir: PathBuf,

    /// DPI for rendering PDF pages to images
    pub dpi: u32,

    /// Path to Tesseract tessdata directory
    pub tessdata_path: PathBuf,

    /// Path to the PDFium shared library binary
    pub pdfium_lib_path: PathBuf,

    /// OCR region of interest configuration
    pub roi: RoiConfig,

    /// Whether to save ROI debug images alongside output
    pub debug_roi: bool,
}

impl Default for RoiConfig {
    fn default() -> Self {
        Self {
            x_start_frac: 0.08,
            x_end_frac: 0.92,
            y_start_frac: 0.155,
            y_end_frac: 0.235,
        }
    }
}
