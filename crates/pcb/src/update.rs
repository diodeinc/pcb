//! Update dependencies to latest compatible versions
//!
//! Non-breaking updates (patch/minor within semver family) are applied automatically.
//! Breaking updates (major version or 0.x minor version changes) require interactive selection.

use anyhow::Result;
use clap::Args;
use colored::Colorize;
use inquire::MultiSelect;
use pcb_zen::cache_index::CacheIndex;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen::{WorkspaceInfo, git, resolve, tags};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::resolution::semver_family;
use semver::Version;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Update dependencies to latest compatible versions")]
pub struct UpdateArgs {
    /// Path to workspace (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Specific packages to update (updates all if not specified)
    #[arg(long, short = 'p')]
    pub packages: Vec<String>,
}

struct PendingUpdate {
    url: String,
    current: Version,
    new_version: Version,
    is_breaking: bool,
    pcb_toml_path: PathBuf,
}

#[derive(Debug, Clone)]
enum UpdateScope {
    Workspace,
    Package { pcb_toml_path: PathBuf },
}

#[derive(Debug, Clone, Copy)]
enum AutoUpdatePolicy {
    /// Auto-update within the same semver family (current behavior).
    SemverFamily,
    /// Auto-update only patch versions (same major+minor).
    PatchOnly,
}

fn is_patch_bump(current: &Version, candidate: &Version) -> bool {
    candidate.major == current.major && candidate.minor == current.minor && candidate > current
}

fn detect_update_scope(workspace: &WorkspaceInfo, start_path: &Path) -> UpdateScope {
    // Start from a directory; if a file was provided, use its parent dir.
    let candidate_dir = if start_path.is_file() {
        start_path.parent().unwrap_or(start_path)
    } else {
        start_path
    };

    // Normalize paths to reduce false negatives when comparing.
    let candidate_dir = candidate_dir
        .canonicalize()
        .unwrap_or_else(|_| candidate_dir.to_path_buf());
    let ws_root = workspace
        .root
        .canonicalize()
        .unwrap_or_else(|_| workspace.root.clone());

    let candidate_pcb_toml = candidate_dir.join("pcb.toml");
    if !candidate_pcb_toml.exists() {
        return UpdateScope::Workspace;
    }

    // If the path is the workspace root, keep workspace-wide behavior.
    if candidate_dir == ws_root {
        return UpdateScope::Workspace;
    }

    // Only scope to a package if the directory matches a discovered workspace member.
    for pkg in workspace.packages.values() {
        let pkg_dir = pkg
            .dir(&workspace.root)
            .canonicalize()
            .unwrap_or_else(|_| pkg.dir(&workspace.root));
        if pkg_dir == candidate_dir {
            return UpdateScope::Package {
                pcb_toml_path: candidate_pcb_toml,
            };
        }
    }

    UpdateScope::Workspace
}

/// Collect all pcb.toml paths in the workspace
fn collect_pcb_tomls(workspace: &WorkspaceInfo, scope: &UpdateScope) -> Vec<PathBuf> {
    match scope {
        UpdateScope::Workspace => {
            let mut paths = Vec::new();
            let root = workspace.root.join("pcb.toml");
            if root.exists() {
                paths.push(root);
            }
            for pkg in workspace.packages.values() {
                let p = pkg.dir(&workspace.root).join("pcb.toml");
                if p.exists() {
                    paths.push(p);
                }
            }
            paths
        }
        UpdateScope::Package { pcb_toml_path } => vec![pcb_toml_path.clone()],
    }
}

fn matches_filter(url: &str, filter: &[String]) -> bool {
    filter.is_empty() || filter.iter().any(|p| url.contains(p))
}

pub fn execute(args: UpdateArgs) -> Result<()> {
    let start_path = args.path.canonicalize().unwrap_or(args.path.clone());
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &start_path)?;

    println!("{}", "Checking for updates...".cyan());

    // Clear branch cache so resolution picks up latest commits
    let index = CacheIndex::open()?;
    index.clear_branch_commits()?;

    let scope = detect_update_scope(&workspace, &start_path);
    if let UpdateScope::Package { pcb_toml_path } = &scope {
        println!(
            "{} {}",
            "Limiting updates to:".cyan(),
            pcb_toml_path.display()
        );
    }

    let policy = match scope {
        UpdateScope::Workspace => AutoUpdatePolicy::SemverFamily,
        UpdateScope::Package { .. } => AutoUpdatePolicy::PatchOnly,
    };

    let version_updates = find_version_updates(&workspace, &args.packages, &scope, policy)?;

    // Display and apply version updates
    let applied_count = apply_version_updates(&version_updates)?;
    let refreshed_branch_pins = resolve::refresh_branch_pins_in_manifests(
        &collect_pcb_tomls(&workspace, &scope),
        &args.packages,
    )?;
    if refreshed_branch_pins > 0 {
        println!(
            "{}",
            format!("Refreshed {} branch pin(s).", refreshed_branch_pins).green()
        );
    }

    // Run resolution to update lockfile (will re-fetch branch commits)
    // locked=false since update is explicitly for updating deps
    let mut ws = workspace.clone();
    let resolution = pcb_zen::resolve_dependencies(&mut ws, false, false)?;
    let lockfile_changed = resolution.lockfile_changed;

    if applied_count > 0 || refreshed_branch_pins > 0 || lockfile_changed {
        println!("{}", "Updated lockfile.".green());
    } else {
        println!("{}", "All dependencies are up to date.".green());
    }

    Ok(())
}

/// Display version updates and apply selected ones. Returns count of applied updates.
fn apply_version_updates(pending: &[PendingUpdate]) -> Result<usize> {
    if pending.is_empty() {
        return Ok(0);
    }

    // Deduplicate for display
    let mut displayed: HashSet<(&str, bool)> = HashSet::new();
    let mut non_breaking: Vec<&PendingUpdate> = Vec::new();
    let mut breaking: Vec<&PendingUpdate> = Vec::new();

    for u in pending {
        let key = (u.url.as_str(), u.is_breaking);
        if displayed.insert(key) {
            if u.is_breaking {
                breaking.push(u);
            } else {
                non_breaking.push(u);
            }
        }
    }

    // Display
    for u in &non_breaking {
        println!(
            "  {}: {} → {}",
            u.url,
            u.current,
            u.new_version.to_string().green()
        );
    }
    for u in &breaking {
        println!(
            "  {}: {} → {} {}",
            u.url,
            u.current,
            u.new_version.to_string().yellow(),
            "(breaking)".yellow()
        );
    }

    // Auto-apply non-breaking, prompt for breaking
    let mut selected_breaking_urls: HashSet<&str> = HashSet::new();

    if !breaking.is_empty() {
        let options: Vec<String> = breaking
            .iter()
            .map(|u| format!("{} {} → {}", u.url, u.current, u.new_version))
            .collect();

        let selected = MultiSelect::new("Select breaking updates to apply:", options.clone())
            .prompt()
            .unwrap_or_default();

        for (i, u) in breaking.iter().enumerate() {
            if selected.contains(&options[i]) {
                selected_breaking_urls.insert(&u.url);
            }
        }
    }

    // Apply: non-breaking always, breaking only if selected
    let to_apply: Vec<_> = pending
        .iter()
        .filter(|u| {
            if u.is_breaking {
                selected_breaking_urls.contains(u.url.as_str())
            } else {
                true
            }
        })
        .collect();

    for u in &to_apply {
        let mut config = PcbToml::from_file(&DefaultFileProvider::new(), &u.pcb_toml_path)?;
        if let Some(spec) = config.dependencies.get_mut(&u.url) {
            *spec = DependencySpec::Version(u.new_version.to_string());
            std::fs::write(&u.pcb_toml_path, toml::to_string_pretty(&config)?)?;
        }
    }

    if !to_apply.is_empty() {
        let files: HashSet<_> = to_apply.iter().map(|u| &u.pcb_toml_path).collect();
        let breaking_count = to_apply.iter().filter(|u| u.is_breaking).count();
        let msg = if breaking_count > 0 {
            format!(
                "Updated {} dependencies in {} files ({} breaking)",
                to_apply.len(),
                files.len(),
                breaking_count
            )
        } else {
            format!(
                "Updated {} dependencies in {} files",
                to_apply.len(),
                files.len()
            )
        };
        println!("{}", msg.green());
    }

    Ok(to_apply.len())
}

fn find_version_updates(
    workspace: &WorkspaceInfo,
    filter: &[String],
    scope: &UpdateScope,
    policy: AutoUpdatePolicy,
) -> Result<Vec<PendingUpdate>> {
    let workspace_members: HashSet<&String> = workspace.packages.keys().collect();
    let mut version_cache: BTreeMap<String, BTreeMap<String, Vec<Version>>> = BTreeMap::new();
    let mut pending = Vec::new();

    for pcb_toml_path in collect_pcb_tomls(workspace, scope) {
        let config = PcbToml::from_file(&DefaultFileProvider::new(), &pcb_toml_path)?;

        for (url, spec) in &config.dependencies {
            // Skip workspace members, filtered packages, and stdlib (pinned to toolchain)
            if workspace_members.contains(url)
                || !matches_filter(url, filter)
                || url == pcb_zen_core::STDLIB_MODULE_PATH
            {
                continue;
            }

            // Only version deps (not branch/rev/path)
            let current = match spec {
                DependencySpec::Version(v) => Version::parse(v).ok(),
                DependencySpec::Detailed(d)
                    if d.branch.is_none() && d.rev.is_none() && d.path.is_none() =>
                {
                    d.version.as_ref().and_then(|v| Version::parse(v).ok())
                }
                _ => None,
            };
            let Some(current) = current else { continue };

            let (repo_url, subpath) = git::split_repo_and_subpath(url);
            let repo_versions = version_cache
                .entry(repo_url.to_string())
                .or_insert_with(|| tags::get_all_versions_for_repo(repo_url).unwrap_or_default());

            let pkg_key = if subpath.is_empty() { "" } else { subpath };
            let Some(available) = repo_versions.get(pkg_key) else {
                continue;
            };

            let current_family = semver_family(&current);

            // Auto-update policy (non-breaking)
            let non_breaking = match policy {
                AutoUpdatePolicy::SemverFamily => available
                    .iter()
                    .find(|v| semver_family(v) == current_family && *v > &current),
                AutoUpdatePolicy::PatchOnly => {
                    available.iter().find(|v| is_patch_bump(&current, v))
                }
            };

            if let Some(v) = non_breaking {
                pending.push(PendingUpdate {
                    url: url.clone(),
                    current: current.clone(),
                    new_version: v.clone(),
                    is_breaking: false,
                    pcb_toml_path: pcb_toml_path.clone(),
                });
            }

            // Breaking update (different family)
            if let Some(v) = available
                .iter()
                .find(|v| semver_family(v) != current_family && *v > &current)
            {
                pending.push(PendingUpdate {
                    url: url.clone(),
                    current: current.clone(),
                    new_version: v.clone(),
                    is_breaking: true,
                    pcb_toml_path: pcb_toml_path.clone(),
                });
            }
        }
    }

    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_zen::MemberPackage;
    use pcb_zen_core::config::PcbToml;
    use std::collections::BTreeMap;

    #[test]
    fn test_is_patch_bump() {
        let cur = Version::parse("1.2.3").unwrap();
        assert!(is_patch_bump(&cur, &Version::parse("1.2.4").unwrap()));
        assert!(!is_patch_bump(&cur, &Version::parse("1.3.0").unwrap()));
        assert!(!is_patch_bump(&cur, &Version::parse("2.0.0").unwrap()));
        assert!(!is_patch_bump(&cur, &Version::parse("1.2.3").unwrap()));

        let cur0 = Version::parse("0.3.2").unwrap();
        assert!(is_patch_bump(&cur0, &Version::parse("0.3.9").unwrap()));
        assert!(!is_patch_bump(&cur0, &Version::parse("0.4.0").unwrap()));
    }

    #[test]
    fn test_detect_update_scope_member_package_dir() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();

        let member_rel = PathBuf::from("packages/foo");
        let member_abs = root.join(&member_rel);
        std::fs::create_dir_all(&member_abs).unwrap();
        std::fs::write(member_abs.join("pcb.toml"), "").unwrap();

        let mut packages = BTreeMap::new();
        packages.insert(
            "github.com/example/foo".to_string(),
            MemberPackage {
                rel_path: member_rel,
                config: PcbToml::default(),
                version: None,
                dirty: false,
            },
        );

        let ws = WorkspaceInfo {
            root: root.clone(),
            cache_dir: PathBuf::new(),
            config: None,
            packages,
            lockfile: None,
            errors: vec![],
        };

        let scope = detect_update_scope(&ws, &member_abs);
        match scope {
            UpdateScope::Package { pcb_toml_path } => {
                let expected = member_abs.join("pcb.toml").canonicalize().unwrap();
                assert_eq!(pcb_toml_path, expected);
            }
            UpdateScope::Workspace => panic!("expected package scope"),
        }
    }

    #[test]
    fn test_detect_update_scope_non_member_dir_with_pcb_toml() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();

        let other = root.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("pcb.toml"), "").unwrap();

        let ws = WorkspaceInfo {
            root: root.clone(),
            cache_dir: PathBuf::new(),
            config: None,
            packages: BTreeMap::new(),
            lockfile: None,
            errors: vec![],
        };

        let scope = detect_update_scope(&ws, &other);
        assert!(matches!(scope, UpdateScope::Workspace));
    }
}
