//! Workspace introspection and package discovery.
//!
//! Provides high-level APIs for querying workspace information without
//! running full dependency resolution. Used by `pcb info` and other commands
//! that need workspace metadata.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use tracing::{info_span, instrument};

use pcb_zen_core::config::PcbToml;
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;

// Re-export core types
pub use pcb_zen_core::workspace::{
    BoardInfo, DiscoveryError, SymbolFileInfo, WorkspaceInfo, WorkspacePackage,
};

use crate::git;
use crate::tags;

struct PackageTagInfo {
    tag: git::Tag,
    version: Version,
}

/// Extension methods for WorkspaceInfo that require native features (git, filesystem)
pub trait WorkspaceInfoExt {
    fn reload(&mut self) -> Result<()>;
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
        let file_provider = DefaultFileProvider::new();
        self.package_url_for_path(&file_provider, zen_path)
            .map(str::to_string)
    }
}

/// Packages with no published tag, or with a path changed since the last
/// publish, committed (`changed_paths`) or not (`status_paths`).
fn discover_dirty_packages(
    workspace: &WorkspaceInfo,
    latest_tags: &HashMap<String, PackageTagInfo>,
    changed_paths: Vec<PathBuf>,
    status_paths: Vec<PathBuf>,
    workspace_subpath: Option<&Path>,
) -> HashSet<String> {
    let unpublished = workspace
        .packages
        .keys()
        .filter(|url| !latest_tags.contains_key(*url))
        .cloned();
    let changed = changed_paths
        .iter()
        .chain(&status_paths)
        .filter_map(|path| package_url_for_path(workspace, path, workspace_subpath));
    unpublished.chain(changed).collect()
}

fn latest_package_tags(
    workspace: &WorkspaceInfo,
    all_tags: &[git::Tag],
) -> HashMap<String, PackageTagInfo> {
    let workspace_path = workspace.path();
    let package_urls_by_tag_path: HashMap<_, _> = workspace
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

    let mut latest = HashMap::new();
    for tag in all_tags {
        let Some((tag_path, version)) = parse_package_tag(&tag.name) else {
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

fn parse_package_tag(tag: &str) -> Option<(String, Version)> {
    tags::parse_tag(tag)
        .or_else(|| tags::parse_root_tag(tag).map(|version| (String::new(), version)))
}

fn package_url_for_path(
    workspace: &WorkspaceInfo,
    path: &Path,
    workspace_subpath: Option<&Path>,
) -> Option<String> {
    let path = match workspace_subpath {
        Some(prefix) => path.strip_prefix(prefix).ok()?,
        None => path,
    };

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

/// Get workspace information (native-only).
///
/// Calls core's get_workspace_info and adds path-patched forks as workspace
/// packages. Package `version`/`published_at`/`dirty` are left unset — they
/// come from git and only matter for presentation; commands that display them
/// (e.g. `pcb info`, `pcb publish`) call [`enrich_git_metadata`].
#[instrument(name = "get_workspace_info", skip_all)]
pub fn get_workspace_info<F: FileProvider>(
    file_provider: &F,
    start_path: &Path,
) -> Result<WorkspaceInfo> {
    let mut info = {
        let _span = info_span!("discover_workspace_packages").entered();
        pcb_zen_core::workspace::get_workspace_info(file_provider, start_path)?
    };

    // Add path-patched forks as workspace packages.
    {
        let _span = info_span!("add_path_patched_forks").entered();
        add_path_patched_forks(file_provider, &mut info)?;
    }

    Ok(info)
}

/// Git reads that need only the workspace root. Each starts on its own thread,
/// so they overlap one another and whatever the caller does before applying
/// them, such as discovering the workspace.
pub struct GitReads {
    root: PathBuf,
    tags: JoinHandle<Vec<git::Tag>>,
    history: JoinHandle<Vec<String>>,
    status_paths: JoinHandle<Vec<PathBuf>>,
    workspace_subpath: JoinHandle<Option<PathBuf>>,
}

impl GitReads {
    pub fn start(workspace_root: &Path) -> Self {
        fn start<T: Send + 'static>(
            root: &Path,
            read: impl FnOnce(&Path) -> T + Send + 'static,
        ) -> JoinHandle<T> {
            let root = root.to_path_buf();
            thread::spawn(move || read(&root))
        }

        Self {
            root: workspace_root.to_path_buf(),
            tags: start(workspace_root, git::list_peeled_tags),
            history: start(workspace_root, |root| git::rev_list(root, "HEAD")),
            status_paths: start(workspace_root, git::status_paths_in_repo),
            workspace_subpath: start(workspace_root, |root| {
                git::get_repo_subpath(root).ok().flatten()
            }),
        }
    }
}

/// Populate package `version` from the newest package tags merged into HEAD.
/// Cheap subset of [`enrich_git_metadata`] for commands that only need
/// versions (e.g. the workspace pins `pcb sync` writes back).
#[instrument(name = "enrich_tag_versions", skip_all)]
pub fn enrich_tag_versions(info: &mut WorkspaceInfo) {
    let (tags, history) = thread::scope(|scope| {
        let tags = scope.spawn(|| git::list_peeled_tags(&info.root));
        let history = git::rev_list(&info.root, "HEAD");
        (tags.join().unwrap_or_default(), history)
    });
    apply_tag_versions(info, tags, &history);
}

/// Set each package's version from its newest tag merged into HEAD, given
/// every tag and HEAD's history. Returns those newest tags.
///
/// For forked packages, version is already set from the fork path, so only
/// update if we find a tag (don't overwrite with None).
fn apply_tag_versions(
    info: &mut WorkspaceInfo,
    mut tags: Vec<git::Tag>,
    history: &[String],
) -> HashMap<String, PackageTagInfo> {
    let merged: HashSet<&str> = history.iter().map(String::as_str).collect();
    tags.retain(|tag| merged.contains(tag.commit.as_str()));

    let latest_tags = latest_package_tags(info, &tags);
    for (url, pkg) in info.packages.iter_mut() {
        if let Some(tag_info) = latest_tags.get(url) {
            pkg.version = Some(tag_info.version.to_string());
        }
    }
    latest_tags
}

/// Populate package `version`, `published_at`, and `dirty` from git tags and
/// working-tree status. Runs several git commands, so it is opt-in for the
/// commands that actually display this metadata.
pub fn enrich_git_metadata(info: &mut WorkspaceInfo) {
    enrich_git_metadata_from(info, GitReads::start(&info.root));
}

/// [`enrich_git_metadata`] from reads the caller started earlier.
#[instrument(name = "enrich_git_metadata", skip_all)]
pub fn enrich_git_metadata_from(info: &mut WorkspaceInfo, reads: GitReads) {
    let root = reads.root.as_path();
    // HEAD's history, newest commit first.
    let history = reads.history.join().unwrap_or_default();
    let latest_tags = apply_tag_versions(info, reads.tags.join().unwrap_or_default(), &history);

    // Changes are measured from the last publish: the newest commit in HEAD's
    // history that one of the latest package tags names.
    let published: HashSet<&str> = latest_tags
        .values()
        .map(|info| info.tag.commit.as_str())
        .collect();
    let base = history
        .iter()
        .find(|commit| published.contains(commit.as_str()));

    let tags: Vec<_> = latest_tags.values().map(|info| info.tag.clone()).collect();
    let (tag_metadata, changed_paths) = thread::scope(|scope| {
        let tag_metadata = scope.spawn(|| git::get_tag_metadata(root, &tags));
        let changed_paths = base
            .map(|base| git::changed_paths_since_in_repo(root, base))
            .unwrap_or_default();
        (tag_metadata.join().unwrap_or_default(), changed_paths)
    });

    let dirty = discover_dirty_packages(
        info,
        &latest_tags,
        changed_paths,
        reads.status_paths.join().unwrap_or_default(),
        reads
            .workspace_subpath
            .join()
            .unwrap_or_default()
            .as_deref(),
    );

    for (url, pkg) in info.packages.iter_mut() {
        if let Some(tag_info) = latest_tags.get(url) {
            pkg.published_at = tag_metadata
                .get(&tag_info.tag.name)
                .map(|metadata| metadata.timestamp.clone());
        }
        pkg.dirty = dirty.contains(url);
    }
}

/// Add path-patched forks as workspace packages.
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

        // Skip if already discovered.
        if info.packages.contains_key(url) {
            continue;
        }

        // Load config and add as a workspace package.
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
            WorkspacePackage {
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
