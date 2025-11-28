//! V2 package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes.

use anyhow::{bail, Result};
use clap::Args;
use colored::Colorize;
use inquire::Confirm;
use pcb_zen::workspace::{compute_tag_prefix, get_workspace_info, MemberPackage, WorkspaceInfo};
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
    url: &'a str,
    pkg: &'a MemberPackage,
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

    if !workspace.config.is_v2() {
        bail!("Not a V2 workspace. Publish requires [workspace] with resolver = \"2\"");
    }

    let remote = if args.force {
        let current_branch = git::symbolic_ref_short_head(&workspace.root)
            .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
        git::get_branch_remote(&workspace.root, &current_branch).ok_or_else(|| {
            anyhow::anyhow!("Branch '{}' is not tracking a remote", current_branch)
        })?
    } else {
        preflight_checks(&workspace.root)?
    };

    // Get publishable packages (no boards) as (url, pkg) tuples
    let all_packages: Vec<(&str, &MemberPackage)> = workspace
        .packages
        .iter()
        .filter(|(_, p)| p.config.board.is_none())
        .map(|(url, pkg)| (url.as_str(), pkg))
        .collect();

    let pkg_by_url: HashMap<&str, &MemberPackage> =
        all_packages.iter().map(|(url, pkg)| (*url, *pkg)).collect();

    let mut remaining_dirty: HashSet<String> = all_packages
        .iter()
        .filter(|(_, p)| p.dirty)
        .map(|(url, _)| url.to_string())
        .collect();

    if remaining_dirty.is_empty() {
        println!("{}", "All packages are up to date".green());
        return Ok(());
    }

    println!(
        "Found {} dirty/unpublished package(s)",
        all_packages.iter().filter(|(_, p)| p.dirty).count()
    );

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    let mut all_tags: Vec<String> = Vec::new();
    let mut made_commits = false;
    let mut wave = 0;

    loop {
        wave += 1;

        // Find publishable: packages whose deps are all not in remaining_dirty
        // Collect URLs first to avoid borrow issues
        let publishable_urls: Vec<String> = remaining_dirty
            .iter()
            .filter(|url| {
                pkg_by_url
                    .get(url.as_str())
                    .map(|pkg| !pkg.dependencies().any(|d| remaining_dirty.contains(d)))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        let publishable: Vec<(&str, &MemberPackage)> = publishable_urls
            .iter()
            .filter_map(|url| pkg_by_url.get(url.as_str()).map(|pkg| (url.as_str(), *pkg)))
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

        // Collect published URLs and versions (owned strings to avoid borrow issues)
        let published_urls: Vec<String> = candidates.iter().map(|c| c.url.to_string()).collect();
        let published_versions: HashMap<&str, (Option<&str>, &Version)> = candidates
            .iter()
            .map(|c| (c.url, (c.pkg.version.as_deref(), &c.next_version)))
            .collect();

        for url in &published_urls {
            remaining_dirty.remove(url);
        }
        for c in &candidates {
            all_tags.push(c.tag_name.clone());
        }

        // Find all packages that depend on just-published packages
        let dependant_urls: Vec<String> = all_packages
            .iter()
            .filter(|(url, pkg)| {
                !published_versions.contains_key(*url)
                    && pkg
                        .dependencies()
                        .any(|d| published_versions.contains_key(d.as_str()))
            })
            .map(|(url, _)| url.to_string())
            .collect();

        let dependants: Vec<(&str, &MemberPackage)> = dependant_urls
            .iter()
            .filter_map(|url| pkg_by_url.get(url.as_str()).map(|pkg| (url.as_str(), *pkg)))
            .collect();

        if !dependants.is_empty() {
            println!();
            println!("  {} {} pcb.toml file(s):", "→".cyan(), dependants.len());
            for (_, pkg) in &dependants {
                println!("    {}/pcb.toml", pkg.rel_path.display());
            }
        }

        if args.dry_run {
            if args.no_cascade {
                break;
            }
            for url in dependant_urls {
                remaining_dirty.insert(url);
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
            let mut changed_pkgs: Vec<&MemberPackage> = Vec::new();
            for (_, pkg) in &dependants {
                if bump_dependency_versions(&pkg.dir.join("pcb.toml"), &published_versions)? {
                    changed_pkgs.push(pkg);
                }
            }
            for url in dependant_urls {
                remaining_dirty.insert(url);
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
    pkg_by_url: &HashMap<&str, &MemberPackage>,
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
                .dependencies()
                .filter(|d| remaining.contains(*d))
                .collect();
            if !blocking.is_empty() {
                let rel = pkg.dir.strip_prefix(root).unwrap_or(&pkg.dir);
                println!("  {} blocked by:", rel.display());
                for dep in blocking {
                    println!("    - {}", dep);
                }
            }
        }
    }
}

fn print_candidate(c: &PublishCandidate, root: &Path) {
    let rel = c.pkg.dir.strip_prefix(root).unwrap_or(&c.pkg.dir);
    let path_display = if rel.as_os_str().is_empty() {
        "(root)".to_string()
    } else {
        rel.display().to_string()
    };
    let version_str = match &c.pkg.version {
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
    packages: &[(&'a str, &'a MemberPackage)],
    workspace: &WorkspaceInfo,
) -> Result<Vec<PublishCandidate<'a>>> {
    packages
        .par_iter()
        .map(|(url, pkg)| {
            let next_version = compute_next_version(pkg);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            // Always recompute hashes - pcb.toml may have been modified by a previous wave
            let content_hash = canonical::compute_content_hash_from_dir(&pkg.dir)?;
            let manifest_content = std::fs::read_to_string(pkg.dir.join("pcb.toml"))?;
            let manifest_hash = canonical::compute_manifest_hash(&manifest_content);

            Ok(PublishCandidate {
                url,
                pkg,
                next_version,
                tag_name,
                content_hash,
                manifest_hash,
            })
        })
        .collect()
}

fn compute_next_version(pkg: &MemberPackage) -> Version {
    match &pkg.version {
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

fn compute_tag_name(pkg: &MemberPackage, version: &Version, workspace: &WorkspaceInfo) -> String {
    let rel_path = pkg.dir.strip_prefix(&workspace.root).ok();
    let ws_path = workspace.path().map(|s| s.to_string());
    let prefix = compute_tag_prefix(rel_path, &ws_path);
    format!("{}{}", prefix, version)
}

fn format_tag_message(candidate: &PublishCandidate) -> String {
    format!(
        "{} v{} {}\n{} v{}/pcb.toml {}",
        candidate.url,
        candidate.next_version,
        candidate.content_hash,
        candidate.url,
        candidate.next_version,
        candidate.manifest_hash
    )
}

fn format_dependency_bump_commit(
    dependants: &[&MemberPackage],
    updates: &HashMap<&str, (Option<&str>, &Version)>,
) -> String {
    let mut pkg_names: Vec<_> = dependants
        .iter()
        .filter_map(|p| p.dir.file_name())
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
                .any(|pkg| pkg.dependencies().any(|d| d == **url))
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
