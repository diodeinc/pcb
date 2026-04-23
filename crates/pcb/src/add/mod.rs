mod dep_id;
mod manifest;
mod mvs;
mod request;
mod scan;
mod target;
mod writeback;

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::is_stdlib_module_path;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use self::mvs::PackageResolver;
use self::request::resolve_direct_dependency_request;
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
    )
}

pub fn execute_tidy(args: TidyArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    run_resolution(&workspace, &targets, args.verbose, None)
}

fn load_target_manifest(target: &AddTarget) -> Result<PcbToml> {
    PcbToml::from_path(&target.pcb_toml_path)
}

fn workspace_relative_path(workspace_root: &Path, path: &Path) -> PathBuf {
    pathdiff::diff_paths(path, workspace_root).unwrap_or_else(|| path.to_path_buf())
}

fn run_resolution(
    workspace: &WorkspaceInfo,
    targets: &[AddTarget],
    verbose: bool,
    direct_overrides: Option<(&str, &DirectOverrides)>,
) -> Result<()> {
    let mut resolver = PackageResolver::new(workspace.clone())?;

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
    }

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
