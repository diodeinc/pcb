//! Diode Star – evaluate .zen designs and return schematic data structures.

pub mod bundle;
pub mod diagnostics;
pub mod load;
pub mod lsp;
pub mod suppression;

use std::path::Path;
use std::sync::Arc;

use crate::load::RemoteLoadResolver;
use pcb_sch::Schematic;
use pcb_zen_core::convert::ToSchematic;
use pcb_zen_core::{
    CompoundLoadResolver, DefaultFileProvider, EvalContext, InputMap, WorkspaceLoadResolver,
};
use starlark::errors::EvalMessage;

pub use diagnostics::render_diagnostic;
pub use pcb_zen_core::bundle::{Bundle, BundleMetadata};
pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, WithDiagnostics};
pub use starlark::errors::EvalSeverity;

/// Evaluate `file` and return a [`Schematic`].
pub fn run(file: &Path) -> WithDiagnostics<Schematic> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    let ctx = EvalContext::new()
        .set_file_provider(Arc::new(DefaultFileProvider))
        .set_load_resolver(Arc::new(CompoundLoadResolver::new(vec![
            Arc::new(RemoteLoadResolver),
            Arc::new(WorkspaceLoadResolver::new(
                abs_path.parent().unwrap().to_path_buf(),
            )),
        ])));

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
