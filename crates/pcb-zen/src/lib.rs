//! Diode Star – evaluate .zen designs and return schematic data structures.

pub mod bundle;
pub mod diagnostics;
pub mod load;
pub mod lsp;
pub mod suppression;

use std::path::Path;
use std::sync::Arc;

use crate::load::DefaultRemoteFetcher;
use pcb_sch::Schematic;
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::workspace::find_workspace_root;
use pcb_zen_core::{CoreLoadResolver, DefaultFileProvider, EvalContext, InputMap};
use starlark::errors::EvalMessage;

pub use diagnostics::render_diagnostic;
pub use pcb_zen_core::bundle::{Bundle, BundleMetadata};
pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, WithDiagnostics};
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
/// let ctx = create_eval_context(workspace);
/// // Now Module() calls within evaluated files will support all import types
/// ```
pub fn create_eval_context(workspace_root: &Path) -> EvalContext {
    let file_provider = Arc::new(DefaultFileProvider);
    let remote_fetcher = Arc::new(DefaultRemoteFetcher);
    let load_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        Some(workspace_root.to_path_buf()),
    ));

    EvalContext::new()
        .set_file_provider(file_provider)
        .set_load_resolver(load_resolver)
}

/// Evaluate `file` and return a [`Schematic`].
pub fn run(file: &Path) -> WithDiagnostics<Schematic> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    // Create a file provider for finding workspace root
    let file_provider = DefaultFileProvider;

    // Find the workspace root by looking for pcb.toml
    let workspace_root = find_workspace_root(&file_provider, &abs_path)
        .unwrap_or_else(|| abs_path.parent().unwrap().to_path_buf());

    let ctx = create_eval_context(&workspace_root);

    // For now we don't inject any external inputs.
    let inputs = InputMap::new();
    let eval_result = ctx
        .set_source_path(abs_path.clone())
        .set_module_name("<root>".to_string())
        .set_inputs(inputs)
        .eval();

    // Collect diagnostics emitted during evaluation.
    let diagnostics = eval_result.diagnostics;
    let schematic = eval_result.output.map(|m| m.sch_module.to_schematic());

    // Determine the overall outcome.  Even if the evaluation emitted error
    // diagnostics we still return `success` as long as a schematic was
    // produced so that callers (e.g. the CLI) can decide based on
    // `has_errors()` whether to treat the build as failed.
    match schematic {
        Some(Ok(mut schematic)) => {
            schematic.assign_reference_designators();
            WithDiagnostics::success(schematic, diagnostics)
        }
        Some(Err(e)) => {
            // Convert the schematic conversion error into a Starlark diagnostic and append it
            // to the existing list so that callers can surface it to users.
            let mut diagnostics_with_error = diagnostics;
            let st_error: starlark::Error = e.into();
            diagnostics_with_error.push(Diagnostic::from_eval_message(EvalMessage::from_error(
                abs_path.as_path(),
                &st_error,
            )));
            WithDiagnostics::failure(diagnostics_with_error)
        }
        None => WithDiagnostics::failure(diagnostics),
    }
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
