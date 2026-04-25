//! Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

use pcb_zen_core::config::PcbToml;
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;

// Re-export core types
pub use pcb_zen_core::workspace::{
    BoardInfo, DiscoveryError, MemberPackage, SymbolFileInfo, WorkspaceInfo,
};

use crate::git;
use crate::tags;

/// Why a package is dirty (has unpublished changes)
#[derive(Debug, Clone)]
pub enum DirtyReason {
    Unpublished,
    Uncommitted,
    Modified,
}

/// Extension methods for WorkspaceInfo that require native features (git, filesystem)
pub trait WorkspaceInfoExt {
    fn reload(&mut self) -> Result<()>;
    fn dirty_packages(&self) -> BTreeMap<String, DirtyReason>;
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
        let all_tags = git::list_tags_merged_into(&self.root, "HEAD");
        discover_dirty_packages(self, &all_tags)
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

fn discover_dirty_packages(
    workspace: &WorkspaceInfo,
    all_tags: &[String],
) -> BTreeMap<String, DirtyReason> {
    let workspace_path = workspace.path().map(|s| s.to_string());
    let latest_tags = latest_package_tags(workspace, all_tags, workspace_path.as_deref());
    let package_tags: BTreeSet<_> = latest_tags.values().cloned().collect();
    let mut dirty = BTreeMap::new();
    for url in workspace.packages.keys() {
        if !latest_tags.contains_key(url) {
            dirty.insert(url.clone(), DirtyReason::Unpublished);
        }
    }

    if let Some(base) = latest_package_tagged_ref(&workspace.root, &package_tags) {
        for path in git::changed_paths_since_in_repo(&workspace.root, &base) {
            if let Some(url) = package_url_for_path(workspace, &path) {
                dirty.insert(url, DirtyReason::Modified);
            }
        }
    }

    for path in git::status_paths_in_repo(&workspace.root) {
        if let Some(url) = package_url_for_path(workspace, &path) {
            dirty.insert(url, DirtyReason::Uncommitted);
        }
    }

    dirty
}

fn latest_package_tags(
    workspace: &WorkspaceInfo,
    all_tags: &[String],
    workspace_path: Option<&str>,
) -> BTreeMap<String, String> {
    workspace
        .packages
        .iter()
        .filter_map(|(url, pkg)| {
            let tag_prefix = tags::compute_tag_prefix(Some(&pkg.rel_path), workspace_path);
            tags::find_latest_tag(all_tags, &tag_prefix).map(|tag| (url.clone(), tag))
        })
        .collect()
}

fn latest_package_tagged_ref(repo_root: &Path, package_tags: &BTreeSet<String>) -> Option<String> {
    if package_tags.is_empty() {
        return None;
    }

    if let Some(tag) = git::describe_tags(repo_root, "HEAD", None)
        && package_tags.contains(&tag)
    {
        return Some(tag);
    }

    for line in git::decorated_commits(repo_root) {
        let (commit, decorations) = line.split_once('\0')?;
        if decorations.split(',').any(|decoration| {
            decoration
                .trim()
                .strip_prefix("tag: ")
                .is_some_and(|tag| package_tags.contains(tag))
        }) {
            return Some(commit.to_string());
        }
    }

    None
}

fn package_url_for_path(workspace: &WorkspaceInfo, path: &Path) -> Option<String> {
    workspace
        .packages
        .iter()
        .filter(|(_, pkg)| {
            !pkg.rel_path.as_os_str().is_empty()
                && (path == pkg.rel_path || path.starts_with(&pkg.rel_path))
        })
        .max_by_key(|(_, pkg)| pkg.rel_path.as_os_str().len())
        .map(|(url, _)| url.clone())
        .or_else(|| {
            workspace
                .packages
                .iter()
                .find(|(_, pkg)| pkg.rel_path.as_os_str().is_empty())
                .map(|(url, _)| url.clone())
        })
}

/// Get workspace information with optional git version enrichment (native-only).
///
/// Calls core's get_workspace_info, adds path-patched forks as workspace members,
/// and optionally enriches with git tag versions.
#[instrument(name = "get_workspace_info", skip_all)]
pub fn get_workspace_info<F: FileProvider>(
    file_provider: &F,
    start_path: &Path,
    enrich_versions: bool,
) -> Result<WorkspaceInfo> {
    let mut info = {
        let _span = info_span!("discover_workspace_members").entered();
        pcb_zen_core::workspace::get_workspace_info(file_provider, start_path)?
    };

    // Add path-patched forks as workspace members
    {
        let _span = info_span!("add_path_patched_forks").entered();
        add_path_patched_forks(file_provider, &mut info)?;
    }

    // Enrich with git tag versions (native-only feature)
    // For forked packages, version is already set from the fork path, so only
    // update if we find a tag (don't overwrite with None)
    let all_tags = git::list_tags_merged_into(&info.root, "HEAD");
    if enrich_versions {
        let _span = info_span!("enrich_workspace_versions").entered();
        let tag_timestamps = git::get_all_tag_timestamps(&info.root);
        let workspace_path = info.path().map(|s| s.to_string());
        for pkg in info.packages.values_mut() {
            let tag_prefix =
                tags::compute_tag_prefix(Some(&pkg.rel_path), workspace_path.as_deref());
            if let Some(tag_name) = tags::find_latest_tag(&all_tags, &tag_prefix) {
                let version_str = tag_name
                    .strip_prefix(&tag_prefix)
                    .expect("find_latest_tag must return a tag with the requested prefix");
                pkg.version = Some(version_str.to_string());
                pkg.published_at = tag_timestamps.get(&tag_name).cloned();
            }
        }
    }

    let dirty_map = discover_dirty_packages(&info, &all_tags);
    for (url, pkg) in info.packages.iter_mut() {
        pkg.dirty = dirty_map.contains_key(url);
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
        if pcb_zen_core::is_stdlib_module_path(url) {
            continue;
        }
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
                published_at: None,
                preferred: false,
                dirty: false,
                entrypoints: Vec::new(),
                symbol_files: Vec::new(),
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
