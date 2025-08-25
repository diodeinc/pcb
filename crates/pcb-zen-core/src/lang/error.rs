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
