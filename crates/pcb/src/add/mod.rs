mod manifest;
mod mvs;
mod scan;
mod target;

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;

use self::mvs::{PackageResolutionDebug, PackageResolver};
use self::target::{AddTarget, discover_add_targets};

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
            "The current MVS v2 debug path only supports bare `pcb add`.\n\
             Requested flags and targeted adds are not implemented yet."
        );
    }

    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;
    validate_workspace(&workspace)?;

    let targets = discover_add_targets(&workspace, &cwd)?;
    let mut resolver = PackageResolver::new(workspace.clone())?;

    for (idx, target) in targets.iter().enumerate() {
        let resolution = resolver.resolve_package(&target.package_url)?;
        if idx > 0 {
            println!();
        }
        print_debug_resolution(target, &resolution);
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

fn print_debug_resolution(target: &AddTarget, resolution: &PackageResolutionDebug) {
    println!(
        "pcb add debug: {} ({})",
        target.package_url,
        target.pcb_toml_path.display()
    );

    println!("  scanned remote deps:");
    if resolution.scanned.remote.is_empty() {
        println!("    (none)");
    } else {
        for (module_path, spec) in &resolution.scanned.remote {
            println!("    - {} = {}", module_path, render_dep_spec(spec));
        }
    }

    println!("  scanned workspace deps:");
    if resolution.scanned.workspace.is_empty() {
        println!("    (none)");
    } else {
        for package_url in &resolution.scanned.workspace {
            println!("    - {}", package_url);
        }
    }

    println!("  implicit seeds:");
    if resolution.scanned.implicit_remote.is_empty() {
        println!("    (none)");
    } else {
        for (module_path, spec) in &resolution.scanned.implicit_remote {
            println!("    - {} = {}", module_path, render_dep_spec(spec));
        }
    }

    println!("  imported workspace floors:");
    if resolution.imported_workspace_floors.is_empty() {
        println!("    (none)");
    } else {
        for (module_path, version) in &resolution.imported_workspace_floors {
            println!("    - {} = {}", module_path, version);
        }
    }

    println!("  resolved remote versions:");
    if resolution.resolved_remote.is_empty() {
        println!("    (none)");
    } else {
        for (module_path, version) in &resolution.resolved_remote {
            println!("    - {} = {}", module_path, version);
        }
    }
}

fn render_dep_spec(spec: &pcb_zen_core::config::DependencySpec) -> String {
    match spec {
        pcb_zen_core::config::DependencySpec::Version(version) => version.clone(),
        pcb_zen_core::config::DependencySpec::Detailed(detail) => {
            let mut parts = Vec::new();
            if let Some(version) = &detail.version {
                parts.push(format!("version={version}"));
            }
            if let Some(branch) = &detail.branch {
                parts.push(format!("branch={branch}"));
            }
            if let Some(rev) = &detail.rev {
                parts.push(format!("rev={rev}"));
            }
            if let Some(path) = &detail.path {
                parts.push(format!("path={path}"));
            }
            format!("{{ {} }}", parts.join(", "))
        }
    }
}
