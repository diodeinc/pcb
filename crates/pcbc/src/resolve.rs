use std::path::Path;

use anyhow::{Result, bail};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::resolution::ResolutionResult;
use tracing::instrument;

use pcb_zen::{get_workspace_info, resolve_workspace_dependencies};

/// Resolve dependencies for read-style commands such as build, bom, layout, and open.
///
/// If `input_path` is None or empty, defaults to the current working directory.
///
/// This helper must not modify source dependency state. Dependency hydration and vendoring
/// belong to explicit write commands such as `pcb sync` and `pcb vendor`.
#[instrument(name = "resolve_dependencies", skip_all)]
pub fn resolve(input_path: Option<&Path>, offline: bool) -> Result<ResolutionResult> {
    let cwd;
    let path = match input_path {
        // Handle both None and empty paths (e.g., "file.zen".parent() returns Some(""))
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            cwd = std::env::current_dir()?;
            &cwd
        }
    };
    let workspace_info = get_workspace_info(&DefaultFileProvider::new(), path)?;

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

    resolve_workspace_dependencies(workspace_info, path, offline)
}
