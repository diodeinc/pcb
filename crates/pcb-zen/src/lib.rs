//! Diode Star – evaluate .zen designs and return schematic data structures.

pub mod diagnostics;
pub mod git;
pub mod load;
pub mod lsp;
pub mod suppression;

use std::path::Path;
use std::sync::Arc;

use crate::load::DefaultRemoteFetcher;
use pcb_sch::Schematic;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, InputMap, NoopRemoteFetcher,
};
use starlark::errors::EvalMessage;

pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, Diagnostics, EvalMode, WithDiagnostics};
pub use starlark::errors::EvalSeverity;

/// Create an evaluation context with proper load resolver setup for a given workspace.
///
/// This helper ensures that the evaluation context has a unified load resolver that handles:
/// - Remote packages (@package style imports like @kicad-symbols/...)
/// - Workspace-relative paths (//...)
/// - Relative paths (./... or ../...)
/// - Absolute paths
///
/// # Arguments
/// * `workspace_root` - The root directory of the workspace (typically where pcb.toml is located)
///
/// # Example
/// ```no_run
/// use std::path::Path;
/// use pcb_zen::create_eval_context;
///
/// let workspace = Path::new("/path/to/my/project");
/// let ctx = create_eval_context(workspace, false);
/// // Now Module() calls within evaluated files will support all import types
/// ```
pub fn create_eval_context(workspace_root: &Path, offline: bool) -> EvalContext {
    let file_provider = Arc::new(DefaultFileProvider);

    // Choose remote fetcher based on offline mode
    let remote_fetcher: Arc<dyn pcb_zen_core::RemoteFetcher> = if offline {
        Arc::new(NoopRemoteFetcher)
    } else {
        Arc::new(DefaultRemoteFetcher::default())
    };

    let load_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        true,
    ));

    EvalContext::new()
        .set_file_provider(file_provider)
        .set_load_resolver(load_resolver)
}

/// Evaluate `file` and return a [`Schematic`].
pub fn run(file: &Path, offline: bool, mode: EvalMode) -> WithDiagnostics<Schematic> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    // Create a file provider for finding workspace root
    let file_provider = DefaultFileProvider;

    // Simple workspace detection: look for pcb.toml, fallback to parent
    let workspace_root = find_workspace_root(&file_provider, &abs_path);

    let ctx = create_eval_context(&workspace_root, offline);

    // For now we don't inject any external inputs.
    let inputs = InputMap::new();
    ctx.set_source_path(abs_path.clone())
        .set_module_name("<root>".to_string())
        .set_inputs(inputs)
        .set_eval_mode(mode)
        .eval()
        .try_map(|m| {
            // Convert schematic conversion error into a Starlark diagnostic
            m.sch_module
                .to_schematic()
                .map_err(|e| EvalMessage::from_error(abs_path.as_path(), &e.into()))
        })
}

pub fn lsp() -> anyhow::Result<()> {
    let ctx = lsp::LspEvalContext::default();
    pcb_starlark_lsp::server::stdio_server(ctx)
}

/// Start the LSP server with `eager` determining whether all workspace files are pre-loaded.
/// When `eager` is `false` the server behaves like before (only open files are parsed).
pub fn lsp_with_eager(eager: bool) -> anyhow::Result<()> {
    let ctx = lsp::LspEvalContext::default().set_eager(eager);
    pcb_starlark_lsp::server::stdio_server(ctx)
}
