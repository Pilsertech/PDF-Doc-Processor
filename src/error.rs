use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProcessorError {
    #[error("Failed to parse counter from filename '{filename}': {reason}")]
    FilenameParseError { filename: String, reason: String },

    #[error("PDF rendering failed for '{path}': {source}")]
    PdfRenderError {
        path: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("Image split failed: image width {width} is too small to split")]
    ImageSplitError { width: u32 },

    #[error("OCR initialization failed: {0}")]
    OcrInitError(String),

    #[error("OCR processing failed: {0}")]
    OcrProcessError(String),

    #[error("No student application number found in OCR output: '{ocr_text}'")]
    ApplicationNumberNotFound { ocr_text: String },

    #[error("PDF assembly failed: {0}")]
    PdfAssemblyError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
