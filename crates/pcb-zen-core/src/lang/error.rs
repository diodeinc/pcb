use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),
}

/// Information about an unstable reference warning.
#[derive(Debug, Error, Clone)]
#[error("Unstable reference detected")]
pub struct UnstableRefError {
    /// Complete chain of LoadSpec transformations from original to final resolution
    pub spec_chain: Vec<crate::LoadSpec>,

    /// The file where the load was called from
    pub calling_file: PathBuf,

    /// Metadata about the remote reference that was unstable
    pub remote_ref_meta: crate::RemoteRefMeta,

    /// The remote reference that caused the warning
    pub remote_ref: crate::RemoteRef,
}

/// Container for diagnostics that were suppressed during aggregation
#[derive(Debug, Error, Clone)]
#[error("Suppressed similar diagnostics")]
pub struct SuppressedDiagnostics {
    /// The diagnostics that were suppressed in favor of a representative diagnostic
    pub suppressed: Vec<crate::Diagnostic>,
}

/// Structured information about a test result from a TestBench check function
#[derive(Debug, Error, Clone)]
#[error("Test result")]
pub struct BenchTestResult {
    /// The name of the TestBench
    pub test_bench_name: String,

    /// The name of the test case (if any)
    pub case_name: Option<String>,

    /// The name of the check function
    pub check_name: String,

    /// The file path where the TestBench was defined
    pub file_path: String,

    /// Whether the test passed or failed
    pub passed: bool,
}

/// Structured diagnostic with a categorization kind for filtering and classification.
///
/// This allows downstream tooling to filter, suppress, or categorize diagnostics
/// based on their kind. The kind is a dot-separated hierarchical identifier.
#[derive(Debug, Error, Clone)]
#[error("{message}")]
pub struct CategorizedDiagnostic {
    /// The diagnostic message
    pub message: String,

    /// The diagnostic kind (e.g., "electrical.voltage_mismatch", "layout.spacing")
    /// Must be non-empty with dot-separated segments, each segment non-empty
    pub kind: String,
}

impl CategorizedDiagnostic {
    pub fn new(message: String, kind: String) -> anyhow::Result<Self> {
        anyhow::ensure!(!kind.is_empty(), "diagnostic kind cannot be empty");
        anyhow::ensure!(
            !kind.split('.').any(|s| s.is_empty()),
            "diagnostic kind has empty segment: '{kind}'"
        );
        Ok(Self { message, kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_categorized_diagnostic_valid_single_segment() {
        let diag = CategorizedDiagnostic::new("test message".to_string(), "electrical".to_string());
        assert!(diag.is_ok());
        let diag = diag.unwrap();
        assert_eq!(diag.message, "test message");
        assert_eq!(diag.kind, "electrical");
    }

    #[test]
    fn test_categorized_diagnostic_valid_multiple_segments() {
        let diag = CategorizedDiagnostic::new(
            "voltage mismatch".to_string(),
            "electrical.voltage_mismatch".to_string(),
        );
        assert!(diag.is_ok());
        let diag = diag.unwrap();
        assert_eq!(diag.kind, "electrical.voltage_mismatch");
    }

    #[test]
    fn test_categorized_diagnostic_valid_deeply_nested() {
        let diag = CategorizedDiagnostic::new(
            "spacing violation".to_string(),
            "layout.spacing.trace.minimum".to_string(),
        );
        assert!(diag.is_ok());
    }

    #[test]
    fn test_categorized_diagnostic_empty_kind() {
        let diag = CategorizedDiagnostic::new("test message".to_string(), "".to_string());
        assert!(diag.is_err());
        assert!(diag.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_categorized_diagnostic_empty_segment() {
        let diag = CategorizedDiagnostic::new(
            "test message".to_string(),
            "electrical..voltage".to_string(),
        );
        assert!(diag.is_err());
        assert!(diag.unwrap_err().to_string().contains("empty segment"));
    }

    #[test]
    fn test_categorized_diagnostic_leading_dot() {
        let diag =
            CategorizedDiagnostic::new("test message".to_string(), ".electrical".to_string());
        assert!(diag.is_err());
    }

    #[test]
    fn test_categorized_diagnostic_trailing_dot() {
        let diag =
            CategorizedDiagnostic::new("test message".to_string(), "electrical.".to_string());
        assert!(diag.is_err());
    }
}
