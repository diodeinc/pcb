//! Package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes. Uses topological sorting to publish packages
//! in dependency order (dependencies before dependants).

use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use colored::Colorize;
use inquire::{Confirm, Select};
use pcb_zen::workspace::{get_workspace_info, MemberPackage, WorkspaceInfo, WorkspaceInfoExt};
use pcb_zen::{canonical, git, tags};
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::path::Path;

use crate::file_walker::{collect_zen_files, resolve_board_target};
use crate::release;

/// Version bump type for publishing
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum BumpType {
    /// Bug fixes only (x.y.Z)
    Patch,
    /// New features, backwards compatible (x.Y.0)
    Minor,
    /// Breaking changes (X.0.0)
    Major,
    /// Prompt interactively (used when --bump is passed without a value)
    #[value(hide = true)]
    Interactive,
}

#[derive(Args, Debug)]
#[command(about = "Publish packages or board releases")]
pub struct PublishArgs {
    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f', hide = true)]
    pub force: bool,

    /// Skip building the workspace before publishing
    #[arg(long, hide = true)]
    pub no_build: bool,

    /// Create commits and tags locally but don't push to remote
    #[arg(long, hide = true)]
    pub no_push: bool,

    /// Suppress diagnostics by kind or severity
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Version bump type. Use --bump for interactive prompt, --bump=patch/minor/major for non-interactive.
    #[arg(long, value_enum, num_args(0..=1), require_equals(true), default_missing_value("interactive"))]
    pub bump: Option<BumpType>,

    /// Exclude specific manufacturing artifacts from the release (can be specified multiple times)
    #[arg(long, value_enum)]
    pub exclude: Vec<release::ArtifactType>,

    /// Path to publish from (defaults to current directory).
    /// If a .zen file is provided, publishes a board release.
    /// Otherwise, publishes dirty packages in the workspace.
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

/// Result of publishing a wave
struct WaveResult {
    tags: Vec<String>,
    commit: Option<String>,
    candidates: BTreeMap<String, PublishCandidate>,
}

/// Expand the dirty set to include packages that depend on dirty packages.
/// If A is dirty and B depends on A, then B must also be published (its pcb.toml
/// will be bumped to the new version of A).
fn expand_dirty_set(
    workspace: &WorkspaceInfo,
    directly_dirty: &HashSet<String>,
) -> HashSet<String> {
    let mut dirty = directly_dirty.clone();
    let mut changed = true;

    // Build reverse dependency map: url -> packages that depend on it
    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    for (url, pkg) in &workspace.packages {
        // Skip boards
        if pkg.config.board.is_some() {
            continue;
        }
        for dep_url in pkg.dependencies() {
            reverse_deps
                .entry(dep_url.to_string())
                .or_default()
                .push(url.clone());
        }
    }

    // Fixed-point iteration: keep adding dependants until no changes
    while changed {
        changed = false;
        let current_dirty: Vec<_> = dirty.iter().cloned().collect();
        for url in current_dirty {
            if let Some(dependants) = reverse_deps.get(&url) {
                for dependant in dependants {
                    if dirty.insert(dependant.clone()) {
                        changed = true;
                    }
                }
            }
        }
    }

    dirty
}

/// Compute waves of packages to publish using Kahn's algorithm.
/// Returns waves in dependency order (dependencies first, dependants last).
fn compute_publish_waves(
    workspace: &WorkspaceInfo,
    dirty_urls: &HashSet<String>,
) -> Vec<Vec<String>> {
    // Build dependency map: url -> vec of dependency urls (only dirty ones)
    let deps: HashMap<String, Vec<String>> = dirty_urls
        .iter()
        .map(|url| {
            let dep_urls = workspace
                .packages
                .get(url)
                .map(|pkg| {
                    pkg.dependencies()
                        .filter(|d| dirty_urls.contains(*d))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            (url.clone(), dep_urls)
        })
        .collect();

    compute_waves_from_deps(&deps)
}

/// Core algorithm: compute publish waves from a dependency map.
/// Each entry maps a package URL to its dependency URLs.
/// Uses Kahn's algorithm for topological sorting.
fn compute_waves_from_deps(deps: &HashMap<String, Vec<String>>) -> Vec<Vec<String>> {
    if deps.is_empty() {
        return Vec::new();
    }

    // Build graph: edge A → B means "A depends on B" (B must be published before A)
    let mut graph = DiGraph::<String, ()>::new();
    let mut url_to_node: HashMap<String, NodeIndex> = HashMap::new();

    for url in deps.keys() {
        let node = graph.add_node(url.clone());
        url_to_node.insert(url.clone(), node);
    }

    for (url, dep_urls) in deps {
        let from_node = url_to_node[url];
        for dep_url in dep_urls {
            if let Some(&to_node) = url_to_node.get(dep_url) {
                graph.add_edge(from_node, to_node, ());
            }
        }
    }

    // Kahn's algorithm: repeatedly extract nodes with no remaining dependencies
    let mut waves = Vec::new();
    let mut remaining: HashSet<NodeIndex> = graph.node_indices().collect();

    while !remaining.is_empty() {
        let wave: Vec<NodeIndex> = remaining
            .iter()
            .copied()
            .filter(|&node| {
                graph
                    .neighbors_directed(node, Direction::Outgoing)
                    .all(|dep| !remaining.contains(&dep))
            })
            .collect();

        if wave.is_empty() {
            panic!("Cycle detected in dependency graph");
        }

        waves.push(wave.iter().map(|&node| graph[node].clone()).collect());
        for node in wave {
            remaining.remove(&node);
        }
    }

    waves
}

pub fn execute(args: PublishArgs) -> Result<()> {
    // Determine if we're publishing a board or packages based on path
    let path = args
        .path
        .as_ref()
        .map(|p| Path::new(p).to_path_buf())
        .unwrap_or_else(|| env::current_dir().unwrap());

    // If path ends in .zen, route to board publish
    if path.extension().is_some_and(|ext| ext == "zen") {
        return publish_board(&path, &args);
    }

    // Otherwise, publish packages
    publish_packages(&path, &args)
}

/// Publish a board release from a .zen file.
///
/// Two modes:
/// - Local hash release (no --bump, non-interactive): just build the release archive
/// - Versioned release (--bump provided): preflight checks, fetch tags, build, upload, tag, push
fn publish_board(zen_path: &Path, args: &PublishArgs) -> Result<()> {
    let target = resolve_board_target(zen_path, "publish")?;

    // Local hash release: no --bump, just build the archive
    if args.bump.is_none() {
        let _zip_path = release::build_board_release(
            target.workspace,
            target.zen_path,
            target.board_name,
            args.suppress.clone(),
            None, // version = None means use git hash
            args.exclude.clone(),
            false,
        )?;
        return Ok(());
    }

    let workspace = target.workspace;
    let board_path = target.zen_path;
    let board_name = target.board_name;
    let pkg_rel_path = target.pkg_rel_path;

    // Preflight checks and get remote (unless --no-push)
    let remote = if !args.no_push {
        let r = if args.force {
            let branch = git::symbolic_ref_short_head(&workspace.root)
                .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
            git::get_branch_remote(&workspace.root, &branch)
                .ok_or_else(|| anyhow::anyhow!("Branch '{}' is not tracking a remote", branch))?
        } else {
            preflight_checks(&workspace.root)?
        };
        eprintln!("Syncing tags from {}...", r.cyan());
        git::fetch_tags(&workspace.root, &r)?;
        Some(r)
    } else {
        None
    };

    // Compute current version from tags (after fetch)
    let tag_prefix = tags::compute_tag_prefix(Some(&pkg_rel_path), workspace.path());
    let all_tags = git::list_all_tags(&workspace.root).unwrap_or_default();
    let current = tags::find_latest_version(&all_tags, &tag_prefix);

    // Resolve bump type (interactive prompt if --bump was passed without a value)
    let bump = match args.bump.unwrap() {
        BumpType::Interactive => prompt_single_bump(&board_name, current.as_ref())?,
        b => b,
    };

    let next_version = compute_next_version(current.as_ref(), bump);
    let tag_name = tags::build_tag_name(&tag_prefix, &next_version);

    // Build the release archive
    let _zip_path = release::build_board_release(
        workspace.clone(),
        board_path,
        board_name.clone(),
        args.suppress.clone(),
        Some(format!("v{}", next_version)),
        args.exclude.clone(),
        false,
    )?;

    // Upload to API (must succeed before creating tag)
    #[cfg(feature = "api")]
    if remote.is_some() {
        let ws_name = workspace
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid workspace root")?;
        eprintln!("Uploading release to Diode...");
        let result = pcb_diode_api::upload_release(&_zip_path, ws_name)?;
        if let Some(release_id) = result.release_id {
            eprintln!(
                "{} Release uploaded: {}",
                "✓".green(),
                format!(
                    "https://app.diode.computer/{}/{}/releases/{}",
                    ws_name, board_name, release_id
                )
                .cyan()
            );
        } else {
            eprintln!("{} Release uploaded", "✓".green());
        }
    }

    // Create git tag
    git::create_tag(
        &workspace.root,
        &tag_name,
        &format!("Release {} version {}", board_name, next_version),
    )
    .context("Failed to create git tag")?;
    eprintln!("{} Created tag {}", "✓".green(), tag_name.bold());

    // Push tag to remote
    if let Some(ref r) = remote {
        eprintln!("Pushing tag to {}...", r.cyan());
        git::push_tag(&workspace.root, &tag_name, r).context("Failed to push tag")?;
        eprintln!("{} Pushed {}", "✓".green(), tag_name.bold());
    }

    Ok(())
}

/// Publish dirty packages in the workspace
fn publish_packages(start_path: &Path, args: &PublishArgs) -> Result<()> {
    let file_provider = DefaultFileProvider::new();
    let mut workspace = get_workspace_info(&file_provider, start_path)?;

    // Fail on workspace discovery errors (invalid pcb.toml files)
    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    // For package publishing, always require remote
    let remote = if args.force {
        let branch = git::symbolic_ref_short_head(&workspace.root)
            .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
        git::get_branch_remote(&workspace.root, &branch)
            .ok_or_else(|| anyhow::anyhow!("Branch '{}' is not tracking a remote", branch))?
    } else {
        preflight_checks(&workspace.root)?
    };

    eprintln!("Syncing tags from {}...", remote.cyan());
    git::fetch_tags(&workspace.root, &remote)?;

    if !args.no_build {
        build_workspace(&workspace, &args.suppress)?;
    }

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    // Populate dirty status and get dirty non-board packages
    workspace.populate_dirty();
    let directly_dirty: HashSet<String> = workspace
        .packages
        .iter()
        .filter(|(_, p)| p.dirty && p.config.board.is_none())
        .map(|(url, _)| url.clone())
        .collect();

    // Expand to include packages that depend on dirty packages (transitively)
    // These need to be published because their pcb.toml will be bumped
    let dirty_urls = expand_dirty_set(&workspace, &directly_dirty);

    let waves = compute_publish_waves(&workspace, &dirty_urls);

    if waves.is_empty() {
        println!("{}", "No packages to publish".green());
        return Ok(());
    }

    // Collect all bump info and show summary upfront
    let bump_map = collect_all_bumps(&workspace, &waves, &dirty_urls, args.bump)?;

    // Show summary and confirm
    let all_tags_list = git::list_all_tags_vec(&workspace.root);
    print_publish_summary(&workspace, &waves, &bump_map, &all_tags_list);

    // Skip confirmation if --no-push (local testing mode)
    if !args.no_push {
        let num_tags = bump_map.len();
        let prompt = format!("Publish and push {} tag(s) to {}?", num_tags, remote);
        if !Confirm::new(&prompt)
            .with_default(true)
            .prompt()
            .unwrap_or(false)
        {
            println!("{}", "Publish cancelled".yellow());
            return Ok(());
        }
    }

    let mut all_tags: Vec<String> = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    let mut published: BTreeMap<String, PublishCandidate> = BTreeMap::new();

    // Process each wave
    let result: Result<()> = (|| {
        for wave_urls in &waves {
            let wave = publish_wave(&workspace, &bump_map, wave_urls, &published)?;

            all_tags.extend(wave.tags);
            if let Some(sha) = wave.commit {
                commits.push(sha);
            }
            published.extend(wave.candidates);
        }
        Ok(())
    })();

    if let Err(e) = result {
        rollback(
            &workspace.root,
            &all_tags,
            commits.first().map(|_| &initial_commit),
        )?;
        return Err(e);
    }

    if all_tags.is_empty() {
        return Ok(());
    }

    // If --no-push, just print summary and exit
    if args.no_push {
        println!();
        println!("{}", "Created locally (not pushed):".cyan().bold());
        for sha in &commits {
            let title = git::run_output_opt(&workspace.root, &["log", "-1", "--format=%s", sha])
                .unwrap_or_default();
            println!("  {} {} {}", "commit:".dimmed(), &sha[..8], title.dimmed());
        }
        for tag in &all_tags {
            println!("  {} {}", "tag:".dimmed(), tag);
        }
        println!();
        println!(
            "To push: {} && {}",
            "git push".cyan(),
            format!("git push origin {}", all_tags.join(" ")).cyan()
        );
        return Ok(());
    }

    println!();
    println!("Pushing to {}...", remote.cyan());
    if !commits.is_empty() {
        git::push_branch(&workspace.root, "main", &remote)?;
        println!("  Pushed main branch");
    }
    git::push_tags(
        &workspace.root,
        &all_tags.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &remote,
    )?;
    for tag in &all_tags {
        println!("  Pushed {}", tag.green());
    }

    Ok(())
}

/// Publish a single wave of packages.
fn publish_wave(
    workspace: &WorkspaceInfo,
    bump_map: &BTreeMap<String, BumpType>,
    package_urls: &[String],
    published: &BTreeMap<String, PublishCandidate>,
) -> Result<WaveResult> {
    let all_tags = git::list_all_tags_vec(&workspace.root);

    // Bump pcb.toml for packages that depend on previously published packages
    let mut changed_pkgs: Vec<&MemberPackage> = Vec::new();
    for url in package_urls {
        if let Some(pkg) = workspace.packages.get(url) {
            let has_published_dep = pkg.dependencies().any(|d| published.contains_key(d));
            if has_published_dep
                && bump_dependency_versions(&pkg.dir(&workspace.root).join("pcb.toml"), published)?
            {
                log::info!("patching: {}/pcb.toml", pkg.rel_path.display());
                changed_pkgs.push(pkg);
            }
        }
    }

    // Commit pcb.toml changes before creating tags
    let commit_sha = if !changed_pkgs.is_empty() {
        let msg = format_dependency_bump_commit(&changed_pkgs, published);
        git::commit_with_trailers(&workspace.root, &msg)?;
        git::rev_parse(&workspace.root, "HEAD")
    } else {
        None
    };

    // Build candidates with fresh hashes (after pcb.toml modifications)
    let candidates = build_candidates(workspace, bump_map, package_urls, &all_tags)?;

    // Create tags
    let mut created_tags = Vec::new();
    for (url, c) in &candidates {
        git::create_tag(&workspace.root, &c.tag_name, &format_tag_message(url, c))?;
        created_tags.push(c.tag_name.clone());
    }

    Ok(WaveResult {
        tags: created_tags,
        commit: commit_sha,
        candidates,
    })
}

fn build_candidates(
    workspace: &WorkspaceInfo,
    bump_map: &BTreeMap<String, BumpType>,
    package_urls: &[String],
    all_tags: &[String],
) -> Result<BTreeMap<String, PublishCandidate>> {
    let ws_path = workspace.path();
    let url_set: HashSet<&String> = package_urls.iter().collect();

    workspace
        .packages
        .par_iter()
        .filter(|(url, _)| url_set.contains(url))
        .map(|(url, pkg)| {
            let bump = bump_map.get(url).copied().unwrap_or(BumpType::Minor);
            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
            let current = tags::find_latest_version(all_tags, &tag_prefix);
            let next_version = compute_next_version(current.as_ref(), bump);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            let pkg_dir = pkg.dir(&workspace.root);
            let content_hash = canonical::compute_content_hash_from_dir(&pkg_dir)?;
            let manifest_content = std::fs::read_to_string(pkg_dir.join("pcb.toml"))?;
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

fn build_workspace(workspace: &WorkspaceInfo, suppress: &[String]) -> Result<()> {
    println!();
    println!("{}", "Building workspace...".cyan().bold());

    let all_zen_files = collect_zen_files(std::slice::from_ref(&workspace.root))?;
    if all_zen_files.is_empty() {
        return Ok(());
    }

    // Filter to workspace member packages only (consistent with pcb build)
    let zen_files: Vec<_> = if workspace.packages.is_empty() {
        all_zen_files
    } else {
        all_zen_files
            .into_iter()
            .filter(|zen_path| {
                workspace
                    .packages
                    .values()
                    .any(|pkg| zen_path.starts_with(pkg.dir(&workspace.root)))
            })
            .collect()
    };

    if zen_files.is_empty() {
        return Ok(());
    }

    let mut ws = workspace.clone();
    let resolution = pcb_zen::resolve_dependencies(&mut ws, false, false)?;
    pcb_zen::vendor_deps(&resolution, &[], None, true)?;

    let mut has_errors = false;
    let mut has_warnings = false;

    for zen_path in &zen_files {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        if let Some(schematic) = crate::build::build(
            zen_path,
            crate::build::create_diagnostics_passes(suppress, &[]),
            false,
            &mut has_errors,
            &mut has_warnings,
            resolution.clone(),
        ) {
            crate::build::print_build_success(&file_name, &schematic);
        }
    }

    if has_errors {
        bail!("Build failed. Fix errors before publishing.");
    }
    Ok(())
}

fn preflight_checks(repo_root: &Path) -> Result<String> {
    if git::has_uncommitted_changes(repo_root)? {
        bail!("Working directory has uncommitted changes.\nCommit or stash your changes before publishing.");
    }

    let branch = git::symbolic_ref_short_head(repo_root).ok_or_else(|| {
        anyhow::anyhow!("Not on a branch (detached HEAD state). Switch to main before publishing.")
    })?;

    if branch != "main" {
        bail!("Must be on 'main' branch to publish.");
    }

    let remote = git::get_branch_remote(repo_root, "main")
        .ok_or_else(|| anyhow::anyhow!("Branch 'main' is not tracking a remote.\nSet upstream with: git branch --set-upstream-to=<remote>/main"))?;

    let sha = git::rev_parse(repo_root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get HEAD commit"))?;

    println!("{} on main @ {}", "✓".green(), &sha[..8]);
    Ok(remote)
}

fn rollback(repo_root: &Path, tags: &[String], reset_to: Option<&String>) -> Result<()> {
    println!("Rolling back...");
    let _ = git::delete_tags(
        repo_root,
        &tags.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    );
    println!("  Deleted {} local tag(s)", tags.len());
    if let Some(commit) = reset_to {
        git::reset_hard(repo_root, commit)?;
        println!("  Reset to initial commit");
    }
    println!("{}", "Publish cancelled".yellow());
    Ok(())
}

fn compute_next_version(current: Option<&Version>, bump: BumpType) -> Version {
    match current {
        None => Version::new(0, 1, 0),
        Some(v) => match bump {
            BumpType::Patch => Version::new(v.major, v.minor, v.patch + 1),
            BumpType::Minor => Version::new(v.major, v.minor + 1, 0),
            BumpType::Major if v.major == 0 => Version::new(0, v.minor + 1, 0),
            BumpType::Major => Version::new(v.major + 1, 0, 0),
            BumpType::Interactive => {
                unreachable!("Interactive should be resolved before calling compute_next_version")
            }
        },
    }
}

fn compute_tag_name(pkg: &MemberPackage, version: &Version, workspace: &WorkspaceInfo) -> String {
    let prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), workspace.path());
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
        .filter_map(|p| p.rel_path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    pkg_names.sort();

    let title = format!("Bump dependency versions: {}", pkg_names.join(", "));

    let mut updates: Vec<_> = candidates
        .iter()
        .filter(|(url, _)| {
            dependants
                .iter()
                .any(|pkg| pkg.dependencies().any(|d| d == url.as_str()))
        })
        .map(|(url, c)| {
            let path = url.split('/').skip(3).collect::<Vec<_>>().join("/");
            let display = if path.is_empty() { url.as_str() } else { &path };
            match c.pkg.version.as_deref() {
                Some(old) => format!("{}: {} → {}", display, old, c.next_version),
                None => format!("{} → {}", display, c.next_version),
            }
        })
        .collect();
    updates.sort();

    format!("{}\n\n{}", title, updates.join("\n"))
}

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

/// Print dependency tree for packages to publish.
fn print_dependency_tree(
    workspace: &WorkspaceInfo,
    dirty_urls: &HashSet<String>,
    all_tags: &[String],
) {
    let ws_path = workspace.path();

    // Build reverse deps: url -> packages that depend on it (within dirty set)
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut has_parent: HashSet<String> = HashSet::new();

    for url in dirty_urls {
        if let Some(pkg) = workspace.packages.get(url) {
            for dep_url in pkg.dependencies() {
                if dirty_urls.contains(dep_url) {
                    children
                        .entry(dep_url.to_string())
                        .or_default()
                        .push(url.clone());
                    has_parent.insert(url.clone());
                }
            }
        }
    }

    for deps in children.values_mut() {
        deps.sort();
    }

    let mut roots: Vec<_> = dirty_urls
        .iter()
        .filter(|url| !has_parent.contains(*url))
        .cloned()
        .collect();
    roots.sort();

    let workspace_url = dirty_urls
        .iter()
        .next()
        .and_then(|url| {
            workspace.packages.get(url).and_then(|pkg| {
                let rel = pkg.rel_path.to_string_lossy();
                url.strip_suffix(&*rel)
                    .map(|s| s.trim_end_matches('/').to_string())
            })
        })
        .unwrap_or_else(|| "workspace".to_string());

    println!();
    println!("{}", "Packages to publish:".cyan().bold());

    let _ = pcb_zen::tree::print_tree(workspace_url, roots, |url| {
        let label = if let Some(pkg) = workspace.packages.get(url) {
            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
            let current = tags::find_latest_version(all_tags, &tag_prefix);
            let ver = current
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "new".into());
            format!("{} {}", pkg.rel_path.display(), ver)
        } else {
            url.clone()
        };
        (label, children.get(url).cloned().unwrap_or_default())
    });
}

/// Collect all bump types upfront, displaying packages and prompting for choices.
fn collect_all_bumps(
    workspace: &WorkspaceInfo,
    waves: &[Vec<String>],
    dirty_urls: &HashSet<String>,
    cli_bump: Option<BumpType>,
) -> Result<BTreeMap<String, BumpType>> {
    let all_tags = git::list_all_tags_vec(&workspace.root);
    let ws_path = workspace.path();

    // Print packages to publish
    print_dependency_tree(workspace, dirty_urls, &all_tags);
    println!();

    // If CLI bump provided (and not interactive), use it for all
    if let Some(bump) = cli_bump {
        if bump != BumpType::Interactive {
            return Ok(waves
                .iter()
                .flat_map(|w| w.iter())
                .map(|url| (url.clone(), bump))
                .collect());
        }
    }

    // Count published packages (unpublished always get 0.1.0)
    let published_count = waves
        .iter()
        .flat_map(|w| w.iter())
        .filter(|url| {
            workspace
                .packages
                .get(*url)
                .and_then(|p| p.version.as_ref())
                .is_some()
        })
        .count();

    // All new packages, use Minor
    if published_count == 0 {
        return Ok(waves
            .iter()
            .flat_map(|w| w.iter())
            .map(|url| (url.clone(), BumpType::Minor))
            .collect());
    }

    // Prompt for strategy
    let total_packages: usize = waves.iter().map(|w| w.len()).sum();
    let choose_individually = if total_packages == 1 {
        true
    } else {
        let options = vec!["Same bump for all", "Choose individually"];
        let choice = Select::new("How do you want to version these packages?", options)
            .prompt()
            .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;
        choice == "Choose individually"
    };

    if choose_individually {
        // Prompt for each package (all at once)
        let mut map = BTreeMap::new();
        for wave_urls in waves {
            for url in wave_urls {
                if let Some(pkg) = workspace.packages.get(url) {
                    let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
                    let current = tags::find_latest_version(&all_tags, &tag_prefix);

                    // Skip prompt for unpublished packages - they always get 0.1.0
                    let bump = if current.is_none() {
                        BumpType::Minor
                    } else {
                        let display_name = pkg.rel_path.display().to_string();
                        prompt_single_bump(&display_name, current.as_ref())?
                    };

                    map.insert(url.clone(), bump);
                }
            }
        }
        Ok(map)
    } else {
        let bump = prompt_bump_type()?;
        Ok(waves
            .iter()
            .flat_map(|w| w.iter())
            .map(|url| (url.clone(), bump))
            .collect())
    }
}

/// Print summary of what will be published.
fn print_publish_summary(
    workspace: &WorkspaceInfo,
    waves: &[Vec<String>],
    bump_map: &BTreeMap<String, BumpType>,
    all_tags: &[String],
) {
    let ws_path = workspace.path();

    println!();
    println!("{}", "Summary:".cyan().bold());
    for (i, wave_urls) in waves.iter().enumerate() {
        println!("  {}:", format!("Wave {}", i + 1).bold());
        for url in wave_urls {
            if let Some(pkg) = workspace.packages.get(url) {
                let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
                let current = tags::find_latest_version(all_tags, &tag_prefix);
                let bump = bump_map.get(url).copied().unwrap_or(BumpType::Minor);
                let next = compute_next_version(current.as_ref(), bump);
                let display_path = pkg.rel_path.display();
                let version_str = match current {
                    Some(v) => format!("{} → {}", v, next),
                    None => format!("→ {}", next),
                };
                println!("    {} {}", display_path, version_str.green());
            }
        }
    }
}

fn prompt_bump_type() -> Result<BumpType> {
    let options = [
        ("Patch (x.y.Z) - bug fixes", BumpType::Patch),
        ("Minor (x.Y.0) - new features", BumpType::Minor),
        ("Major (X.0.0) - breaking changes", BumpType::Major),
    ];

    let labels: Vec<_> = options.iter().map(|(l, _)| *l).collect();
    let selected = Select::new("Select version bump:", labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(l, _)| *l == selected)
        .map(|(_, b)| *b)
        .unwrap_or(BumpType::Minor))
}

fn prompt_single_bump(name: &str, current: Option<&Version>) -> Result<BumpType> {
    let ver = current
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unpublished".to_string());
    let is_pre_1_0 = current.is_none_or(|v| v.major == 0);

    let options: Vec<_> = if is_pre_1_0 {
        [BumpType::Patch, BumpType::Minor]
            .into_iter()
            .map(|b| {
                let label = if b == BumpType::Patch {
                    "Patch"
                } else {
                    "Minor/Major"
                };
                (
                    format!("{} → {}", label, compute_next_version(current, b)),
                    b,
                )
            })
            .collect()
    } else {
        [BumpType::Patch, BumpType::Minor, BumpType::Major]
            .into_iter()
            .map(|b| {
                let label = match b {
                    BumpType::Patch => "Patch",
                    BumpType::Minor => "Minor",
                    BumpType::Major => "Major",
                    BumpType::Interactive => unreachable!(),
                };
                (
                    format!("{} → {}", label, compute_next_version(current, b)),
                    b,
                )
            })
            .collect()
    };

    let labels: Vec<_> = options.iter().map(|(l, _)| l.as_str()).collect();
    let selected = Select::new(&format!("{} ({})", name, ver), labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(l, _)| l == selected)
        .map(|(_, b)| *b)
        .unwrap_or(BumpType::Minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a dependency map from a list of (package, [dependencies])
    fn deps(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(pkg, deps)| {
                (
                    pkg.to_string(),
                    deps.iter().map(|d| d.to_string()).collect(),
                )
            })
            .collect()
    }

    /// Helper to normalize waves for comparison (sort within each wave)
    fn normalize(waves: &[Vec<String>]) -> Vec<Vec<String>> {
        waves
            .iter()
            .map(|wave| {
                let mut sorted = wave.clone();
                sorted.sort();
                sorted
            })
            .collect()
    }

    #[test]
    fn test_empty_deps() {
        let result = compute_waves_from_deps(&HashMap::new());
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_package_no_deps() {
        let result = compute_waves_from_deps(&deps(&[("a", &[])]));
        assert_eq!(normalize(&result), vec![vec!["a"]]);
    }

    #[test]
    fn test_multiple_independent_packages() {
        let result = compute_waves_from_deps(&deps(&[("a", &[]), ("b", &[]), ("c", &[])]));
        // All should be in the same wave since they're independent
        assert_eq!(result.len(), 1);
        assert_eq!(normalize(&result), vec![vec!["a", "b", "c"]]);
    }

    #[test]
    fn test_linear_chain() {
        // c depends on b, b depends on a
        // Should publish: a, then b, then c
        let result = compute_waves_from_deps(&deps(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]));
        assert_eq!(normalize(&result), vec![vec!["a"], vec!["b"], vec!["c"]]);
    }

    #[test]
    fn test_diamond_dependency() {
        // d depends on b and c, both b and c depend on a
        // Should publish: a, then b+c together, then d
        let result = compute_waves_from_deps(&deps(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &["a"]),
            ("d", &["b", "c"]),
        ]));
        assert_eq!(result.len(), 3);
        assert_eq!(normalize(&result)[0], vec!["a"]);
        assert_eq!(normalize(&result)[1], vec!["b", "c"]);
        assert_eq!(normalize(&result)[2], vec!["d"]);
    }

    #[test]
    fn test_multiple_roots() {
        // Two independent chains: a->b and c->d
        let result = compute_waves_from_deps(&deps(&[
            ("a", &[]),
            ("b", &["a"]),
            ("c", &[]),
            ("d", &["c"]),
        ]));
        assert_eq!(result.len(), 2);
        assert_eq!(normalize(&result)[0], vec!["a", "c"]);
        assert_eq!(normalize(&result)[1], vec!["b", "d"]);
    }

    #[test]
    fn test_complex_graph() {
        // e depends on c and d
        // c depends on a and b
        // d depends on b
        // a, b are roots
        let result = compute_waves_from_deps(&deps(&[
            ("a", &[]),
            ("b", &[]),
            ("c", &["a", "b"]),
            ("d", &["b"]),
            ("e", &["c", "d"]),
        ]));
        assert_eq!(result.len(), 3);
        assert_eq!(normalize(&result)[0], vec!["a", "b"]);
        assert_eq!(normalize(&result)[1], vec!["c", "d"]);
        assert_eq!(normalize(&result)[2], vec!["e"]);
    }

    #[test]
    fn test_deps_to_non_dirty_packages_ignored() {
        // b depends on a, but only b is in the map (a is clean/already published)
        // b should be in wave 1 since its dependency on a doesn't count
        let result = compute_waves_from_deps(&deps(&[("b", &["a"])]));
        assert_eq!(normalize(&result), vec![vec!["b"]]);
    }

    #[test]
    #[should_panic(expected = "Cycle detected")]
    fn test_cycle_detection() {
        // a depends on b, b depends on a - cycle!
        compute_waves_from_deps(&deps(&[("a", &["b"]), ("b", &["a"])]));
    }

    #[test]
    fn test_version_bump_patch() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), BumpType::Patch),
            Version::new(1, 2, 4)
        );
    }

    #[test]
    fn test_version_bump_minor() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), BumpType::Minor),
            Version::new(1, 3, 0)
        );
    }

    #[test]
    fn test_version_bump_major() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), BumpType::Major),
            Version::new(2, 0, 0)
        );
    }

    #[test]
    fn test_version_bump_major_pre_1_0() {
        // For 0.x, major bump should increment minor (semver convention)
        let v = Version::new(0, 3, 5);
        assert_eq!(
            compute_next_version(Some(&v), BumpType::Major),
            Version::new(0, 4, 0)
        );
    }

    #[test]
    fn test_version_initial() {
        assert_eq!(
            compute_next_version(None, BumpType::Minor),
            Version::new(0, 1, 0)
        );
        assert_eq!(
            compute_next_version(None, BumpType::Patch),
            Version::new(0, 1, 0)
        );
        assert_eq!(
            compute_next_version(None, BumpType::Major),
            Version::new(0, 1, 0)
        );
    }
}
