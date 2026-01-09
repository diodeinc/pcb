use anyhow::{bail, Result};
use ignore::WalkBuilder;
use pcb_zen::file_extensions;
use std::path::{Path, PathBuf};

pub use pcb_zen::ast_utils::skip_vendor;

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

    for root in walk_paths {
        let mut builder = WalkBuilder::new(&root);

        // Always use these settings: recursive, respect gitignore, skip vendor
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
/// Returns deterministically sorted paths. Errors if no files are found.
pub fn collect_workspace_zen_files(
    paths: &[PathBuf],
    workspace_info: &pcb_zen::WorkspaceInfo,
) -> Result<Vec<PathBuf>> {
    let zen_files = if workspace_info.is_v2() {
        // Canonicalize input paths (or use current dir if empty)
        let search_paths: Vec<PathBuf> = if paths.is_empty() {
            vec![std::env::current_dir()?]
        } else {
            paths
                .iter()
                .map(|p| p.canonicalize())
                .collect::<Result<Vec<_>, _>>()?
        };

        // Collect all .zen files from search paths
        let all_zen_files = collect_zen_files(&search_paths, false)?;

        // Skip filtering if no packages discovered (e.g., standalone inline manifest)
        if workspace_info.packages.is_empty() {
            all_zen_files
        } else {
            // Filter to only include files within workspace member packages
            all_zen_files
                .into_iter()
                .filter(|zen_path| {
                    workspace_info
                        .packages
                        .values()
                        .any(|pkg| zen_path.starts_with(pkg.dir(&workspace_info.root)))
                })
                .collect()
        }
    } else {
        // V1 mode: collect zen files from the given paths (or current dir)
        collect_zen_files(paths, false)?
    };

    if zen_files.is_empty() {
        let cwd = std::env::current_dir()?;
        bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    Ok(zen_files)
}
