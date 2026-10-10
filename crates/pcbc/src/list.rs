use crate::pcb_mod;
use crate::pcb_mod::request::{available_versions_for_module, resolve_module_request};
use crate::pcb_mod::target::discover_package_target;
use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_zen::package_resolver::{PackageResolver, compatibility_lane};
use pcb_zen::resolve::ensure_package_manifest_in_cache;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::{DefaultFileProvider, TOOLCHAIN_VERSION};
use semver::Version;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "List package dependency information")]
pub struct ListArgs {
    /// Go-style list arguments. Supported: -m -u, -m -versions DEP, -m -json [PKG[@VERSION]]
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
    Json(Option<String>),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AvailableUpdates {
    compatible: Option<Version>,
    breaking: Option<Version>,
}

pub fn execute(args: ListArgs) -> Result<()> {
    match parse_args(&args.args)? {
        ListCommand::Updates => list_updates(),
        ListCommand::Versions(dep) => list_versions(&dep),
        ListCommand::Json(package) => list_json(package.as_deref()),
    }
}

fn parse_args(args: &[String]) -> Result<ListCommand> {
    match args {
        [module, updates] if module == "-m" && updates == "-u" => Ok(ListCommand::Updates),
        [module, versions, dep] if module == "-m" && versions == "-versions" => {
            Ok(ListCommand::Versions(dep.clone()))
        }
        [module, json, package @ ..] if module == "-m" && json == "-json" && package.len() <= 1 => {
            Ok(ListCommand::Json(package.first().cloned()))
        }
        _ => bail!("unsupported `pcb list` arguments\n\n{}", usage()),
    }
}

fn usage() -> &'static str {
    "Usage:\n  pcb list -m -u\n  pcb list -m -versions <dependency>\n  pcb list -m -json [<package>[@<version>]]"
}

fn list_updates() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &cwd)?;
    pcb_mod::validate_workspace(&workspace)?;

    let Some(target) = discover_package_target(&workspace, &cwd) else {
        bail!("`pcb list -m -u` must be run from a package directory.");
    };

    let mut resolver = PackageResolver::new(workspace.clone())?;
    let resolution = resolver.resolve_package(&target.package_url)?;

    for dep_id in &resolution.direct_remote_ids {
        let current = resolution.resolved_remote.get(dep_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Resolved direct dependency {} is missing a selected version",
                dep_id.path
            )
        })?;
        let updates = available_updates(&dep_id.path, current)
            .with_context(|| format!("Failed to check available versions for {}", dep_id.path))?;
        print_update_line(&dep_id.path, current, &updates);
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

/// A package's local source directory, as printed by `pcb list -m -json`.
/// Workspace packages have no version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Module {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<Version>,
    dir: PathBuf,
}

/// Print the stdlib, workspace packages, and resolved dependencies, or the one
/// package matching `package`. A versioned package, or a bare one the workspace
/// does not resolve, is fetched from its registry, like `go mod download -json`.
fn list_json(package: Option<&str>) -> Result<()> {
    let modules = match package {
        None => build_list()?,
        Some(package) if has_version(package) => vec![fetch_module(package)?],
        Some(package) => vec![find_module(package)?],
    };

    pcb_ui::write_stdout(|out| {
        modules.iter().try_for_each(|module| {
            serde_json::to_writer(&mut *out, module)?;
            writeln!(out)
        })
    })?;
    Ok(())
}

/// Resolve the whole workspace containing the current directory, so the
/// listing is the same from any directory in it.
fn build_list() -> Result<Vec<Module>> {
    let root = find_workspace_root(&DefaultFileProvider::new(), &std::env::current_dir()?)?;
    let resolution = crate::resolve::resolve(Some(&root), false)?;
    let workspace = &resolution.workspace_info;
    let stdlib = Module {
        path: "@stdlib".to_string(),
        version: Some(Version::parse(TOOLCHAIN_VERSION)?),
        dir: workspace.workspace_stdlib_dir(),
    };
    let packages = workspace.packages.iter().map(|(url, package)| Module {
        path: url.clone(),
        version: None,
        dir: package.dir(&workspace.root),
    });
    // Dependency roots are keyed `<url>@<version>`; other keys are workspace packages.
    let dependencies = resolution
        .package_roots()
        .into_iter()
        .filter_map(|(coord, dir)| {
            let (path, version) = coord.rsplit_once('@')?;
            Some(Module {
                path: path.to_string(),
                version: Some(Version::parse(version).ok()?),
                dir,
            })
        });

    let mut modules: Vec<_> = std::iter::once(stdlib)
        .chain(packages)
        .chain(dependencies)
        .collect();
    modules.sort();
    Ok(modules)
}

/// `<url>@<version>`, as opposed to a bare URL or `@stdlib`.
fn has_version(package: &str) -> bool {
    package
        .rsplit_once('@')
        .is_some_and(|(path, _)| !path.is_empty())
}

/// A bare path names the workspace's package (highest version if several
/// lanes), falling back to the latest release wherever the workspace does not
/// resolve it, including outside a workspace or when resolution fails.
fn find_module(package: &str) -> Result<Module> {
    let resolved = build_list().ok().and_then(|modules| {
        modules
            .into_iter()
            .rev()
            .find(|module| module.path == package)
    });
    resolved.map_or_else(|| fetch_module(package), Ok)
}

fn fetch_module(package: &str) -> Result<Module> {
    let (path, version) = resolve_module_request(package)?;
    let mut dir = ensure_package_manifest_in_cache(&path, &version)
        .with_context(|| format!("Failed to fetch {path}@{version}"))?;
    dir.pop();
    Ok(Module {
        path,
        version: Some(version),
        dir,
    })
}

fn print_update_line(dep: &str, current: &Version, updates: &AvailableUpdates) {
    let mut line = format!("{dep} {current}");
    if let Some(compatible) = &updates.compatible {
        line.push_str(&format!(" [{compatible}]"));
    }
    if let Some(breaking) = &updates.breaking {
        line.push_str(&format!(" [breaking: {breaking}]"));
    }
    println!("{line}");
}

fn available_updates(module_path: &str, current: &Version) -> Result<AvailableUpdates> {
    let versions = available_versions_for_module(module_path)?;
    Ok(select_available_updates(&versions, current))
}

fn select_available_updates(versions: &[Version], current: &Version) -> AvailableUpdates {
    let lane = compatibility_lane(current);
    let mut updates = AvailableUpdates::default();

    for version in versions
        .iter()
        .filter(|version| version.pre.is_empty() && **version > *current)
    {
        let target = if compatibility_lane(version) == lane {
            &mut updates.compatible
        } else {
            &mut updates.breaking
        };
        if target.as_ref().is_none_or(|current| version > current) {
            *target = Some(version.clone());
        }
    }

    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    #[test]
    fn has_version_excludes_stdlib_and_bare_urls() {
        assert!(has_version("github.com/acme/foo@1.0.0"));
        assert!(has_version("github.com/acme/foo@latest"));
        assert!(!has_version("github.com/acme/foo"));
        assert!(!has_version("@stdlib"));
    }

    #[test]
    fn latest_compatible_ignores_prereleases() {
        let versions = vec![v("1.3.0-rc.1"), v("1.2.1"), v("1.2.0")];
        assert_eq!(
            select_available_updates(&versions, &v("1.2.0")).compatible,
            Some(v("1.2.1"))
        );
    }

    #[test]
    fn latest_compatible_stays_in_major_lane() {
        let versions = vec![v("2.0.0"), v("1.3.0"), v("1.2.1")];
        assert_eq!(
            select_available_updates(&versions, &v("1.2.0")).compatible,
            Some(v("1.3.0"))
        );
    }

    #[test]
    fn latest_compatible_treats_zero_minor_as_lane() {
        let versions = vec![v("0.4.0"), v("0.3.9"), v("0.3.2")];
        assert_eq!(
            select_available_updates(&versions, &v("0.3.2")).compatible,
            Some(v("0.3.9"))
        );
    }

    #[test]
    fn latest_breaking_uses_newer_different_lane() {
        let versions = vec![v("0.3.6"), v("0.2.1"), v("0.1.3"), v("0.1.2")];
        assert_eq!(
            select_available_updates(&versions, &v("0.1.2")),
            AvailableUpdates {
                compatible: Some(v("0.1.3")),
                breaking: Some(v("0.3.6")),
            }
        );
    }

    #[test]
    fn latest_breaking_ignores_prereleases() {
        let versions = vec![v("2.0.0-rc.1"), v("1.3.0"), v("1.2.0")];
        assert_eq!(
            select_available_updates(&versions, &v("1.2.0")),
            AvailableUpdates {
                compatible: Some(v("1.3.0")),
                breaking: None,
            }
        );
    }
}
