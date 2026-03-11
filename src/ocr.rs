use image::{DynamicImage, GrayImage, ImageBuffer, ImageEncoder, Luma};
use leptess::LepTess;
use regex::Regex;
use std::io::Cursor;
use std::path::Path;
use std::sync::OnceLock;
use tracing::{debug, warn};

use crate::config::{Config, RoiPixels};
use crate::error::ProcessorError;

/// Regex matching a student application number: 4–12 consecutive digits.
/// Compiled once at first use via OnceLock.
static STUDENT_NO_RE: OnceLock<Regex> = OnceLock::new();

fn student_no_regex() -> &'static Regex {
    STUDENT_NO_RE
        .get_or_init(|| Regex::new(r"\b(\d{4,12})\b").expect("invalid student number regex"))
}

/// Crop the ROI strip from a full A4 image.
///
/// The APPLICATION NO. field is in the upper portion of page 1 (right half of file_A).
/// We crop ONLY that narrow horizontal band — this means Tesseract processes
/// ~8% of the page area instead of 100%, making OCR ~10x faster and more accurate.
pub fn crop_roi(img: &DynamicImage, roi: &RoiPixels) -> DynamicImage {
    img.crop_imm(roi.x1, roi.y1, roi.width(), roi.height())
}

/// Preprocess the ROI image for optimal Tesseract accuracy:
///   1. Convert to grayscale
///   2. Upscale 2× (Tesseract performs better on larger text)
///   3. Apply adaptive thresholding to produce clean black-on-white binary image
///      (handles uneven scanner lighting and slight yellowing)
pub fn preprocess_for_ocr(roi: &DynamicImage) -> GrayImage {
    // 1. Grayscale
    let gray = roi.to_luma8();

    // 2. Upscale 2× with Lanczos3 for crisp character edges
    let scaled = image::imageops::resize(
        &gray,
        gray.width() * 2,
        gray.height() * 2,
        image::imageops::FilterType::Lanczos3,
    );

    // 3. Adaptive threshold (Sauvola-style approximation via local mean)
    //    Window size of 31 handles the ~20px tall digits at 300 DPI × 2 scale
    adaptive_threshold(&scaled, 31, 10)
}

/// Simple adaptive threshold: each pixel compared to local mean minus constant C.
/// Equivalent to OpenCV's adaptiveThreshold with ADAPTIVE_THRESH_MEAN_C.
fn adaptive_threshold(img: &GrayImage, window: u32, c: i32) -> GrayImage {
    let (w, h) = img.dimensions();
    let half = (window / 2) as i64;

    ImageBuffer::from_fn(w, h, |x, y| {
        // Clamp window to image bounds
        let x0 = (x as i64 - half).max(0) as u32;
        let y0 = (y as i64 - half).max(0) as u32;
        let x1 = (x as i64 + half).min(w as i64 - 1) as u32;
        let y1 = (y as i64 + half).min(h as i64 - 1) as u32;

        // Compute local mean
        let mut sum: u64 = 0;
        let mut count: u64 = 0;
        for py in y0..=y1 {
            for px in x0..=x1 {
                sum += img.get_pixel(px, py).0[0] as u64;
                count += 1;
            }
        }
        let mean = (sum / count) as i32;
        let pixel_val = img.get_pixel(x, y).0[0] as i32;

        // Pixel is foreground (black=0) if it's darker than mean - C
        if pixel_val < mean - c {
            Luma([0u8]) // black — ink
        } else {
            Luma([255u8]) // white — background
        }
    })
}

/// Extract the student application number from the right half of file_A (page 1).
///
/// Strategy:
///   1. Crop the tight ROI band (only ~8% of page area)  
///   2. Preprocess to clean binary image
///   3. Run Tesseract restricted to digits + relevant chars
///   4. Extract longest digit sequence matching the student number pattern
///
/// Returns the number as a String, or an error if OCR yields nothing valid.
pub fn extract_application_number(
    page1_image: &DynamicImage,
    config: &Config,
) -> Result<String, ProcessorError> {
    let roi_pixels = config
        .roi
        .to_pixels(page1_image.width(), page1_image.height());

    debug!(
        "Cropping ROI: x=[{}–{}] y=[{}–{}]  ({}×{}px)",
        roi_pixels.x1,
        roi_pixels.x2,
        roi_pixels.y1,
        roi_pixels.y2,
        roi_pixels.width(),
        roi_pixels.height(),
    );

    // Step 1: Crop tight ROI — only this strip goes to OCR
    let roi_img = crop_roi(page1_image, &roi_pixels);

    // Step 2: Preprocess
    let binary = preprocess_for_ocr(&roi_img);

    // Step 3: Save debug image if requested
    if config.debug_roi {
        let debug_path = config.output_dir.join("_debug_roi.png");
        binary
            .save(&debug_path)
            .unwrap_or_else(|e| warn!("Could not save debug ROI image: {e}"));
        debug!("Saved ROI debug image: {}", debug_path.display());
    }

    // Step 4: Run Tesseract OCR
    let ocr_text = run_tesseract(&binary, &config.tessdata_path)?;

    debug!("OCR raw output: {:?}", ocr_text);

    // Step 5: Extract student number via regex
    extract_number_from_text(&ocr_text)
}

/// Run Tesseract on a grayscale image and return the extracted text.
///
/// Configuration:
///   - Language: English
///   - PSM 6: Assume a uniform block of text (best for a header region)
///   - Whitelist: digits only to reduce false positives
fn run_tesseract(img: &GrayImage, tessdata_path: &Path) -> Result<String, ProcessorError> {
    let datapath_str = tessdata_path
        .to_str()
        .ok_or_else(|| ProcessorError::OcrInitError("tessdata path is not valid UTF-8".into()))?;

    let mut tess = LepTess::new(Some(datapath_str), "eng").map_err(|e| {
        ProcessorError::OcrInitError(format!(
            "{e} — tried: '{}' (from config: '{}')",
            datapath_str,
            tessdata_path.display()
        ))
    })?;

    // PSM 7 = Treat image as a single text line — better than PSM 6 for a single
    // number sitting on one line (PSM 6 was treating it as a block, causing misreads)
    tess.set_variable(leptess::Variable::TesseditPagesegMode, "7")
        .map_err(|e| ProcessorError::OcrProcessError(format!("set PSM: {e}")))?;

    // Whitelist: DIGITS ONLY. The number is the only thing we need — every extra
    // character in the whitelist gives Tesseract more chances to misread a digit.
    // Previously "STUDENAPPLIOCATIO. :" was included, causing '2'→'8', '1'→'4' etc.
    tess.set_variable(leptess::Variable::TesseditCharWhitelist, "0123456789")
        .map_err(|e| ProcessorError::OcrProcessError(format!("set whitelist: {e}")))?;

    // Feed the image as a PNG buffer — set_image_from_mem needs an encoded image
    // (PNG/TIFF/etc.), NOT raw pixel bytes. Passing as_raw() caused silent failures.
    let png_bytes = {
        let mut buf = Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::L8,
            )
            .map_err(|e| ProcessorError::OcrProcessError(format!("PNG encode for OCR: {e}")))?;
        buf.into_inner()
    };

    tess.set_image_from_mem(&png_bytes)
        .map_err(|e| ProcessorError::OcrProcessError(format!("set image: {e}")))?;

    tess.set_source_resolution(600); // 300 DPI × 2 from our upscaling

    let text = tess
        .get_utf8_text()
        .map_err(|e| ProcessorError::OcrProcessError(format!("get text: {e}")))?;

    Ok(text.trim().to_string())
}

/// From raw OCR text, extract the student application number.
///
/// Picks the longest digit sequence that matches 4–12 digits.
/// In practice the number ("21143") is clearly the only candidate in the ROI.
fn extract_number_from_text(text: &str) -> Result<String, ProcessorError> {
    let re = student_no_regex();

    let candidates: Vec<&str> = re
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();

    if candidates.is_empty() {
        return Err(ProcessorError::ApplicationNumberNotFound {
            ocr_text: text.to_string(),
        });
    }

    // Pick the longest candidate (application numbers tend to be 5–6 digits)
    let best = candidates.into_iter().max_by_key(|s| s.len()).unwrap(); // safe: we checked non-empty above

    Ok(best.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_from_clean_text() {
        let text = "STUDENT'S APPLICATION NO. 21143";
        assert_eq!(extract_number_from_text(text).unwrap(), "21143");
    }

    #[test]
    fn test_extract_prefers_longer() {
        let text = "NO. 21143 DATE 09";
        assert_eq!(extract_number_from_text(text).unwrap(), "21143");
    }

    #[test]
    fn test_extract_fails_gracefully() {
        let text = "no numbers here at all";
        assert!(extract_number_from_text(text).is_err());
    }
}
