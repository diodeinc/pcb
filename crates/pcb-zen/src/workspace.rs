//! Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use pcb_zen_core::config::PcbToml;
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;

// Re-export core types
pub use pcb_zen_core::workspace::{BoardInfo, DiscoveryError, MemberPackage, WorkspaceInfo};

use crate::canonical::{compute_content_hash_from_dir, compute_manifest_hash};
use crate::git;
use crate::tags;

// Re-export compute_tag_prefix from tags module for backwards compatibility
pub use crate::tags::compute_tag_prefix;

/// Why a package is dirty (has unpublished changes)
#[derive(Debug, Clone)]
pub enum DirtyReason {
    Unpublished,
    Uncommitted,
    LegacyTag,
    Modified {
        content_hash: String,
        manifest_hash: String,
    },
}

/// Transitive dependency closure for a package
#[derive(Debug, Clone, Default)]
pub struct PackageClosure {
    pub local_packages: HashSet<String>,
    pub remote_packages: HashSet<(String, String)>,
    pub assets: HashSet<(String, String)>,
}

/// Extension methods for WorkspaceInfo that require native features (git, filesystem)
pub trait WorkspaceInfoExt {
    fn reload(&mut self) -> Result<()>;
    fn dirty_packages(&self) -> BTreeMap<String, DirtyReason>;
    fn populate_dirty(&mut self);
    fn board_name_for_zen(&self, zen_path: &Path) -> Option<String>;
    fn board_info_for_zen(&self, zen_path: &Path) -> Option<BoardInfo>;
    fn package_url_for_zen(&self, zen_path: &Path) -> Option<String>;
}

impl WorkspaceInfoExt for WorkspaceInfo {
    fn reload(&mut self) -> Result<()> {
        let file_provider = DefaultFileProvider::new();
        let pcb_toml_path = self.root.join("pcb.toml");
        if file_provider.exists(&pcb_toml_path) {
            self.config = Some(PcbToml::from_file(&file_provider, &pcb_toml_path)?);
        }
        for pkg in self.packages.values_mut() {
            let pkg_toml_path = pkg.dir(&self.root).join("pcb.toml");
            pkg.config = PcbToml::from_file(&file_provider, &pkg_toml_path)?;
        }
        Ok(())
    }

    fn dirty_packages(&self) -> BTreeMap<String, DirtyReason> {
        let tags = git::list_all_tags_vec(&self.root);
        let tag_annotations = git::get_all_tag_annotations(&self.root);
        let workspace_path = self.path().map(|s| s.to_string());

        self.packages
            .par_iter()
            .filter_map(|(url, pkg)| {
                is_dirty(pkg, &self.root, &workspace_path, &tags, &tag_annotations)
                    .map(|reason| (url.clone(), reason))
            })
            .collect()
    }

    fn populate_dirty(&mut self) {
        let dirty_map = self.dirty_packages();
        for (url, pkg) in self.packages.iter_mut() {
            pkg.dirty = dirty_map.contains_key(url);
        }
    }

    fn board_name_for_zen(&self, zen_path: &Path) -> Option<String> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards()
            .into_values()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
            .map(|b| b.name)
    }

    fn board_info_for_zen(&self, zen_path: &Path) -> Option<BoardInfo> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards()
            .into_values()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
    }

    fn package_url_for_zen(&self, zen_path: &Path) -> Option<String> {
        let canon_zen = zen_path.canonicalize().ok()?;
        // Longest prefix match: find most specific package containing this path
        self.packages
            .iter()
            .filter(|(_, pkg)| canon_zen.starts_with(pkg.dir(&self.root)))
            .max_by_key(|(_, pkg)| pkg.rel_path.as_os_str().len())
            .map(|(url, _)| url.clone())
    }
}

/// Check if a package is dirty (native-only, requires git)
fn is_dirty(
    pkg: &MemberPackage,
    workspace_root: &Path,
    workspace_path: &Option<String>,
    tags: &[String],
    tag_annotations: &HashMap<String, String>,
) -> Option<DirtyReason> {
    let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), workspace_path.as_deref());

    let Some(tag_name) = tags::find_latest_tag(tags, &tag_prefix) else {
        return Some(DirtyReason::Unpublished);
    };

    if has_uncommitted_changes(workspace_root, &pkg.dir(workspace_root)) {
        return Some(DirtyReason::Uncommitted);
    }

    let Some(tagged) = tag_annotations
        .get(&tag_name)
        .and_then(|body| parse_hashes_from_tag_body(body))
    else {
        return Some(DirtyReason::LegacyTag);
    };

    let pkg_dir = pkg.dir(workspace_root);
    let current_content = compute_content_hash_from_dir(&pkg_dir).ok();
    let current_manifest = std::fs::read_to_string(pkg_dir.join("pcb.toml"))
        .ok()
        .map(|content| compute_manifest_hash(&content));

    if current_content != tagged.content_hash || current_manifest != tagged.manifest_hash {
        Some(DirtyReason::Modified {
            content_hash: current_content.unwrap_or_default(),
            manifest_hash: current_manifest.unwrap_or_default(),
        })
    } else {
        None
    }
}

fn has_uncommitted_changes(workspace_root: &Path, package_dir: &Path) -> bool {
    let rel_path = package_dir
        .strip_prefix(workspace_root)
        .unwrap_or(package_dir);
    git::has_uncommitted_changes_in_path(workspace_root, rel_path)
}

struct TagHashes {
    content_hash: Option<String>,
    manifest_hash: Option<String>,
}

fn parse_hashes_from_tag_body(body: &str) -> Option<TagHashes> {
    let mut content_hash = None;
    let mut manifest_hash = None;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(hash_start) = line.find(" h1:") {
            let hash = line[hash_start + 1..].to_string();
            let before_hash = &line[..hash_start];
            if before_hash.ends_with("/pcb.toml") {
                manifest_hash = Some(hash);
            } else {
                content_hash = Some(hash);
            }
        }
    }

    if content_hash.is_some() && manifest_hash.is_some() {
        Some(TagHashes {
            content_hash,
            manifest_hash,
        })
    } else {
        None
    }
}

/// Get workspace information with git version enrichment (native-only)
///
/// Calls core's get_workspace_info, adds path-patched forks as workspace members,
/// and enriches with git tag versions.
pub fn get_workspace_info<F: FileProvider>(
    file_provider: &F,
    start_path: &Path,
) -> Result<WorkspaceInfo> {
    let mut info = pcb_zen_core::workspace::get_workspace_info(file_provider, start_path)?;

    // Add path-patched forks as workspace members
    add_path_patched_forks(file_provider, &mut info)?;

    // Enrich with git tag versions (native-only feature)
    // For forked packages, version is already set from the fork path, so only
    // update if we find a tag (don't overwrite with None)
    let all_tags = git::list_all_tags_vec(&info.root);
    let workspace_path = info.path().map(|s| s.to_string());
    for pkg in info.packages.values_mut() {
        let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), workspace_path.as_deref());
        if let Some(v) = tags::find_latest_version(&all_tags, &tag_prefix) {
            pkg.version = Some(v.to_string());
        }
    }

    Ok(info)
}

/// Add path-patched forks as workspace members.
///
/// This allows forks to be treated like regular workspace packages for dependency
/// resolution, without requiring special handling in resolve.rs.
fn add_path_patched_forks<F: FileProvider>(
    file_provider: &F,
    info: &mut WorkspaceInfo,
) -> Result<()> {
    let Some(root_cfg) = info.config.as_ref() else {
        return Ok(());
    };

    for (url, patch) in &root_cfg.patch {
        let Some(rel_path) = patch.path.as_ref() else {
            continue;
        };

        let abs = info.root.join(rel_path);

        // Only support forks that live under the workspace root
        if !abs.starts_with(&info.root) {
            continue;
        }

        let pcb_toml_path = abs.join("pcb.toml");
        if !file_provider.exists(&pcb_toml_path) {
            continue;
        }

        // Skip if already a member
        if info.packages.contains_key(url) {
            continue;
        }

        // Load config and add as a member
        let pkg_cfg = PcbToml::from_file(file_provider, &pcb_toml_path)?;

        // Extract version from fork path if under fork/ directory
        // Fork paths are: fork/<url>/<version>/
        let fork_version = if rel_path.starts_with("fork/") {
            Path::new(rel_path)
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| Version::parse(s).ok())
                .map(|v| v.to_string())
        } else {
            None
        };

        info.packages.insert(
            url.clone(),
            MemberPackage {
                rel_path: PathBuf::from(rel_path),
                config: pkg_cfg,
                version: fork_version, // Use fork path version if available
                dirty: false,          // Will be populated by populate_dirty()
            },
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hashes_from_tag_body() {
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

        assert!(parse_hashes_from_tag_body("").is_none());
        assert!(parse_hashes_from_tag_body("github.com/foo v1.0.0 h1:abc123=\n").is_none());
        assert!(
            parse_hashes_from_tag_body("github.com/foo v1.0.0/pcb.toml h1:abc123=\n").is_none()
        );
    }
}
