//! Workspace introspection and member discovery
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::thread;
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

struct PackageTagInfo {
    tag: String,
    version: Version,
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
        let latest_tags = latest_package_tags(self, &all_tags);
        let latest_tag_names = latest_tag_names(&latest_tags);
        let tag_metadata = git::get_tag_metadata(&self.root, &latest_tag_names);
        let status_paths = git::status_paths_in_repo(&self.root);
        discover_dirty_packages(self, &latest_tags, &tag_metadata, status_paths)
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
    latest_tags: &BTreeMap<String, PackageTagInfo>,
    tag_metadata: &HashMap<String, git::TagMetadata>,
    status_paths: Vec<PathBuf>,
) -> BTreeMap<String, DirtyReason> {
    let mut dirty = BTreeMap::new();
    for url in workspace.packages.keys() {
        if !latest_tags.contains_key(url) {
            dirty.insert(url.clone(), DirtyReason::Unpublished);
        }
    }

    if let Some(base) = latest_package_tagged_ref(&workspace.root, latest_tags, tag_metadata) {
        for path in git::changed_paths_since_in_repo(&workspace.root, &base) {
            if let Some(url) = package_url_for_path(workspace, &path) {
                dirty.insert(url, DirtyReason::Modified);
            }
        }
    }

    for path in status_paths {
        if let Some(url) = package_url_for_path(workspace, &path) {
            dirty.insert(url, DirtyReason::Uncommitted);
        }
    }

    dirty
}

fn latest_tag_names(latest_tags: &BTreeMap<String, PackageTagInfo>) -> Vec<String> {
    latest_tags.values().map(|info| info.tag.clone()).collect()
}

fn latest_package_tags(
    workspace: &WorkspaceInfo,
    all_tags: &[String],
) -> BTreeMap<String, PackageTagInfo> {
    let workspace_path = workspace.path();
    let package_urls_by_tag_path: BTreeMap<_, _> = workspace
        .packages
        .iter()
        .map(|(url, pkg)| {
            let tag_path = match (workspace_path, pkg.rel_path.as_os_str().is_empty()) {
                (Some(path), true) => path.to_string(),
                (Some(path), false) => format!("{}/{}", path, pkg.rel_path.to_string_lossy()),
                (None, _) => pkg.rel_path.to_string_lossy().into_owned(),
            };
            (tag_path, url.clone())
        })
        .collect();

    let mut latest = BTreeMap::new();
    for tag in all_tags {
        let parsed = if let Some((tag_path, version)) = tags::parse_tag(tag) {
            Some((tag_path, version))
        } else {
            tags::parse_root_tag(tag).map(|version| (String::new(), version))
        };
        let Some((tag_path, version)) = parsed else {
            continue;
        };
        let Some(url) = package_urls_by_tag_path.get(&tag_path) else {
            continue;
        };
        let entry = latest.entry(url.clone()).or_insert_with(|| PackageTagInfo {
            tag: tag.clone(),
            version: version.clone(),
        });
        if version > entry.version {
            *entry = PackageTagInfo {
                tag: tag.clone(),
                version,
            };
        }
    }
    latest
}

fn latest_package_tagged_ref(
    repo_root: &Path,
    latest_tags: &BTreeMap<String, PackageTagInfo>,
    tag_metadata: &HashMap<String, git::TagMetadata>,
) -> Option<String> {
    if latest_tags.is_empty() {
        return None;
    }

    if let Some(head) = git::rev_parse_head(repo_root)
        && let Some(tag) = latest_tags.values().find_map(|info| {
            tag_metadata
                .get(&info.tag)
                .filter(|metadata| metadata.target == head)
                .map(|_| info.tag.clone())
        })
    {
        return Some(tag);
    }

    let package_tags: BTreeSet<_> = latest_tags.values().map(|info| info.tag.clone()).collect();
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
    let latest_tags = latest_package_tags(&info, &all_tags);

    let latest_tag_names = latest_tag_names(&latest_tags);
    let (tag_metadata, status_paths) = thread::scope(|scope| {
        let tag_metadata = scope.spawn(|| git::get_tag_metadata(&info.root, &latest_tag_names));
        let status_paths = scope.spawn(|| git::status_paths_in_repo(&info.root));
        (
            tag_metadata.join().unwrap_or_default(),
            status_paths.join().unwrap_or_default(),
        )
    });
    let dirty_map = discover_dirty_packages(&info, &latest_tags, &tag_metadata, status_paths);

    if enrich_versions {
        let _span = info_span!("enrich_workspace_versions").entered();
        for (url, pkg) in info.packages.iter_mut() {
            if let Some(tag_info) = latest_tags.get(url) {
                pkg.version = Some(tag_info.version.to_string());
                pkg.published_at = tag_metadata
                    .get(&tag_info.tag)
                    .map(|metadata| metadata.timestamp.clone());
            }
        }
    }

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
