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
use pcb_zen_core::FileProvider;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, LoadResolver, NoopRemoteFetcher,
};

pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, Diagnostics, WithDiagnostics};
pub use starlark::errors::EvalSeverity;

#[derive(Debug, Clone, Copy)]
pub struct EvalConfig {
    pub offline: bool,
    pub use_vendor: bool,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            offline: false,
            use_vendor: true,
        }
    }
}

/// Evaluate a .zen file and return EvalOutput (module + signature + prints) with diagnostics.
pub fn eval(file: &Path, cfg: EvalConfig) -> WithDiagnostics<EvalOutput> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    let file_provider = Arc::new(DefaultFileProvider::new());
    let workspace_root = find_workspace_root(&*file_provider, &abs_path);

    let remote_fetcher: Arc<dyn pcb_zen_core::RemoteFetcher> = if cfg.offline {
        Arc::new(NoopRemoteFetcher)
    } else {
        Arc::new(DefaultRemoteFetcher::default())
    };

    let load_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        cfg.use_vendor,
    ));

    // Track workspace-level pcb.toml if present for dependency awareness
    let pcb_toml_path = workspace_root.join("pcb.toml");
    if file_provider.exists(&pcb_toml_path) {
        load_resolver.track_file(&pcb_toml_path);
    }

    EvalContext::new()
        .set_file_provider(file_provider)
        .set_load_resolver(load_resolver)
        .set_source_path(abs_path)
        .set_module_name("<root>".to_string())
        .eval()
}

/// Evaluate `file` and return a [`Schematic`].
pub fn run(file: &Path, cfg: EvalConfig) -> WithDiagnostics<Schematic> {
    let eval_result = eval(file, cfg);

    // Handle evaluation failure
    if eval_result.output.is_none() {
        return WithDiagnostics {
            output: None,
            diagnostics: eval_result.diagnostics,
        };
    }

    let eval_output = eval_result.output.unwrap();
    let mut schematic_result = eval_output.sch_module.to_schematic_with_diagnostics();
    // Merge diagnostics from eval and schematic conversion
    schematic_result.diagnostics.extend(eval_result.diagnostics);
    schematic_result
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
