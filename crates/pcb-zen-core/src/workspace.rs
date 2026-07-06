//! Workspace discovery and package metadata types.
//!
//! Provides cross-platform workspace discovery using FileProvider abstraction.
//! Native code can enrich with git tag versions after discovery.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::FileProvider;
use crate::config::{
    PcbToml, WorkspaceConfig, find_workspace_root, parse_pcb_version, pcb_version_from_cargo,
    pcb_version_is_older,
};

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

pub(crate) const LOCAL_WORKSPACE_ROOT_URL: &str = "workspace";
pub const WORKSPACE_DISCOVERY_MAX_DEPTH: usize = 8;

pub const WORKSPACE_DISCOVERY_EXCLUDE_DIRS: &[&str] =
    &[".git", ".pcb", "fork", "node_modules", "target", "vendor"];

pub fn package_url_covers(prefix: &str, url: &str) -> bool {
    url == prefix
        || url
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn build_workspace_base_url(repository: Option<&str>, path: Option<&str>) -> Option<String> {
    match (repository, path) {
        (Some(repo), Some(path)) => Some(format!("{repo}/{path}")),
        (Some(repo), None) => Some(repo.to_string()),
        _ => None,
    }
}

fn validate_workspace_pcb_version(config: &PcbToml, source: &Path) -> anyhow::Result<()> {
    let current = pcb_version_from_cargo();
    let Some(required) = config
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.pcb_version.as_deref())
    else {
        return Ok(());
    };

    if pcb_version_is_older(&current, required) == Some(true) {
        anyhow::bail!(
            "Workspace requires pcb-version = \"{}\" but the current pcb is {}\n  \
             Upgrade pcb before building or updating this workspace.\n  \
             Manifest: {}",
            required,
            current,
            source.display()
        );
    }

    Ok(())
}

/// A discovered package in the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacePackage {
    /// Package directory relative to workspace root
    pub rel_path: PathBuf,
    /// Parsed pcb.toml config
    #[serde(default, skip_serializing_if = "is_default")]
    pub config: PcbToml,
    /// Latest published version from git tags (None if unpublished or not computed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Publish timestamp for the latest published version (ISO 8601, if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    /// Whether this package is listed in `[workspace].preferred`
    #[serde(default)]
    pub preferred: bool,
    /// Whether this package has unpublished changes (computed on demand)
    #[serde(default, skip_serializing_if = "is_default")]
    pub dirty: bool,
    /// Top-level Zener files in this package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoints: Vec<PathBuf>,
    /// Top-level KiCad symbol libraries in this package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_files: Vec<SymbolFileInfo>,
}

impl WorkspacePackage {
    /// Get absolute package directory
    pub fn dir(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(&self.rel_path)
    }

    /// Get dependency URLs from config
    pub fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.config.dependencies.direct.keys()
    }
}

/// Symbol names discovered from a top-level KiCad symbol library file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolFileInfo {
    /// Package-relative path to the `.kicad_sym` file.
    pub path: PathBuf,
    /// Top-level symbols defined in the file.
    pub symbols: Vec<String>,
}

/// Board discovery information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardInfo {
    /// Board name
    pub name: String,
    /// Path to the .zen file (relative to workspace root)
    pub zen_path: String,
    /// Board description
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

/// Comprehensive workspace information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Workspace root directory
    pub root: PathBuf,
    /// Global package cache directory (e.g. `~/.pcb/cache`).
    /// Set by native workspace discovery; empty on WASM.
    #[serde(skip)]
    pub cache_dir: PathBuf,
    /// Root pcb.toml config (if present)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<PcbToml>,
    /// Discovered packages keyed by URL
    pub packages: BTreeMap<String, WorkspacePackage>,
    /// Discovery errors
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<DiscoveryError>,
}

impl WorkspaceInfo {
    /// Workspace-local cache path (typically a symlink to `~/.pcb/cache`).
    pub fn workspace_cache_dir(&self) -> PathBuf {
        self.root.join(".pcb/cache")
    }

    /// Optional stdlib patch path from `[patch]` at workspace root.
    ///
    /// Uses only the canonical virtual key (`"stdlib"`).
    pub fn stdlib_patch_path(&self) -> Option<PathBuf> {
        let root_cfg = self.config.as_ref()?;
        root_cfg
            .patch
            .get(crate::STDLIB_MODULE_PATH)
            .and_then(|patch| patch.path.as_ref())
            .map(|path| self.root.join(path))
    }

    /// Workspace-local toolchain stdlib materialization path.
    pub fn workspace_stdlib_dir(&self) -> PathBuf {
        self.stdlib_patch_path()
            .unwrap_or_else(|| crate::workspace_stdlib_root(&self.root))
    }

    /// Get workspace config section (with defaults if not present)
    pub fn workspace_config(&self) -> WorkspaceConfig {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.clone())
            .unwrap_or_default()
    }

    /// Iterate all manifest configs in the workspace (root first, then packages).
    pub fn manifests(&self) -> impl Iterator<Item = &PcbToml> {
        self.config
            .iter()
            .chain(self.packages.values().map(|pkg| &pkg.config))
    }

    /// Get repository URL from workspace config
    pub fn repository(&self) -> Option<&str> {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .and_then(|w| w.repository.as_deref())
    }

    /// The workspace package namespace derived from `[workspace].repository` and
    /// optional `[workspace].path`.
    ///
    /// Package URLs are constructed under this base URL during workspace
    /// discovery. When absent, the workspace has no explicit package namespace.
    pub fn workspace_base_url(&self) -> Option<String> {
        build_workspace_base_url(self.repository(), self.path())
    }

    /// Whether `url` belongs to this workspace's declared package namespace.
    pub fn is_workspace_namespace_url(&self, url: &str) -> bool {
        self.workspace_base_url()
            .as_deref()
            .is_some_and(|base| package_url_covers(base, url))
    }

    /// Return the most specific workspace package URL that contains `url`.
    pub fn package_url_for_url(&self, url: &str) -> Option<&str> {
        self.packages
            .keys()
            .filter(|package_url| package_url_covers(package_url, url))
            .max_by_key(|package_url| package_url.len())
            .map(String::as_str)
    }

    /// Return the most specific workspace package URL that owns `path`.
    pub fn package_url_for_path(
        &self,
        file_provider: &dyn FileProvider,
        path: &Path,
    ) -> Option<&str> {
        let canonical_workspace_root = file_provider
            .canonicalize(&self.root)
            .unwrap_or_else(|_| self.root.clone());
        let canonical_path = file_provider
            .canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf());
        let workspace_relative = canonical_path
            .strip_prefix(&canonical_workspace_root)
            .ok()?;

        self.packages
            .iter()
            .filter(|(_, pkg)| workspace_relative.starts_with(&pkg.rel_path))
            .max_by_key(|(_, pkg)| pkg.rel_path.as_os_str().len())
            .map(|(url, _)| url.as_str())
    }

    /// Get optional subpath within repository
    pub fn path(&self) -> Option<&str> {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .and_then(|w| w.path.as_deref())
    }

    /// Get minimum pcb toolchain version
    pub fn pcb_version(&self) -> Option<&str> {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .and_then(|w| w.pcb_version.as_deref())
    }

    pub fn requires_mvs_v2(&self) -> bool {
        self.pcb_version()
            .and_then(parse_pcb_version)
            .is_some_and(|version| version >= (0, 4))
    }

    /// Get all packages as a vector
    pub fn all_packages(&self) -> Vec<&WorkspacePackage> {
        self.packages.values().collect()
    }

    /// Get publishable packages (excludes packages with board sections)
    pub fn publishable_packages(&self) -> Vec<&WorkspacePackage> {
        self.packages
            .values()
            .filter(|p| p.config.board.is_none())
            .collect()
    }

    /// Get total package count
    pub fn package_count(&self) -> usize {
        self.packages.len()
    }

    /// Get boards derived from packages with [board] sections
    pub fn boards(&self) -> BTreeMap<String, BoardInfo> {
        self.packages
            .values()
            .filter_map(|pkg| {
                let b = pkg.config.board.as_ref()?;
                // board.path is populated by get_workspace_info()
                let zen = b.path.as_ref()?;
                let rel_zen = pkg.rel_path.join(zen);
                Some((
                    b.name.clone(),
                    BoardInfo {
                        name: b.name.clone(),
                        zen_path: rel_zen.to_string_lossy().into_owned(),
                        description: b.description.clone(),
                    },
                ))
            })
            .collect()
    }

    /// Find a board by name, returning an error with available boards if not found
    pub fn find_board_by_name(&self, board_name: &str) -> anyhow::Result<BoardInfo> {
        let boards = self.boards();
        boards.get(board_name).cloned().ok_or_else(|| {
            let available: Vec<_> = boards.keys().map(|k| k.as_str()).collect();
            anyhow::anyhow!(
                "Board '{}' not found. Available: [{}]",
                board_name,
                available.join(", ")
            )
        })
    }
}

/// Find single .zen file in a directory using a FileProvider
fn find_single_zen_file<F: FileProvider>(file_provider: &F, dir: &Path) -> Option<String> {
    let entries = file_provider.list_directory_entries(dir).ok()?;
    let zen_files: Vec<_> = entries
        .into_iter()
        .filter(|e| !e.is_dir && e.path.extension().is_some_and(|ext| ext == "zen"))
        .collect();

    if zen_files.len() == 1 {
        zen_files[0]
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    } else {
        None
    }
}

fn rel_path_string(path: &Path) -> String {
    path.iter()
        .map(|c| c.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Build a GlobSet from exclude patterns, adding exact match variants for
/// directory subtree patterns like `foo/*` and `foo/**`.
fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        if let Some(exact) = pattern
            .strip_suffix("/*")
            .or_else(|| pattern.strip_suffix("/**"))
        {
            builder.add(Glob::new(exact)?);
        }
    }
    builder.build()
}

fn is_builtin_excluded_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| WORKSPACE_DISCOVERY_EXCLUDE_DIRS.contains(&name))
}

/// Walk workspace directories and collect package roots.
///
/// Discovery is implicit: every descendant directory containing `pcb.toml` is a
/// package candidate unless it is excluded. Excluded directories are pruned.
fn discover_package_dirs<F: FileProvider>(
    file_provider: &F,
    root: &Path,
    exclude_set: &GlobSet,
    errors: &mut Vec<DiscoveryError>,
) -> Vec<(PathBuf, PathBuf)> {
    let mut result = Vec::new();
    // (directory, depth relative to root)
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        let entries = match file_provider.list_directory_entries(&dir) {
            Ok(e) => e,
            Err(e) => {
                if dir != root {
                    errors.push(DiscoveryError {
                        path: dir,
                        error: e.to_string(),
                    });
                }
                continue;
            }
        };

        // The listing tells us both whether this directory is a package and
        // which subdirectories to visit, without any per-entry stat calls.
        if depth > 0
            && entries
                .iter()
                .any(|e| !e.is_dir && e.path.file_name().is_some_and(|n| n == "pcb.toml"))
            && let Ok(rel_path) = dir.strip_prefix(root)
        {
            result.push((dir.clone(), rel_path.to_path_buf()));
        }

        if depth == WORKSPACE_DISCOVERY_MAX_DEPTH {
            continue;
        }

        for entry in entries {
            // Never descend into symlinks (e.g., .pcb/cache contains symlinked packages)
            if !entry.is_dir || entry.is_symlink || is_builtin_excluded_dir(&entry.path) {
                continue;
            }

            let Ok(rel_path) = entry.path.strip_prefix(root) else {
                continue;
            };

            if exclude_set.is_match(rel_path_string(rel_path)) {
                continue;
            }

            stack.push((entry.path, depth + 1));
        }
    }

    result
}

fn root_manifest_is_package(config: &PcbToml, no_descendant_packages: bool) -> bool {
    no_descendant_packages
        || config.board.is_some()
        || !config.dependencies.is_empty()
        || !config.parts.is_empty()
}

/// Get workspace information using FileProvider for cross-platform support.
///
/// This discovers packages and populates board zen paths, but does NOT populate
/// version fields (that requires git). Native code should use `pcb_zen::workspace::get_workspace_info`
/// which adds git version enrichment.
pub fn get_workspace_info<F: FileProvider>(
    file_provider: &F,
    start_path: &Path,
) -> Result<WorkspaceInfo, anyhow::Error> {
    let workspace_root = find_workspace_root(file_provider, start_path)?;
    let pcb_toml_path = workspace_root.join("pcb.toml");

    // Load root config
    let mut config_source: Option<PathBuf> = None;
    let config: Option<PcbToml> = if file_provider.exists(&pcb_toml_path) {
        config_source = Some(pcb_toml_path.clone());
        Some(PcbToml::from_file(file_provider, &pcb_toml_path)?)
    } else if start_path.extension().is_some_and(|ext| ext == "zen") {
        let zen_content = file_provider.read_file(start_path)?;
        match PcbToml::from_zen_content(&zen_content) {
            Some(Ok(cfg)) => {
                config_source = Some(start_path.to_path_buf());
                Some(cfg)
            }
            Some(Err(e)) => return Err(e),
            None => None,
        }
    } else {
        None
    };

    if let Some(cfg) = &config {
        let src = config_source.as_deref().unwrap_or(pcb_toml_path.as_path());
        validate_workspace_pcb_version(cfg, src)?;
    }

    let workspace_config = config
        .as_ref()
        .and_then(|c| c.workspace.clone())
        .unwrap_or_default();

    let base_url = build_workspace_base_url(
        workspace_config.repository.as_deref(),
        workspace_config.path.as_deref(),
    );

    let mut packages = BTreeMap::new();
    let mut errors = Vec::new();
    let preferred_paths = workspace_config.preferred.clone();

    // Only a real root workspace manifest discovers descendant packages. Inline
    // standalone manifests and non-workspace package manifests remain single-package
    // roots.
    let discover_descendants = file_provider.exists(&pcb_toml_path)
        && config
            .as_ref()
            .is_some_and(|cfg| cfg.workspace.as_ref().is_some());

    if discover_descendants {
        let exclude_set = build_glob_set(&workspace_config.exclude)?;

        let dirs = discover_package_dirs(file_provider, &workspace_root, &exclude_set, &mut errors);

        for (dir, rel_path) in dirs {
            let pkg_toml_path = dir.join("pcb.toml");
            let pkg_config = match PcbToml::from_file(file_provider, &pkg_toml_path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    errors.push(DiscoveryError {
                        path: pkg_toml_path,
                        error: e.to_string(),
                    });
                    continue;
                }
            };

            if pkg_config.is_workspace() {
                errors.push(DiscoveryError {
                    path: pkg_toml_path,
                    error: "workspace package cannot have a [workspace] section".to_string(),
                });
                continue;
            }

            let rel_str = rel_path_string(&rel_path);
            let url = base_url
                .as_ref()
                .map(|base| format!("{}/{}", base, rel_str))
                .unwrap_or_else(|| rel_str.clone());

            packages.insert(
                url,
                WorkspacePackage {
                    rel_path,
                    config: pkg_config,
                    version: None,
                    published_at: None,
                    preferred: preferred_paths.contains(&rel_str),
                    dirty: false,
                    entrypoints: Vec::new(),
                    symbol_files: Vec::new(),
                },
            );
        }
    }

    // Add the root package when a real root manifest participates in resolution.
    if let Some(root_config) = config
        .as_ref()
        .filter(|cfg| root_manifest_is_package(cfg, packages.is_empty()))
        .cloned()
    {
        let url = base_url
            .clone()
            .unwrap_or_else(|| LOCAL_WORKSPACE_ROOT_URL.to_string());
        packages.insert(
            url,
            WorkspacePackage {
                rel_path: PathBuf::new(),
                config: root_config,
                version: None,
                published_at: None,
                preferred: false,
                dirty: false,
                entrypoints: Vec::new(),
                symbol_files: Vec::new(),
            },
        );
    }

    // Populate discovered zen paths for boards without explicit paths
    for pkg in packages.values_mut() {
        if let Some(board) = &mut pkg.config.board
            && board.path.is_none()
        {
            let pkg_dir = workspace_root.join(&pkg.rel_path);
            board.path = find_single_zen_file(file_provider, &pkg_dir);
        }
    }

    Ok(WorkspaceInfo {
        root: workspace_root,
        cache_dir: file_provider.cache_dir(),
        config,
        packages,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;
    use crate::config::{parse_pcb_version, pcb_version_from_cargo};
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn test_package_level_workspace_section_is_discovery_error() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
"#
                .to_string(),
            ),
            (
                "/repo/boards/demo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
"#
                .to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert_eq!(info.errors.len(), 1);
        assert!(
            info.errors[0]
                .error
                .contains("workspace package cannot have a [workspace] section")
        );
    }

    #[test]
    fn test_package_discovery_ignores_dot_pcb_directories() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
"#
                .to_string(),
            ),
            (
                "/repo/boards/demo/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/boards/demo/.pcb/edit/github.com/example/dep/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
"#
                .to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert_eq!(info.packages.len(), 1);
        assert!(info.errors.is_empty());
        assert!(info.packages.contains_key("boards/demo"));
    }

    #[test]
    fn test_workspace_exclude_prunes_discovery() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
exclude = ["modules/ignored/**"]
"#
                .to_string(),
            ),
            (
                "/repo/modules/keep/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/modules/ignored/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/modules/ignored/nested/pcb.toml".to_string(),
                "[workspace]\npcb-version = \"0.4\"\n".to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert!(info.errors.is_empty());
        assert!(info.packages.contains_key("modules/keep"));
        assert!(!info.packages.contains_key("modules/ignored"));
        assert!(!info.packages.contains_key("modules/ignored/nested"));
    }

    #[test]
    fn test_workspace_discovery_max_depth_is_eight() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                "[workspace]\npcb-version = \"0.4\"\n".to_string(),
            ),
            (
                "/repo/a/b/c/d/e/f/g/h/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/a/b/c/d/e/f/g/h/i/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert!(info.errors.is_empty());
        assert!(info.packages.contains_key("a/b/c/d/e/f/g/h"));
        assert!(!info.packages.contains_key("a/b/c/d/e/f/g/h/i"));
    }

    #[test]
    fn test_package_url_for_path_uses_most_specific_package() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                "[workspace]\npcb-version = \"0.4\"\n".to_string(),
            ),
            (
                "/repo/modules/parent/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/modules/parent/child/pcb.toml".to_string(),
                "[dependencies]\n".to_string(),
            ),
            (
                "/repo/modules/parent/child/board.zen".to_string(),
                "".to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();

        assert_eq!(
            info.package_url_for_path(&provider, Path::new("/repo/modules/parent/child/board.zen")),
            Some("modules/parent/child")
        );
        assert_eq!(
            info.package_url_for_path(&provider, Path::new("/repo/modules/parent/local.zen")),
            Some("modules/parent")
        );
        assert_eq!(
            info.package_url_for_path(&provider, Path::new("/repo/modules/parental/board.zen")),
            None
        );
    }

    #[test]
    fn test_workspace_stdlib_dir_uses_patch_path() {
        let files = HashMap::from([(
            "/repo/pcb.toml".to_string(),
            r#"
[workspace]
pcb-version = "0.4"

[patch]
stdlib = { path = "third_party/stdlib" }
"#
            .to_string(),
        )]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert_eq!(
            info.workspace_stdlib_dir(),
            Path::new("/repo").join("third_party/stdlib")
        );
    }

    #[test]
    fn test_workspace_stdlib_dir_ignores_legacy_patch_key() {
        let files = HashMap::from([(
            "/repo/pcb.toml".to_string(),
            r#"
[workspace]
pcb-version = "0.4"

[patch]
"github.com/diodeinc/stdlib" = { path = "../stdlib-fork" }
"#
            .to_string(),
        )]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert_eq!(
            info.workspace_stdlib_dir(),
            Path::new("/repo").join(".pcb/stdlib")
        );
    }

    #[test]
    fn test_workspace_preferred_marks_matching_packages() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.4"
preferred = ["components/preferred-part"]
"#
                .to_string(),
            ),
            (
                "/repo/components/preferred-part/pcb.toml".to_string(),
                r#"
[dependencies]
"github.com/diodeinc/stdlib" = "0.5.11"
"#
                .to_string(),
            ),
            (
                "/repo/modules/regular-module/pcb.toml".to_string(),
                r#"
[dependencies]
"github.com/diodeinc/stdlib" = "0.5.11"
"#
                .to_string(),
            ),
        ]);
        let provider = InMemoryFileProvider::new(files);

        let info = get_workspace_info(&provider, Path::new("/repo")).unwrap();
        assert!(
            info.packages["components/preferred-part"].preferred,
            "preferred package should be annotated"
        );
        assert!(
            !info.packages["modules/regular-module"].preferred,
            "non-preferred package should not be annotated"
        );
    }

    #[test]
    fn test_workspace_rejects_newer_required_pcb_version() {
        let (major, minor) = parse_pcb_version(&pcb_version_from_cargo()).unwrap();
        let required = format!("{}.{}", major, minor + 1);
        let files = HashMap::from([(
            "/repo/pcb.toml".to_string(),
            format!(
                r#"
[workspace]
pcb-version = "{}"
"#,
                required
            ),
        )]);
        let provider = InMemoryFileProvider::new(files);

        let err = get_workspace_info(&provider, Path::new("/repo"))
            .expect_err("expected workspace requiring a newer pcb minor version to fail");
        assert!(err.to_string().contains(&required));
    }
}
