use image::DynamicImage;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, Stream};
use pdfium_render::prelude::Pdfium;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::config::Config;
use crate::error::ProcessorError;
use crate::ocr::extract_application_number;
use crate::splitter::render_and_split;

/// The result of processing one pair of scan files
pub struct ProcessResult {
    pub output_path: PathBuf,
    pub student_number: String,
    pub file_a: PathBuf,
    pub file_b: PathBuf,
}

/// Process one matched pair of A3 scan files into a single 4-page A4 PDF.
///
/// ## Page order logic:
/// ```
/// file_A (lower counter, outer sheet):
///   right half → page 1  ← APPLICATION NO. is here
///   left  half → page 4
///
/// file_B (higher counter, inner sheet):
///   right half → page 2
///   left  half → page 3
///
/// Output PDF page order: [page1, page2, page3, page4]
/// ```
pub fn process_pair(
    file_a: &Path,
    file_b: &Path,
    config: &Config,
    pdfium: &Pdfium,
) -> Result<ProcessResult, ProcessorError> {
    info!(
        "Processing pair: {} + {}",
        file_a.file_name().unwrap_or_default().to_string_lossy(),
        file_b.file_name().unwrap_or_default().to_string_lossy(),
    );

    // Step 1: Render and split both A3 PDFs into A4 halves
    info!("  [1/4] Rendering and splitting PDF A...");
    let split_a = render_and_split(file_a, config.dpi, pdfium)?;
    info!("  [1/4] ✓ PDF A rendered");

    info!("  [2/4] Rendering and splitting PDF B...");
    let split_b = render_and_split(file_b, config.dpi, pdfium)?;
    info!("  [2/4] ✓ PDF B rendered");

    // Resolve page order from config — each slot is "A_right", "A_left",
    // "B_right", or "B_left". Change [page_order] in config.toml to fix
    // wrong ordering without recompiling.
    let resolve = |slot: &str| -> DynamicImage {
        match slot {
            "A_right" => split_a.right.clone(),
            "A_left" => split_a.left.clone(),
            "B_right" => split_b.right.clone(),
            "B_left" => split_b.left.clone(),
            other => {
                tracing::warn!("Unknown page_order slot '{}', defaulting to A_right", other);
                split_a.right.clone()
            }
        }
    };

    info!(
        "  Page order: page1={} page2={} page3={} page4={}",
        config.page_order.page1,
        config.page_order.page2,
        config.page_order.page3,
        config.page_order.page4,
    );

    let page1 = resolve(&config.page_order.page1);
    let page2 = resolve(&config.page_order.page2);
    let page3 = resolve(&config.page_order.page3);
    let page4 = resolve(&config.page_order.page4);

    // Step 2: OCR on page 1 ONLY — this is the only page with APPLICATION NO.
    info!("  [3/4] Running OCR on page 1...");
    let student_number = match extract_application_number(&page1, config) {
        Ok(num) => {
            info!("  [3/4] ✓ Student application number: {}", num);
            num
        }
        Err(e) => {
            let fallback = fallback_filename(file_a);
            warn!("  [3/4] ⚠ OCR failed: {}", e);
            warn!("  [3/4]    Cause may be:");
            warn!("  [3/4]    1. TESSDATA_PREFIX not pointing to tessdata folder (check log above for 'TESSDATA_PREFIX =')");
            warn!("  [3/4]    2. ROI coordinates are off — the number isn't in the cropped strip");
            warn!(
                "  [3/4]    3. eng.traineddata not installed: sudo apt install tesseract-ocr-eng"
            );
            warn!(
                "  [3/4]    Page 1 dimensions: {}×{} px",
                page1.width(),
                page1.height()
            );
            warn!(
                "  [3/4]    ROI config: y=[{:.1}%–{:.1}%]  x=[{:.1}%–{:.1}%]",
                config.roi.y_start_frac * 100.0,
                config.roi.y_end_frac * 100.0,
                config.roi.x_start_frac * 100.0,
                config.roi.x_end_frac * 100.0,
            );
            warn!("  [3/4] ⚠ Using fallback name: {}", fallback);
            fallback
        }
    };

    // Save ROI image
    {
        use crate::ocr::crop_roi;
        let roi_pixels = config.roi.to_pixels(page1.width(), page1.height());
        let roi_img = crop_roi(&page1, &roi_pixels);
        let roi_path = config.output_dir.join(format!("{}.png", student_number));
        if let Err(e) = roi_img.save(&roi_path) {
            warn!("  [3/4]    ↳ Could not save ROI PNG: {}", e);
        }
    }

    // Step 3: Assemble the 4 pages into one output PDF
    info!("  [4/4] Assembling output PDF...");
    let output_path = config.output_dir.join(format!("{}.pdf", student_number));

    assemble_pdf(
        &[&page1, &page2, &page3, &page4],
        &output_path,
        config.dpi,
        config.jpeg_quality,
    )?;

    info!("  [4/4] ✓ Output: {}", output_path.display());

    Ok(ProcessResult {
        output_path,
        student_number,
        file_a: file_a.to_path_buf(),
        file_b: file_b.to_path_buf(),
    })
}

/// Assemble multiple A4 page images into a single multi-page PDF using lopdf.
///
/// Each image is embedded as a JPEG-compressed XObject and painted onto a page
/// sized exactly to the image at the given DPI.
///
/// lopdf works at a lower level than PyMuPDF, so we explicitly construct:
///   - A Page dictionary with MediaBox
///   - An XObject stream containing the JPEG image bytes
///   - A content stream with the Do operator to paint the image
fn assemble_pdf(
    pages: &[&DynamicImage],
    output_path: &Path,
    dpi: u32,
    jpeg_quality: u8,
) -> Result<(), ProcessorError> {
    let mut doc = Document::with_version("1.5");

    // PDF catalog and pages node
    let pages_id = doc.new_object_id();
    let mut page_ids: Vec<Object> = Vec::new();

    for (i, img) in pages.iter().enumerate() {
        let width = img.width();
        let height = img.height();

        // Convert pixels → PDF points (1 point = 1/72 inch)
        let width_pt = width as f64 * 72.0 / dpi as f64;
        let height_pt = height as f64 * 72.0 / dpi as f64;

        // Encode image as JPEG into memory buffer
        let jpeg_bytes = encode_jpeg(img, jpeg_quality).map_err(|e| {
            ProcessorError::PdfAssemblyError(format!("JPEG encode page {}: {e}", i + 1))
        })?;

        let jpeg_len = jpeg_bytes.len();

        // Build image XObject dictionary
        let img_dict = Dictionary::from_iter(vec![
            ("Type", Object::Name(b"XObject".to_vec())),
            ("Subtype", Object::Name(b"Image".to_vec())),
            ("Width", Object::Integer(width as i64)),
            ("Height", Object::Integer(height as i64)),
            ("ColorSpace", Object::Name(b"DeviceRGB".to_vec())),
            ("BitsPerComponent", Object::Integer(8)),
            ("Filter", Object::Name(b"DCTDecode".to_vec())), // DCT = JPEG
            ("Length", Object::Integer(jpeg_len as i64)),
        ]);

        let img_stream = Stream::new(img_dict, jpeg_bytes);
        let img_id = doc.add_object(img_stream);

        // Content stream: scale image to fill page, then paint it
        // q = save graphics state
        // {w} 0 0 {h} 0 0 cm = transformation matrix (scale to page size in points)
        // /Im Do = paint the image XObject named "Im"
        // Q = restore graphics state
        let content_ops = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        width_pt.into(),
                        0.into(),
                        0.into(),
                        height_pt.into(),
                        0.into(),
                        0.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"Im".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };

        let content_bytes = content_ops
            .encode()
            .map_err(|e| ProcessorError::PdfAssemblyError(format!("encode content: {e}")))?;

        let content_stream = Stream::new(Dictionary::new(), content_bytes);
        let content_id = doc.add_object(content_stream);

        // Resources dictionary: our image XObject is named "Im"
        let xobject_dict = Dictionary::from_iter(vec![("Im", Object::Reference(img_id))]);
        let resources = Dictionary::from_iter(vec![("XObject", Object::Dictionary(xobject_dict))]);

        // Page dictionary
        let page_dict = Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Page".to_vec())),
            ("Parent", Object::Reference(pages_id)),
            (
                "MediaBox",
                Object::Array(vec![0.into(), 0.into(), width_pt.into(), height_pt.into()]),
            ),
            ("Contents", Object::Reference(content_id)),
            ("Resources", Object::Dictionary(resources)),
        ]);

        let page_id = doc.add_object(page_dict);
        page_ids.push(Object::Reference(page_id));
    }

    // Pages node (parent of all pages)
    let pages_dict = Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Pages".to_vec())),
        ("Count", Object::Integer(page_ids.len() as i64)),
        ("Kids", Object::Array(page_ids)),
    ]);
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));

    // Catalog
    let catalog = Dictionary::from_iter(vec![
        ("Type", Object::Name(b"Catalog".to_vec())),
        ("Pages", Object::Reference(pages_id)),
    ]);
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));

    // Write to file
    doc.save(output_path)
        .map_err(|e| ProcessorError::PdfAssemblyError(format!("save PDF: {e}")))?;

    Ok(())
}

/// Encode a DynamicImage as JPEG bytes with the given quality (0–100).
fn encode_jpeg(img: &DynamicImage, quality: u8) -> anyhow::Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Cursor::new(Vec::new());

    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    encoder.encode_image(&rgb)?;

    Ok(buf.into_inner())
}

fn fallback_filename(file_a: &Path) -> String {
    let name = file_a
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    if name.len() >= 9 {
        name[3..9].to_string()
    } else {
        name.to_string()
    }
}
