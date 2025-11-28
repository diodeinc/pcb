//! Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{find_workspace_root, Lockfile, PcbToml, WorkspaceConfig};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use rayon::prelude::*;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::canonical::{compute_content_hash_from_dir, compute_manifest_hash};
use crate::git;

/// Why a package is dirty (has unpublished changes)
#[derive(Debug, Clone)]
pub enum DirtyReason {
    /// Package has never been published
    Unpublished,
    /// Package has uncommitted changes in the working tree
    Uncommitted,
    /// Published tag has no hash annotations (legacy tag format)
    LegacyTag,
    /// Content or manifest hashes differ from published version
    Modified {
        content_hash: String,
        manifest_hash: String,
    },
}

/// A discovered member package in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberPackage {
    /// Package directory (absolute path)
    pub dir: PathBuf,
    /// Package directory relative to workspace root
    pub rel_path: PathBuf,
    /// Parsed pcb.toml config
    pub config: PcbToml,
    /// Latest published version from git tags (None if unpublished)
    pub version: Option<String>,
}

impl MemberPackage {
    /// Get dependency URLs from config
    pub fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.config.dependencies.keys()
    }

    /// Get asset count from config
    pub fn asset_count(&self) -> usize {
        self.config.assets.len()
    }

    /// Check if this package has unpublished changes.
    /// Returns Some(DirtyReason) if dirty, None if clean.
    ///
    /// Uses fast paths to avoid expensive hash computation when possible:
    /// - Unpublished: Some(Unpublished)
    /// - Uncommitted changes: Some(Uncommitted)
    /// - Published but no hashes in tag: Some(LegacyTag)
    /// - Published with hashes that differ: Some(Modified)
    /// - Clean: None
    pub fn is_dirty(
        &self,
        workspace_root: &Path,
        workspace_path: &Option<String>,
        tags: &[String],
        tag_annotations: &HashMap<String, String>,
    ) -> Option<DirtyReason> {
        // Compute tag prefix based on package type
        let tag_prefix = if let Some(b) = &self.config.board {
            format!("{}/v", b.name)
        } else {
            compute_tag_prefix(Some(&self.rel_path), workspace_path)
        };

        // Find the latest matching tag
        let Some(tag_name) = find_latest_tag(tags, &tag_prefix) else {
            return Some(DirtyReason::Unpublished);
        };

        // Check for uncommitted changes first (cheaper than hashing)
        if has_uncommitted_changes(workspace_root, &self.dir) {
            return Some(DirtyReason::Uncommitted);
        }

        // Look up hashes from pre-fetched tag annotations
        let Some(tagged) = tag_annotations
            .get(&tag_name)
            .and_then(|body| parse_hashes_from_tag_body(body))
        else {
            return Some(DirtyReason::LegacyTag);
        };

        // Compute current hashes and compare
        let current_content = compute_content_hash_from_dir(&self.dir).ok();
        let current_manifest = std::fs::read_to_string(self.dir.join("pcb.toml"))
            .ok()
            .map(|content| compute_manifest_hash(&content));

        if current_content != tagged.content_hash || current_manifest != tagged.manifest_hash {
            // Return the current hashes so they can be reused
            Some(DirtyReason::Modified {
                content_hash: current_content.unwrap_or_default(),
                manifest_hash: current_manifest.unwrap_or_default(),
            })
        } else {
            None // Clean
        }
    }
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

/// Comprehensive workspace information - the single source of truth
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Workspace root directory
    pub root: PathBuf,

    /// Root pcb.toml config (full)
    pub config: PcbToml,

    /// Discovered member packages keyed by URL (includes root if applicable)
    pub packages: BTreeMap<String, MemberPackage>,

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

    /// Get repository URL from workspace config
    pub fn repository(&self) -> Option<&str> {
        self.config
            .workspace
            .as_ref()
            .and_then(|w| w.repository.as_deref())
    }

    /// Get optional subpath within repository
    pub fn path(&self) -> Option<&str> {
        self.config
            .workspace
            .as_ref()
            .and_then(|w| w.path.as_deref())
    }

    /// Get minimum pcb toolchain version
    pub fn pcb_version(&self) -> Option<&str> {
        self.config
            .workspace
            .as_ref()
            .and_then(|w| w.pcb_version.as_deref())
    }

    /// Get member glob patterns
    pub fn member_patterns(&self) -> Vec<String> {
        self.config
            .workspace
            .as_ref()
            .map(|w| w.members.clone())
            .unwrap_or_default()
    }

    /// Get all packages as a vector (for iteration)
    pub fn all_packages(&self) -> Vec<&MemberPackage> {
        self.packages.values().collect()
    }

    /// Get publishable packages (excludes packages with board sections)
    pub fn publishable_packages(&self) -> Vec<&MemberPackage> {
        self.packages
            .values()
            .filter(|p| p.config.board.is_none())
            .collect()
    }

    /// Get total package count
    pub fn package_count(&self) -> usize {
        self.packages.len()
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

    /// Get boards derived from packages with [board] sections
    pub fn boards(&self) -> BTreeMap<String, BoardInfo> {
        self.packages
            .values()
            .filter_map(|pkg| {
                let b = pkg.config.board.as_ref()?;
                let zen = b.path.clone().or_else(|| find_single_zen_file(&pkg.dir))?;
                Some((
                    b.name.clone(),
                    BoardInfo {
                        name: b.name.clone(),
                        zen_path: pkg.rel_path.join(&zen).to_string_lossy().to_string(),
                        description: b.description.clone(),
                    },
                ))
            })
            .collect()
    }

    /// Given an absolute .zen path, return the board name
    pub fn board_name_for_zen(&self, zen_path: &Path) -> Option<String> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards()
            .into_values()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
            .map(|b| b.name)
    }

    /// Given an absolute .zen path, return the full BoardInfo
    pub fn board_info_for_zen(&self, zen_path: &Path) -> Option<BoardInfo> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards()
            .into_values()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
    }

    /// Find a board by name, returning an error with available boards if not found
    pub fn find_board_by_name(&self, board_name: &str) -> Result<BoardInfo> {
        let boards = self.boards();
        boards.get(board_name).cloned().ok_or_else(|| {
            let available: Vec<_> = boards.keys().map(|k| k.as_str()).collect();
            anyhow::anyhow!(
                "Board '{board_name}' not found. Available: [{}]",
                available.join(", ")
            )
        })
    }

    /// Get all dirty packages with their reasons.
    /// Computes in parallel for performance.
    pub fn dirty_packages(&self) -> BTreeMap<String, DirtyReason> {
        let tags = git::list_all_tags_vec(&self.root);
        let tag_annotations = git::get_all_tag_annotations(&self.root);
        let workspace_path = self.path().map(|s| s.to_string());

        self.packages
            .par_iter()
            .filter_map(|(url, pkg)| {
                pkg.is_dirty(&self.root, &workspace_path, &tags, &tag_annotations)
                    .map(|reason| (url.clone(), reason))
            })
            .collect()
    }
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

/// Parsed hashes from tag annotation
struct TagHashes {
    content_hash: Option<String>,
    manifest_hash: Option<String>,
}

/// Parse hashes from tag annotation body (pcb.sum style format)
///
/// Format: Each line is "module_path version hash" where:
/// - Content hash line: "module_path v0.1.0 h1:base64"
/// - Manifest hash line: "module_path v0.1.0/pcb.toml h1:base64"
fn parse_hashes_from_tag_body(body: &str) -> Option<TagHashes> {
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
        Some(TagHashes {
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
/// Discovers packages, computes dirty status and hashes, derives boards, and loads lockfile.
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

    // Compute base URL from workspace config
    let base_url = match (&workspace_config.repository, &workspace_config.path) {
        (Some(repo), Some(p)) => Some(format!("{}/{}", repo, p)),
        (Some(repo), None) => Some(repo.clone()),
        _ => None,
    };

    // Fetch git tags once for version lookup
    let tags = git::list_all_tags_vec(&workspace_root);

    // First pass: discover all package directories
    let patterns = &workspace_config.members;
    let mut package_dirs: Vec<(PathBuf, String, PcbToml)> = Vec::new(); // (dir, url, config)

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
            if rel_path.as_os_str().is_empty() {
                continue; // Skip root, handled separately
            }
            if !glob_set.is_match(rel_path) {
                continue;
            }
            let pkg_config = PcbToml::from_file(file_provider, &pkg_toml_path)?;
            let rel_str = rel_path.to_string_lossy();
            let url = base_url
                .as_ref()
                .map(|base| format!("{}/{}", base, rel_str))
                .unwrap_or_else(|| rel_str.to_string());

            package_dirs.push((path.to_path_buf(), url, pkg_config));
        }
    }

    // Add root package if it has repository URL and (has deps/assets OR no members found)
    if let Some(url) = &base_url {
        let has_deps = !config.dependencies.is_empty() || !config.assets.is_empty();
        let no_other_packages = package_dirs.is_empty();

        if has_deps || no_other_packages {
            package_dirs.push((workspace_root.clone(), url.clone(), config.clone()));
        }
    }

    // Build packages (no longer computing dirty status eagerly)
    let packages: BTreeMap<String, MemberPackage> = package_dirs
        .iter()
        .map(|(dir, url, pkg_config)| {
            let rel_path = dir.strip_prefix(&workspace_root).unwrap_or(Path::new(""));

            // For board packages, use board-specific tag prefix for version
            let version = if let Some(b) = &pkg_config.board {
                let board_tag_prefix = format!("{}/v", b.name);
                find_latest_version(&tags, &board_tag_prefix)
            } else {
                let pkg_tag_prefix = compute_tag_prefix(Some(rel_path), &workspace_config.path);
                find_latest_version(&tags, &pkg_tag_prefix)
            };

            (
                url.clone(),
                MemberPackage {
                    dir: dir.clone(),
                    rel_path: rel_path.to_path_buf(),
                    config: pkg_config.clone(),
                    version,
                },
            )
        })
        .collect();

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
        lockfile,
        errors: Vec::new(),
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
