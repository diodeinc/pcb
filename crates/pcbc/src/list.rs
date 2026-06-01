use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_zen::WorkspaceInfo;
use pcb_zen::package_resolver::{PackageResolver, compatibility_lane};
use pcb_zen::tags;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::split_repo_and_subpath;
use semver::Version;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
#[command(about = "List package dependency information")]
pub struct ListArgs {
    /// Go-style list arguments. Supported: -m -u, -m -versions DEP
    #[arg(
        value_name = "ARGS",
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    pub args: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum ListCommand {
    Updates,
    Versions(String),
}

pub fn execute(args: ListArgs) -> Result<()> {
    match parse_args(&args.args)? {
        ListCommand::Updates => list_updates(),
        ListCommand::Versions(dep) => list_versions(&dep),
    }
}

fn parse_args(args: &[String]) -> Result<ListCommand> {
    match args {
        [module, updates] if module == "-m" && updates == "-u" => Ok(ListCommand::Updates),
        [module, versions, dep] if module == "-m" && versions == "-versions" => {
            Ok(ListCommand::Versions(dep.clone()))
        }
        _ => bail!("unsupported `pcb list` arguments\n\n{}", usage()),
    }
}

fn usage() -> &'static str {
    "Usage:\n  pcb list -m -u\n  pcb list -m -versions <dependency>"
}

fn list_updates() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd)?;
    validate_workspace(&workspace)?;

    let Some(package_url) = discover_package_target(&workspace, &cwd) else {
        bail!("`pcb list -m -u` must be run from a package directory.");
    };

    let mut resolver = PackageResolver::new(workspace.clone(), false)?;
    let resolution = resolver.resolve_package(&package_url)?;

    for dep_id in &resolution.direct_remote_ids {
        if is_configured_kicad_repo(&workspace, &dep_id.path) {
            continue;
        }

        let current = resolution.resolved_remote.get(dep_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Resolved direct dependency {} is missing a selected version",
                dep_id.path
            )
        })?;
        let latest = latest_stable_compatible_version(&dep_id.path, current)
            .with_context(|| format!("Failed to check available versions for {}", dep_id.path))?;
        print_update_line(&dep_id.path, current, latest.as_ref());
    }

    Ok(())
}

fn list_versions(dep: &str) -> Result<()> {
    if dep.contains('@') {
        bail!("`pcb list -m -versions` expects a dependency URL without a version selector");
    }

    let mut versions = available_versions_for_module(dep)
        .with_context(|| format!("Failed to fetch versions for {dep}"))?;
    if versions.is_empty() {
        bail!("No published versions found for {dep}");
    }
    versions.sort();

    let rendered = versions
        .iter()
        .map(Version::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    println!("{dep} {rendered}");
    Ok(())
}

fn print_update_line(dep: &str, current: &Version, latest: Option<&Version>) {
    match latest {
        Some(latest) => println!("{dep} {current} [{latest}]"),
        None => println!("{dep} {current}"),
    }
}

fn latest_stable_compatible_version(
    module_path: &str,
    current: &Version,
) -> Result<Option<Version>> {
    let versions = available_versions_for_module(module_path)?;
    Ok(select_latest_stable_compatible(&versions, current))
}

fn available_versions_for_module(module_path: &str) -> Result<Vec<Version>> {
    let (repo_url, subpath) = split_repo_and_subpath(module_path);
    let all_versions = tags::get_all_versions_for_repo(repo_url)?;
    Ok(all_versions.get(subpath).cloned().unwrap_or_default())
}

fn select_latest_stable_compatible(versions: &[Version], current: &Version) -> Option<Version> {
    let lane = compatibility_lane(current);
    versions
        .iter()
        .filter(|version| {
            version.pre.is_empty() && **version > *current && compatibility_lane(version) == lane
        })
        .max()
        .cloned()
}

fn discover_package_target(workspace: &WorkspaceInfo, start_path: &Path) -> Option<String> {
    let candidate_dir = candidate_dir(start_path);
    workspace
        .packages
        .iter()
        .filter_map(|(package_url, package)| {
            let package_dir = canonicalize(&package.dir(&workspace.root));
            (candidate_dir == package_dir || candidate_dir.starts_with(&package_dir))
                .then(|| (package_dir.as_os_str().len(), package_url.clone()))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, package_url)| package_url)
}

fn candidate_dir(start_path: &Path) -> PathBuf {
    let dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };
    canonicalize(dir)
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_configured_kicad_repo(workspace: &WorkspaceInfo, module_path: &str) -> bool {
    workspace
        .kicad_library_entries()
        .iter()
        .any(|entry| entry.repo_urls().any(|repo| repo == module_path))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    #[test]
    fn parse_update_args() {
        let args = vec!["-m".to_string(), "-u".to_string()];
        assert_eq!(parse_args(&args).unwrap(), ListCommand::Updates);
    }

    #[test]
    fn parse_versions_args() {
        let args = vec![
            "-m".to_string(),
            "-versions".to_string(),
            "github.com/acme/foo".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            ListCommand::Versions("github.com/acme/foo".to_string())
        );
    }

    #[test]
    fn latest_compatible_ignores_prereleases() {
        let versions = vec![v("1.3.0-rc.1"), v("1.2.1"), v("1.2.0")];
        assert_eq!(
            select_latest_stable_compatible(&versions, &v("1.2.0")),
            Some(v("1.2.1"))
        );
    }

    #[test]
    fn latest_compatible_stays_in_major_lane() {
        let versions = vec![v("2.0.0"), v("1.3.0"), v("1.2.1")];
        assert_eq!(
            select_latest_stable_compatible(&versions, &v("1.2.0")),
            Some(v("1.3.0"))
        );
    }

    #[test]
    fn latest_compatible_treats_zero_minor_as_lane() {
        let versions = vec![v("0.4.0"), v("0.3.9"), v("0.3.2")];
        assert_eq!(
            select_latest_stable_compatible(&versions, &v("0.3.2")),
            Some(v("0.3.9"))
        );
    }
}
