use anyhow::{bail, Result};
use ignore::WalkBuilder;
use pcb_zen::file_extensions;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub use pcb_zen::ast_utils::skip_vendor;

#[derive(Debug, Error)]
pub enum CollectZenFilesError {
    #[error("No .zen source files found in {}", .0.canonicalize().unwrap_or_else(|_| .0.clone()).display())]
    NoFilesFound(PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Walk(#[from] ignore::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Validate that a path is a .zen file (not a directory or other file type).
/// Used by file-level commands (bom, sim, layout, open, release).
pub fn require_zen_file(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("File not found: {}", path.display());
    }
    if path.is_dir() {
        // Look for .zen files in the directory to provide a helpful suggestion
        let zen_files = collect_zen_files(&[path.to_path_buf()]).unwrap_or_default();
        let hint = match zen_files.as_slice() {
            [] => format!("No .zen files found in {}", path.display()),
            [file] => format!("Did you mean: {}?", file.display()),
            [first, ..] => format!("Did you mean: {}?", first.display()),
        };
        bail!(
            "Expected a .zen file, got a directory: {}\n{}",
            path.display(),
            hint
        );
    }
    if !file_extensions::is_starlark_file(path.extension()) {
        bail!("Expected a .zen file, got: {}", path.display());
    }
    Ok(())
}

/// Collect .zen file paths from a directory.
///
/// Features:
/// - Always recursive traversal
/// - Always skips vendor/ and hidden directories
/// - Always respects git ignore patterns
/// - Returns deterministically sorted paths
pub fn collect_zen_files(paths: &[impl AsRef<Path>]) -> Result<Vec<PathBuf>> {
    let walk_paths: Vec<_> = if paths.is_empty() {
        vec![std::env::current_dir()?]
    } else {
        paths.iter().map(|p| p.as_ref().to_path_buf()).collect()
    };

    let Some((first, rest)) = walk_paths.split_first() else {
        return Ok(vec![]);
    };

    let mut builder = WalkBuilder::new(first);
    for path in rest {
        builder.add(path);
    }
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(skip_vendor);

    let mut zen_files = Vec::new();
    for result in builder.build() {
        let entry = result?;
        let path = entry.path();
        if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
            zen_files.push(path.to_path_buf());
        }
    }

    zen_files.sort();
    Ok(zen_files)
}

/// Collect .zen files respecting V2 workspace boundaries.
///
/// In V2 mode: canonicalizes path, collects files, and filters to workspace members only.
/// In V1 mode: simply collects files from the given path.
/// Defaults to current directory if path is None.
///
/// Returns `CollectZenFilesError::NoFilesFound` if no files found.
pub fn collect_workspace_zen_files(
    path: Option<&Path>,
    workspace_info: &pcb_zen::WorkspaceInfo,
) -> Result<Vec<PathBuf>, CollectZenFilesError> {
    let path = path.unwrap_or(Path::new(".")).canonicalize()?;
    let mut zen_files = collect_zen_files(std::slice::from_ref(&path))?;

    // In V2 mode, filter to workspace member packages only
    if workspace_info.is_v2() && !workspace_info.packages.is_empty() {
        zen_files.retain(|p| {
            workspace_info
                .packages
                .values()
                .any(|pkg| p.starts_with(pkg.dir(&workspace_info.root)))
        });
    }

    if zen_files.is_empty() {
        return Err(CollectZenFilesError::NoFilesFound(path));
    }

    Ok(zen_files)
}
