//! V2 Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying V2 workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{find_workspace_root, PcbToml};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use walkdir::WalkDir;

use crate::resolve_v2::{compute_content_hash_from_dir, compute_manifest_hash};

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

    load_v2_workspace(file_provider, &workspace_root, &config)
}

/// Load V2 workspace information from a known V2 workspace root
fn load_v2_workspace(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    config: &PcbToml,
) -> Result<Option<V2Workspace>> {
    let ws = config.workspace.as_ref();

    let repository = ws.and_then(|w| w.repository.clone());
    let path = ws.and_then(|w| w.path.clone());
    let pcb_version = ws.and_then(|w| w.pcb_version.clone());
    let member_patterns = ws
        .map(|w| w.members.clone())
        .unwrap_or_else(|| vec!["boards/*".to_string()]);

    // Discover member directories
    let member_dirs = discover_member_dirs(workspace_root, &member_patterns)?;

    // Get git tags for version discovery
    let tags = get_local_git_tags(workspace_root);

    // Build base URL for package paths
    let base_url = match (&repository, &path) {
        (Some(repo), Some(p)) => Some(format!("{}/{}", repo, p)),
        (Some(repo), None) => Some(repo.clone()),
        _ => None,
    };

    // Build package infos for members
    let mut packages = Vec::new();
    for dir in &member_dirs {
        if let Ok(rel_path) = dir.strip_prefix(workspace_root) {
            let rel_str = rel_path.to_string_lossy();
            if rel_str.is_empty() {
                continue; // Skip root, handled separately
            }

            let url = base_url
                .as_ref()
                .map(|base| format!("{}/{}", base, rel_str))
                .unwrap_or_else(|| rel_str.to_string());

            let pkg_info =
                build_package_info(file_provider, dir, &url, workspace_root, &path, &tags)?;
            packages.push(pkg_info);
        }
    }

    // Build root package info only if:
    // 1. It has at least one dependency or asset, OR
    // 2. No other packages were found (so there's always at least one package)
    let root_package = if let Some(base) = &base_url {
        let has_deps = !config.dependencies.is_empty() || !config.assets.is_empty();
        let no_other_packages = packages.is_empty();

        if has_deps || no_other_packages {
            Some(build_package_info(
                file_provider,
                workspace_root,
                base,
                workspace_root,
                &path,
                &tags,
            )?)
        } else {
            None
        }
    } else {
        None
    };

    Ok(Some(V2Workspace {
        root: workspace_root.to_path_buf(),
        repository,
        path,
        pcb_version,
        member_patterns,
        packages,
        root_package,
    }))
}

/// Build PackageInfo for a single package directory
fn build_package_info(
    file_provider: &dyn FileProvider,
    dir: &Path,
    url: &str,
    workspace_root: &Path,
    ws_path: &Option<String>,
    tags: &[String],
) -> Result<PackageInfo> {
    let pcb_toml_path = dir.join("pcb.toml");
    let config = if file_provider.exists(&pcb_toml_path) {
        PcbToml::from_file(file_provider, &pcb_toml_path).ok()
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
            let board_dirty = compute_dirty_status(workspace_root, dir, &board_tag_prefix, tags);
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
    let dirty_result = compute_dirty_status(workspace_root, dir, &tag_prefix, tags);

    Ok(PackageInfo {
        url: url.to_string(),
        path: dir.to_path_buf(),
        latest_version,
        board,
        dependencies,
        asset_count,
        dirty: dirty_result.dirty,
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

    // Try to read hashes from tag annotation
    let tagged_hashes = get_hashes_from_tag_annotation(workspace_root, &tag_name);

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

    let path_arg = if rel_path == Path::new("") {
        ".".to_string()
    } else {
        rel_path.to_string_lossy().to_string()
    };

    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("status")
        .arg("--porcelain")
        .arg("--")
        .arg(&path_arg)
        .output();

    match output {
        Ok(o) if o.status.success() => !o.stdout.is_empty(),
        _ => true,
    }
}

/// Read package hashes from a git tag annotation
///
/// Expected tag annotation format (pcb.sum style):
/// ```
/// module_path v0.1.0 h1:<content_hash>
/// module_path v0.1.0/pcb.toml h1:<manifest_hash>
/// ```
fn get_hashes_from_tag_annotation(workspace_root: &Path, tag_name: &str) -> Option<PackageHashes> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("tag")
        .arg("-l")
        .arg("--format=%(contents)")
        .arg(tag_name)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let body = String::from_utf8_lossy(&output.stdout);
    parse_hashes_from_tag_body(&body)
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

/// Get local git tags from a repository
fn get_local_git_tags(workspace_root: &Path) -> Vec<String> {
    use std::process::Command;

    Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("tag")
        .arg("-l")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Discover member package directories matching glob patterns
pub fn discover_member_dirs(workspace_root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    if patterns.is_empty() {
        return Ok(vec![]);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        // Also match exact directory (e.g., "boards" matches "boards/*")
        if let Some(exact) = pattern.strip_suffix("/*") {
            builder.add(Glob::new(exact)?);
        }
    }
    let glob_set = builder.build()?;

    let mut dirs = Vec::new();
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_dir() || !path.join("pcb.toml").exists() {
            continue;
        }
        if let Ok(rel_path) = path.strip_prefix(workspace_root) {
            if glob_set.is_match(rel_path) {
                dirs.push(path.to_path_buf());
            }
        }
    }

    Ok(dirs)
}

/// Build workspace member versions map for auto-deps
///
/// Returns map: module_path -> version (latest published or "0.1.0" for unpublished)
pub fn build_workspace_member_versions(
    config: &PcbToml,
    workspace_root: &Path,
    member_dirs: &[PathBuf],
) -> HashMap<String, String> {
    let Some(ws) = &config.workspace else {
        return HashMap::new();
    };
    let Some(repo) = &ws.repository else {
        return HashMap::new();
    };

    let base = match &ws.path {
        Some(p) => format!("{}/{}", repo, p),
        None => repo.clone(),
    };

    let tags = get_local_git_tags(workspace_root);

    let mut versions = HashMap::new();

    // Root package
    let root_prefix = match &ws.path {
        Some(p) => format!("{}/v", p),
        None => "v".to_string(),
    };
    versions.insert(
        base.clone(),
        find_latest_version(&tags, &root_prefix).unwrap_or_else(|| "0.1.0".to_string()),
    );

    // Member packages
    for dir in member_dirs {
        if let Ok(rel) = dir.strip_prefix(workspace_root) {
            let rel_str = rel.to_string_lossy();
            if !rel_str.is_empty() {
                let url = format!("{}/{}", base, rel_str);
                let tag_prefix = match &ws.path {
                    Some(p) => format!("{}/{}/v", p, rel_str),
                    None => format!("{}/v", rel_str),
                };
                versions.insert(
                    url,
                    find_latest_version(&tags, &tag_prefix).unwrap_or_else(|| "0.1.0".to_string()),
                );
            }
        }
    }

    versions
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
        let body = "github.com/akhilles/registry/harness v0.1.0 h1:mIGycQL5u80O2Jx/p3sUzJ566E74nA/Qof630p+ojSg=\ngithub.com/akhilles/registry/harness v0.1.0/pcb.toml h1:rxNJufX5oaagQE3qNtzJSZvLJcmtwRK3zJqTyuQfMmI=\n";

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
