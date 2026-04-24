mod dep_id;
mod manifest;
mod materialize;
mod mvs;
mod request;
mod resolve;
mod scan;
mod target;
mod writeback;

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::tags;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::is_stdlib_module_path;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use self::materialize::{materialize_selected, vendor_selected};
use self::mvs::{DepGraph, DepGraphNode, PackageResolver};
use self::request::resolve_direct_dependency_request;
use self::resolve::{
    FrozenResolutionMap, build_frozen_resolution_map, target_package_urls_for_path,
};
use self::target::{AddTarget, discover_add_targets};
use self::writeback::write_package_manifest;

type DirectOverrides = BTreeMap<String, DependencySpec>;

#[derive(Args, Debug)]
#[command(about = "Add or update a direct dependency")]
pub struct ModAddArgs {
    /// Dependency to add or update, e.g. github.com/acme/foo@latest
    #[arg(value_name = "DEPENDENCY")]
    pub dependency: String,
}

#[derive(Args, Debug)]
#[command(about = "Reconcile source imports and hydrate package dependency manifests")]
pub struct TidyArgs {
    /// Print changed manifests
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
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
#[command(about = "Print the frozen MVS v2 resolution table for a target")]
pub struct ModResolveArgs {
    /// .zen file or directory to resolve. Defaults to current directory.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: Option<PathBuf>,
}

pub fn execute_mod_add(args: ModAddArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    let [target] = targets.as_slice() else {
        bail!("`pcb mod add` must be run from a package directory, not the workspace root.");
    };
    let current_config = load_target_manifest(target)?;
    let (module_path, spec) = resolve_direct_dependency_request(&args.dependency, &current_config)?;

    if is_stdlib_module_path(&module_path) {
        bail!(
            "`pcb mod add` does not support stdlib module paths: {}",
            module_path
        );
    }
    if workspace.packages.contains_key(&module_path)
        || workspace.workspace_base_url().as_deref() == Some(module_path.as_str())
    {
        bail!(
            "`pcb mod add` does not support workspace-local package URLs: {}",
            module_path
        );
    }

    run_resolution(
        &workspace,
        std::slice::from_ref(target),
        false,
        Some((&target.package_url, &BTreeMap::from([(module_path, spec)]))),
        false,
    )
}

pub fn execute_tidy(args: TidyArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    run_resolution(
        &workspace,
        &targets,
        args.verbose,
        None,
        is_workspace_root(&workspace, &cwd),
    )
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
    let workspace = get_workspace_info(&DefaultFileProvider::new(), path, true)?;
    validate_workspace(&workspace)?;

    let package_urls = target_package_urls_for_path(&workspace, path)?;
    for (idx, package_url) in package_urls.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        let resolution = build_frozen_resolution_map(&workspace, package_url)?;
        print_frozen_resolution(&workspace, package_url, &resolution);
    }

    Ok(())
}

fn load_target_manifest(target: &AddTarget) -> Result<PcbToml> {
    PcbToml::from_path(&target.pcb_toml_path)
}

fn load_single_target_workspace(command_name: &str) -> Result<(WorkspaceInfo, AddTarget)> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    let [target] = targets.as_slice() else {
        bail!("`{command_name}` must be run from a package directory, not the workspace root.");
    };
    Ok((workspace, target.clone()))
}

fn build_target_graph(workspace: &WorkspaceInfo, target: &AddTarget) -> Result<DepGraph> {
    let mut resolver = PackageResolver::new(workspace.clone())?;
    resolver.build_package_graph(&target.package_url)
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

fn run_resolution(
    workspace: &WorkspaceInfo,
    targets: &[AddTarget],
    verbose: bool,
    direct_overrides: Option<(&str, &DirectOverrides)>,
    prune_vendor: bool,
) -> Result<()> {
    let mut resolver = PackageResolver::new(workspace.clone())?;
    let mut package_roots = BTreeSet::new();

    for target in targets {
        let overrides = direct_overrides
            .filter(|(package_url, _)| *package_url == target.package_url)
            .map(|(_, overrides)| overrides);
        let resolution =
            resolver.resolve_package_with_direct_overrides(&target.package_url, overrides)?;
        let summary = write_package_manifest(target, &resolution)?;
        if verbose && summary.changed {
            println!(
                "pcb: updated {}",
                workspace_relative_path(&workspace.root, &target.pcb_toml_path).display()
            );
        }
        package_roots.extend(materialize_selected(
            workspace,
            &resolution.resolved_remote,
        )?);
    }

    vendor_selected(workspace, &package_roots, prune_vendor)?;

    Ok(())
}

fn validate_workspace(workspace: &WorkspaceInfo) -> Result<()> {
    if workspace.errors.is_empty() {
        return Ok(());
    }

    for err in &workspace.errors {
        eprintln!("{}: {}", err.path.display(), err.error);
    }
    bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
}
