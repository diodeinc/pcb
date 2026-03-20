//! Package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes. Uses topological sorting to publish packages
//! in dependency order (dependencies before dependants).

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use colored::Colorize;
use inquire::{Confirm, Select};
use pcb_zen::workspace::{MemberPackage, WorkspaceInfo, WorkspaceInfoExt, get_workspace_info};
use pcb_zen::{git, tags};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt;
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
    /// Infer bumps from conventional commits and dependency waves
    Infer,
    /// Prompt interactively (used when --bump is passed without a value)
    #[value(hide = true)]
    Interactive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ReleaseBump {
    Patch,
    Minor,
    Major,
}

impl ReleaseBump {
    const SELECTABLE: [Self; 3] = [Self::Patch, Self::Minor, Self::Major];

    fn breaking_for(current: &Version) -> Self {
        if current.major == 0 {
            Self::Minor
        } else {
            Self::Major
        }
    }

    fn infer_from_commit(subject: &str, current: &Version) -> Self {
        let Some((header, _)) = subject.split_once(':') else {
            return Self::breaking_for(current);
        };

        let header = header.trim();
        if header.is_empty() {
            return Self::breaking_for(current);
        }

        let breaking = header.ends_with('!');
        let header = header.strip_suffix('!').unwrap_or(header);
        let commit_type = &header[..header.find('(').unwrap_or(header.len())];

        if commit_type.is_empty() || !commit_type.chars().all(|c| c.is_ascii_lowercase()) {
            return Self::breaking_for(current);
        }

        if breaking {
            return Self::breaking_for(current);
        }

        if current.major == 0 {
            return Self::Patch;
        }

        match commit_type {
            // In Zener packages, "fix" commonly means a hardware/design correction
            // that can affect downstream designs, so stable fixes still warrant a minor bump.
            "chore" => Self::Patch,
            "feat" | "layout" => Self::Minor,
            "fix" => Self::Minor,
            _ => Self::breaking_for(current),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Patch => "Patch",
            Self::Minor => "Minor",
            Self::Major => "Major",
        }
    }
}

impl BumpType {
    fn release(self) -> Option<ReleaseBump> {
        match self {
            Self::Patch => Some(ReleaseBump::Patch),
            Self::Minor => Some(ReleaseBump::Minor),
            Self::Major => Some(ReleaseBump::Major),
            Self::Infer | Self::Interactive => None,
        }
    }
}

const CONVENTIONAL_COMMIT_SUBJECT_LEN: usize = 72;
const DEPENDENCY_BUMP_TITLE: &str = "chore: bump deps";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BumpStrategy {
    Infer,
    SameForAll,
    ChooseIndividually,
}

impl fmt::Display for BumpStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Infer => "Accept inferred versions",
            Self::SameForAll => "Same bump for all",
            Self::ChooseIndividually => "Choose individually",
        };
        f.write_str(label)
    }
}

#[derive(Args, Debug)]
#[command(about = "Publish packages or board releases")]
pub struct PublishArgs {
    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f', hide = true)]
    pub force: bool,

    /// Assume yes for the final publish confirmation
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Skip building the workspace before publishing
    #[arg(long, hide = true)]
    pub no_build: bool,

    /// Create commits and tags locally but don't push to remote
    #[arg(long, hide = true)]
    pub no_push: bool,

    /// Suppress diagnostics by kind or severity
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    pub suppress: Vec<String>,

    /// Version bump type. Use --bump for interactive prompt, --bump=patch/minor/major/infer for non-interactive.
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

/// Tracks local git state created during publishing.
///
/// Automatically rolls back (deletes tags, resets commits) on drop unless
/// [`disarm`](Self::disarm) is called after a successful publish.
struct PublishGuard {
    repo_root: std::path::PathBuf,
    initial_commit: String,
    tags: Vec<String>,
    has_commits: bool,
    armed: bool,
}

impl PublishGuard {
    fn new(repo_root: &Path, initial_commit: String) -> Self {
        Self {
            repo_root: repo_root.to_path_buf(),
            initial_commit,
            tags: Vec::new(),
            has_commits: false,
            armed: true,
        }
    }

    /// Prevent rollback on drop (call after successful push).
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PublishGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        println!("Rolling back...");
        if !self.tags.is_empty() {
            let _ = git::delete_tags(
                &self.repo_root,
                &self.tags.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            );
            println!("  Deleted {} local tag(s)", self.tags.len());
        }
        if self.has_commits {
            let _ = git::reset_hard(&self.repo_root, &self.initial_commit);
            println!("  Reset to initial commit");
        }
        println!("{}", "Publish cancelled".yellow());
    }
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
) -> Result<Vec<Vec<String>>> {
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
        .map_err(|cycle| anyhow::anyhow!(format_cycle_error(workspace, &cycle)))
}

/// Core algorithm: compute publish waves from a dependency map.
/// Each entry maps a package URL to its dependency URLs.
/// Uses Kahn's algorithm for topological sorting.
fn compute_waves_from_deps(
    deps: &HashMap<String, Vec<String>>,
) -> std::result::Result<Vec<Vec<String>>, Vec<String>> {
    if deps.is_empty() {
        return Ok(Vec::new());
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
            let remaining_urls: HashSet<String> =
                remaining.iter().map(|node| graph[*node].clone()).collect();
            let cycle = find_dependency_cycle(deps, &remaining_urls)
                .unwrap_or_else(|| sorted_urls(&remaining_urls));
            return Err(cycle);
        }

        waves.push(wave.iter().map(|&node| graph[node].clone()).collect());
        for node in wave {
            remaining.remove(&node);
        }
    }

    Ok(waves)
}

fn find_dependency_cycle(
    deps: &HashMap<String, Vec<String>>,
    nodes: &HashSet<String>,
) -> Option<Vec<String>> {
    fn dfs(
        node: &str,
        deps: &HashMap<String, Vec<String>>,
        nodes: &HashSet<String>,
        visiting: &mut HashMap<String, usize>,
        visited: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        visiting.insert(node.to_string(), stack.len());
        stack.push(node.to_string());

        let mut next_nodes: Vec<String> = deps
            .get(node)
            .into_iter()
            .flatten()
            .filter(|dep| nodes.contains(*dep))
            .cloned()
            .collect();
        next_nodes.sort();

        for dep in next_nodes {
            if let Some(&start) = visiting.get(&dep) {
                let mut cycle = stack[start..].to_vec();
                cycle.push(dep);
                return Some(cycle);
            }

            if visited.contains(&dep) {
                continue;
            }

            if let Some(cycle) = dfs(&dep, deps, nodes, visiting, visited, stack) {
                return Some(cycle);
            }
        }

        stack.pop();
        visiting.remove(node);
        visited.insert(node.to_string());
        None
    }

    let mut visiting = HashMap::new();
    let mut visited = HashSet::new();
    let mut stack = Vec::new();

    for node in sorted_urls(nodes) {
        if visited.contains(&node) {
            continue;
        }
        if let Some(cycle) = dfs(&node, deps, nodes, &mut visiting, &mut visited, &mut stack) {
            return Some(cycle);
        }
    }

    None
}

fn sorted_urls(urls: &HashSet<String>) -> Vec<String> {
    let mut sorted: Vec<String> = urls.iter().cloned().collect();
    sorted.sort();
    sorted
}

fn format_cycle_error(workspace: &WorkspaceInfo, cycle: &[String]) -> String {
    let mut message =
        String::from("Circular package dependency detected while computing publish order:");

    for edge in cycle.windows(2) {
        let from = format_cycle_node(workspace, &edge[0]);
        let to = format_cycle_node(workspace, &edge[1]);
        message.push_str(&format!("\n  {from} depends on {to}"));
    }

    message.push_str("\n\nInspect the [dependencies] entries in the listed pcb.toml files.");
    message
}

fn format_cycle_node(workspace: &WorkspaceInfo, url: &str) -> String {
    workspace
        .packages
        .get(url)
        .map(|pkg| {
            if pkg.rel_path.as_os_str().is_empty() {
                ".".to_string()
            } else {
                pkg.rel_path.display().to_string()
            }
        })
        .unwrap_or_else(|| url.to_string())
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

    let remote = if !args.no_push {
        let r = resolve_remote(&workspace.root, args.force)?;
        eprintln!("Syncing with {}...", r.cyan());
        git::fetch_tags(&workspace.root, &r)?;
        if !args.force {
            git::fetch_branch(&workspace.root, &r, "main")?;
            preflight_checks(&workspace.root, &r)?;
        }
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
        BumpType::Infer => {
            bail!("--bump=infer is only supported when publishing packages.");
        }
        b => b
            .release()
            .expect("explicit board bump must be a release bump"),
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
    if !args.force && std::env::var("CI").is_err() {
        bail!(
            "Package publishing is only supported in CI.\nUse --force to publish manually (only if you know what you're doing)."
        );
    }

    let file_provider = DefaultFileProvider::new();
    let mut workspace = get_workspace_info(&file_provider, start_path, true)?;

    // Fail on workspace discovery errors (invalid pcb.toml files)
    if !workspace.errors.is_empty() {
        for err in &workspace.errors {
            eprintln!("{}", err.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let remote = resolve_remote(&workspace.root, args.force)?;

    eprintln!("Syncing with {}...", remote.cyan());
    git::fetch_tags(&workspace.root, &remote)?;
    if !args.force {
        git::fetch_branch(&workspace.root, &remote, "main")?;
        preflight_checks(&workspace.root, &remote)?;
    }

    if !args.no_build {
        build_workspace(&workspace, &args.suppress)?;
        if git::has_uncommitted_changes(&workspace.root)? {
            bail!(
                "Build modified the repository.\n\
                 Resolve or commit the changes before publishing."
            );
        }
    }

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

    let waves = compute_publish_waves(&workspace, &dirty_urls)?;

    if waves.is_empty() {
        println!("{}", "No packages to publish".green());
        return Ok(());
    }

    // Collect all bump info and show summary upfront
    let bump_map = collect_all_bumps(&workspace, &waves, args.bump)?;

    // Show summary and confirm
    let all_tags_list = git::list_all_tags_vec(&workspace.root);
    print_publish_summary("Summary", &workspace, &bump_map, &all_tags_list);

    // Skip confirmation if --no-push (local testing mode) or --yes
    if !args.no_push && !args.yes {
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

    // Guard created after all early returns — only active during mutation phase
    let mut guard = PublishGuard::new(
        &workspace.root,
        git::rev_parse(&workspace.root, "HEAD")
            .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?,
    );

    let mut published: BTreeMap<String, PublishCandidate> = BTreeMap::new();

    // Process each wave — guard auto-rolls back on early return via `?`
    for wave_urls in &waves {
        let wave = publish_wave(&workspace, &bump_map, wave_urls, &published)?;

        guard.tags.extend(wave.tags);
        guard.has_commits |= wave.commit.is_some();
        published.extend(wave.candidates);
    }

    if guard.tags.is_empty() {
        guard.disarm();
        return Ok(());
    }

    // If --no-push, just print summary and exit
    if args.no_push {
        guard.disarm();
        println!();
        println!("{}", "Created locally (not pushed):".cyan().bold());
        if guard.has_commits {
            let title = git::run_output_opt(&workspace.root, &["log", "-1", "--format=%s", "HEAD"])
                .unwrap_or_default();
            let sha = git::rev_parse(&workspace.root, "HEAD").unwrap_or_default();
            println!(
                "  {} {} {}",
                "commit:".dimmed(),
                &sha[..8.min(sha.len())],
                title.dimmed()
            );
        }
        for tag in &guard.tags {
            println!("  {} {}", "tag:".dimmed(), tag);
        }
        println!();
        println!(
            "To push: {} && {}",
            format!("git push {}", remote).cyan(),
            format!("git push {} {}", remote, guard.tags.join(" ")).cyan()
        );
        return Ok(());
    }

    // Push to remote — guard auto-rolls back on failure
    println!();
    println!("Pushing to {}...", remote.cyan());
    if guard.has_commits {
        git::push_branch(&workspace.root, "main", &remote)?;
        // Commits are on remote now — don't reset them if tag push fails
        guard.has_commits = false;
        println!("  Pushed main branch");
    }
    git::push_tags(
        &workspace.root,
        &guard.tags.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &remote,
    )?;
    for tag in &guard.tags {
        println!("  Pushed {}", tag.green());
    }

    guard.disarm();
    Ok(())
}

/// Publish a single wave of packages.
fn publish_wave(
    workspace: &WorkspaceInfo,
    bump_map: &BTreeMap<String, ReleaseBump>,
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
    bump_map: &BTreeMap<String, ReleaseBump>,
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
            let bump = bump_map.get(url).copied().unwrap_or(ReleaseBump::Minor);
            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
            let current = tags::find_latest_version(all_tags, &tag_prefix);
            let next_version = compute_next_version(current.as_ref(), bump);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            let pkg_dir = pkg.dir(&workspace.root);
            let content_hash = pcb_canonical::compute_content_hash_from_dir(&pkg_dir)?;
            let manifest_content = std::fs::read_to_string(pkg_dir.join("pcb.toml"))?;
            let manifest_hash = pcb_canonical::compute_manifest_hash(&manifest_content);

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

fn resolve_remote(repo_root: &Path, force: bool) -> Result<String> {
    let branch = git::symbolic_ref_short_head(repo_root).ok_or_else(|| {
        anyhow::anyhow!("Not on a branch (detached HEAD state). Switch to main before publishing.")
    })?;
    if !force && branch != "main" {
        bail!("Must be on 'main' branch to publish.");
    }
    git::get_branch_remote(repo_root, &branch).ok_or_else(|| {
        anyhow::anyhow!(
            "Branch '{}' is not tracking a remote.\nSet upstream with: git branch --set-upstream-to=<remote>/{}",
            branch, branch
        )
    })
}

/// Preflight checks run after fetching remote state.
fn preflight_checks(repo_root: &Path, remote: &str) -> Result<()> {
    if git::has_uncommitted_changes(repo_root)? {
        bail!(
            "Working directory has uncommitted changes.\nCommit or stash your changes before publishing."
        );
    }

    let local_sha = git::rev_parse(repo_root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get HEAD commit"))?;
    let remote_ref = format!("{}/main", remote);
    let remote_sha = git::rev_parse(repo_root, &remote_ref)
        .ok_or_else(|| anyhow::anyhow!("Failed to resolve {}", remote_ref))?;

    if local_sha != remote_sha {
        bail!(
            "Local main ({}) is out of sync with {}/main ({}).\nPull or push changes before publishing.",
            &local_sha[..8],
            remote,
            &remote_sha[..8]
        );
    }

    println!("{} on main @ {}", "✓".green(), &local_sha[..8]);
    Ok(())
}

fn compute_next_version(current: Option<&Version>, bump: ReleaseBump) -> Version {
    match current {
        None => Version::new(0, 1, 0),
        Some(v) => match bump {
            ReleaseBump::Patch => Version::new(v.major, v.minor, v.patch + 1),
            ReleaseBump::Minor => Version::new(v.major, v.minor + 1, 0),
            ReleaseBump::Major => Version::new(v.major + 1, 0, 0),
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

    let title = format_dependency_bump_title(&pkg_names);

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

fn format_dependency_bump_title(pkg_names: &[String]) -> String {
    match pkg_names {
        [pkg_name] => {
            let title = format!("chore({pkg_name}): bump deps");
            if title.chars().count() <= CONVENTIONAL_COMMIT_SUBJECT_LEN {
                title
            } else {
                DEPENDENCY_BUMP_TITLE.to_string()
            }
        }
        _ => DEPENDENCY_BUMP_TITLE.to_string(),
    }
}

fn infer_self_bump(
    workspace: &WorkspaceInfo,
    pkg: &MemberPackage,
    current: Option<&Version>,
    latest_tag: Option<&str>,
) -> Option<ReleaseBump> {
    let current = current?;
    let range = latest_tag.map(|tag| format!("{tag}..HEAD"));
    git::log_subjects(
        &workspace.root,
        range.as_deref(),
        Some(pkg.rel_path.as_path()),
    )
    .into_iter()
    .map(|subject| ReleaseBump::infer_from_commit(&subject, current))
    .max()
}

fn current_package_version(
    pkg: &MemberPackage,
    ws_path: Option<&str>,
    all_tags: &[String],
) -> Option<Version> {
    let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
    tags::find_latest_version(all_tags, &tag_prefix)
}

fn infer_all_bumps(
    workspace: &WorkspaceInfo,
    waves: &[Vec<String>],
    all_tags: &[String],
) -> BTreeMap<String, ReleaseBump> {
    let ws_path = workspace.path();
    let mut inferred = BTreeMap::new();

    for wave_urls in waves {
        for url in wave_urls {
            let Some(pkg) = workspace.packages.get(url) else {
                continue;
            };

            let current = current_package_version(pkg, ws_path, all_tags);

            if current.is_none() {
                inferred.insert(url.clone(), ReleaseBump::Minor);
                continue;
            }

            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
            let latest_tag = tags::find_latest_tag(all_tags, &tag_prefix);
            let self_bump =
                infer_self_bump(workspace, pkg, current.as_ref(), latest_tag.as_deref());
            let dep_floor = pkg
                .dependencies()
                .filter_map(|dep_url| inferred.get(dep_url).copied())
                .max();

            let inferred_bump = match (self_bump, dep_floor) {
                (Some(self_bump), Some(dep_floor)) => self_bump.max(dep_floor),
                (Some(self_bump), None) => self_bump,
                (None, Some(dep_floor)) => dep_floor,
                (None, None) => ReleaseBump::Patch,
            };

            inferred.insert(url.clone(), inferred_bump);
        }
    }

    inferred
}

fn uniform_bump_map(waves: &[Vec<String>], bump: ReleaseBump) -> BTreeMap<String, ReleaseBump> {
    waves
        .iter()
        .flat_map(|wave| wave.iter())
        .map(|url| (url.clone(), bump))
        .collect()
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

fn collect_publish_tree(
    workspace: &WorkspaceInfo,
    package_urls: &HashSet<String>,
) -> (String, Vec<String>, HashMap<String, Vec<String>>) {
    let mut children: HashMap<String, Vec<String>> = HashMap::new();
    let mut has_parent: HashSet<String> = HashSet::new();

    for url in package_urls {
        if let Some(pkg) = workspace.packages.get(url) {
            for dep_url in pkg.dependencies() {
                if package_urls.contains(dep_url) {
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

    let mut roots: Vec<_> = package_urls
        .iter()
        .filter(|url| !has_parent.contains(*url))
        .cloned()
        .collect();
    roots.sort();

    let workspace_url = package_urls
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

    (workspace_url, roots, children)
}

fn print_publish_tree(
    title: &str,
    workspace: &WorkspaceInfo,
    package_urls: &HashSet<String>,
    mut format_label: impl FnMut(&str, &MemberPackage) -> String,
) {
    let (workspace_url, roots, children) = collect_publish_tree(workspace, package_urls);

    println!("{}", format!("{title}:").cyan().bold());

    let _ = pcb_zen::tree::print_tree(workspace_url, roots, |url| {
        let label = if let Some(pkg) = workspace.packages.get(url) {
            format_label(url, pkg)
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
    cli_bump: Option<BumpType>,
) -> Result<BTreeMap<String, ReleaseBump>> {
    if let Some(bump) = cli_bump.and_then(BumpType::release) {
        return Ok(uniform_bump_map(waves, bump));
    }

    let all_tags = git::list_all_tags_vec(&workspace.root);
    let ws_path = workspace.path();
    let inferred_bumps = infer_all_bumps(workspace, waves, &all_tags);

    if matches!(cli_bump, Some(BumpType::Infer)) {
        return Ok(inferred_bumps);
    }

    let has_published_packages = waves.iter().flat_map(|w| w.iter()).any(|url| {
        workspace
            .packages
            .get(url)
            .and_then(|pkg| pkg.version.as_ref())
            .is_some()
    });

    if !has_published_packages {
        return Ok(inferred_bumps);
    }

    collect_interactive_bumps(workspace, waves, &all_tags, ws_path, inferred_bumps)
}

fn collect_interactive_bumps(
    workspace: &WorkspaceInfo,
    waves: &[Vec<String>],
    all_tags: &[String],
    ws_path: Option<&str>,
    inferred_bumps: BTreeMap<String, ReleaseBump>,
) -> Result<BTreeMap<String, ReleaseBump>> {
    println!();
    print_publish_summary("Packages to publish", workspace, &inferred_bumps, all_tags);
    println!();

    match prompt_bump_strategy(waves.iter().map(|wave| wave.len()).sum())? {
        BumpStrategy::Infer => Ok(inferred_bumps),
        BumpStrategy::SameForAll => Ok(uniform_bump_map(waves, prompt_bump_type()?)),
        BumpStrategy::ChooseIndividually => waves
            .iter()
            .flat_map(|wave| wave.iter())
            .filter_map(|url| workspace.packages.get(url).map(|pkg| (url, pkg)))
            .map(|(url, pkg)| {
                let current = current_package_version(pkg, ws_path, all_tags);
                let bump = match current.as_ref() {
                    Some(current) => {
                        prompt_single_bump(&pkg.rel_path.display().to_string(), Some(current))?
                    }
                    None => ReleaseBump::Minor,
                };
                Ok((url.clone(), bump))
            })
            .collect(),
    }
}

/// Print summary of what will be published.
fn print_publish_summary(
    title: &str,
    workspace: &WorkspaceInfo,
    bump_map: &BTreeMap<String, ReleaseBump>,
    all_tags: &[String],
) {
    let ws_path = workspace.path();
    let package_urls = bump_map.keys().cloned().collect::<HashSet<_>>();

    print_publish_tree(title, workspace, &package_urls, |url, pkg| {
        let current = current_package_version(pkg, ws_path, all_tags);
        let bump = bump_map.get(url).copied().unwrap_or(ReleaseBump::Minor);
        let next = compute_next_version(current.as_ref(), bump);
        let version_str = match current {
            Some(v) => format!("{} → {}", v, next),
            None => format!("→ {}", next),
        };
        format!("{} {}", pkg.rel_path.display(), version_str.green())
    });
}

fn prompt_bump_strategy(total_packages: usize) -> Result<BumpStrategy> {
    let mut options = vec![BumpStrategy::Infer];
    if total_packages != 1 {
        options.push(BumpStrategy::SameForAll);
    }
    options.push(BumpStrategy::ChooseIndividually);

    Select::new("How do you want to version these packages?", options)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))
}

fn prompt_bump_type() -> Result<ReleaseBump> {
    let options = [
        ("Patch (x.y.Z) - bug fixes", ReleaseBump::Patch),
        ("Minor (x.Y.0) - new features", ReleaseBump::Minor),
        ("Major (X.0.0) - breaking changes", ReleaseBump::Major),
    ];

    let labels: Vec<_> = options.iter().map(|(l, _)| *l).collect();
    let selected = Select::new("Select version bump:", labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(l, _)| *l == selected)
        .map(|(_, b)| *b)
        .unwrap_or(ReleaseBump::Minor))
}

fn single_bump_options(current: Option<&Version>) -> Vec<(String, ReleaseBump)> {
    ReleaseBump::SELECTABLE
        .into_iter()
        .map(|b| {
            (
                format!("{} → {}", b.label(), compute_next_version(current, b)),
                b,
            )
        })
        .collect()
}

fn prompt_single_bump(name: &str, current: Option<&Version>) -> Result<ReleaseBump> {
    let ver = current
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unpublished".to_string());
    let options = single_bump_options(current);

    let labels: Vec<_> = options.iter().map(|(l, _)| l.as_str()).collect();
    let selected = Select::new(&format!("{} ({})", name, ver), labels)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    Ok(options
        .iter()
        .find(|(l, _)| l == selected)
        .map(|(_, b)| *b)
        .unwrap_or(ReleaseBump::Minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_test_utils::sandbox::Sandbox;

    const TEST_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["modules/*"]
"#;

    const TEST_MODULE_ZEN: &str = r#"
P1 = io("P1", Net)
"#;

    fn setup_publish_workspace(sb: &mut Sandbox, packages: &[(&str, &str)]) {
        sb.cwd("src").write("pcb.toml", TEST_WORKSPACE_PCB_TOML);

        for (name, pcb_toml) in packages {
            sb.write(format!("modules/{name}/pcb.toml"), pcb_toml)
                .write(format!("modules/{name}/{name}.zen"), TEST_MODULE_ZEN);
        }

        sb.hash_globs(["**/diodeinc/stdlib/*.zen"])
            .init_git()
            .commit("chore: initial workspace");
    }

    fn tag_package(sb: &mut Sandbox, name: &str, version: &str) {
        sb.cwd("src")
            .cmd("git", ["tag", &format!("modules/{name}/v{version}")])
            .stdout_null()
            .stderr_null()
            .run()
            .expect("tag package");
    }

    fn load_workspace(sb: &Sandbox) -> WorkspaceInfo {
        get_workspace_info(
            &DefaultFileProvider::new(),
            &sb.root_path().join("src"),
            true,
        )
        .unwrap()
    }

    fn dirty_urls(urls: &[&str]) -> HashSet<String> {
        urls.iter().map(|url| (*url).to_string()).collect()
    }

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

    #[test]
    fn test_infer_all_bumps_filters_commit_history_per_package() {
        let mut sb = Sandbox::new();
        setup_publish_workspace(&mut sb, &[("Foo", ""), ("Bar", "")]);
        tag_package(&mut sb, "Foo", "1.2.3");
        tag_package(&mut sb, "Bar", "1.2.3");

        sb.cwd("src")
            .write(
                "modules/Foo/Foo.zen",
                "P1 = io(\"P1\", Net)\nP2 = io(\"P2\", Net)\n",
            )
            .commit("feat(Foo): add second pin")
            .write("modules/Bar/Bar.zen", "P1 = io(\"P1\", Net)\n# cleanup\n")
            .commit("chore(Bar): tidy package");

        let workspace = load_workspace(&sb);
        let waves = compute_publish_waves(&workspace, &dirty_urls(&["modules/Foo", "modules/Bar"]))
            .unwrap();
        let all_tags = git::list_all_tags_vec(&workspace.root);
        let inferred = infer_all_bumps(&workspace, &waves, &all_tags);

        assert_eq!(inferred.get("modules/Foo"), Some(&ReleaseBump::Minor));
        assert_eq!(inferred.get("modules/Bar"), Some(&ReleaseBump::Patch));
    }

    #[test]
    fn test_infer_all_bumps_uses_dependency_floor_across_waves() {
        let mut sb = Sandbox::new();
        setup_publish_workspace(
            &mut sb,
            &[
                ("Dep", ""),
                (
                    "App",
                    r#"
[dependencies]
"modules/Dep" = "1.2.3"
"#,
                ),
            ],
        );
        tag_package(&mut sb, "Dep", "1.2.3");
        tag_package(&mut sb, "App", "1.2.3");

        sb.cwd("src")
            .write(
                "modules/Dep/Dep.zen",
                "P1 = io(\"P1\", Net)\nP2 = io(\"P2\", Net)\n",
            )
            .commit("feat(Dep): add optional pin")
            .write("modules/App/App.zen", "P1 = io(\"P1\", Net)\n# doc tweak\n")
            .commit("chore(App): update docs");

        let workspace = load_workspace(&sb);
        let waves = compute_publish_waves(&workspace, &dirty_urls(&["modules/Dep", "modules/App"]))
            .unwrap();
        let all_tags = git::list_all_tags_vec(&workspace.root);
        let inferred = infer_all_bumps(&workspace, &waves, &all_tags);

        assert_eq!(
            waves,
            vec![
                vec!["modules/Dep".to_string()],
                vec!["modules/App".to_string()]
            ]
        );
        assert_eq!(inferred.get("modules/Dep"), Some(&ReleaseBump::Minor));
        assert_eq!(inferred.get("modules/App"), Some(&ReleaseBump::Minor));
    }

    #[test]
    fn test_infer_all_bumps_treats_non_conventional_commits_conservatively() {
        let mut sb = Sandbox::new();
        setup_publish_workspace(&mut sb, &[("Legacy", ""), ("Stable", "")]);
        tag_package(&mut sb, "Legacy", "0.4.2");
        tag_package(&mut sb, "Stable", "1.4.2");

        sb.cwd("src")
            .write(
                "modules/Legacy/Legacy.zen",
                "P1 = io(\"P1\", Net)\n# compatibility break\n",
            )
            .commit("misc cleanup")
            .write(
                "modules/Stable/Stable.zen",
                "P1 = io(\"P1\", Net)\n# compatibility break\n",
            )
            .commit("some random message");

        let workspace = load_workspace(&sb);
        let waves = compute_publish_waves(
            &workspace,
            &dirty_urls(&["modules/Legacy", "modules/Stable"]),
        )
        .unwrap();
        let all_tags = git::list_all_tags_vec(&workspace.root);
        let inferred = infer_all_bumps(&workspace, &waves, &all_tags);

        assert_eq!(inferred.get("modules/Legacy"), Some(&ReleaseBump::Minor));
        assert_eq!(inferred.get("modules/Stable"), Some(&ReleaseBump::Major));
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

    fn assert_dependency_bump_title(pkg_names: &[&str], expected: &str) {
        let pkg_names = pkg_names
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        let title = format_dependency_bump_title(&pkg_names);
        assert_eq!(title, expected);
        assert!(title.chars().count() <= CONVENTIONAL_COMMIT_SUBJECT_LEN);
    }

    #[test]
    fn test_format_dependency_bump_title_single_package_uses_scope() {
        assert_dependency_bump_title(&["PCM9211x"], "chore(PCM9211x): bump deps");
    }

    #[test]
    fn test_format_dependency_bump_title_multiple_packages_omits_scope() {
        assert_dependency_bump_title(&["UsbC", "LedIndicator"], "chore: bump deps");
    }

    #[test]
    fn test_format_dependency_bump_title_long_single_package_omits_scope() {
        assert_dependency_bump_title(
            &["VeryLongPackageNameThatWouldOverflowTheConventionalCommitSubjectLimit"],
            "chore: bump deps",
        );
    }

    #[test]
    fn test_empty_deps() {
        let result = compute_waves_from_deps(&HashMap::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_single_package_no_deps() {
        let result = compute_waves_from_deps(&deps(&[("a", &[])])).unwrap();
        assert_eq!(normalize(&result), vec![vec!["a"]]);
    }

    #[test]
    fn test_multiple_independent_packages() {
        let result = compute_waves_from_deps(&deps(&[("a", &[]), ("b", &[]), ("c", &[])])).unwrap();
        // All should be in the same wave since they're independent
        assert_eq!(result.len(), 1);
        assert_eq!(normalize(&result), vec![vec!["a", "b", "c"]]);
    }

    #[test]
    fn test_linear_chain() {
        // c depends on b, b depends on a
        // Should publish: a, then b, then c
        let result =
            compute_waves_from_deps(&deps(&[("a", &[]), ("b", &["a"]), ("c", &["b"])])).unwrap();
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
        ]))
        .unwrap();
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
        ]))
        .unwrap();
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
        ]))
        .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(normalize(&result)[0], vec!["a", "b"]);
        assert_eq!(normalize(&result)[1], vec!["c", "d"]);
        assert_eq!(normalize(&result)[2], vec!["e"]);
    }

    #[test]
    fn test_deps_to_non_dirty_packages_ignored() {
        // b depends on a, but only b is in the map (a is clean/already published)
        // b should be in wave 1 since its dependency on a doesn't count
        let result = compute_waves_from_deps(&deps(&[("b", &["a"])])).unwrap();
        assert_eq!(normalize(&result), vec![vec!["b"]]);
    }

    #[test]
    fn test_cycle_detection() {
        // a depends on b, b depends on a - cycle!
        let err = compute_waves_from_deps(&deps(&[("a", &["b"]), ("b", &["a"])]))
            .expect_err("expected cycle error");
        assert_eq!(err, vec!["a", "b", "a"]);
    }

    #[test]
    fn test_self_cycle_detection() {
        let err =
            compute_waves_from_deps(&deps(&[("a", &["a"])])).expect_err("expected self cycle");
        assert_eq!(err, vec!["a", "a"]);
    }

    #[test]
    fn test_version_bump_patch() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), ReleaseBump::Patch),
            Version::new(1, 2, 4)
        );
    }

    #[test]
    fn test_version_bump_minor() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), ReleaseBump::Minor),
            Version::new(1, 3, 0)
        );
    }

    #[test]
    fn test_version_bump_major() {
        let v = Version::new(1, 2, 3);
        assert_eq!(
            compute_next_version(Some(&v), ReleaseBump::Major),
            Version::new(2, 0, 0)
        );
    }

    #[test]
    fn test_version_bump_major_pre_1_0() {
        // For 0.x, a major bump promotes the release to 1.0.0
        let v = Version::new(0, 3, 5);
        assert_eq!(
            compute_next_version(Some(&v), ReleaseBump::Major),
            Version::new(1, 0, 0)
        );
    }

    #[test]
    fn test_version_initial() {
        assert_eq!(
            compute_next_version(None, ReleaseBump::Minor),
            Version::new(0, 1, 0)
        );
        assert_eq!(
            compute_next_version(None, ReleaseBump::Patch),
            Version::new(0, 1, 0)
        );
        assert_eq!(
            compute_next_version(None, ReleaseBump::Major),
            Version::new(0, 1, 0)
        );
    }

    #[test]
    fn test_single_bump_options_always_show_patch_minor_major() {
        let cases = [
            (
                Some(Version::new(0, 3, 5)),
                vec![
                    ("Patch → 0.3.6".to_string(), ReleaseBump::Patch),
                    ("Minor → 0.4.0".to_string(), ReleaseBump::Minor),
                    ("Major → 1.0.0".to_string(), ReleaseBump::Major),
                ],
            ),
            (
                None,
                vec![
                    ("Patch → 0.1.0".to_string(), ReleaseBump::Patch),
                    ("Minor → 0.1.0".to_string(), ReleaseBump::Minor),
                    ("Major → 0.1.0".to_string(), ReleaseBump::Major),
                ],
            ),
        ];

        for (current, expected) in cases {
            assert_eq!(single_bump_options(current.as_ref()), expected);
        }
    }
}
