//! Diode Star – evaluate .zen designs and return schematic data structures.

pub mod archive;
pub mod ast_utils;
mod auto_deps;
pub mod cache_index;
pub mod canonical;
pub mod diagnostics;
pub mod fork;
pub mod git;
pub mod lsp;
pub mod resolve;
pub mod suppression;
pub mod tags;
pub mod tree;
pub mod workspace;

use std::path::Path;
use std::sync::Arc;

use pcb_sch::Schematic;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::FileProvider;
use pcb_zen_core::{CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, LoadResolver};

pub use pcb_zen_core::file_extensions;
pub use pcb_zen_core::{Diagnostic, Diagnostics, WithDiagnostics};
pub use resolve::{
    copy_dir_all, ensure_sparse_checkout, resolve_dependencies, vendor_deps, ResolutionResult,
    VendorResult,
};
pub use starlark::errors::EvalSeverity;
pub use workspace::{get_workspace_info, MemberPackage, PackageClosure, WorkspaceInfo};

pub use tags::get_all_versions_for_repo;

/// Evaluate a .zen file and return EvalOutput (module + signature + prints) with diagnostics.
pub fn eval(file: &Path, resolution_result: ResolutionResult) -> WithDiagnostics<EvalOutput> {
    let abs_path = file
        .canonicalize()
        .expect("failed to canonicalise input path");

    let file_provider = Arc::new(DefaultFileProvider::new());
    let workspace_root =
        find_workspace_root(&*file_provider, &abs_path).expect("failed to find workspace root");

    let load_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        resolution_result.package_resolutions,
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
pub fn run(file: &Path, resolution_result: ResolutionResult) -> WithDiagnostics<Schematic> {
    let eval_result = eval(file, resolution_result);

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
