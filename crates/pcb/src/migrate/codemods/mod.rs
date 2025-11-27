use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod alias_expansion;
pub mod escape_paths;
pub mod manifest_v2;
pub mod path_correction;
pub mod remove_directory_loads;
pub mod workspace_paths;

/// Context passed to all codemods during migration
#[derive(Debug, Clone)]
pub struct MigrateContext {
    /// Absolute path to workspace root on disk
    pub workspace_root: PathBuf,
    /// Repository URL (e.g., "github.com/user/repo")
    pub repository: String,
    /// Subpath within the git repo if workspace is not at repo root (e.g., "hardware")
    pub repo_subpath: Option<PathBuf>,
}

pub trait Codemod {
    fn apply(&self, ctx: &MigrateContext, path: &Path, content: &str) -> Result<Option<String>>;
}
