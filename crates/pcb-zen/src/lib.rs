//! Diode Star – evaluate .zen designs and return schematic data structures.

pub mod ast_utils;
mod auto_deps;
pub mod canonical;
pub mod diagnostics;
pub mod git;
pub mod load;
pub mod lsp;
mod remote_discovery;
pub mod resolve_v2;
pub mod suppression;
pub mod workspace;

use std::path::Path;
use std::sync::Arc;

use crate::load::DefaultRemoteFetcher;
use pcb_sch::Schematic;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::FileProvider;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, LoadResolver, NoopRemoteFetcher,
};

pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, Diagnostics, WithDiagnostics};
pub use resolve_v2::{resolve_dependencies, vendor_deps, ResolutionResult, VendorResult};
pub use starlark::errors::EvalSeverity;
pub use workspace::{compute_tag_prefix, get_workspace_info, MemberPackage, WorkspaceInfo};

#[derive(Debug, Clone)]
pub struct EvalConfig {
    pub offline: bool,
    pub use_vendor: bool,
    pub resolution_result: Option<ResolutionResult>,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            offline: false,
            use_vendor: true,
            resolution_result: None,
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

    let (use_vendor, v2_resolutions) = match cfg.resolution_result {
        Some(res) => (false, Some(res.package_resolutions)),
        None => (cfg.use_vendor, None),
    };

    let load_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        use_vendor,
        v2_resolutions,
    ));

    // Track workspace-level pcb.toml if present for dependency awareness
    let pcb_toml_path = workspace_root.join("pcb.toml");
    if file_provider.exists(&pcb_toml_path) {
        load_resolver.track_file(&pcb_toml_path);
    }

    EvalContext::new(load_resolver)
        .set_source_path(abs_path)
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
    let mut schematic_result = eval_output.to_schematic_with_diagnostics();
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
