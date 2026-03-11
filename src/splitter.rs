use anyhow::Context;
use image::{DynamicImage, RgbImage};
use pdfium_render::prelude::*;
use std::path::Path;
use tracing::debug;

use crate::error::ProcessorError;

/// Holds both A4 halves split from one A3 scan file.
pub struct A3Split {
    /// Left A4 half  (for file_A: contains page 4 — declaration/back page)
    pub left: DynamicImage,
    /// Right A4 half (for file_A: contains page 1 — APPLICATION FORM with student number)
    pub right: DynamicImage,
}

/// Render the first page of a PDF to an RGB image at the given DPI,
/// then split it vertically into left and right A4 halves.
///
/// Uses pdfium-render which wraps Google's PDFium library (same engine as Chrome).
/// PDFium handles even malformed or complex PDFs reliably.
///
/// # Arguments
/// * `pdf_path`    - path to the input PDF file
/// * `dpi`         - render resolution (300 recommended for OCR quality)
/// * `pdfium`      - initialized Pdfium instance (create once, reuse)
pub fn render_and_split(
    pdf_path: &Path,
    dpi: u32,
    pdfium: &Pdfium,
) -> Result<A3Split, ProcessorError> {
    debug!("Rendering PDF: {}", pdf_path.display());

    // Open the PDF document
    let doc =
        pdfium
            .load_pdf_from_file(pdf_path, None)
            .map_err(|e| ProcessorError::PdfRenderError {
                path: pdf_path.display().to_string(),
                source: anyhow::anyhow!("pdfium open error: {:?}", e),
            })?;

    // We always take page 0 — scanner produces single-page PDFs
    let page = doc
        .pages()
        .get(0)
        .map_err(|e| ProcessorError::PdfRenderError {
            path: pdf_path.display().to_string(),
            source: anyhow::anyhow!("cannot get page 0: {:?}", e),
        })?;

    // Scale factor: PDF points are 72 per inch, we want `dpi` pixels per inch
    let scale = dpi as f32 / 72.0;

    let render_config = PdfRenderConfig::new()
        .scale_page_by_factor(scale)
        .render_annotations(true)
        .render_form_data(true);

    // Render page → RGBA bitmap
    let bitmap =
        page.render_with_config(&render_config)
            .map_err(|e| ProcessorError::PdfRenderError {
                path: pdf_path.display().to_string(),
                source: anyhow::anyhow!("render error: {:?}", e),
            })?;

    // Convert PDFium bitmap → image::DynamicImage (RGB)
    let rgba_image = bitmap.as_image();

    debug!(
        "Rendered {}  →  {}×{} px",
        pdf_path.file_name().unwrap_or_default().to_string_lossy(),
        rgba_image.width(),
        rgba_image.height()
    );

    split_vertical(rgba_image)
}

/// Split an A3 image vertically at the horizontal midpoint into two A4 halves.
///
/// The scanner outputs A3 landscape, so the midpoint split yields two portrait A4 pages.
fn split_vertical(img: DynamicImage) -> Result<A3Split, ProcessorError> {
    let width = img.width();
    let height = img.height();

    if width < 2 {
        return Err(ProcessorError::ImageSplitError { width });
    }

    let mid = width / 2;

    // image::crop_imm(x, y, width, height)
    let left = img.crop_imm(0, 0, mid, height);
    let right = img.crop_imm(mid, 0, width - mid, height);

    debug!("Split A3 ({width}×{height}) → two A4s ({mid}×{height})");

    Ok(A3Split { left, right })
}

/// Initialize the PDFium library from a shared library path.
/// Call this ONCE at startup and pass the returned Pdfium to all render calls.
///
/// Download PDFium binary from:
///   https://github.com/bblanchon/pdfium-binaries/releases
/// Extract libpdfium.so (Linux) / pdfium.dll (Windows) / libpdfium.dylib (macOS)
pub fn init_pdfium(lib_path: &Path) -> anyhow::Result<Pdfium> {
    let bindings = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(
        lib_path.to_str().unwrap_or("."),
    ))
    .or_else(|_| Pdfium::bind_to_system_library())
    .map_err(|e| {
        anyhow::anyhow!(
            "Cannot load PDFium library from '{}'. \
         Download from https://github.com/bblanchon/pdfium-binaries/releases\n\
         Error: {:?}",
            lib_path.display(),
            e
        )
    })?;

    Ok(Pdfium::new(bindings))
}
