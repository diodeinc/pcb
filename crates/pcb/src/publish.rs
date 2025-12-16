//! V2 package publishing
//!
//! Publishes dirty/unpublished packages by creating annotated git tags with
//! content and manifest hashes. Uses topological sorting to publish packages
//! in dependency order (dependencies before dependants).

use anyhow::{bail, Result};
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

#[derive(Args, Debug)]
#[command(about = "Publish packages by creating version tags")]
pub struct PublishArgs {
    /// Skip preflight checks (uncommitted changes, branch, remote)
    #[arg(long, short = 'f')]
    pub force: bool,

    /// Suppress diagnostics by kind or severity
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

/// Result of publishing a wave
struct WaveResult {
    tags: Vec<String>,
    commit: Option<String>,
    candidates: BTreeMap<String, PublishCandidate>,
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
    let start_path = args
        .path
        .as_ref()
        .map(|p| Path::new(p).to_path_buf())
        .unwrap_or_else(|| env::current_dir().unwrap());

    let file_provider = DefaultFileProvider::new();
    let workspace = get_workspace_info(&file_provider, &start_path)?;

    let remote = if args.force {
        let branch = git::symbolic_ref_short_head(&workspace.root)
            .ok_or_else(|| anyhow::anyhow!("Not on a branch (detached HEAD state)"))?;
        git::get_branch_remote(&workspace.root, &branch)
            .ok_or_else(|| anyhow::anyhow!("Branch '{}' is not tracking a remote", branch))?
    } else {
        preflight_checks(&workspace.root)?
    };

    println!("Syncing tags from {}...", remote.cyan());
    git::fetch_tags(&workspace.root, &remote)?;

    build_workspace(&workspace, &args.suppress)?;

    let initial_commit = git::rev_parse(&workspace.root, "HEAD")
        .ok_or_else(|| anyhow::anyhow!("Failed to get initial commit"))?;

    // Get dirty non-board packages and compute publish waves
    let dirty_urls: HashSet<String> = workspace
        .dirty_packages()
        .keys()
        .filter(|url| {
            workspace
                .packages
                .get(*url)
                .is_some_and(|p| p.config.board.is_none())
        })
        .cloned()
        .collect();

    let waves = compute_publish_waves(&workspace, &dirty_urls);

    if waves.is_empty() {
        println!("{}", "No packages to publish".green());
        return Ok(());
    }

    // Determine bump strategy
    let bump_strategy = if let Some(bump) = args.bump {
        BumpStrategy::SameForAll(bump)
    } else {
        let packages: Vec<_> = dirty_urls
            .iter()
            .filter_map(|url| workspace.packages.get_key_value(url))
            .collect();
        prompt_bump_strategy_choice(&packages)?
    };

    let mut all_tags: Vec<String> = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    let mut published: BTreeMap<String, PublishCandidate> = BTreeMap::new();

    // Process each wave
    let result: Result<()> = (|| {
        for (wave_idx, wave_urls) in waves.iter().enumerate() {
            let wave = publish_wave(
                &workspace,
                &bump_strategy,
                wave_idx + 1,
                wave_urls,
                &published,
            )?;

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

    // Confirm and push
    println!();
    let prompt = if commits.is_empty() {
        format!("Push {} tag(s) to {}?", all_tags.len(), remote)
    } else {
        format!(
            "Push main branch and {} tag(s) to {}?",
            all_tags.len(),
            remote
        )
    };

    if !Confirm::new(&prompt)
        .with_default(false)
        .prompt()
        .unwrap_or(false)
    {
        return rollback(
            &workspace.root,
            &all_tags,
            commits.first().map(|_| &initial_commit),
        );
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
    bump_strategy: &BumpStrategy,
    wave_num: usize,
    package_urls: &[String],
    published: &BTreeMap<String, PublishCandidate>,
) -> Result<WaveResult> {
    let all_tags = git::list_all_tags_vec(&workspace.root);
    let ws_path = workspace.path();

    // Get bump types
    let bump_map: BTreeMap<String, BumpType> = match bump_strategy {
        BumpStrategy::SameForAll(bump) => package_urls
            .iter()
            .map(|url| (url.clone(), *bump))
            .collect(),
        BumpStrategy::ChooseIndividually => {
            let mut map = BTreeMap::new();
            for url in package_urls {
                if let Some(pkg) = workspace.packages.get(url) {
                    let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), ws_path);
                    let current = tags::find_latest_version(&all_tags, &tag_prefix);
                    map.insert(url.clone(), prompt_single_bump(url, current.as_ref())?);
                }
            }
            map
        }
    };

    println!();
    println!("{}", format!("Wave {}:", wave_num).cyan().bold());
    println!("{} package(s) to publish:", package_urls.len());

    // Bump pcb.toml for packages that depend on previously published packages
    let mut changed_pkgs: Vec<&MemberPackage> = Vec::new();
    for url in package_urls {
        if let Some(pkg) = workspace.packages.get(url) {
            let has_published_dep = pkg.dependencies().any(|d| published.contains_key(d));
            if has_published_dep && bump_dependency_versions(&pkg.dir.join("pcb.toml"), published)?
            {
                println!("  patching: {}/pcb.toml", pkg.rel_path.display());
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
    let candidates = build_candidates(workspace, &bump_map, package_urls, &all_tags)?;

    // Create tags
    let mut created_tags = Vec::new();
    for (url, c) in &candidates {
        print_candidate(c, &workspace.root);
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
            let current_version = tags::find_latest_version(all_tags, &tag_prefix);
            let next_version = compute_next_version(current_version.as_ref(), bump);
            let tag_name = compute_tag_name(pkg, &next_version, workspace);

            let content_hash = canonical::compute_content_hash_from_dir(&pkg.dir)?;
            let manifest_content = std::fs::read_to_string(pkg.dir.join("pcb.toml"))?;
            let manifest_hash = canonical::compute_manifest_hash(&manifest_content);

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

fn build_workspace(workspace: &WorkspaceInfo, suppress: &[String]) -> Result<()> {
    println!();
    println!("{}", "Building workspace...".cyan().bold());

    let zen_files = collect_zen_files(std::slice::from_ref(&workspace.root), false)?;
    if zen_files.is_empty() {
        return Ok(());
    }

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

fn preflight_checks(repo_root: &Path) -> Result<String> {
    if git::has_uncommitted_changes(repo_root)? {
        bail!("Working directory has uncommitted changes.\nCommit or stash your changes before publishing.");
    }

    let branch = git::symbolic_ref_short_head(repo_root).ok_or_else(|| {
        anyhow::anyhow!("Not on a branch (detached HEAD state). Switch to main before publishing.")
    })?;

    if branch != "main" {
        bail!(
            "Must be on 'main' branch to publish.\nCurrent branch: '{}'\nRun: git checkout main",
            branch
        );
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

fn print_candidate(c: &PublishCandidate, root: &Path) {
    let rel = c.pkg.dir.strip_prefix(root).unwrap_or(&c.pkg.dir);
    let path = if rel.as_os_str().is_empty() {
        "(root)".to_string()
    } else {
        rel.display().to_string()
    };
    let version = match &c.current_version {
        Some(v) => format!("{} → {}", v, c.next_version),
        None => format!("{} (initial)", c.next_version),
    };
    println!("  {}: {} [{}]", path, version.green(), c.tag_name.cyan());
}

fn compute_next_version(current: Option<&Version>, bump: BumpType) -> Version {
    match current {
        None => Version::new(0, 1, 0),
        Some(v) => match bump {
            BumpType::Patch => Version::new(v.major, v.minor, v.patch + 1),
            BumpType::Minor => Version::new(v.major, v.minor + 1, 0),
            BumpType::Major if v.major == 0 => Version::new(0, v.minor + 1, 0),
            BumpType::Major => Version::new(v.major + 1, 0, 0),
        },
    }
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

fn prompt_bump_strategy_choice(packages: &[(&String, &MemberPackage)]) -> Result<BumpStrategy> {
    let published: Vec<_> = packages
        .iter()
        .filter(|(_, pkg)| pkg.version.is_some())
        .collect();

    if published.is_empty() {
        return Ok(BumpStrategy::SameForAll(BumpType::Minor));
    }

    println!();
    println!(
        "{} package(s) with unpublished changes:",
        published.len().to_string().cyan()
    );
    for (url, pkg) in &published {
        let version = pkg.version.as_deref().unwrap_or("unpublished");
        let name = url.split('/').next_back().unwrap_or(url);
        println!("  {} ({})", name, version);
    }
    println!();

    if published.len() == 1 {
        return Ok(BumpStrategy::ChooseIndividually);
    }

    let options = vec!["Same bump for all", "Choose individually"];
    let choice = Select::new("How do you want to version these packages?", options)
        .prompt()
        .map_err(|e| anyhow::anyhow!("Prompt cancelled: {}", e))?;

    if choice == "Same bump for all" {
        Ok(BumpStrategy::SameForAll(prompt_bump_type()?))
    } else {
        Ok(BumpStrategy::ChooseIndividually)
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

fn prompt_single_bump(url: &str, current: Option<&Version>) -> Result<BumpType> {
    let name = url.split('/').next_back().unwrap_or(url);
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
