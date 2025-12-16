//! V2 package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes.

use anyhow::{bail, Result};
use clap::{Args, ValueEnum};
use colored::Colorize;
use inquire::{Confirm, Select};
use pcb_zen::workspace::{
    get_workspace_info, DirtyReason, MemberPackage, WorkspaceInfo, WorkspaceInfoExt,
};
use pcb_zen::{canonical, git, tags};
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use rayon::prelude::*;
use semver::Version;
use std::collections::BTreeMap;
use std::env;
use std::path::Path;

use crate::file_walker::collect_zen_files;

/// Version bump type for publishing
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum BumpType {
    /// Bug fixes only (x.y.Z)
    Patch,
    /// New features, backwards compatible (x.Y.0)
    Minor,
    /// Breaking changes (X.0.0)
    Major,
}

/// Strategy for determining version bumps across waves
enum BumpStrategy {
    /// Apply the same bump type to all packages
    SameForAll(BumpType),
    /// Prompt for each package individually
    ChooseIndividually,
}

/// Get publishable (non-board) dirty packages from workspace
fn get_dirty_packages<'a>(
    workspace: &'a WorkspaceInfo,
    dirty_map: &BTreeMap<String, DirtyReason>,
) -> Vec<(&'a String, &'a MemberPackage)> {
    dirty_map
        .keys()
        .filter_map(|url| {
            workspace
                .packages
                .get_key_value(url)
                .filter(|(_, pkg)| pkg.config.board.is_none())
        })
        .collect()
}

/// Result of running a single publish wave
enum WaveResult {
    /// Packages were published
    Published {
        tags: Vec<String>,
        commit: Option<String>,
    },
    /// No dirty packages found
    NothingToPublish,
}

/// Run a single wave of publishing
fn run_wave(
    workspace: &WorkspaceInfo,
    bump_strategy: &BumpStrategy,
    wave: usize,
) -> Result<WaveResult> {
    // Compute dirty packages fresh each wave (parallel)
    let dirty_map = workspace.dirty_packages();
    let dirty_packages = get_dirty_packages(workspace, &dirty_map);

    if dirty_packages.is_empty() {
        return Ok(WaveResult::NothingToPublish);
    }

    // Fetch fresh tags to get current versions (tags may have been created in previous waves)
    let all_tags = git::list_all_tags_vec(&workspace.root);
    let ws_path = workspace.path();

    // Get bump types for this wave's dirty packages based on strategy
    let bump_map: BTreeMap<String, BumpType> = match bump_strategy {
        BumpStrategy::SameForAll(bump) => dirty_packages
            .iter()
            .map(|(url, _)| ((*url).clone(), *bump))
            .collect(),
        BumpStrategy::ChooseIndividually => {
            let mut map = BTreeMap::new();
            for (url, pkg) in &dirty_packages {
                let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
                let current_version = tags::find_latest_version(&all_tags, &tag_prefix);
                let bump = prompt_single_bump(url, current_version.as_ref())?;
                map.insert((*url).clone(), bump);
            }
            map
        }
    };

    let candidates = build_publish_candidates(workspace, &dirty_map, &bump_map)?;

    if candidates.is_empty() {
        return Ok(WaveResult::NothingToPublish);
    }

    // Display wave
    println!();
    println!("{}", format!("Wave {}:", wave).cyan().bold());
    println!("{} package(s) to publish:", candidates.len());

    let mut created_tags = Vec::new();
    for (url, c) in &candidates {
        print_candidate(c, &workspace.root);
        git::create_tag(&workspace.root, &c.tag_name, &format_tag_message(url, c))?;
        created_tags.push(c.tag_name.clone());
    }

    // Find all packages that depend on just-published packages and bump their versions
    let changed_pkgs: Vec<&MemberPackage> = workspace
        .packages
        .iter()
        .filter(|(_, pkg)| pkg.dependencies().any(|d| candidates.contains_key(d)))
        .filter_map(|(_, pkg)| {
            let changed = bump_dependency_versions(&pkg.dir.join("pcb.toml"), &candidates).unwrap();
            if changed {
                println!("  patching: {}/pcb.toml", pkg.rel_path.display());
                Some(pkg)
            } else {
                None
            }
        })
        .collect();

    // Commit changes
    let commit_sha = if !changed_pkgs.is_empty() {
        let commit_msg = format_dependency_bump_commit(&changed_pkgs, &candidates);
        git::commit_with_trailers(&workspace.root, &commit_msg)?;
        git::rev_parse(&workspace.root, "HEAD")
    } else {
        None
    };

    Ok(WaveResult::Published {
        tags: created_tags,
        commit: commit_sha,
    })
}

#[derive(Args, Debug)]
#[command(about = "Publish packages by creating version tags")]
pub struct PublishArgs {
    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f')]
    pub force: bool,

    /// Suppress diagnostics by kind or severity. Use 'warnings' or 'errors' for all
    /// warnings/errors, or specific kinds like 'electrical.voltage_mismatch'.
    /// Supports hierarchical matching (e.g., 'electrical' matches 'electrical.voltage_mismatch')
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Version bump type (non-interactive). Applies same bump to all packages.
    #[arg(long, value_enum)]
    pub bump: Option<BumpType>,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

/// Info about a package that will be published
struct PublishCandidate {
    pkg: MemberPackage,
    current_version: Option<Version>,
    next_version: Version,
    tag_name: String,
    content_hash: String,
    manifest_hash: String,
}

pub fn execute(args: PublishArgs) -> Result<()> {
    let start_path = args
        .path
        .as_ref()
        .map(|p| Path::new(p).to_path_buf())
        .unwrap_or_else(|| env::current_dir().unwrap());

    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info(&file_provider, &start_path)?;

    let remote = if args.force {
        let current_branch = git::symbolic_ref_short_head(&workspace.root)
            .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
        git::get_branch_remote(&workspace.root, &current_branch).ok_or_else(|| {
            anyhow::anyhow!("Branch '{}' is not tracking a remote", current_branch)
        })?
    } else {
        preflight_checks(&workspace.root)?
    };

    // Sync local tags with remote to ensure accurate version detection
    println!("Syncing tags from {}...", remote.cyan());
    git::fetch_tags(&workspace.root, &remote)?;

    // Build workspace to validate before publishing
    build_workspace(&workspace, &args.suppress)?;

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    // Determine bump strategy (CLI flag or interactive prompt)
    // This is used across all waves for consistency
    let bump_strategy: BumpStrategy = if let Some(bump) = args.bump {
        BumpStrategy::SameForAll(bump)
    } else {
        // Compute initial dirty packages to determine strategy
        let initial_dirty_map = workspace.dirty_packages();
        let dirty_packages = get_dirty_packages(&workspace, &initial_dirty_map);

        if dirty_packages.is_empty() {
            println!("{}", "No packages to publish".green());
            return Ok(());
        }

        prompt_bump_strategy_choice(&dirty_packages)?
    };

    let mut all_tags: Vec<String> = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    let mut wave = 0;

    // Run waves until no more dirty packages, with rollback on error
    let mut wave_loop = || -> Result<()> {
        loop {
            wave += 1;
            match run_wave(&workspace, &bump_strategy, wave)? {
                WaveResult::Published { tags, commit } => {
                    all_tags.extend(tags);
                    if let Some(sha) = commit {
                        commits.push(sha);
                    }
                }
                WaveResult::NothingToPublish => {
                    if wave == 1 {
                        println!("{}", "No packages to publish".green());
                    }
                    break;
                }
            }
        }
        Ok(())
    };

    if let Err(e) = wave_loop() {
        let _ = rollback(
            &workspace.root,
            &all_tags,
            (!commits.is_empty()).then_some(&initial_commit),
        );
        return Err(e);
    }

    if all_tags.is_empty() {
        return Ok(());
    }

    // Confirm and push
    println!();
    let prompt = if !commits.is_empty() {
        format!(
            "Push main branch and {} tag(s) to {}?",
            all_tags.len(),
            remote
        )
    } else {
        format!("Push {} tag(s) to {}?", all_tags.len(), remote)
    };

    if !Confirm::new(&prompt)
        .with_default(false)
        .prompt()
        .unwrap_or(false)
    {
        return rollback(
            &workspace.root,
            &all_tags,
            (!commits.is_empty()).then_some(&initial_commit),
        );
    }

    println!();
    println!("Pushing to {}...", remote.cyan());
    if !commits.is_empty() {
        git::push_branch(&workspace.root, "main", &remote)?;
        println!("  Pushed main branch");
    }
    let tag_refs: Vec<&str> = all_tags.iter().map(|s| s.as_str()).collect();
    git::push_tags(&workspace.root, &tag_refs, &remote)?;
    for tag in &all_tags {
        println!("  Pushed {}", tag.green());
    }

    Ok(())
}

fn build_workspace(workspace: &WorkspaceInfo, suppress: &[String]) -> Result<()> {
    println!();
    println!("{}", "Building workspace...".cyan().bold());

    // Collect all .zen files from workspace (skips vendor/, respects gitignore)
    let zen_files = collect_zen_files(std::slice::from_ref(&workspace.root), false)?;

    if zen_files.is_empty() {
        return Ok(());
    }

    // Resolve dependencies if v2 workspace
    let resolution_result = if workspace.is_v2() {
        let mut ws = workspace.clone();
        let resolution = pcb_zen::resolve_dependencies(&mut ws, false)?;
        pcb_zen::vendor_deps(&ws, &resolution, &[], None)?;
        Some(resolution)
    } else {
        None
    };

    let mut has_errors = false;
    let mut has_warnings = false;

    for zen_path in &zen_files {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        if let Some(schematic) = crate::build::build(
            zen_path,
            false,
            crate::build::create_diagnostics_passes(suppress, &[]),
            false,
            &mut has_errors,
            &mut has_warnings,
            resolution_result.clone(),
        ) {
            crate::build::print_build_success(&file_name, &schematic);
        }
    }

    if has_errors {
        bail!("Build failed. Fix errors before publishing.");
    }

    Ok(())
}

fn rollback(repo_root: &Path, tags: &[String], reset_to: Option<&String>) -> Result<()> {
    println!("Rolling back...");
    let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
    let _ = git::delete_tags(repo_root, &tag_refs);
    println!("  Deleted {} local tag(s)", tags.len());
    if let Some(commit) = reset_to {
        git::reset_hard(repo_root, commit)?;
        println!("  Reset to initial commit");
    }
    println!("{}", "Publish cancelled".yellow());
    Ok(())
}

fn print_candidate(c: &PublishCandidate, root: &Path) {
    let rel = c.pkg.dir.strip_prefix(root).unwrap_or(&c.pkg.dir);
    let path_display = if rel.as_os_str().is_empty() {
        "(root)".to_string()
    } else {
        rel.display().to_string()
    };
    let version_str = match &c.current_version {
        Some(v) => format!("{} → {}", v, c.next_version),
        None => format!("{} (initial)", c.next_version),
    };
    println!(
        "  {}: {} [{}]",
        path_display,
        version_str.green(),
        c.tag_name.cyan()
    );
}

fn preflight_checks(repo_root: &Path) -> Result<String> {
    if git::has_uncommitted_changes(repo_root)? {
        bail!(
            "Working directory has uncommitted changes.\n\
             Commit or stash your changes before publishing."
        );
    }

    let current_branch = git::symbolic_ref_short_head(repo_root).ok_or_else(|| {
        anyhow::anyhow!("Not on a branch (detached HEAD state). Switch to main before publishing.")
    })?;

    if current_branch != "main" {
        bail!(
            "Must be on 'main' branch to publish.\n\
             Current branch: '{}'\n\
             Run: git checkout main",
            current_branch
        );
    }

    let remote = git::get_branch_remote(repo_root, "main").ok_or_else(|| {
        anyhow::anyhow!(
            "Branch 'main' is not tracking a remote.\n\
             Set upstream with: git branch --set-upstream-to=<remote>/main"
        )
    })?;

    let local_sha = git::rev_parse(repo_root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get HEAD commit"))?;

    println!("{} on main @ {}", "✓".green(), &local_sha[..8]);

    Ok(remote)
}

fn build_publish_candidates(
    workspace: &WorkspaceInfo,
    dirty_map: &BTreeMap<String, DirtyReason>,
    bump_map: &BTreeMap<String, BumpType>,
) -> Result<BTreeMap<String, PublishCandidate>> {
    // Fetch fresh tags to get current versions (tags may have been created in previous waves)
    let all_tags = git::list_all_tags_vec(&workspace.root);
    let ws_path = workspace.path();

    workspace
        .packages
        .par_iter()
        .filter_map(|(url, pkg)| dirty_map.get(url).map(|reason| (url, pkg, reason)))
        .filter(|(_, pkg, _)| pkg.config.board.is_none())
        .map(|(url, pkg, reason)| {
            let bump = bump_map.get(url).copied().unwrap_or(BumpType::Minor);

            // Get current version from git tags (fresh, not cached in pkg.version)
            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
            let current_version = tags::find_latest_version(&all_tags, &tag_prefix);

            let next_version = compute_next_version(current_version.as_ref(), bump);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            // Reuse hashes from DirtyReason::Modified if available, otherwise compute
            let (content_hash, manifest_hash) = match reason {
                DirtyReason::Modified {
                    content_hash,
                    manifest_hash,
                } => (content_hash.clone(), manifest_hash.clone()),
                _ => {
                    // Unpublished, Uncommitted, or LegacyTag - need to compute hashes
                    let content_hash = canonical::compute_content_hash_from_dir(&pkg.dir)?;
                    let manifest_content = std::fs::read_to_string(pkg.dir.join("pcb.toml"))?;
                    let manifest_hash = canonical::compute_manifest_hash(&manifest_content);
                    (content_hash, manifest_hash)
                }
            };

            Ok((
                url.clone(),
                PublishCandidate {
                    pkg: pkg.clone(),
                    current_version,
                    next_version,
                    tag_name,
                    content_hash,
                    manifest_hash,
                },
            ))
        })
        .collect()
}

fn compute_next_version(current: Option<&Version>, bump: BumpType) -> Version {
    match current {
        None => Version::new(0, 1, 0), // Always 0.1.0 for unpublished
        Some(current) => match bump {
            BumpType::Patch => Version::new(current.major, current.minor, current.patch + 1),
            BumpType::Minor => Version::new(current.major, current.minor + 1, 0),
            BumpType::Major => {
                if current.major == 0 {
                    // For 0.x, "major" means bump the minor (semver convention)
                    Version::new(0, current.minor + 1, 0)
                } else {
                    Version::new(current.major + 1, 0, 0)
                }
            }
        },
    }
}

/// Prompt user to choose a bump strategy (same for all vs individual)
/// Returns the strategy to use across all waves
fn prompt_bump_strategy_choice(packages: &[(&String, &MemberPackage)]) -> Result<BumpStrategy> {
    // Filter to only published packages (unpublished always get 0.1.0)
    let published: Vec<_> = packages
        .iter()
        .filter(|(_, pkg)| pkg.version.is_some())
        .collect();

    if published.is_empty() {
        // All packages are unpublished, no prompt needed - use minor as default
        return Ok(BumpStrategy::SameForAll(BumpType::Minor));
    }

    // Show package list
    println!();
    println!(
        "{} package(s) with unpublished changes:",
        published.len().to_string().cyan()
    );
    for (url, pkg) in &published {
        let version = pkg.version.as_deref().unwrap_or("unpublished");
        let short_name = url.split('/').next_back().unwrap_or(url);
        println!("  {} ({})", short_name, version);
    }
    println!();

    // Skip "same for all" prompt if only one published package - go straight to individual
    if published.len() == 1 {
        return Ok(BumpStrategy::ChooseIndividually);
    }

    // Ask if same bump for all or individual
    let strategy_options = vec!["Same bump for all", "Choose individually"];
    let strategy = Select::new(
        "How do you want to version these packages?",
        strategy_options,
    )
    .prompt()
    .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    if strategy == "Same bump for all" {
        let bump = prompt_bump_type_selection()?;
        Ok(BumpStrategy::SameForAll(bump))
    } else {
        Ok(BumpStrategy::ChooseIndividually)
    }
}

/// Prompt for a single bump type selection
fn prompt_bump_type_selection() -> Result<BumpType> {
    let options = [
        ("Patch (x.y.Z) - bug fixes", BumpType::Patch),
        ("Minor (x.Y.0) - new features", BumpType::Minor),
        ("Major (X.0.0) - breaking changes", BumpType::Major),
    ];

    let labels: Vec<&str> = options.iter().map(|(label, _)| *label).collect();
    let selected = Select::new("Select version bump:", labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(label, _)| *label == selected)
        .map(|(_, bump)| *bump)
        .unwrap_or(BumpType::Minor))
}

/// Prompt for version bump for a single package
fn prompt_single_bump(url: &str, current_version: Option<&Version>) -> Result<BumpType> {
    let short_name = url.split('/').next_back().unwrap_or(url);
    let current_ver = current_version
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unpublished".to_string());
    let is_pre_1_0 = current_version.is_none_or(|v| v.major == 0);

    // Build options with computed next versions
    let options: Vec<_> = if is_pre_1_0 {
        // For 0.x, major bump is same as minor (both increment minor)
        [BumpType::Patch, BumpType::Minor]
            .into_iter()
            .map(|bump| {
                let label = match bump {
                    BumpType::Patch => "Patch",
                    _ => "Minor/Major",
                };
                (
                    format!(
                        "{} → {}",
                        label,
                        compute_next_version(current_version, bump)
                    ),
                    bump,
                )
            })
            .collect()
    } else {
        [BumpType::Patch, BumpType::Minor, BumpType::Major]
            .into_iter()
            .map(|bump| {
                let label = match bump {
                    BumpType::Patch => "Patch",
                    BumpType::Minor => "Minor",
                    BumpType::Major => "Major",
                };
                (
                    format!(
                        "{} → {}",
                        label,
                        compute_next_version(current_version, bump)
                    ),
                    bump,
                )
            })
            .collect()
    };

    let labels: Vec<&str> = options.iter().map(|(label, _)| label.as_str()).collect();
    let prompt = format!("{} ({})", short_name, current_ver);
    let selected = Select::new(&prompt, labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(label, _)| label == selected)
        .map(|(_, bump)| *bump)
        .unwrap_or(BumpType::Minor))
}

fn compute_tag_name(pkg: &MemberPackage, version: &Version, workspace: &WorkspaceInfo) -> String {
    let rel_path = pkg.dir.strip_prefix(&workspace.root).ok();
    let prefix = tags::compute_tag_prefix(rel_path, workspace.path());
    tags::build_tag_name(&prefix, version)
}

fn format_tag_message(url: &str, c: &PublishCandidate) -> String {
    format!(
        "{} v{} {}\n{} v{}/pcb.toml {}",
        url, c.next_version, c.content_hash, url, c.next_version, c.manifest_hash
    )
}

fn format_dependency_bump_commit(
    dependants: &[&MemberPackage],
    candidates: &BTreeMap<String, PublishCandidate>,
) -> String {
    let mut pkg_names: Vec<_> = dependants
        .iter()
        .filter_map(|p| p.dir.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    pkg_names.sort();

    let title = format!("Bump dependency versions: {}", pkg_names.join(", "));

    // Collect only deps that were actually updated in these dependants
    let mut actual_updates: Vec<(&str, Option<&str>, &Version)> = candidates
        .iter()
        .filter(|(url, _)| {
            dependants
                .iter()
                .any(|pkg| pkg.dependencies().any(|d| d == url.as_str()))
        })
        .map(|(url, c)| (url.as_str(), c.pkg.version.as_deref(), &c.next_version))
        .collect();
    actual_updates.sort_by_key(|(url, _, _)| *url);

    // Body: list each dependency that was bumped
    let mut body = String::from("\n");
    for (dep_url, old_version, new_version) in actual_updates {
        // Extract readable path from URL (e.g., "github.com/org/repo/modules/basic" -> "modules/basic")
        let dep_path = dep_url
            .split('/')
            .skip(3) // skip github.com/org/repo
            .collect::<Vec<_>>()
            .join("/");
        let display = if dep_path.is_empty() {
            dep_url
        } else {
            &dep_path
        };

        match old_version {
            Some(old) => body.push_str(&format!("{}: {} → {}\n", display, old, new_version)),
            None => body.push_str(&format!("{} → {}\n", display, new_version)),
        }
    }

    format!("{}\n{}", title, body)
}

/// Returns true if any changes were made
fn bump_dependency_versions(
    pcb_toml_path: &Path,
    candidates: &BTreeMap<String, PublishCandidate>,
) -> Result<bool> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut changed = false;

    for (dep_url, c) in candidates {
        if let Some(existing) = config.dependencies.get(dep_url) {
            let new_spec = DependencySpec::Version(c.next_version.to_string());
            if *existing != new_spec {
                config.dependencies.insert(dep_url.clone(), new_spec);
                changed = true;
            }
        }
    }

    if changed {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }
    Ok(changed)
}
