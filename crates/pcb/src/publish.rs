//! V2 package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes.

use anyhow::{bail, Result};
use clap::Args;
use colored::Colorize;
use inquire::Confirm;
use pcb_zen::workspace::{compute_tag_prefix, detect_v2_workspace, PackageInfo, V2Workspace};
use pcb_zen::{git, resolve_v2};
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
    let start_path = match &args.path {
        Some(path) => Path::new(path).to_path_buf(),
        None => env::current_dir()?,
    };

    // Detect V2 workspace once (computes all content hashes upfront)
    let Some(workspace) = detect_v2_workspace(&start_path)? else {
        bail!("Not a V2 workspace. Publish requires [workspace] with resolver = \"2\"");
    };

    // Safety checks
    let remote = preflight_checks(&workspace.root)?;

    // Get all non-board packages
    let all_packages: Vec<&PackageInfo> = workspace
        .all_packages()
        .into_iter()
        .filter(|p| p.board.is_none())
        .collect();

    if all_packages.is_empty() {
        println!("No packages found in workspace");
        return Ok(());
    }

    // Build URL -> PackageInfo map for dependency lookups
    let pkg_by_url: HashMap<&str, &PackageInfo> =
        all_packages.iter().map(|p| (p.url.as_str(), *p)).collect();

    // Track remaining dirty packages by URL
    let mut remaining_dirty: HashSet<&str> = all_packages
        .iter()
        .filter(|p| p.dirty)
        .map(|p| p.url.as_str())
        .collect();

    let initial_dirty_count = remaining_dirty.len();

    if remaining_dirty.is_empty() {
        println!("{}", "All packages are up to date".green());
        return Ok(());
    }

    println!("Found {} dirty/unpublished package(s)", initial_dirty_count);

    // Accumulate all tags across waves
    let mut all_tags: Vec<String> = Vec::new();
    let mut wave = 0;

    loop {
        wave += 1;

        // Find publishable: dirty packages whose deps are all not in remaining_dirty
        let publishable: Vec<&PackageInfo> = remaining_dirty
            .iter()
            .filter_map(|url| pkg_by_url.get(url).copied())
            .filter(|pkg| {
                !pkg.dependencies
                    .iter()
                    .any(|dep_url| remaining_dirty.contains(dep_url.as_str()))
            })
            .collect();

        if publishable.is_empty() {
            // No more packages can be published
            if all_tags.is_empty() {
                // First wave and nothing publishable - show blocking info
                println!();
                println!("{}", "No packages can be published yet.".yellow());
                println!("All dirty packages depend on other dirty/unpublished packages.");
                println!();
                println!("Dirty packages and their blocking dependencies:");
                for url in &remaining_dirty {
                    if let Some(pkg) = pkg_by_url.get(url) {
                        let blocking: Vec<_> = pkg
                            .dependencies
                            .iter()
                            .filter(|dep_url| remaining_dirty.contains(dep_url.as_str()))
                            .map(|s| s.as_str())
                            .collect();
                        if !blocking.is_empty() {
                            let rel_path = pkg
                                .path
                                .strip_prefix(&workspace.root)
                                .unwrap_or(&pkg.path);
                            println!("  {} blocked by:", rel_path.display());
                            for dep in blocking {
                                println!("    - {}", dep);
                            }
                        }
                    }
                }
            }
            break;
        }

        // Build publish candidates with computed versions and hashes
        let candidates = build_publish_candidates(&publishable, &workspace)?;

        // Display wave header if multiple waves
        if wave > 1 || candidates.len() < initial_dirty_count {
            println!();
            println!("{}", format!("Wave {}:", wave).cyan().bold());
        }

        println!("{} package(s) can be published:", publishable.len());

        // Display what will be published
        for candidate in &candidates {
            let rel_path = candidate
                .pkg
                .path
                .strip_prefix(&workspace.root)
                .unwrap_or(&candidate.pkg.path);
            let path_display = if rel_path.as_os_str().is_empty() {
                "(root)".to_string()
            } else {
                rel_path.display().to_string()
            };
            let version_str = match &candidate.pkg.latest_version {
                Some(v) => format!("{} → {}", v, candidate.next_version),
                None => format!("{} (initial)", candidate.next_version),
            };
            println!(
                "  {}: {} [{}]",
                path_display,
                version_str.green(),
                candidate.tag_name.cyan()
            );
        }

        // Remove published packages from remaining_dirty and accumulate tags
        for candidate in &candidates {
            remaining_dirty.remove(candidate.pkg.url.as_str());
            all_tags.push(candidate.tag_name.clone());
        }

        if args.dry_run {
            // Continue to show all waves in dry run
            continue;
        }

        // Create tags locally
        for candidate in &candidates {
            let message = format_tag_message(candidate);
            git::create_tag(&workspace.root, &candidate.tag_name, &message)?;
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

    // Confirm push of all accumulated tags
    println!();
    let confirmed = Confirm::new(&format!("Push {} tag(s) to {}?", all_tags.len(), remote))
        .with_default(false)
        .prompt()
        .unwrap_or(false);

    if !confirmed {
        // User declined - delete all tags we created across all waves
        println!("Cleaning up {} local tag(s)...", all_tags.len());
        let tag_refs: Vec<&str> = all_tags.iter().map(|s| s.as_str()).collect();
        let _ = git::delete_tags(&workspace.root, &tag_refs);
        println!("{}", "Publish cancelled".yellow());
        return Ok(());
    }

    // Push all tags in one command
    println!();
    println!("Pushing to {}...", remote.cyan());
    let tag_refs: Vec<&str> = all_tags.iter().map(|s| s.as_str()).collect();
    git::push_tags(&workspace.root, &tag_refs, &remote)?;
    for tag in &all_tags {
        println!("  Pushed {}", tag.green());
    }

    Ok(())
}

/// Run preflight safety checks before publishing
///
/// Returns the remote name that main is tracking
fn preflight_checks(repo_root: &Path) -> Result<String> {
    // 1. Check for uncommitted changes
    if git::has_uncommitted_changes(repo_root)? {
        bail!(
            "Working directory has uncommitted changes.\n\
             Commit or stash your changes before publishing."
        );
    }

    // 2. Check we're on the main branch
    let current_branch = git::symbolic_ref_short_head(repo_root)
        .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state). Switch to main before publishing."))?;

    if current_branch != "main" {
        bail!(
            "Must be on 'main' branch to publish.\n\
             Current branch: '{}'\n\
             Run: git checkout main",
            current_branch
        );
    }

    // 3. Get the remote that main is tracking
    let remote = git::get_branch_remote(repo_root, "main").ok_or_else(|| {
        anyhow::anyhow!(
            "Branch 'main' is not tracking a remote.\n\
             Set upstream with: git branch --set-upstream-to=<remote>/main"
        )
    })?;

    let local_sha = git::rev_parse(repo_root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get HEAD commit"))?;

    println!(
        "{} on main @ {}",
        "✓".green(),
        &local_sha[..8]
    );

    Ok(remote)
}

/// Build publish candidates with computed versions and hashes
fn build_publish_candidates<'a>(
    packages: &[&'a PackageInfo],
    workspace: &V2Workspace,
) -> Result<Vec<PublishCandidate<'a>>> {
    packages
        .iter()
        .map(|pkg| {
            let next_version = compute_next_version(pkg);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            // Reuse hashes from PackageInfo if available, otherwise compute
            let content_hash = pkg
                .content_hash
                .clone()
                .map(Ok)
                .unwrap_or_else(|| resolve_v2::compute_content_hash_from_dir(&pkg.path))?;

            let manifest_hash = pkg.manifest_hash.clone().unwrap_or_else(|| {
                let content = std::fs::read_to_string(pkg.path.join("pcb.toml")).unwrap();
                resolve_v2::compute_manifest_hash(&content)
            });

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

/// Compute next version for a package
///
/// - Unpublished: 0.1.0
/// - Published: major version bump (0.x.y → 0.(x+1).0, N.x.y → (N+1).0.0 for N≥1)
fn compute_next_version(pkg: &PackageInfo) -> Version {
    match &pkg.latest_version {
        None => Version::new(0, 1, 0),
        Some(v) => {
            let current = Version::parse(v).unwrap_or_else(|_| Version::new(0, 0, 0));
            if current.major == 0 {
                // 0.x.y → 0.(x+1).0
                Version::new(0, current.minor + 1, 0)
            } else {
                // N.x.y → (N+1).0.0
                Version::new(current.major + 1, 0, 0)
            }
        }
    }
}

/// Compute tag name for a package
fn compute_tag_name(pkg: &PackageInfo, version: &Version, workspace: &V2Workspace) -> String {
    let rel_path = pkg.path.strip_prefix(&workspace.root).ok();
    let prefix = compute_tag_prefix(rel_path, &workspace.path);
    format!("{}{}", prefix, version)
}

/// Format tag message with hashes (pcb.sum format)
fn format_tag_message(candidate: &PublishCandidate) -> String {
    let module_path = &candidate.pkg.url;
    let version = &candidate.next_version;

    format!(
        "{} v{} {}\n{} v{}/pcb.toml {}",
        module_path,
        version,
        candidate.content_hash,
        module_path,
        version,
        candidate.manifest_hash
    )
}
