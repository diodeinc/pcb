use anyhow::Result;
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

/// Walk directories and process .zen files with a callback
///
/// Features:
/// - Always recursive traversal
/// - Always skips vendor/ directories  
/// - Always respects git ignore patterns
/// - Filters to .zen files only
pub fn walk_zen_files<F>(
    paths: &[impl AsRef<Path>],
    hidden: bool,
    mut processor: F,
) -> Result<usize>
where
    F: FnMut(&Path) -> Result<()>,
{
    let walk_paths: Vec<_> = if paths.is_empty() {
        vec![std::env::current_dir()?]
    } else {
        paths.iter().map(|p| p.as_ref().to_path_buf()).collect()
    };

    let mut found_files = 0;

    let Some((first, rest)) = walk_paths.split_first() else {
        return Ok(0);
    };
    let mut builder = WalkBuilder::new(first);
    for path in rest {
        builder.add(path);
    }
    builder
        .hidden(!hidden)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(skip_vendor);

    for result in builder.build() {
        let entry = result?;
        let path = entry.path();

        // Only process .zen files
        if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
            processor(path)?;
            found_files += 1;
        }
    }

    Ok(found_files)
}

/// Walk directories and collect .zen file paths into a Vec
///
/// Features:
/// - Always recursive traversal
/// - Always skips vendor/ directories  
/// - Always respects git ignore patterns
/// - Filters to .zen files only
/// - Returns deterministically sorted paths
pub fn collect_zen_files(paths: &[impl AsRef<Path>], hidden: bool) -> Result<Vec<PathBuf>> {
    let mut zen_files = Vec::new();
    walk_zen_files(paths, hidden, |path| {
        zen_files.push(path.to_path_buf());
        Ok(())
    })?;
    zen_files.sort(); // Deterministic ordering
    Ok(zen_files)
}

/// Collect .zen files respecting V2 workspace boundaries.
///
/// In V2 mode: canonicalizes paths, collects files, and filters to workspace members only.
/// In V1 mode: simply collects files from the given paths.
///
/// Returns `CollectZenFilesError::NoFilesFound` if no files found.
pub fn collect_workspace_zen_files(
    paths: &[PathBuf],
    workspace_info: &pcb_zen::WorkspaceInfo,
) -> Result<Vec<PathBuf>, CollectZenFilesError> {
    // Canonicalize paths in V2 mode
    let search_paths: Vec<PathBuf> = if workspace_info.is_v2() && !paths.is_empty() {
        paths
            .iter()
            .map(|p| p.canonicalize())
            .collect::<Result<Vec<_>, _>>()?
    } else {
        paths.to_vec()
    };

    let mut zen_files = collect_zen_files(&search_paths, false)?;

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
        return Err(CollectZenFilesError::NoFilesFound(std::env::current_dir()?));
    }

    Ok(zen_files)
}
