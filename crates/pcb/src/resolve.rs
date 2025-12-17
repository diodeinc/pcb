use std::path::Path;

use anyhow::Result;
use pcb_zen_core::DefaultFileProvider;

/// Resolve V2 dependencies if the workspace is V2, otherwise return None.
/// This is a shared helper used by build, bom, layout, and open commands.
///
/// If `input_path` is None or empty, defaults to the current working directory.
///
/// When `locked` is true:
/// - Auto-deps will not modify pcb.toml files
/// - The lockfile (pcb.sum) will not be written
/// - Resolution will fail if pcb.toml or pcb.sum would need to be modified
pub fn resolve_v2_if_needed(
    input_path: Option<&Path>,
    offline: bool,
    locked: bool,
) -> Result<(pcb_zen::WorkspaceInfo, Option<pcb_zen::ResolutionResult>)> {
    let cwd;
    let path = match input_path {
        // Handle both None and empty paths (e.g., "file.zen".parent() returns Some(""))
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            cwd = std::env::current_dir()?;
            &cwd
        }
    };
    let mut workspace_info = pcb_zen::get_workspace_info(&DefaultFileProvider::new(), path)?;

    let resolution = if workspace_info.is_v2() {
        let mut res = pcb_zen::resolve_dependencies(&mut workspace_info, offline, locked)?;

        // Sync vendor dir: add missing, prune stale (only prune when not offline)
        let vendor_result = pcb_zen::vendor_deps(&workspace_info, &res, &[], None, !offline)?;

        // If we pruned stale entries, re-run resolution so the dep map points to valid paths
        if vendor_result.pruned_count > 0 {
            log::debug!(
                "Pruned {} stale vendor entries, re-running resolution",
                vendor_result.pruned_count
            );
            res = pcb_zen::resolve_dependencies(&mut workspace_info, offline, locked)?;
        }
        Some(res)
    } else {
        None
    };

    Ok((workspace_info, resolution))
}
