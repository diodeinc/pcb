use anyhow::Result;
use ignore::WalkBuilder;
use pcb_zen::file_extensions;
use std::path::{Path, PathBuf};

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
            .filter_entry(|entry| {
                // Skip vendor directories
                if entry.file_type().is_some_and(|ft| ft.is_dir()) {
                    if let Some(name) = entry.file_name().to_str() {
                        if name == "vendor" {
                            return false;
                        }
                    }
                }
                true
            });

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
