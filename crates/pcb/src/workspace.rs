//! Common workspace and dependency handling utilities

use anyhow::{Context, Result};
use log::{debug, info};
use pcb_zen::load::{cache_dir, DefaultRemoteFetcher};
use pcb_zen_core::workspace::find_workspace_root;
use pcb_zen_core::{
    normalize_path, CoreLoadResolver, DefaultFileProvider, EvalContext, EvalOutput, InputMap,
    LoadSpec, WithDiagnostics,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::tracking_resolver::TrackingLoadResolver;

/// Common workspace information used by both vendor and release commands
pub struct WorkspaceInfo {
    /// Canonical path to the .zen file being processed
    pub zen_path: PathBuf,
    /// Root directory of the workspace
    pub workspace_root: PathBuf,
    /// Dependency tracker for finding all referenced files
    pub tracker: Arc<TrackingLoadResolver>,
    /// Evaluation result containing the parsed zen file
    pub eval_result: WithDiagnostics<EvalOutput>,
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
pub fn gather_workspace_info(zen_path: PathBuf) -> Result<WorkspaceInfo> {
    debug!("Starting workspace information gathering");

    // Canonicalize the zen path
    let zen_path = zen_path.canonicalize()?;

    // Try to find workspace root by walking up for pcb.toml
    let initial_workspace_root = find_workspace_root(&DefaultFileProvider, &zen_path);

    // Evaluate the zen file and track dependencies
    let (tracker, eval_result) = eval_zen_entrypoint(&zen_path, &initial_workspace_root)?;

    // Refine workspace root based on tracked files if no pcb.toml was found
    let workspace_root = if initial_workspace_root.join("pcb.toml").exists() {
        initial_workspace_root
    } else {
        // No pcb.toml found, use common ancestor of tracked files
        detect_workspace_root_from_files(&zen_path, &tracker.files())?
    };

    // Log workspace root info for debugging
    info!("Using workspace root: {}", workspace_root.display());

    Ok(WorkspaceInfo {
        zen_path,
        workspace_root,
        tracker,
        eval_result,
    })
}

/// Detect workspace root from tracked files when no pcb.toml is found
pub fn detect_workspace_root_from_files(
    entry: &Path,
    tracked: &HashSet<PathBuf>,
) -> Result<PathBuf> {
    let cache_root = cache_dir()?.canonicalize()?;

    let mut paths: Vec<PathBuf> = tracked
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| !p.starts_with(&cache_root))
        .collect();

    paths.push(entry.canonicalize()?);

    let root = paths
        .into_iter()
        .try_fold(None::<PathBuf>, |acc, path| -> Result<Option<PathBuf>> {
            let current_root = match acc {
                None => path.parent().map(|p| p.to_path_buf()),
                Some(existing) => common_ancestor(&existing, &path),
            };
            Ok(current_root)
        })?
        .context("Unable to determine workspace root from tracked files")?;

    info!("Detected workspace root from files: {}", root.display());
    Ok(root)
}

/// Find common ancestor of two paths
pub fn common_ancestor(a: &Path, b: &Path) -> Option<PathBuf> {
    let mut a_components = a.components();
    let mut b_components = b.components();
    let mut common = PathBuf::new();

    loop {
        match (a_components.next(), b_components.next()) {
            (Some(a_comp), Some(b_comp)) if a_comp == b_comp => {
                common.push(a_comp);
            }
            _ => break,
        }
    }

    if common.as_os_str().is_empty() {
        None
    } else {
        Some(common)
    }
}

/// Evaluate zen file and track dependencies
pub fn eval_zen_entrypoint(
    entry: &Path,
    workspace_root: &Path,
) -> Result<(Arc<TrackingLoadResolver>, WithDiagnostics<EvalOutput>)> {
    debug!("Starting zen file evaluation: {}", entry.display());

    let file_provider = Arc::new(DefaultFileProvider);

    let remote_fetcher = Arc::new(DefaultRemoteFetcher);
    let base_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root.to_path_buf(),
        false,
    ));

    let tracking_resolver = Arc::new(TrackingLoadResolver::new(
        base_resolver,
        file_provider.clone(),
    ));

    // Pre-seed with the entrypoint itself
    tracking_resolver.track(entry.to_path_buf());

    let eval_context = EvalContext::new()
        .set_file_provider(file_provider.clone())
        .set_load_resolver(tracking_resolver.clone())
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
    Ok((tracking_resolver, eval_result))
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
    tracker: &TrackingLoadResolver,
) -> FileClassification<'a> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if !matches!(ext, "zen" | "kicad_mod" | "kicad_sym") && !ext.starts_with("kicad_") {
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
    } else if let Some(load_spec) = tracker.get_load_spec(path) {
        debug!("Classified as vendor: {}", path.display());
        FileClassification::Vendor(load_spec.clone())
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
    tracker: &TrackingLoadResolver,
) -> bool {
    matches!(
        classify_file(workspace_root, path, tracker),
        FileClassification::Vendor(_)
    )
}
