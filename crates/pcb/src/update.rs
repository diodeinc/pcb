//! Update dependencies to latest compatible versions
//!
//! Non-breaking updates (patch/minor within semver family) are applied automatically.
//! Breaking updates (major version or 0.x minor version changes) require interactive selection.

use anyhow::{bail, Result};
use clap::Args;
use colored::Colorize;
use inquire::MultiSelect;
use pcb_zen::cache_index::CacheIndex;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen::{get_all_versions_for_repo, git, semver_family, WorkspaceInfo};
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use semver::Version;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Update dependencies to latest compatible versions")]
pub struct UpdateArgs {
    /// Path to workspace (defaults to current directory)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Remove unused entries from lockfile
    #[arg(long)]
    pub tidy: bool,

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

/// Collect all pcb.toml paths in the workspace
fn collect_pcb_tomls(workspace: &WorkspaceInfo) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let root = workspace.root.join("pcb.toml");
    if root.exists() {
        paths.push(root);
    }
    for pkg in workspace.packages.values() {
        let p = pkg.dir.join("pcb.toml");
        if p.exists() {
            paths.push(p);
        }
    }
    paths
}

fn matches_filter(url: &str, filter: &[String]) -> bool {
    filter.is_empty() || filter.iter().any(|p| url.contains(p))
}

pub fn execute(args: UpdateArgs) -> Result<()> {
    let start_path = args.path.canonicalize().unwrap_or(args.path.clone());
    let workspace = get_workspace_info(&DefaultFileProvider::new(), &start_path)?;

    if !workspace.is_v2() {
        bail!(
            "pcb update requires a V2 workspace.\n\
             Run 'pcb migrate' to upgrade your workspace."
        );
    }

    println!("{}", "Checking for updates...".cyan());

    let branch_updates = refresh_branch_deps(&workspace, &args.packages)?;
    let version_updates = find_version_updates(&workspace, &args.packages)?;

    // Display branch updates
    for (url, branch, old, new) in &branch_updates {
        println!(
            "  {}: {} → {} {}",
            url,
            &old[..12],
            new[..12].green(),
            format!("(branch: {})", branch).dimmed()
        );
    }

    // Display and apply version updates
    let applied_count = apply_version_updates(&version_updates)?;

    // Summary
    if !branch_updates.is_empty() {
        println!(
            "{}",
            format!("Refreshed {} branch dependencies.", branch_updates.len()).green()
        );
    }
    if applied_count > 0 || !branch_updates.is_empty() {
        println!("Run {} to update the lockfile.", "pcb build".cyan());
    } else if version_updates.is_empty() && branch_updates.is_empty() {
        println!("{}", "All dependencies are up to date.".green());
    }

    if args.tidy {
        tidy_lockfile(&workspace)?;
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
    let mut urls_to_apply: HashSet<&str> = non_breaking.iter().map(|u| u.url.as_str()).collect();

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
                urls_to_apply.insert(&u.url);
            }
        }
    }

    // Apply
    let to_apply: Vec<_> = pending
        .iter()
        .filter(|u| urls_to_apply.contains(u.url.as_str()))
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
) -> Result<Vec<PendingUpdate>> {
    let workspace_members: HashSet<&String> = workspace.packages.keys().collect();
    let mut version_cache: BTreeMap<String, BTreeMap<String, Vec<Version>>> = BTreeMap::new();
    let mut pending = Vec::new();

    for pcb_toml_path in collect_pcb_tomls(workspace) {
        let config = PcbToml::from_file(&DefaultFileProvider::new(), &pcb_toml_path)?;

        for (url, spec) in &config.dependencies {
            if workspace_members.contains(url) || !matches_filter(url, filter) {
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
                .or_insert_with(|| get_all_versions_for_repo(repo_url).unwrap_or_default());

            let pkg_key = if subpath.is_empty() { "" } else { subpath };
            let Some(available) = repo_versions.get(pkg_key) else {
                continue;
            };

            let current_family = semver_family(&current);

            // Non-breaking update (same family)
            if let Some(v) = available
                .iter()
                .find(|v| semver_family(v) == current_family && *v > &current)
            {
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

fn refresh_branch_deps(
    workspace: &WorkspaceInfo,
    filter: &[String],
) -> Result<Vec<(String, String, String, String)>> {
    let mut updates = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let index = CacheIndex::open()?;

    for pcb_toml_path in collect_pcb_tomls(workspace) {
        let config = PcbToml::from_file(&DefaultFileProvider::new(), &pcb_toml_path)?;

        for (url, spec) in &config.dependencies {
            let branch = match spec {
                DependencySpec::Detailed(d) => d.branch.as_ref(),
                _ => None,
            };
            let Some(branch) = branch else { continue };

            if !matches_filter(url, filter) {
                continue;
            }

            let (repo_url, _) = git::split_repo_and_subpath(url);
            if !seen.insert((repo_url.to_string(), branch.clone())) {
                continue;
            }

            let Some(old_commit) = index.get_branch_commit(repo_url, branch) else {
                continue; // Not cached yet
            };

            log::debug!("Fetching latest commit for {} (branch: {})", url, branch);
            let new_commit =
                match git::ls_remote_with_fallback(url, &format!("refs/heads/{}", branch)) {
                    Ok((commit, _)) => {
                        log::debug!("  {} -> {}", branch, &commit[..12]);
                        commit
                    }
                    Err(e) => {
                        log::warn!("Failed to fetch {}: {}", url, e);
                        continue;
                    }
                };

            if new_commit != old_commit {
                index.set_branch_commit(repo_url, branch, &new_commit)?;
                updates.push((url.clone(), branch.clone(), old_commit, new_commit));
            }
        }
    }

    Ok(updates)
}

fn tidy_lockfile(workspace: &WorkspaceInfo) -> Result<()> {
    use pcb_zen_core::config::Lockfile;

    let lockfile_path = workspace.root.join("pcb.sum");
    if !lockfile_path.exists() {
        return Ok(());
    }

    println!("{}", "Tidying lockfile...".cyan());

    let lockfile = Lockfile::parse(&std::fs::read_to_string(&lockfile_path)?)?;
    let mut ws = workspace.clone();
    let resolution = pcb_zen::resolve_dependencies(&mut ws, false)?;

    let used: HashSet<(String, String)> = resolution
        .closure
        .iter()
        .chain(resolution.assets.keys())
        .map(|(p, v)| (p.clone(), v.clone()))
        .collect();

    let mut new_lockfile = Lockfile::default();
    let mut removed = 0;

    for entry in lockfile.iter() {
        if used.contains(&(entry.module_path.clone(), entry.version.clone())) {
            new_lockfile.insert(entry.clone());
        } else {
            removed += 1;
        }
    }

    if removed > 0 {
        std::fs::write(&lockfile_path, new_lockfile.to_string())?;
        println!("{}", format!("Removed {} unused entries.", removed).green());
    } else {
        println!("{}", "Lockfile already clean.".green());
    }

    Ok(())
}
