use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use pcb_zen::file_extensions;
use pcb_zen::workspace::{get_workspace_info, WorkspaceInfo, WorkspaceInfoExt};
use pcb_zen_core::DefaultFileProvider;
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

/// Collect .zen files.
///
/// Canonicalizes path, collects files, and filters to workspace members only.
/// Defaults to current directory if path is None.
///
/// Returns `CollectZenFilesError::NoFilesFound` if no files found.
pub fn collect_workspace_zen_files(
    path: Option<&Path>,
    workspace_info: &pcb_zen::WorkspaceInfo,
) -> Result<Vec<PathBuf>, CollectZenFilesError> {
    let path = path.unwrap_or(Path::new(".")).canonicalize()?;
    let mut zen_files = collect_zen_files(std::slice::from_ref(&path))?;

    // filter to workspace member packages only
    if !workspace_info.packages.is_empty() {
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

/// Resolved board target containing workspace, path, and board name.
pub struct BoardTarget {
    pub workspace: WorkspaceInfo,
    pub zen_path: PathBuf,
    pub board_name: String,
    pub pkg_rel_path: PathBuf,
}

/// Resolve a .zen file path to a validated board target.
///
/// Validates that:
/// - The path is a valid .zen file
/// - The workspace is valid (no pcb.toml errors)
/// - The file belongs to a board package (has [board] section in pcb.toml)
pub fn resolve_board_target(path: &Path, action: &str) -> Result<BoardTarget> {
    require_zen_file(path)?;
    let file_provider = DefaultFileProvider::new();
    let start_path = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let workspace = get_workspace_info(&file_provider, start_path)?;

    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let zen_path = path.canonicalize().context("Board file not found")?;
    let pkg_url = workspace
        .package_url_for_zen(&zen_path)
        .ok_or_else(|| anyhow::anyhow!("File not found in workspace: {}", path.display()))?;
    let pkg = &workspace.packages[&pkg_url];
    if pkg.config.board.is_none() {
        bail!(
            "Not a board package: {}\n\nTo {} a board, the package's pcb.toml must have a [board] section.",
            path.display(),
            action
        );
    }

    let board_name = workspace
        .board_name_for_zen(&zen_path)
        .unwrap_or_else(|| zen_path.file_stem().unwrap().to_string_lossy().to_string());

    let pkg_rel_path = pkg.rel_path.clone();

    Ok(BoardTarget {
        workspace,
        zen_path,
        board_name,
        pkg_rel_path,
    })
}
