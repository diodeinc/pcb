//! Common workspace and dependency handling utilities

use anyhow::Result;
use log::{debug, info};
use pcb_zen::load::DefaultRemoteFetcher;
use pcb_zen_core::config::{get_workspace_info, WorkspaceInfo as ConfigWorkspaceInfo};
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, InputMap, WithDiagnostics,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Common workspace information used by both vendor and release commands
pub struct WorkspaceInfo {
    /// From config discovery
    pub config: ConfigWorkspaceInfo,
    /// Canonical path to the .zen file being processed
    pub zen_path: PathBuf,
    /// Core resolver that tracked all file dependencies during evaluation
    pub resolver: Arc<CoreLoadResolver>,
    /// Evaluation result containing the parsed zen file
    pub eval_result: WithDiagnostics<EvalOutput>,
}

impl WorkspaceInfo {
    /// Get the board name for this workspace's zen file
    pub fn board_name(&self) -> Option<String> {
        self.config.board_name_for_zen(&self.zen_path)
    }

    /// Get a human-friendly board display name, with a safe fallback to the .zen file stem
    pub fn board_display_name(&self) -> String {
        self.board_name().unwrap_or_else(|| {
            self.zen_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
    }

    pub fn root(&self) -> &Path {
        &self.config.root
    }
}

/// Gather common workspace information for both vendor and release commands
pub fn gather_workspace_info(zen_path: PathBuf, use_vendor_path: bool) -> Result<WorkspaceInfo> {
    debug!("Starting workspace information gathering");

    // Canonicalize the zen path
    let zen_path = zen_path.canonicalize()?;

    // 1. Reuse config.rs to get workspace + board list
    let config = get_workspace_info(&DefaultFileProvider, &zen_path)?;

    // 2. Evaluate the zen file – workspace root comes out of config
    let (resolver, eval_result) = eval_zen_entrypoint(&zen_path, &config.root, use_vendor_path)?;

    Ok(WorkspaceInfo {
        config,
        zen_path,
        resolver,
        eval_result,
    })
}

/// Evaluate zen file and track dependencies using CoreLoadResolver directly
pub fn eval_zen_entrypoint(
    entry: &Path,
    workspace_root: &Path,
    use_vendor_path: bool,
) -> Result<(Arc<CoreLoadResolver>, WithDiagnostics<EvalOutput>)> {
    debug!("Starting zen file evaluation: {}", entry.display());

    let file_provider = Arc::new(DefaultFileProvider);
    let remote_fetcher = Arc::new(DefaultRemoteFetcher::default());

    let core_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        use_vendor_path,
    ));

    let eval_context = EvalContext::new()
        .set_file_provider(file_provider.clone())
        .set_load_resolver(core_resolver.clone())
        .set_source_path(entry.to_path_buf())
        .set_inputs(InputMap::new());

    let eval_result = eval_context.eval();

    // Check for errors and bail if evaluation failed
    if !eval_result.is_success() {
        let errors: Vec<String> = eval_result
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.to_string())
            .collect();
        if !errors.is_empty() {
            anyhow::bail!("Zen file evaluation failed:\n{}", errors.join("\n"));
        }
    }

    info!("Zen file evaluation completed successfully");
    Ok((core_resolver, eval_result))
}
