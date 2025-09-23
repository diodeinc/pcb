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
