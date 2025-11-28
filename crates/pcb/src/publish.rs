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
use std::collections::BTreeMap;
use std::env;
use std::path::Path;

#[derive(Args, Debug)]
#[command(about = "Publish packages by creating version tags")]
pub struct PublishArgs {
    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f')]
    pub force: bool,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

/// Info about a package that will be published
struct PublishCandidate {
    pkg: MemberPackage,
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
    let mut workspace = get_workspace_info(&file_provider, &start_path)?;

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

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    let mut all_tags: Vec<String> = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    let mut wave = 0;

    loop {
        wave += 1;

        let candidates = build_publish_candidates(&workspace)?;

        if candidates.is_empty() {
            println!("{}", "No packages to publish".green());
            break;
        }

        // Display wave
        println!();
        println!("{}", format!("Wave {}:", wave).cyan().bold());
        println!("{} package(s) to publish:", candidates.len());
        for (url, c) in &candidates {
            print_candidate(c, &workspace.root);
            all_tags.push(c.tag_name.clone());
            git::create_tag(&workspace.root, &c.tag_name, &format_tag_message(url, c))?;
            workspace.packages.get_mut(url).unwrap().dirty = false;
        }

        // Find all packages that depend on just-published packages and bump their versions
        let changed_pkgs: Vec<&MemberPackage> = workspace
            .packages
            .iter_mut()
            .filter(|(_, pkg)| pkg.dependencies().any(|d| candidates.contains_key(d)))
            .filter_map(|(_, pkg)| {
                println!("  patching: {}/pcb.toml", pkg.rel_path.display());
                match bump_dependency_versions(&pkg.dir.join("pcb.toml"), &candidates).unwrap() {
                    true => {
                        pkg.dirty = true;
                        Some(pkg as &MemberPackage)
                    }
                    false => None,
                }
            })
            .collect();

        // Commit changes
        if !changed_pkgs.is_empty() {
            let commit_msg = format_dependency_bump_commit(&changed_pkgs, &candidates);
            git::commit_with_trailers(&workspace.root, &commit_msg)?;
            if let Some(sha) = git::rev_parse(&workspace.root, "HEAD") {
                commits.push(sha);
            }
        }
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

fn build_publish_candidates(
    workspace: &WorkspaceInfo,
) -> Result<BTreeMap<String, PublishCandidate>> {
    workspace
        .packages
        .par_iter()
        .filter(|(_, p)| p.dirty)
        .filter(|(_, p)| p.config.board.is_none())
        .map(|(url, pkg)| {
            let next_version = compute_next_version(pkg);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            // Always recompute hashes - pcb.toml may have been modified by a previous wave
            let content_hash = canonical::compute_content_hash_from_dir(&pkg.dir)?;
            let manifest_content = std::fs::read_to_string(pkg.dir.join("pcb.toml"))?;
            let manifest_hash = canonical::compute_manifest_hash(&manifest_content);

            Ok((
                url.clone(),
                PublishCandidate {
                    pkg: pkg.clone(),
                    next_version,
                    tag_name,
                    content_hash,
                    manifest_hash,
                },
            ))
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
