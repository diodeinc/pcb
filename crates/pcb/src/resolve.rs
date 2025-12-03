use std::path::Path;

use anyhow::Result;
use pcb_zen_core::DefaultFileProvider;

/// Resolve V2 dependencies if the workspace is V2, otherwise return None.
/// This is a shared helper used by build, bom, layout, and open commands.
///
/// If `input_path` is None, defaults to the current working directory.
pub fn resolve_v2_if_needed(
    input_path: Option<&Path>,
    offline: bool,
) -> Result<(pcb_zen::WorkspaceInfo, Option<pcb_zen::ResolutionResult>)> {
    let cwd;
    let path = match input_path {
        Some(p) => p,
        None => {
            cwd = std::env::current_dir()?;
            &cwd
        }
    };
    let mut workspace_info = pcb_zen::get_workspace_info(&DefaultFileProvider::new(), path)?;

    let resolution = if workspace_info.is_v2() {
        let res = pcb_zen::resolve_dependencies(&mut workspace_info, offline)?;
        pcb_zen::vendor_deps(&workspace_info, &res, &[], None)?;
        Some(res)
    } else {
        None
    };

    Ok((workspace_info, resolution))
}
