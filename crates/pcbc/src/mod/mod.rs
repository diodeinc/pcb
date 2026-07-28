pub(crate) mod request;
pub(crate) mod target;
mod writeback;

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::cache_index::{CacheIndex, ensure_workspace_cache_symlink};
use pcb_zen::package_resolver::{
    DepGraph, DepGraphNode, PackageResolver, build_frozen_resolution_maps, plan_vendor_selected,
    target_package_urls_for_path,
};
use pcb_zen::resolve::VendorPlan;
use pcb_zen::resolve::ensure_package_manifest_in_cache;
use pcb_zen::tags;
use pcb_zen::workspace::{enrich_tag_versions, get_workspace_info};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::is_stdlib_module_path;
use pcb_zen_core::resolution::FrozenResolutionMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use self::request::resolve_direct_dependency_request;
use self::target::{AddTarget, discover_add_targets, discover_package_target};
use self::writeback::{ManifestEdit, plan_package_manifest};

type DirectOverrides = BTreeMap<String, DependencySpec>;

#[derive(Args, Debug)]
#[command(about = "Add or update a direct dependency")]
pub struct ModAddArgs {
    /// Upgrade existing direct dependency floor(s)
    #[arg(short = 'u', long = "upgrade")]
    pub upgrade: bool,

    /// Dependency to add or update, e.g. github.com/acme/foo@latest
    #[arg(value_name = "DEPENDENCY")]
    pub dependency: Option<String>,
}

#[derive(Args, Debug)]
#[command(about = "Reconcile source imports and hydrate package dependency manifests")]
pub struct SyncArgs {
    /// Print changed manifests
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Verify pcb.toml and vendor/ are in sync across the whole workspace
    /// without modifying them; exit non-zero on drift
    #[arg(long = "check")]
    pub check: bool,
}

#[derive(Args, Debug)]
#[command(about = "Download modules to the package cache")]
pub struct ModDownloadArgs {
    /// Dependency to download exactly, e.g. github.com/acme/foo@1.2.3.
    /// Defaults to the hydrated closure for the current package or workspace.
    #[arg(value_name = "DEPENDENCY")]
    pub dependency: Option<String>,
}

#[derive(Args, Debug)]
#[command(about = "Print why a dependency is needed")]
pub struct ModWhyArgs {
    /// Dependency path or exact resolved node like github.com/acme/foo@1.2.3
    #[arg(value_name = "DEPENDENCY")]
    pub dependency: String,
}

#[derive(Args, Debug)]
#[command(about = "Print the lane-aware dependency graph")]
pub struct ModGraphArgs {}

#[derive(Args, Debug)]
#[command(about = "Print the frozen dependency resolution table for a target")]
pub struct ModResolveArgs {
    /// .zen file or directory to resolve. Defaults to current directory.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: Option<PathBuf>,
}

/// Load and validate the workspace for `pcb mod` / `pcb sync`, with package
/// versions from git tags: they feed the workspace pins written to pcb.toml.
fn load_workspace(start_path: &Path) -> Result<WorkspaceInfo> {
    let mut workspace = get_workspace_info(&DefaultFileProvider::new(), start_path)?;
    enrich_tag_versions(&mut workspace);
    validate_workspace(&workspace)?;
    Ok(workspace)
}

pub fn execute_mod_add(args: ModAddArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = load_workspace(&cwd)?;

    let Some(target) = discover_package_target(&workspace, &cwd) else {
        bail!("must be run from a package directory.");
    };
    let overrides = add_overrides(&workspace, &target, &args)?;

    run_resolution(
        &workspace,
        std::slice::from_ref(&target),
        false,
        Some((&target.package_url, &overrides)),
        false,
        SyncMode::Write,
    )
}

pub fn execute_sync(args: SyncArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    execute_sync_from(&cwd, args)
}

pub(crate) fn execute_sync_from(cwd: &Path, args: SyncArgs) -> Result<()> {
    let workspace = load_workspace(cwd)?;

    // --check always verifies the whole workspace, regardless of cwd.
    let scope = if args.check { &workspace.root } else { cwd };
    let targets = discover_add_targets(&workspace, scope)?;
    let mode = if args.check {
        SyncMode::Check
    } else {
        SyncMode::Write
    };
    run_resolution(
        &workspace,
        &targets,
        args.verbose,
        None,
        is_workspace_root(&workspace, scope),
        mode,
    )
}

pub fn execute_mod_download(args: ModDownloadArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = load_workspace(&cwd)?;
    ensure_workspace_cache_symlink(&workspace.root)?;

    if let Some(dependency) = args.dependency {
        let (module_path, version) = parse_download_dependency(&dependency)?;
        let index = CacheIndex::open()?;
        ensure_package_manifest_in_cache(&module_path, &version, &index)?;
        return Ok(());
    }

    let package_urls = discover_add_targets(&workspace, &cwd)?
        .into_iter()
        .map(|target| target.package_url);
    build_frozen_resolution_maps(&workspace, package_urls, false)?;
    Ok(())
}

pub fn execute_mod_why(args: ModWhyArgs) -> Result<()> {
    let (workspace, target) = load_single_target_workspace("pcb mod why")?;
    let graph = build_target_graph(&workspace, &target)?;
    let target_node = resolve_graph_target(&graph, &args.dependency)?;

    println!("# {}", args.dependency);
    if let Some(target_node) = target_node
        && let Some(path) = graph.shortest_path_to(&target_node)
    {
        for node in path {
            println!("{}", node.display());
        }
    } else {
        println!("(main package does not depend on {})", args.dependency);
    }

    Ok(())
}

pub fn execute_mod_graph(_args: ModGraphArgs) -> Result<()> {
    let (workspace, target) = load_single_target_workspace("pcb mod graph")?;
    let graph = build_target_graph(&workspace, &target)?;
    for (from, to) in graph.formatted_edges() {
        println!("{from} {to}");
    }
    Ok(())
}

pub fn execute_mod_resolve(args: ModResolveArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let path = args.path.as_deref().unwrap_or(&cwd);
    let workspace = load_workspace(path)?;

    let package_urls = target_package_urls_for_path(&workspace, path)?;
    let resolutions = build_frozen_resolution_maps(&workspace, package_urls.clone(), false)?;
    for (idx, package_url) in package_urls.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        let resolution = resolutions
            .get(package_url)
            .expect("frozen resolution was built for requested package");
        print_frozen_resolution(&workspace, package_url, resolution);
    }

    Ok(())
}

fn load_target_manifest(target: &AddTarget) -> Result<PcbToml> {
    PcbToml::from_path(&target.pcb_toml_path)
}

fn load_single_target_workspace(command_name: &str) -> Result<(WorkspaceInfo, AddTarget)> {
    let cwd = std::env::current_dir()?;
    let workspace = load_workspace(&cwd)?;

    let Some(target) = discover_package_target(&workspace, &cwd) else {
        bail!("`{command_name}` must be run from a package directory, not the workspace root.");
    };
    Ok((workspace, target))
}

fn build_target_graph(workspace: &WorkspaceInfo, target: &AddTarget) -> Result<DepGraph> {
    let mut resolver = PackageResolver::new(workspace.clone())?;
    resolver.build_package_graph(&target.package_url)
}

fn add_overrides(
    workspace: &WorkspaceInfo,
    target: &AddTarget,
    args: &ModAddArgs,
) -> Result<DirectOverrides> {
    let current_config = load_target_manifest(target)?;

    if args.upgrade {
        return upgrade_overrides(workspace, &current_config, args.dependency.as_deref());
    }

    let Some(dependency) = args.dependency.as_deref() else {
        bail!("requires a dependency unless -u/--upgrade is used.");
    };
    let (module_path, spec) = resolve_direct_dependency_request(dependency, &current_config)?;
    validate_mod_add_target(workspace, &module_path)?;
    Ok(BTreeMap::from([(module_path, spec)]))
}

fn upgrade_overrides(
    workspace: &WorkspaceInfo,
    current_config: &PcbToml,
    dependency: Option<&str>,
) -> Result<DirectOverrides> {
    let mut overrides = BTreeMap::new();

    if let Some(dependency) = dependency {
        if dependency.contains('@') {
            bail!("-u/--upgrade expects a bare dependency URL, not a version selector.");
        }
        let Some((module_path, spec)) =
            current_config.dependencies.direct.get_key_value(dependency)
        else {
            bail!(
                "can only update an existing direct dependency: {}",
                dependency
            );
        };
        if !is_remote_dependency(workspace, module_path, spec) {
            bail!("not a remote dependency: {}", dependency);
        }
        validate_mod_add_target(workspace, module_path)?;
        let (_, spec) = resolve_direct_dependency_request(module_path, current_config)?;
        overrides.insert(module_path.clone(), spec);
        return Ok(overrides);
    }

    for (module_path, spec) in &current_config.dependencies.direct {
        if !is_remote_dependency(workspace, module_path, spec) {
            continue;
        }
        let (_, spec) = resolve_direct_dependency_request(module_path, current_config)?;
        overrides.insert(module_path.clone(), spec);
    }

    if overrides.is_empty() {
        bail!("No direct remote dependencies to upgrade");
    }

    Ok(overrides)
}

fn validate_mod_add_target(workspace: &WorkspaceInfo, module_path: &str) -> Result<()> {
    if is_stdlib_module_path(module_path) {
        bail!("does not support stdlib module paths: {}", module_path);
    }
    if workspace.packages.contains_key(module_path)
        || workspace.workspace_base_url().as_deref() == Some(module_path)
    {
        bail!(
            "does not support workspace-local package URLs: {}",
            module_path
        );
    }
    Ok(())
}

fn is_remote_dependency(
    workspace: &WorkspaceInfo,
    module_path: &str,
    spec: &DependencySpec,
) -> bool {
    !is_stdlib_module_path(module_path)
        && !workspace.packages.contains_key(module_path)
        && workspace.workspace_base_url().as_deref() != Some(module_path)
        && !matches!(spec, DependencySpec::Detailed(detail) if detail.path.is_some())
}

fn parse_download_dependency(raw: &str) -> Result<(String, semver::Version)> {
    let Some((module_path, raw_version)) = raw.rsplit_once('@') else {
        bail!("`pcb mod download` requires an exact dependency like github.com/acme/foo@1.2.3");
    };
    if module_path.is_empty() || raw_version.is_empty() {
        bail!("`pcb mod download` requires an exact dependency like github.com/acme/foo@1.2.3");
    }
    let version = tags::parse_version(raw_version)
        .ok_or_else(|| anyhow::anyhow!("Invalid version '{}'", raw_version))?;
    Ok((module_path.to_string(), version))
}

fn resolve_graph_target(graph: &DepGraph, raw: &str) -> Result<Option<DepGraphNode>> {
    let raw = raw.trim();
    if graph.contains_package(raw) {
        return Ok(Some(DepGraphNode::Package(raw.to_string())));
    }

    if let Some((path, version)) = parse_exact_remote_node(raw) {
        if let Some(node) = graph.find_remote_exact(path, &version) {
            return Ok(Some(node));
        }
        return Ok(None);
    }

    let matches = graph.find_remote_by_path(raw);
    match matches.as_slice() {
        [] => Ok(None),
        [node] => Ok(Some(node.clone())),
        _ => {
            let options = matches
                .iter()
                .map(DepGraphNode::display)
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Dependency {} resolves to multiple lanes/versions; use one of: {}",
                raw,
                options
            );
        }
    }
}

fn parse_exact_remote_node(raw: &str) -> Option<(&str, semver::Version)> {
    let (path, version) = raw.rsplit_once('@')?;
    if path.is_empty() {
        return None;
    }
    let version = tags::parse_version(version)?;
    Some((path, version))
}

fn workspace_relative_path(workspace_root: &Path, path: &Path) -> PathBuf {
    pathdiff::diff_paths(path, workspace_root).unwrap_or_else(|| path.to_path_buf())
}

fn is_workspace_root(workspace: &WorkspaceInfo, path: &Path) -> bool {
    let cwd = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());
    cwd == root
}

fn print_frozen_resolution(
    workspace: &WorkspaceInfo,
    package_url: &str,
    resolution: &FrozenResolutionMap,
) {
    println!("pcb mod resolve: {package_url}");

    println!("  selected remote deps:");
    if resolution.selected_remote.is_empty() {
        println!("    (none)");
    } else {
        for (dep_id, version) in &resolution.selected_remote {
            println!("    - {}@{} = {}", dep_id.path, dep_id.lane, version);
        }
    }

    println!("  packages:");
    for (package_root, package) in &resolution.packages {
        println!(
            "    {} ({})",
            package.identity.display(),
            workspace_relative_path(&workspace.root, package_root).display()
        );
        if package.deps.is_empty() {
            println!("      (none)");
            continue;
        }
        for (dep_url, dep_root) in &package.deps {
            println!(
                "      {} -> {}",
                dep_url,
                workspace_relative_path(&workspace.root, dep_root).display()
            );
        }
    }
}

pub(crate) fn sync_targets(
    workspace: &WorkspaceInfo,
    targets: &[AddTarget],
    verbose: bool,
    prune_vendor: bool,
) -> Result<()> {
    run_resolution(
        workspace,
        targets,
        verbose,
        None,
        prune_vendor,
        SyncMode::Write,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncMode {
    Write,
    Check,
}

fn run_resolution(
    workspace: &WorkspaceInfo,
    targets: &[AddTarget],
    verbose: bool,
    direct_overrides: Option<(&str, &DirectOverrides)>,
    prune_vendor: bool,
    mode: SyncMode,
) -> Result<()> {
    let mut resolver = PackageResolver::new(workspace.clone())?;
    let mut selected_remote = BTreeSet::new();
    let mut manifest_edits = Vec::new();

    for target in targets {
        let overrides = direct_overrides
            .filter(|(package_url, _)| *package_url == target.package_url)
            .map(|(_, overrides)| overrides);
        let resolution =
            resolver.resolve_package_with_direct_overrides(&target.package_url, overrides)?;
        if let Some(edit) = plan_package_manifest(target, &resolution)? {
            manifest_edits.push(edit);
        }
        selected_remote.extend(resolution.resolved_remote);
    }

    let package_roots = resolver.materialize_selected(
        selected_remote
            .iter()
            .map(|(dep_id, version)| (dep_id, version)),
    )?;
    let vendor_plan = plan_vendor_selected(workspace, &package_roots, prune_vendor)?;

    match mode {
        SyncMode::Write => apply_sync_plan(workspace, manifest_edits, vendor_plan, verbose),
        SyncMode::Check => report_sync_drift(workspace, &manifest_edits, &vendor_plan),
    }
}

fn apply_sync_plan(
    workspace: &WorkspaceInfo,
    manifest_edits: Vec<ManifestEdit>,
    vendor_plan: VendorPlan,
    verbose: bool,
) -> Result<()> {
    for edit in manifest_edits {
        edit.apply()?;
        if verbose {
            println!(
                "pcb: updated {}",
                workspace_relative_path(&workspace.root, &edit.path).display()
            );
        }
    }
    vendor_plan.apply()?;
    Ok(())
}

fn report_sync_drift(
    workspace: &WorkspaceInfo,
    manifest_edits: &[ManifestEdit],
    vendor_plan: &VendorPlan,
) -> Result<()> {
    if manifest_edits.is_empty() && vendor_plan.is_empty() {
        return Ok(());
    }

    for edit in manifest_edits {
        eprintln!(
            "would update {}",
            workspace_relative_path(&workspace.root, &edit.path).display()
        );
    }
    for copy in &vendor_plan.copies {
        eprintln!(
            "would vendor {}",
            workspace_relative_path(&workspace.root, &copy.dst).display()
        );
    }
    for prune in &vendor_plan.prunes {
        eprintln!(
            "would prune {}",
            workspace_relative_path(&workspace.root, prune).display()
        );
    }

    bail!("workspace is not synced; run `pcb sync` and commit the changes")
}

pub(crate) fn validate_workspace(workspace: &WorkspaceInfo) -> Result<()> {
    if workspace.errors.is_empty() {
        return Ok(());
    }

    for err in &workspace.errors {
        eprintln!("{}: {}", err.path.display(), err.error);
    }
    bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
}
