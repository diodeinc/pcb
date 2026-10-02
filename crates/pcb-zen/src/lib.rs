// Use pipe-safe replacements for standard printing macros in CLI output paths.
#[macro_use(eprintln)]
extern crate anstream;

pub mod ast_utils;
pub mod cache_index;
pub mod diagnostics;
pub mod git;
pub mod import_scanner;
pub mod lsp;
pub mod package_resolver;
pub mod resolve;
pub mod suppression;
pub mod tags;
pub mod tree;
pub mod workspace;

use std::path::Path;
use std::sync::Arc;

use pcb_sch::Schematic;
use pcb_zen_core::resolution::ResolutionResult;
use pcb_zen_core::{DefaultFileProvider, EvalContext, EvalOutput};
use serde_json::Value as JsonValue;
use starlark::collections::SmallMap;

pub use git::split_repo_and_subpath;
pub use package_resolver::resolve_workspace_dependencies;
pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, Diagnostics, WithDiagnostics};
pub use resolve::{VendorResult, copy_dir_all, vendor_deps};
pub use starlark::errors::EvalSeverity;
pub use workspace::{WorkspaceInfo, WorkspacePackage, get_workspace_info};

/// Evaluate a .zen file and return EvalOutput (module + signature + prints) with diagnostics.
pub fn eval(
    file: &Path,
    resolution_result: ResolutionResult,
    inputs: SmallMap<String, JsonValue>,
) -> WithDiagnostics<EvalOutput> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    let file_provider = Arc::new(DefaultFileProvider::new());
    let mut ctx = EvalContext::new(file_provider, resolution_result).set_source_path(abs_path);
    ctx.set_json_inputs(inputs);
    ctx.eval()
}

/// Evaluate `file` and return a [`Schematic`].
pub fn run(
    file: &Path,
    resolution_result: ResolutionResult,
    inputs: SmallMap<String, JsonValue>,
) -> WithDiagnostics<Schematic> {
    eval(file, resolution_result, inputs)
        .and_then(|eval_output| eval_output.to_schematic_with_diagnostics())
}

pub fn lsp() -> anyhow::Result<()> {
    let ctx = lsp::LspEvalContext::default();
    pcb_starlark_lsp::server::stdio_server(ctx).map_err(Into::into)
}

/// Start the LSP server with dependency resolution mode and a custom request
/// handler.
pub fn lsp_with_custom_request_handler<F>(
    eager: bool,
    offline: bool,
    handler: F,
) -> anyhow::Result<()>
where
    F: Fn(&str, &serde_json::Value) -> anyhow::Result<Option<serde_json::Value>>
        + Send
        + Sync
        + 'static,
{
    let ctx = lsp::LspEvalContext::default()
        .set_eager(eager)
        .set_offline(offline)
        .with_custom_request_handler(handler);
    pcb_starlark_lsp::server::stdio_server(ctx).map_err(Into::into)
}
