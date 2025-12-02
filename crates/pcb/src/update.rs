//! Update dependencies to latest compatible versions
//!
//! Non-breaking updates (patch/minor within semver family) are applied automatically.
//! Breaking updates (major version or 0.x minor version changes) require interactive selection.

use anyhow::{bail, Result};
use clap::Args;
use colored::Colorize;
use inquire::MultiSelect;
use pcb_zen::workspace::get_workspace_info;
use pcb_zen::{get_all_versions_for_repo, git, semver_family};
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use semver::Version;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

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

/// A specific update to apply to a pcb.toml file
#[derive(Debug, Clone)]
struct PendingUpdate {
    url: String,
    current: Version,
    new_version: Version,
    is_breaking: bool,
    pcb_toml_path: PathBuf,
}

pub fn execute(args: UpdateArgs) -> Result<()> {
    let start_path = args.path.canonicalize().unwrap_or(args.path.clone());
    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info(&file_provider, &start_path)?;

    if !workspace.is_v2() {
        bail!(
            "pcb update requires a V2 workspace.\n\
             Run 'pcb migrate' to upgrade your workspace."
        );
    }

    println!("{}", "Checking for updates...".cyan());

    // Find all pending updates across all pcb.toml files
    let pending = find_pending_updates(&workspace, &args.packages)?;

    if !pending.is_empty() {
        // Separate and deduplicate for display (show each URL once)
        let mut displayed: HashSet<(&str, bool)> = HashSet::new();
        let mut non_breaking: Vec<&PendingUpdate> = Vec::new();
        let mut breaking: Vec<&PendingUpdate> = Vec::new();

        for u in &pending {
            if u.is_breaking {
                if displayed.insert((&u.url, true)) {
                    breaking.push(u);
                }
            } else if displayed.insert((&u.url, false)) {
                non_breaking.push(u);
            }
        }

        // Display non-breaking updates
        for u in &non_breaking {
            println!(
                "  {}: {} → {}",
                u.url,
                u.current,
                u.new_version.to_string().green()
            );
        }

        // Display breaking updates (always show, but only prompt if --breaking)
        for u in &breaking {
            println!(
                "  {}: {} → {} {}",
                u.url,
                u.current,
                u.new_version.to_string().yellow(),
                "(breaking)".yellow()
            );
        }

        // Collect URLs to apply
        let mut urls_to_apply: HashSet<&str> =
            non_breaking.iter().map(|u| u.url.as_str()).collect();

        // Breaking updates require interactive selection
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

        // Apply all matching updates
        let updates_to_apply: Vec<_> = pending
            .iter()
            .filter(|u| urls_to_apply.contains(u.url.as_str()))
            .collect();

        if !updates_to_apply.is_empty() {
            let breaking_count = updates_to_apply.iter().filter(|u| u.is_breaking).count();
            for u in &updates_to_apply {
                update_dependency_version(&u.pcb_toml_path, &u.url, &u.new_version)?;
            }

            // Summary - count unique files updated
            let files_updated: HashSet<_> =
                updates_to_apply.iter().map(|u| &u.pcb_toml_path).collect();
            let summary = if breaking_count > 0 {
                format!(
                    "Updated {} dependencies in {} files ({} breaking)",
                    updates_to_apply.len(),
                    files_updated.len(),
                    breaking_count
                )
            } else {
                format!(
                    "Updated {} dependencies in {} files",
                    updates_to_apply.len(),
                    files_updated.len()
                )
            };
            println!("{}", summary.green());
            println!("Run {} to update the lockfile.", "pcb build".cyan());
        }
    } else {
        println!("{}", "All dependencies are up to date.".green());
    }

    // Handle --tidy flag
    if args.tidy {
        tidy_lockfile(&workspace)?;
    }

    Ok(())
}

/// Remove unused entries from the lockfile
fn tidy_lockfile(workspace: &pcb_zen::WorkspaceInfo) -> Result<()> {
    use pcb_zen_core::config::Lockfile;

    let lockfile_path = workspace.root.join("pcb.sum");
    if !lockfile_path.exists() {
        return Ok(());
    }

    println!("{}", "Tidying lockfile...".cyan());

    let content = std::fs::read_to_string(&lockfile_path)?;
    let lockfile = Lockfile::parse(&content)?;

    let mut ws = workspace.clone();
    let resolution = pcb_zen::resolve_dependencies(&mut ws, false)?;

    let used_entries: HashSet<(String, String)> = resolution
        .closure
        .iter()
        .map(|(path, version)| (path.clone(), version.clone()))
        .collect();

    let used_assets: HashSet<(String, String)> = resolution
        .assets
        .keys()
        .map(|(path, ref_str)| (path.clone(), ref_str.clone()))
        .collect();

    let mut new_lockfile = Lockfile::default();
    let mut removed_count = 0;

    for entry in lockfile.iter() {
        let key = (entry.module_path.clone(), entry.version.clone());
        if used_entries.contains(&key) || used_assets.contains(&key) {
            new_lockfile.insert(entry.clone());
        } else {
            removed_count += 1;
        }
    }

    if removed_count > 0 {
        std::fs::write(&lockfile_path, new_lockfile.to_string())?;
        println!(
            "{}",
            format!("Removed {} unused entries.", removed_count).green()
        );
    } else {
        println!("{}", "Lockfile already clean.".green());
    }

    Ok(())
}

/// Find all pending updates across all pcb.toml files in the workspace
fn find_pending_updates(
    workspace: &pcb_zen::WorkspaceInfo,
    filter_packages: &[String],
) -> Result<Vec<PendingUpdate>> {
    let workspace_members: HashSet<&String> = workspace.packages.keys().collect();

    // Cache: repo_url -> (pkg_subpath -> versions)
    let mut version_cache: BTreeMap<String, BTreeMap<String, Vec<Version>>> = BTreeMap::new();

    let mut pending: Vec<PendingUpdate> = Vec::new();

    // Collect all pcb.toml paths to process
    let mut pcb_tomls: Vec<PathBuf> = Vec::new();
    let workspace_pcb_toml = workspace.root.join("pcb.toml");
    if workspace_pcb_toml.exists() {
        pcb_tomls.push(workspace_pcb_toml);
    }
    for pkg in workspace.packages.values() {
        let p = pkg.dir.join("pcb.toml");
        if p.exists() {
            pcb_tomls.push(p);
        }
    }

    for pcb_toml_path in pcb_tomls {
        let config = PcbToml::from_file(&DefaultFileProvider::new(), &pcb_toml_path)?;

        for (url, spec) in &config.dependencies {
            // Skip workspace members
            if workspace_members.contains(url) {
                continue;
            }

            // Apply filter
            if !filter_packages.is_empty() && !filter_packages.iter().any(|p| url.contains(p)) {
                continue;
            }

            // Extract current version
            let current = match extract_version(spec) {
                Some(v) => v,
                None => continue,
            };

            // Get available versions (cached per repo)
            let (repo_url, subpath) = git::split_repo_and_subpath(url);
            let repo_versions = version_cache
                .entry(repo_url.to_string())
                .or_insert_with(|| get_all_versions_for_repo(repo_url).unwrap_or_default());

            let pkg_key = if subpath.is_empty() { "" } else { subpath };
            let Some(available) = repo_versions.get(pkg_key) else {
                continue;
            };

            // Find best updates
            let current_family = semver_family(&current);

            if let Some(new_ver) = available
                .iter()
                .find(|v| semver_family(v) == current_family && *v > &current)
            {
                pending.push(PendingUpdate {
                    url: url.clone(),
                    current: current.clone(),
                    new_version: new_ver.clone(),
                    is_breaking: false,
                    pcb_toml_path: pcb_toml_path.clone(),
                });
            }

            if let Some(new_ver) = available
                .iter()
                .find(|v| semver_family(v) != current_family && *v > &current)
            {
                pending.push(PendingUpdate {
                    url: url.clone(),
                    current: current.clone(),
                    new_version: new_ver.clone(),
                    is_breaking: true,
                    pcb_toml_path: pcb_toml_path.clone(),
                });
            }
        }
    }

    Ok(pending)
}

fn extract_version(spec: &DependencySpec) -> Option<Version> {
    match spec {
        DependencySpec::Version(v) => Version::parse(v).ok(),
        DependencySpec::Detailed(d) => {
            if d.branch.is_some() || d.rev.is_some() || d.path.is_some() {
                return None;
            }
            d.version.as_ref().and_then(|v| Version::parse(v).ok())
        }
    }
}

/// Update a dependency version in a pcb.toml file
fn update_dependency_version(pcb_toml_path: &Path, url: &str, new_version: &Version) -> Result<()> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;

    if let Some(spec) = config.dependencies.get_mut(url) {
        *spec = DependencySpec::Version(new_version.to_string());
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok(())
}
