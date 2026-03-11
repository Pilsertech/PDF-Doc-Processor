use crate::error::ProcessorError;
use std::path::Path;

/// Extract the 6-digit incrementing counter from a scanner filename.
///
/// Filename format: `doc{NNNNNN}{timestamp}.pdf`
/// Example: `doc00488320260310093843.pdf` → 4883
///
/// The counter is at byte positions 3..9 (0-indexed, exclusive end).
pub fn extract_counter(path: &Path) -> Result<u32, ProcessorError> {
    let filename = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        ProcessorError::FilenameParseError {
            filename: path.display().to_string(),
            reason: "cannot read filename as UTF-8".into(),
        }
    })?;

    if filename.len() < 9 {
        return Err(ProcessorError::FilenameParseError {
            filename: filename.to_string(),
            reason: format!("filename too short (need ≥9 chars, got {})", filename.len()),
        });
    }

    // Counter is at positions 3..9 inclusive (6 digits)
    let counter_str = &filename[3..9];

    counter_str
        .parse::<u32>()
        .map_err(|e| ProcessorError::FilenameParseError {
            filename: filename.to_string(),
            reason: format!("counter segment '{}' is not a number: {}", counter_str, e),
        })
}

/// Returns true if the path looks like a scanner output file we should process.
pub fn is_scan_file(path: &Path) -> bool {
    let ext_ok = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false);

    let name_ok = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("doc"))
        .unwrap_or(false);

    ext_ok && name_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_extract_counter_valid() {
        let p = PathBuf::from("doc00488320260310093843.pdf");
        assert_eq!(extract_counter(&p).unwrap(), 4883);
    }

    #[test]
    fn test_extract_counter_second_file() {
        let p = PathBuf::from("doc00488420260310094018.pdf");
        assert_eq!(extract_counter(&p).unwrap(), 4884);
    }

    #[test]
    fn test_is_scan_file_valid() {
        assert!(is_scan_file(Path::new("doc00488320260310093843.pdf")));
    }

    #[test]
    fn test_is_scan_file_invalid() {
        assert!(!is_scan_file(Path::new("output_12345.pdf")));
        assert!(!is_scan_file(Path::new("doc004883.txt")));
    }
}
