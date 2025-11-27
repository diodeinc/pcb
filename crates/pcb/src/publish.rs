//! V2 package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes.

use anyhow::{bail, Result};
use clap::Args;
use colored::Colorize;
use inquire::Confirm;
use pcb_zen::workspace::{compute_tag_prefix, detect_v2_workspace, PackageInfo, V2Workspace};
use pcb_zen::{canonical, git};
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use rayon::prelude::*;
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::Path;

#[derive(Args, Debug)]
#[command(about = "Publish packages by creating version tags")]
pub struct PublishArgs {
    /// Dry run - show what would be published without creating tags
    #[arg(long)]
    pub dry_run: bool,

    /// Only publish the first wave of packages without cascading updates
    #[arg(long)]
    pub no_cascade: bool,

    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f')]
    pub force: bool,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

/// Info about a package that will be published
struct PublishCandidate<'a> {
    pkg: &'a PackageInfo,
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

    let Some(workspace) = detect_v2_workspace(&start_path)? else {
        bail!("Not a V2 workspace. Publish requires [workspace] with resolver = \"2\"");
    };

    let remote = if args.force {
        let current_branch = git::symbolic_ref_short_head(&workspace.root)
            .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
        git::get_branch_remote(&workspace.root, &current_branch).ok_or_else(|| {
            anyhow::anyhow!("Branch '{}' is not tracking a remote", current_branch)
        })?
    } else {
        preflight_checks(&workspace.root)?
    };

    let all_packages: Vec<&PackageInfo> = workspace
        .all_packages()
        .into_iter()
        .filter(|p| p.board.is_none())
        .collect();

    if all_packages.is_empty() {
        println!("No packages found in workspace");
        return Ok(());
    }

    let pkg_by_url: HashMap<&str, &PackageInfo> =
        all_packages.iter().map(|p| (p.url.as_str(), *p)).collect();

    let mut remaining_dirty: HashSet<String> = all_packages
        .iter()
        .filter(|p| p.dirty || p.transitive_dirty)
        .map(|p| p.url.clone())
        .collect();

    if remaining_dirty.is_empty() {
        println!("{}", "All packages are up to date".green());
        return Ok(());
    }

    println!(
        "Found {} dirty/unpublished package(s)",
        all_packages.iter().filter(|p| p.dirty).count()
    );

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    let mut all_tags: Vec<String> = Vec::new();
    let mut made_commits = false;
    let mut wave = 0;

    loop {
        wave += 1;

        // Find publishable: packages whose deps are all not in remaining_dirty
        let publishable: Vec<&PackageInfo> = remaining_dirty
            .iter()
            .filter_map(|url| pkg_by_url.get(url.as_str()).copied())
            .filter(|pkg| !pkg.dependencies.iter().any(|d| remaining_dirty.contains(d)))
            .collect();

        if publishable.is_empty() {
            if all_tags.is_empty() {
                print_blocking_info(&remaining_dirty, &pkg_by_url, &workspace.root);
            }
            break;
        }

        let candidates = build_publish_candidates(&publishable, &workspace)?;

        // Display wave
        if wave > 1 {
            println!();
        }
        println!("{}", format!("Wave {}:", wave).cyan().bold());
        println!("{} package(s) to publish:", candidates.len());
        for c in &candidates {
            print_candidate(c, &workspace.root);
        }

        let published_versions: HashMap<&str, (Option<&str>, &Version)> = candidates
            .iter()
            .map(|c| {
                (
                    c.pkg.url.as_str(),
                    (c.pkg.latest_version.as_deref(), &c.next_version),
                )
            })
            .collect();

        for c in &candidates {
            remaining_dirty.remove(&c.pkg.url);
            all_tags.push(c.tag_name.clone());
        }

        // Find dependants needing pcb.toml updates
        let dependants: Vec<&PackageInfo> = remaining_dirty
            .iter()
            .filter_map(|url| pkg_by_url.get(url.as_str()).copied())
            .filter(|pkg| {
                pkg.dependencies
                    .iter()
                    .any(|d| published_versions.contains_key(d.as_str()))
            })
            .collect();

        if !dependants.is_empty() {
            println!();
            println!("  {} {} pcb.toml file(s):", "→".cyan(), dependants.len());
            for pkg in &dependants {
                let rel = pkg.path.strip_prefix(&workspace.root).unwrap_or(&pkg.path);
                println!("    {}/pcb.toml", rel.display());
            }
        }

        if args.dry_run {
            if args.no_cascade {
                break;
            }
            for pkg in &dependants {
                remaining_dirty.insert(pkg.url.clone());
            }
            continue;
        }

        // Create tags
        for c in &candidates {
            git::create_tag(&workspace.root, &c.tag_name, &format_tag_message(c))?;
        }

        if args.no_cascade {
            break;
        }

        // Update pcb.toml files and commit for cascade mode
        if !dependants.is_empty() {
            let mut changed_pkgs: Vec<&PackageInfo> = Vec::new();
            for pkg in &dependants {
                if bump_dependency_versions(&pkg.path.join("pcb.toml"), &published_versions)? {
                    changed_pkgs.push(pkg);
                }
                remaining_dirty.insert(pkg.url.clone());
            }

            if !changed_pkgs.is_empty() {
                let commit_msg = format_dependency_bump_commit(&changed_pkgs, &published_versions);
                git::commit_with_trailers(&workspace.root, &commit_msg)?;
                made_commits = true;
            }
        }
    }

    if all_tags.is_empty() {
        return Ok(());
    }

    if args.dry_run {
        println!();
        println!(
            "{} ({} total)",
            "Dry run - no tags created".yellow(),
            all_tags.len()
        );
        return Ok(());
    }

    // Confirm and push
    println!();
    let prompt = if made_commits {
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
            made_commits.then_some(&initial_commit),
        );
    }

    println!();
    println!("Pushing to {}...", remote.cyan());
    if made_commits {
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

fn print_blocking_info(
    remaining: &HashSet<String>,
    pkg_by_url: &HashMap<&str, &PackageInfo>,
    root: &Path,
) {
    println!();
    println!("{}", "No packages can be published yet.".yellow());
    println!("All dirty packages depend on other dirty/unpublished packages.");
    println!();
    println!("Dirty packages and their blocking dependencies:");
    for url in remaining {
        if let Some(pkg) = pkg_by_url.get(url.as_str()) {
            let blocking: Vec<_> = pkg
                .dependencies
                .iter()
                .filter(|d| remaining.contains(*d))
                .collect();
            if !blocking.is_empty() {
                let rel = pkg.path.strip_prefix(root).unwrap_or(&pkg.path);
                println!("  {} blocked by:", rel.display());
                for dep in blocking {
                    println!("    - {}", dep);
                }
            }
        }
    }
}

fn print_candidate(c: &PublishCandidate, root: &Path) {
    let rel = c.pkg.path.strip_prefix(root).unwrap_or(&c.pkg.path);
    let path_display = if rel.as_os_str().is_empty() {
        "(root)".to_string()
    } else {
        rel.display().to_string()
    };
    let version_str = match &c.pkg.latest_version {
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

fn build_publish_candidates<'a>(
    packages: &[&'a PackageInfo],
    workspace: &V2Workspace,
) -> Result<Vec<PublishCandidate<'a>>> {
    packages
        .par_iter()
        .map(|pkg| {
            let next_version = compute_next_version(pkg);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            // Always recompute hashes - pcb.toml may have been modified by a previous wave
            let content_hash = canonical::compute_content_hash_from_dir(&pkg.path)?;
            let manifest_content = std::fs::read_to_string(pkg.path.join("pcb.toml"))?;
            let manifest_hash = canonical::compute_manifest_hash(&manifest_content);

            Ok(PublishCandidate {
                pkg,
                next_version,
                tag_name,
                content_hash,
                manifest_hash,
            })
        })
        .collect()
}

fn compute_next_version(pkg: &PackageInfo) -> Version {
    match &pkg.latest_version {
        None => Version::new(0, 1, 0),
        Some(v) => {
            let current = Version::parse(v).unwrap_or_else(|_| Version::new(0, 0, 0));
            if current.major == 0 {
                Version::new(0, current.minor + 1, 0)
            } else {
                Version::new(current.major + 1, 0, 0)
            }
        }
    }
}

fn compute_tag_name(pkg: &PackageInfo, version: &Version, workspace: &V2Workspace) -> String {
    let rel_path = pkg.path.strip_prefix(&workspace.root).ok();
    let prefix = compute_tag_prefix(rel_path, &workspace.path);
    format!("{}{}", prefix, version)
}

fn format_tag_message(candidate: &PublishCandidate) -> String {
    format!(
        "{} v{} {}\n{} v{}/pcb.toml {}",
        candidate.pkg.url,
        candidate.next_version,
        candidate.content_hash,
        candidate.pkg.url,
        candidate.next_version,
        candidate.manifest_hash
    )
}

fn format_dependency_bump_commit(
    dependants: &[&PackageInfo],
    updates: &HashMap<&str, (Option<&str>, &Version)>,
) -> String {
    let mut pkg_names: Vec<_> = dependants
        .iter()
        .filter_map(|p| p.path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    pkg_names.sort();

    let title = format!("Bump dependency versions: {}", pkg_names.join(", "));

    // Collect only deps that were actually updated in these dependants
    let mut actual_updates: Vec<(&str, Option<&str>, &Version)> = updates
        .iter()
        .filter(|(url, _)| {
            dependants
                .iter()
                .any(|pkg| pkg.dependencies.iter().any(|d| d == **url))
        })
        .map(|(url, (old, new))| (*url, *old, *new))
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
    updates: &HashMap<&str, (Option<&str>, &Version)>,
) -> Result<bool> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut changed = false;

    for (dep_url, (_, new_version)) in updates {
        if let Some(existing) = config.dependencies.get(*dep_url) {
            let new_spec = DependencySpec::Version(new_version.to_string());
            if *existing != new_spec {
                config.dependencies.insert(dep_url.to_string(), new_spec);
                changed = true;
            }
        }
    }

    if changed {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }
    Ok(changed)
}
