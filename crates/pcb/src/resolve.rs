use std::path::Path;

use anyhow::{Result, bail};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::resolution::ResolutionResult;
use tracing::instrument;

use pcb_zen::{get_workspace_info, resolve_dependencies};

#[instrument(name = "vendor", skip_all)]
fn vendor(res: &ResolutionResult, prune: bool) -> Result<pcb_zen::VendorResult> {
    pcb_zen::vendor_deps(res, &[], None, prune)
}

/// Resolve dependencies for a workspace/board.
/// This is a shared helper used by build, bom, layout, open, etc.
///
/// If `input_path` is None or empty, defaults to the current working directory.
///
/// When `locked` is true:
/// - Auto-deps will not modify pcb.toml files
/// - The lockfile (pcb.sum) will not be written
/// - Resolution will fail if pcb.toml or pcb.sum would need to be modified
#[instrument(name = "resolve_dependencies", skip_all)]
pub fn resolve(input_path: Option<&Path>, offline: bool, locked: bool) -> Result<ResolutionResult> {
    let cwd;
    let path = match input_path {
        // Handle both None and empty paths (e.g., "file.zen".parent() returns Some(""))
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            cwd = std::env::current_dir()?;
            &cwd
        }
    };
    let mut workspace_info = get_workspace_info(&DefaultFileProvider::new(), path)?;

    // Fail on workspace discovery errors (invalid pcb.toml files)
    if !workspace_info.errors.is_empty() {
        for err in &workspace_info.errors {
            eprintln!("{}: {}", err.path.display(), err.error);
        }
        bail!(
            "Found {} invalid pcb.toml file(s)",
            workspace_info.errors.len()
        );
    }

    let mut res = resolve_dependencies(&mut workspace_info, offline, locked)?;

    // Sync vendor dir: add missing, prune stale (only prune when not offline and not locked)
    let prune = !offline && !locked;
    let vendor_result = vendor(&res, prune)?;

    // If we pruned stale entries, re-run resolution so the dep map points to valid paths
    if vendor_result.pruned_count > 0 {
        log::debug!(
            "Pruned {} stale vendor entries, re-running resolution",
            vendor_result.pruned_count
        );
        res = resolve_dependencies(&mut workspace_info, offline, locked)?;
    }

    Ok(res)
}
