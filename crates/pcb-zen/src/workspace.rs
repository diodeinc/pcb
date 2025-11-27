//! Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{find_workspace_root, Lockfile, PcbToml, WorkspaceConfig};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

use crate::canonical::{compute_content_hash_from_dir, compute_manifest_hash};
use crate::git;

/// A discovered member package in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberPackage {
    /// Package directory (absolute path)
    pub dir: PathBuf,
    /// Package directory relative to workspace root
    pub rel_path: PathBuf,
    /// Parsed pcb.toml config
    pub config: PcbToml,
    /// Latest published version from git tags (or "0.1.0" if unpublished)
    pub version: String,
}

/// Board discovery information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardInfo {
    /// Board name
    pub name: String,
    /// Path to the .zen file (relative to workspace root)
    pub zen_path: String,
    /// Board description
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub description: String,
}

impl BoardInfo {
    /// Get the absolute path to the board's .zen file
    pub fn absolute_zen_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(&self.zen_path)
    }
}

/// Discovery errors that can occur during board discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryError {
    pub path: PathBuf,
    pub error: String,
}

/// Result of board discovery with any errors encountered
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub boards: Vec<BoardInfo>,
    pub errors: Vec<DiscoveryError>,
}

/// Comprehensive workspace information - the single source of truth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Workspace root directory
    pub root: PathBuf,

    /// Root pcb.toml config (full)
    pub config: PcbToml,

    /// Discovered member packages keyed by URL (includes root if applicable)
    pub packages: BTreeMap<String, MemberPackage>,

    /// Boards derived from packages with [board] sections
    pub boards: Vec<BoardInfo>,

    /// Optional lockfile (loaded from pcb.sum if present)
    #[serde(skip)]
    pub lockfile: Option<Lockfile>,

    /// Discovery errors
    pub errors: Vec<DiscoveryError>,
}

impl WorkspaceInfo {
    /// Get workspace config section (with defaults if not present)
    pub fn workspace_config(&self) -> WorkspaceConfig {
        self.config.workspace.clone().unwrap_or_default()
    }

    /// Reload pcb.toml configs after auto-deps modifications.
    /// Only re-parses existing files - no re-discovery needed since
    /// auto-deps only adds dependencies to known packages.
    pub fn reload(&mut self) -> Result<()> {
        let file_provider = DefaultFileProvider::new();

        // Re-parse root config
        let pcb_toml_path = self.root.join("pcb.toml");
        if file_provider.exists(&pcb_toml_path) {
            self.config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
        }

        // Re-parse each package's config
        for pkg in self.packages.values_mut() {
            let pkg_toml_path = pkg.dir.join("pcb.toml");
            pkg.config = PcbToml::from_file(&file_provider, &pkg_toml_path)?;
        }

        Ok(())
    }

    /// Given an absolute .zen path, return the board name
    pub fn board_name_for_zen(&self, zen_path: &Path) -> Option<String> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards
            .iter()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
            .map(|b| b.name.clone())
    }

    /// Given an absolute .zen path, return the full BoardInfo
    pub fn board_info_for_zen(&self, zen_path: &Path) -> Option<&BoardInfo> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards
            .iter()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
    }

    /// Find a board by name, returning an error with available boards if not found
    pub fn find_board_by_name(&self, board_name: &str) -> Result<&BoardInfo> {
        self.boards
            .iter()
            .find(|b| b.name == board_name)
            .ok_or_else(|| {
                let available: Vec<_> = self.boards.iter().map(|b| b.name.as_str()).collect();
                anyhow::anyhow!(
                    "Board '{board_name}' not found. Available: [{}]",
                    available.join(", ")
                )
            })
    }
}

/// Information about a V2 workspace package member
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    /// Package import URL (e.g., "github.com/diodeinc/registry/reference/ti/tps54331")
    pub url: String,

    /// Absolute path to the package directory
    pub path: PathBuf,

    /// Latest published version from git tags (None if unpublished)
    pub latest_version: Option<String>,

    /// Board configuration if this package defines a board
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<BoardSummary>,

    /// Declared dependency URLs (for publish ordering)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,

    /// Number of declared assets
    pub asset_count: usize,

    /// Whether the package has unpublished changes
    /// True if: no published version, or content differs from published version
    pub dirty: bool,

    /// Whether the package depends (directly or transitively) on a dirty package
    /// When a dirty package is published, all transitive_dirty packages will need
    /// their pcb.toml updated and republished
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub transitive_dirty: bool,

    /// Current content hash (h1:base64)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,

    /// Current manifest hash (h1:base64)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
}

/// Summary of board configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zen_path: Option<String>,
    /// Board version from <board_name>/v<version> tags
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether the board has unpublished changes
    pub dirty: bool,
}

/// V2 workspace information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct V2Workspace {
    /// Workspace root directory
    pub root: PathBuf,

    /// Repository URL from workspace config
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Optional subpath within repository
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Minimum pcb toolchain version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcb_version: Option<String>,

    /// Member glob patterns
    pub member_patterns: Vec<String>,

    /// Discovered package members
    pub packages: Vec<PackageInfo>,

    /// Root package info (if workspace root is also a package)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_package: Option<PackageInfo>,
}

impl V2Workspace {
    /// Get all packages including root package
    pub fn all_packages(&self) -> Vec<&PackageInfo> {
        let mut all: Vec<&PackageInfo> = self.packages.iter().collect();
        if let Some(root) = &self.root_package {
            all.insert(0, root);
        }
        all
    }

    /// Get total package count
    pub fn package_count(&self) -> usize {
        self.packages.len() + if self.root_package.is_some() { 1 } else { 0 }
    }
}

/// Detect and load V2 workspace information from a path
///
/// Returns None if not a V2 workspace.
pub fn detect_v2_workspace(start_path: &Path) -> Result<Option<V2Workspace>> {
    let file_provider = Arc::new(DefaultFileProvider::new());
    detect_v2_workspace_with_provider(&*file_provider, start_path)
}

/// Detect V2 workspace with custom file provider (for testing)
pub fn detect_v2_workspace_with_provider(
    file_provider: &dyn FileProvider,
    start_path: &Path,
) -> Result<Option<V2Workspace>> {
    let workspace_root = find_workspace_root(file_provider, start_path);

    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !file_provider.exists(&pcb_toml_path) {
        return Ok(None);
    }

    let config = PcbToml::from_file(file_provider, &pcb_toml_path)?;
    if !config.is_v2() {
        return Ok(None);
    }

    load_v2_workspace(&workspace_root, &config)
}

/// Load V2 workspace information from a known V2 workspace root
fn load_v2_workspace(workspace_root: &Path, config: &PcbToml) -> Result<Option<V2Workspace>> {
    use rayon::prelude::*;

    let file_provider = DefaultFileProvider::new();
    let ws = config.workspace.as_ref();
    let repository = ws.and_then(|w| w.repository.clone());
    let path = ws.and_then(|w| w.path.clone());
    let pcb_version = ws.and_then(|w| w.pcb_version.clone());
    let workspace_config = config.workspace.clone().unwrap_or_default();

    // Use get_workspace_info to discover packages
    let workspace_info = get_workspace_info(&file_provider, workspace_root)?;
    let member_packages = &workspace_info.packages;
    let tags = git::list_all_tags_vec(workspace_root);
    let tag_annotations = git::get_all_tag_annotations(workspace_root);

    let base_url = match (&repository, &path) {
        (Some(repo), Some(p)) => Some(format!("{}/{}", repo, p)),
        (Some(repo), None) => Some(repo.clone()),
        _ => None,
    };

    // Build package infos in parallel
    let packages: Vec<PackageInfo> = member_packages
        .par_iter()
        .filter_map(|(url, pkg)| {
            let rel_str = pkg.rel_path.to_string_lossy();
            if rel_str.is_empty() {
                return None;
            }
            build_package_info(
                &pkg.dir,
                url,
                workspace_root,
                &path,
                &tags,
                &tag_annotations,
            )
            .ok()
        })
        .collect();

    // Build root package info only if:
    // 1. It has at least one dependency or asset, OR
    // 2. No other packages were found (so there's always at least one package)
    let root_package = if let Some(base) = &base_url {
        let has_deps = !config.dependencies.is_empty() || !config.assets.is_empty();
        let no_other_packages = packages.is_empty();

        if has_deps || no_other_packages {
            Some(build_package_info(
                workspace_root,
                base,
                workspace_root,
                &path,
                &tags,
                &tag_annotations,
            )?)
        } else {
            None
        }
    } else {
        None
    };

    // Compute transitive dirty status after all packages are built
    let mut packages = packages;
    let mut root_package = root_package;
    compute_transitive_dirty(&mut packages, &mut root_package);

    Ok(Some(V2Workspace {
        root: workspace_root.to_path_buf(),
        repository,
        path,
        pcb_version,
        member_patterns: workspace_config.members.clone(),
        packages,
        root_package,
    }))
}

/// Compute transitive dirty status for all packages
///
/// A package is transitive_dirty if it depends (directly or transitively) on a dirty package.
/// Uses BFS from all dirty packages, following reverse dependency edges.
fn compute_transitive_dirty(packages: &mut [PackageInfo], root_package: &mut Option<PackageInfo>) {
    use std::collections::{HashSet, VecDeque};

    // Build reverse dependency graph: dep_url -> list of dependant URLs (using owned Strings)
    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();

    // Build the reverse dependency map
    for pkg in packages.iter().chain(root_package.iter()) {
        for dep_url in &pkg.dependencies {
            reverse_deps
                .entry(dep_url.clone())
                .or_default()
                .push(pkg.url.clone());
        }
    }

    // Find all directly dirty packages
    let dirty_urls: HashSet<String> = packages
        .iter()
        .chain(root_package.iter())
        .filter(|p| p.dirty)
        .map(|p| p.url.clone())
        .collect();

    // BFS from dirty packages to find all transitively affected packages
    let mut transitive_dirty_urls: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = dirty_urls.iter().cloned().collect();

    while let Some(url) = queue.pop_front() {
        // Get all packages that depend on this dirty/transitive_dirty package
        if let Some(dependants) = reverse_deps.get(&url) {
            for dependant_url in dependants {
                // Skip if already marked as dirty or transitive_dirty
                if dirty_urls.contains(dependant_url) {
                    continue;
                }
                if transitive_dirty_urls.insert(dependant_url.clone()) {
                    // Newly discovered - add to queue to propagate further
                    queue.push_back(dependant_url.clone());
                }
            }
        }
    }

    // Apply transitive_dirty flag to packages
    for pkg in packages.iter_mut().chain(root_package.iter_mut()) {
        if transitive_dirty_urls.contains(&pkg.url) {
            pkg.transitive_dirty = true;
        }
    }
}

/// Build PackageInfo for a single package directory
fn build_package_info(
    dir: &Path,
    url: &str,
    workspace_root: &Path,
    ws_path: &Option<String>,
    tags: &[String],
    tag_annotations: &HashMap<String, String>,
) -> Result<PackageInfo> {
    let pcb_toml_path = dir.join("pcb.toml");
    let config = if pcb_toml_path.exists() {
        let file_provider = DefaultFileProvider::new();
        PcbToml::from_file(&file_provider, &pcb_toml_path).ok()
    } else {
        None
    };

    // Compute tag prefix for version lookup
    let rel_path = dir.strip_prefix(workspace_root).ok();
    let tag_prefix = compute_tag_prefix(rel_path, ws_path);

    let latest_version = find_latest_version(tags, &tag_prefix);

    let board = config.as_ref().and_then(|c| {
        c.board.as_ref().map(|b| {
            // If no path specified, try to find a single .zen file in the directory
            let zen_path = b.path.clone().or_else(|| find_single_zen_file(dir));
            // Boards use <board_name>/v<version> tag format
            let board_tag_prefix = format!("{}/v", b.name);
            let board_version = find_latest_version(tags, &board_tag_prefix);
            let board_dirty = compute_dirty_status(
                workspace_root,
                dir,
                &board_tag_prefix,
                tags,
                tag_annotations,
            );
            BoardSummary {
                name: b.name.clone(),
                description: if b.description.is_empty() {
                    None
                } else {
                    Some(b.description.clone())
                },
                zen_path,
                version: board_version,
                dirty: board_dirty.dirty,
            }
        })
    });

    let dependencies: Vec<String> = config
        .as_ref()
        .map(|c| c.dependencies.keys().cloned().collect())
        .unwrap_or_default();
    let asset_count = config.as_ref().map(|c| c.assets.len()).unwrap_or(0);

    // Compute dirty status (with fast path for unpublished packages)
    let dirty_result =
        compute_dirty_status(workspace_root, dir, &tag_prefix, tags, tag_annotations);

    Ok(PackageInfo {
        url: url.to_string(),
        path: dir.to_path_buf(),
        latest_version,
        board,
        dependencies,
        asset_count,
        dirty: dirty_result.dirty,
        transitive_dirty: false, // Computed after all packages are built
        content_hash: dirty_result.hashes.content_hash,
        manifest_hash: dirty_result.hashes.manifest_hash,
    })
}

/// Compute the git tag prefix for a package
///
/// Examples:
/// - Root package with no ws.path: "v"
/// - Root package with ws.path="hardware": "hardware/v"
/// - Member at "ti/tps54331" with no ws.path: "ti/tps54331/v"
/// - Member at "ti/tps54331" with ws.path="hardware": "hardware/ti/tps54331/v"
pub fn compute_tag_prefix(rel_path: Option<&Path>, ws_path: &Option<String>) -> String {
    let rel_str = rel_path
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    match (ws_path, rel_str.is_empty()) {
        (Some(p), true) => format!("{}/v", p),
        (Some(p), false) => format!("{}/{}/v", p, rel_str),
        (None, true) => "v".to_string(),
        (None, false) => format!("{}/v", rel_str),
    }
}

/// Find the latest semver version matching a tag prefix
fn find_latest_version(tags: &[String], prefix: &str) -> Option<String> {
    tags.iter()
        .filter_map(|tag| {
            let version_str = tag.strip_prefix(prefix)?;
            Version::parse(version_str).ok()
        })
        .max()
        .map(|v| v.to_string())
}

/// Package hashes (content + manifest)
#[derive(Debug, Clone, Default)]
struct PackageHashes {
    content_hash: Option<String>,
    manifest_hash: Option<String>,
}

impl PackageHashes {
    fn compute(dir: &Path) -> Self {
        let content_hash = compute_content_hash_from_dir(dir).ok();
        let manifest_hash = std::fs::read_to_string(dir.join("pcb.toml"))
            .ok()
            .map(|content| compute_manifest_hash(&content));
        Self {
            content_hash,
            manifest_hash,
        }
    }
}

/// Result of dirty detection with optional hashes
struct DirtyResult {
    dirty: bool,
    hashes: PackageHashes,
}

/// Check if a package is dirty (has changes compared to published version)
///
/// Returns dirty status and computed hashes. Uses fast paths:
/// - Unpublished: dirty, skip content hashing
/// - Published but no hashes in tag: dirty (legacy tag), compute current hashes
/// - Published with hashes: compare current vs tag hashes
fn compute_dirty_status(
    workspace_root: &Path,
    package_dir: &Path,
    tag_prefix: &str,
    tags: &[String],
    tag_annotations: &HashMap<String, String>,
) -> DirtyResult {
    // Find the latest matching tag
    let latest_tag = find_latest_tag(tags, tag_prefix);

    let Some(tag_name) = latest_tag else {
        // Fast path: unpublished = dirty, skip content hashing
        return DirtyResult {
            dirty: true,
            hashes: PackageHashes::default(),
        };
    };

    // Check for uncommitted changes first (cheaper than hashing)
    if has_uncommitted_changes(workspace_root, package_dir) {
        return DirtyResult {
            dirty: true,
            hashes: PackageHashes::compute(package_dir),
        };
    }

    // Look up hashes from pre-fetched tag annotations
    let tagged_hashes = tag_annotations
        .get(&tag_name)
        .and_then(|body| parse_hashes_from_tag_body(body));

    let Some(tagged) = tagged_hashes else {
        // No hashes in tag annotation (legacy tag) = assume dirty
        // Still compute current hashes for display
        return DirtyResult {
            dirty: true,
            hashes: PackageHashes::compute(package_dir),
        };
    };

    // Compute current hashes and compare
    let current_hashes = PackageHashes::compute(package_dir);
    let dirty = current_hashes.content_hash != tagged.content_hash
        || current_hashes.manifest_hash != tagged.manifest_hash;

    DirtyResult {
        dirty,
        hashes: current_hashes,
    }
}

/// Find the latest tag matching a prefix, returning the full tag name
fn find_latest_tag(tags: &[String], prefix: &str) -> Option<String> {
    tags.iter()
        .filter_map(|tag| {
            let version_str = tag.strip_prefix(prefix)?;
            let version = Version::parse(version_str).ok()?;
            Some((tag.clone(), version))
        })
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(tag, _)| tag)
}

/// Check if there are uncommitted changes in a directory
fn has_uncommitted_changes(workspace_root: &Path, package_dir: &Path) -> bool {
    let rel_path = package_dir
        .strip_prefix(workspace_root)
        .unwrap_or(package_dir);
    git::has_uncommitted_changes_in_path(workspace_root, rel_path)
}

/// Parse hashes from tag annotation body (pcb.sum style format)
///
/// Format: Each line is "module_path version hash" where:
/// - Content hash line: "module_path v0.1.0 h1:base64"
/// - Manifest hash line: "module_path v0.1.0/pcb.toml h1:base64"
fn parse_hashes_from_tag_body(body: &str) -> Option<PackageHashes> {
    let mut content_hash = None;
    let mut manifest_hash = None;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Find the hash at the end (h1:...)
        if let Some(hash_start) = line.find(" h1:") {
            let hash = line[hash_start + 1..].to_string(); // Include "h1:"

            // Check if this is a manifest hash (contains /pcb.toml before the hash)
            let before_hash = &line[..hash_start];
            if before_hash.ends_with("/pcb.toml") {
                manifest_hash = Some(hash);
            } else {
                content_hash = Some(hash);
            }
        }
    }

    // Both hashes must be present
    if content_hash.is_some() && manifest_hash.is_some() {
        Some(PackageHashes {
            content_hash,
            manifest_hash,
        })
    } else {
        None
    }
}

/// Find single .zen file in a directory (for board path discovery)
fn find_single_zen_file(dir: &Path) -> Option<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let zen_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "zen"))
        .collect();

    if zen_files.len() == 1 {
        zen_files[0]
            .file_name()
            .to_string_lossy()
            .to_string()
            .into()
    } else {
        None
    }
}

/// Get complete workspace information - the single entry point for all workspace discovery.
///
/// Discovers packages, derives boards, builds workspace_members lookup, and optionally loads lockfile.
pub fn get_workspace_info(
    file_provider: &dyn FileProvider,
    start_path: &Path,
) -> Result<WorkspaceInfo> {
    let workspace_root = find_workspace_root(file_provider, start_path);
    let pcb_toml_path = workspace_root.join("pcb.toml");

    // Load root config
    let config = if file_provider.exists(&pcb_toml_path) {
        PcbToml::from_file(file_provider, &pcb_toml_path)?
    } else {
        // Empty config for directories without pcb.toml
        PcbToml::parse("")?
    };
    let workspace_config = config.workspace.clone().unwrap_or_default();

    // Discover packages inline
    let patterns = &workspace_config.members;
    let mut packages = BTreeMap::new();

    // Compute base URL from workspace config
    let base_url = match (&workspace_config.repository, &workspace_config.path) {
        (Some(repo), Some(p)) => Some(format!("{}/{}", repo, p)),
        (Some(repo), None) => Some(repo.clone()),
        _ => None,
    };

    // Fetch git tags once for version lookup
    let tags = git::list_all_tags_vec(&workspace_root);

    if !patterns.is_empty() {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern)?);
            if let Some(exact) = pattern.strip_suffix("/*") {
                builder.add(Glob::new(exact)?);
            }
        }
        let glob_set = builder.build()?;

        for entry in WalkDir::new(&workspace_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let pkg_toml_path = path.join("pcb.toml");
            if !file_provider.exists(&pkg_toml_path) {
                continue;
            }
            let Ok(rel_path) = path.strip_prefix(&workspace_root) else {
                continue;
            };
            if !glob_set.is_match(rel_path) {
                continue;
            }
            let pkg_config = PcbToml::from_file(file_provider, &pkg_toml_path)?;
            let rel_str = rel_path.to_string_lossy();
            let url = base_url
                .as_ref()
                .map(|base| format!("{}/{}", base, rel_str))
                .unwrap_or_else(|| rel_str.to_string());

            // Compute version from git tags
            let tag_prefix = compute_tag_prefix(Some(rel_path), &workspace_config.path);
            let version =
                find_latest_version(&tags, &tag_prefix).unwrap_or_else(|| "0.1.0".to_string());

            packages.insert(
                url,
                MemberPackage {
                    dir: path.to_path_buf(),
                    rel_path: rel_path.to_path_buf(),
                    config: pkg_config,
                    version,
                },
            );
        }
    }

    // Add root package if it has a repository URL
    if let Some(url) = &base_url {
        packages.insert(
            url.clone(),
            MemberPackage {
                dir: workspace_root.clone(),
                rel_path: PathBuf::new(),
                config: config.clone(),
                version: "0.1.0".to_string(), // Root version computed separately if needed
            },
        );
    }

    // Derive boards from packages
    let mut boards_by_name: BTreeMap<String, BoardInfo> = BTreeMap::new();
    let mut errors = Vec::new();

    for pkg in packages.values() {
        let Some(board_config) = &pkg.config.board else {
            continue;
        };

        let zen_path = if let Some(configured_path) = &board_config.path {
            configured_path.clone()
        } else {
            match find_single_zen_file(&pkg.dir) {
                Some(zen_file) => zen_file,
                None => {
                    errors.push(DiscoveryError {
                        path: pkg.dir.join("pcb.toml"),
                        error: "No path specified and no single .zen file found".to_string(),
                    });
                    continue;
                }
            }
        };

        let board = BoardInfo {
            name: board_config.name.clone(),
            zen_path: pkg.rel_path.join(&zen_path).to_string_lossy().to_string(),
            description: board_config.description.clone(),
        };

        let has_conflict = boards_by_name
            .keys()
            .any(|k| k.eq_ignore_ascii_case(&board.name));
        if has_conflict {
            errors.push(DiscoveryError {
                path: pkg.dir.join("pcb.toml"),
                error: format!("Duplicate board name: '{}'", board.name),
            });
        } else {
            boards_by_name.insert(board.name.clone(), board);
        }
    }

    let mut boards: Vec<_> = boards_by_name.into_values().collect();
    boards.sort_by(|a, b| a.name.cmp(&b.name));

    // Load lockfile if present
    let lockfile_path = workspace_root.join("pcb.sum");
    let lockfile = if lockfile_path.exists() {
        match std::fs::read_to_string(&lockfile_path) {
            Ok(content) => Lockfile::parse(&content).ok(),
            Err(_) => None,
        }
    } else {
        None
    };

    Ok(WorkspaceInfo {
        root: workspace_root,
        config,
        packages,
        boards,
        lockfile,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_tag_prefix() {
        // Root with no ws.path
        assert_eq!(compute_tag_prefix(None, &None), "v");
        assert_eq!(compute_tag_prefix(Some(Path::new("")), &None), "v");

        // Root with ws.path
        assert_eq!(
            compute_tag_prefix(None, &Some("hardware".to_string())),
            "hardware/v"
        );

        // Member with no ws.path
        assert_eq!(
            compute_tag_prefix(Some(Path::new("ti/tps54331")), &None),
            "ti/tps54331/v"
        );

        // Member with ws.path
        assert_eq!(
            compute_tag_prefix(
                Some(Path::new("ti/tps54331")),
                &Some("hardware".to_string())
            ),
            "hardware/ti/tps54331/v"
        );
    }

    #[test]
    fn test_find_latest_version() {
        let tags = vec![
            "v0.1.0".to_string(),
            "v0.2.0".to_string(),
            "v0.2.1".to_string(),
            "other/v1.0.0".to_string(),
        ];

        assert_eq!(find_latest_version(&tags, "v"), Some("0.2.1".to_string()));
        assert_eq!(
            find_latest_version(&tags, "other/v"),
            Some("1.0.0".to_string())
        );
        assert_eq!(find_latest_version(&tags, "nonexistent/v"), None);
    }

    #[test]
    fn test_parse_hashes_from_tag_body() {
        // pcb.sum style format (actual format used by publish)
        let body = "github.com/diodeinc/registry/harness v0.1.0 h1:mIGycQL5u80O2Jx/p3sUzJ566E74nA/Qof630p+ojSg=\ngithub.com/diodeinc/registry/harness v0.1.0/pcb.toml h1:rxNJufX5oaagQE3qNtzJSZvLJcmtwRK3zJqTyuQfMmI=\n";

        let hashes = parse_hashes_from_tag_body(body).expect("should parse hashes");
        assert_eq!(
            hashes.content_hash,
            Some("h1:mIGycQL5u80O2Jx/p3sUzJ566E74nA/Qof630p+ojSg=".to_string())
        );
        assert_eq!(
            hashes.manifest_hash,
            Some("h1:rxNJufX5oaagQE3qNtzJSZvLJcmtwRK3zJqTyuQfMmI=".to_string())
        );

        // Empty body
        assert!(parse_hashes_from_tag_body("").is_none());

        // Missing manifest hash
        let body_no_manifest = "github.com/foo v1.0.0 h1:abc123=\n";
        assert!(parse_hashes_from_tag_body(body_no_manifest).is_none());

        // Missing content hash
        let body_no_content = "github.com/foo v1.0.0/pcb.toml h1:abc123=\n";
        assert!(parse_hashes_from_tag_body(body_no_content).is_none());
    }
}
