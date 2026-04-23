mod manifest;
mod mvs;
mod scan;
mod target;
mod writeback;

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;

use self::mvs::PackageResolver;
use self::target::discover_add_targets;
use self::writeback::write_package_manifest;

#[derive(Args, Debug)]
#[command(about = "Add, reconcile, and hydrate package dependencies")]
pub struct AddArgs {
    /// Add or pin a specific dependency (for example `github.com/acme/lib@1.2.3`)
    #[arg(value_name = "DEPENDENCY")]
    pub dependency: Option<String>,

    /// Raise direct dependency floors
    #[arg(short = 'u', long = "update")]
    pub update: bool,

    /// Rehydrate from the committed manifest without re-resolving
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: AddArgs) -> Result<()> {
    if args.dependency.is_some() || args.update || args.locked {
        bail!(
            "The current MVS v2 path only supports bare `pcb add`.\n\
             Requested flags and targeted adds are not implemented yet."
        );
    }

    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    let mut resolver = PackageResolver::new(workspace.clone())?;

    for target in &targets {
        let resolution = resolver.resolve_package(&target.package_url)?;
        let summary = write_package_manifest(target, &resolution)?;
        let action = if summary.changed {
            "Updated"
        } else {
            "Already up to date"
        };
        println!(
            "{} {} ({} direct, {} indirect)",
            action,
            target.pcb_toml_path.display(),
            summary.direct_count,
            summary.indirect_count,
        );
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
