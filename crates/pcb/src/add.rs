use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen::{MemberPackage, WorkspaceInfo};
use pcb_zen_core::DefaultFileProvider;

#[derive(Args, Debug)]
#[command(about = "Add, reconcile, and hydrate package dependencies (MVS v2 scaffold)")]
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AddTarget {
    package_url: String,
    pcb_toml_path: PathBuf,
}

pub fn execute(args: AddArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd, true)?;

    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}: {}", err.path.display(), err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let targets = discover_add_targets(&workspace, &cwd)?;
    let target_list = targets
        .iter()
        .map(|target| {
            format!(
                "  - {} ({})",
                target.package_url,
                target.pcb_toml_path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let requested = args
        .dependency
        .as_deref()
        .map(|dep| format!("\nRequested dependency: {dep}"))
        .unwrap_or_default();
    let mode = match (args.update, args.locked) {
        (true, true) => "Mode: update + locked",
        (true, false) => "Mode: update",
        (false, true) => "Mode: locked",
        (false, false) => "Mode: reconcile",
    };

    bail!(
        "`pcb add` plumbing is wired, but MVS v2 resolution is not implemented yet.\n\
         Targets:\n{target_list}\n\
         {mode}{requested}"
    );
}

fn discover_add_targets(workspace: &WorkspaceInfo, start_path: &Path) -> Result<Vec<AddTarget>> {
    let candidate_dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };
    let candidate_dir = candidate_dir
        .canonicalize()
        .unwrap_or_else(|_| candidate_dir.to_path_buf());
    let workspace_root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());

    if candidate_dir == workspace_root {
        let mut targets: Vec<_> = workspace
            .packages
            .iter()
            .map(|(package_url, pkg)| add_target_for_member(&workspace.root, package_url, pkg))
            .collect();
        targets.sort_by(|a, b| a.pcb_toml_path.cmp(&b.pcb_toml_path));

        if !targets.is_empty() {
            return Ok(targets);
        }

        let root_pcb_toml = workspace_root.join("pcb.toml");
        if root_pcb_toml.exists() {
            return Ok(vec![AddTarget {
                package_url: root_package_url(workspace),
                pcb_toml_path: root_pcb_toml,
            }]);
        }
    }

    let mut best_match: Option<(usize, AddTarget)> = None;
    for (package_url, pkg) in &workspace.packages {
        let pkg_dir = pkg
            .dir(&workspace.root)
            .canonicalize()
            .unwrap_or_else(|_| pkg.dir(&workspace.root));
        if candidate_dir == pkg_dir || candidate_dir.starts_with(&pkg_dir) {
            let score = pkg_dir.as_os_str().len();
            let target = add_target_for_member(&workspace.root, package_url, pkg);
            if best_match
                .as_ref()
                .map(|(best_score, _)| score > *best_score)
                .unwrap_or(true)
            {
                best_match = Some((score, target));
            }
        }
    }

    if let Some((_, target)) = best_match {
        return Ok(vec![target]);
    }

    bail!(
        "`pcb add` must be run from a package directory or the workspace root.\n\
         Current path: {}\n\
         Workspace root: {}",
        candidate_dir.display(),
        workspace_root.display()
    );
}

fn add_target_for_member(
    workspace_root: &Path,
    package_url: &str,
    pkg: &MemberPackage,
) -> AddTarget {
    AddTarget {
        package_url: package_url.to_string(),
        pcb_toml_path: pkg.dir(workspace_root).join("pcb.toml"),
    }
}

fn root_package_url(workspace: &WorkspaceInfo) -> String {
    workspace
        .workspace_base_url()
        .unwrap_or_else(|| "workspace".to_string())
}
