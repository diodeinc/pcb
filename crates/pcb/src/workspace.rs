//! Common workspace and dependency handling utilities

use anyhow::{Context, Result};
use log::{debug, info};
use pcb_zen::load::DefaultRemoteFetcher;
use pcb_zen_core::config::{get_workspace_info, WorkspaceInfo as ConfigWorkspaceInfo};
use pcb_zen_core::{
    normalize_path, CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, InputMap,
    LoadSpec, WithDiagnostics,
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

/// Classification of a tracked file
#[derive(Debug)]
pub enum FileClassification<'a> {
    /// Local file within workspace (contains relative path)
    Local(&'a Path),
    /// Vendor dependency (contains LoadSpec)
    Vendor(LoadSpec),
    /// Not relevant for packaging
    Irrelevant,
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
    let remote_fetcher = Arc::new(DefaultRemoteFetcher);

    let core_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        use_vendor_path,
    ));

    // Track the entrypoint (though it won't have a LoadSpec, which is fine)
    core_resolver.track_file(entry.to_path_buf());

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

/// Convert LoadSpec to vendor path
pub fn loadspec_to_vendor_path(spec: &LoadSpec) -> Result<PathBuf> {
    // Resolve package aliases to canonical git form
    let canonical_spec = match spec {
        LoadSpec::Package { .. } => spec
            .resolve(None)
            .context("Failed to resolve package alias to canonical form")?,
        _ => spec.clone(),
    };

    // Convert canonical spec to vendor path
    match canonical_spec {
        LoadSpec::Github {
            user,
            repo,
            rev,
            path,
        } => {
            let mut vendor_path = PathBuf::from("github.com").join(user).join(repo).join(rev);
            // Normalize and add path components (handles .. and . components)
            if !path.as_os_str().is_empty() && path != Path::new(".") {
                vendor_path.push(normalize_path(&path));
            }
            Ok(vendor_path)
        }
        LoadSpec::Gitlab {
            project_path,
            rev,
            path,
        } => {
            let mut vendor_path = PathBuf::from("gitlab.com").join(project_path).join(rev);
            // Normalize and add path components (handles .. and . components)
            if !path.as_os_str().is_empty() && path != Path::new(".") {
                vendor_path.push(normalize_path(&path));
            }
            Ok(vendor_path)
        }
        LoadSpec::Package { package, tag, path } => {
            info!("Package spec not resolved to canonical form: {package}");
            let mut vendor_path = PathBuf::from("packages").join(package);
            // Avoid creating empty tag directories
            if !tag.is_empty() {
                vendor_path.push(tag);
            }
            if !path.as_os_str().is_empty() && path != Path::new(".") {
                vendor_path.push(normalize_path(&path));
            }
            Ok(vendor_path)
        }
        LoadSpec::Path { .. } | LoadSpec::WorkspacePath { .. } => {
            anyhow::bail!(
                "Local path dependency detected during vendoring. This typically indicates zen files \
                from different workspaces are being processed together.\n\
                \n\
                Local dependencies should not be vendored - they belong to your workspace.\n\
                \n\
                Solution: Run 'pcb vendor' separately for each workspace, or ensure all zen files \
                belong to the same workspace."
            )
        }
    }
}

/// Classify a tracked file for packaging purposes
pub fn classify_file<'a>(
    workspace_root: &Path,
    path: &'a Path,
    resolver: &CoreLoadResolver,
) -> FileClassification<'a> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default();
    if !matches!(ext, "zen" | "kicad_mod" | "kicad_sym")
        && !ext.starts_with("kicad_")
        && filename != "pcb.toml"
    {
        return FileClassification::Irrelevant;
    }

    // Use proper path comparison instead of string matching
    if path.starts_with(workspace_root) {
        if let Ok(rel) = path.strip_prefix(workspace_root) {
            debug!(
                "Classified as local: {} (relative: {})",
                path.display(),
                rel.display()
            );
            FileClassification::Local(rel)
        } else {
            FileClassification::Irrelevant
        }
    } else if let Some(load_spec) = resolver.get_load_spec_for_path(path) {
        debug!("Classified as vendor: {}", path.display());
        FileClassification::Vendor(load_spec)
    } else {
        debug!(
            "Classified as irrelevant: {} (outside workspace, no LoadSpec)",
            path.display()
        );
        FileClassification::Irrelevant
    }
}

/// Check if a file is a vendor dependency (external to workspace) - compatibility helper
pub fn is_vendor_dependency(
    workspace_root: &Path,
    path: &Path,
    resolver: &CoreLoadResolver,
) -> bool {
    matches!(
        classify_file(workspace_root, path, resolver),
        FileClassification::Vendor(_)
    )
}
