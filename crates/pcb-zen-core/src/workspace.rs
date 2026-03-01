//! Workspace discovery and member package types.
//!
//! Provides cross-platform workspace discovery using FileProvider abstraction.
//! Native code can enrich with git tag versions after discovery.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::FileProvider;
use crate::config::{KicadLibraryConfig, Lockfile, PcbToml, WorkspaceConfig, find_workspace_root};
use crate::kicad_library::validate_kicad_library_config;

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

const LOCAL_WORKSPACE_ROOT_URL: &str = "workspace";

/// A discovered member package in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberPackage {
    /// Package directory relative to workspace root
    pub rel_path: PathBuf,
    /// Parsed pcb.toml config
    #[serde(default, skip_serializing_if = "is_default")]
    pub config: PcbToml,
    /// Latest published version from git tags (None if unpublished or not computed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Whether this package has unpublished changes (computed on demand)
    #[serde(default, skip_serializing_if = "is_default")]
    pub dirty: bool,
}

impl MemberPackage {
    /// Get absolute package directory
    pub fn dir(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(&self.rel_path)
    }

    /// Get dependency URLs from config
    pub fn dependencies(&self) -> impl Iterator<Item = &String> {
        self.config.dependencies.keys()
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
    /// Discovered member packages keyed by URL
    pub packages: BTreeMap<String, MemberPackage>,
    /// Optional lockfile
    #[serde(skip)]
    pub lockfile: Option<Lockfile>,
    /// Discovery errors
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<DiscoveryError>,
}

impl WorkspaceInfo {
    /// Workspace-local cache path (typically a symlink to `~/.pcb/cache`).
    pub fn workspace_cache_dir(&self) -> PathBuf {
        self.root.join(".pcb/cache")
    }

    /// Get workspace config section (with defaults if not present)
    pub fn workspace_config(&self) -> WorkspaceConfig {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.clone())
            .unwrap_or_default()
    }

    /// Concrete versions for all configured asset dependency repos (symbols, footprints, models).
    ///
    /// When a repo is referenced by multiple entries, the highest configured version wins.
    pub fn asset_dep_versions(&self) -> BTreeMap<String, Version> {
        let mut selected = BTreeMap::<String, Version>::new();
        for entry in self.kicad_library_entries() {
            for repo in std::iter::once(&entry.symbols)
                .chain(std::iter::once(&entry.footprints))
                .chain(entry.models.values())
            {
                selected
                    .entry(repo.clone())
                    .and_modify(|cur| {
                        if entry.version > *cur {
                            *cur = entry.version.clone();
                        }
                    })
                    .or_insert_with(|| entry.version.clone());
            }
        }
        selected
    }

    /// Get configured `[[workspace.kicad_library]]` entries.
    pub fn kicad_library_entries(&self) -> &[KicadLibraryConfig] {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .map(|w| w.kicad_library.as_slice())
            .unwrap_or(&[])
    }

    /// Iterate all manifest configs in the workspace (root first, then members).
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

    /// Get member glob patterns
    pub fn member_patterns(&self) -> Vec<String> {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.as_ref())
            .map(|w| w.members.clone())
            .unwrap_or_default()
    }

    /// Get all packages as a vector
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
    let entries = file_provider.list_directory(dir).ok()?;
    let zen_files: Vec<_> = entries
        .into_iter()
        .filter(|p| !file_provider.is_directory(p) && p.extension().is_some_and(|ext| ext == "zen"))
        .collect();

    if zen_files.len() == 1 {
        zen_files[0]
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Build a GlobSet from patterns, adding exact match variants for `foo/*` patterns
fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        // Also match exact path for `foo/*` patterns (e.g., `hardware/*` matches `hardware`)
        if let Some(exact) = pattern.strip_suffix("/*") {
            builder.add(Glob::new(exact)?);
        }
    }
    builder.build()
}

/// Walk directories matching member patterns.
/// Prunes at depth 1 to only descend into directories that match pattern prefixes
/// (e.g., for "boards/*", only descend into "boards/").
fn walk_directories<F: FileProvider>(
    file_provider: &F,
    root: &Path,
    include_set: &GlobSet,
    exclude_set: Option<&GlobSet>,
    patterns: &[String],
    errors: &mut Vec<DiscoveryError>,
) -> Vec<(PathBuf, PathBuf)> {
    // Extract first path component from each pattern for pruning at depth 1
    let prefixes: Vec<&str> = patterns
        .iter()
        .filter_map(|p| p.split('/').next())
        .filter(|s| !s.contains(['*', '?', '[']))
        .collect();

    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match file_provider.list_directory(&dir) {
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

        for entry in entries {
            if !file_provider.is_directory(&entry) {
                continue;
            }

            // Never descend into symlinks (e.g., .pcb/cache contains symlinked packages)
            if file_provider.is_symlink(&entry) {
                continue;
            }

            let Ok(rel_path) = entry.strip_prefix(root) else {
                continue;
            };

            // At depth 1, skip directories not matching any pattern prefix
            if rel_path.components().count() == 1 && !prefixes.is_empty() {
                let name = rel_path.to_string_lossy();
                if !prefixes.contains(&&*name) {
                    continue;
                }
            }

            let rel_str: String = rel_path
                .iter()
                .map(|c| c.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            if include_set.is_match(&rel_str) {
                if exclude_set.is_some_and(|ex| ex.is_match(&rel_str)) {
                    continue;
                }
                result.push((entry.clone(), rel_path.to_path_buf()));
            }

            stack.push(entry);
        }
    }

    result
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

    if let Some(cfg) = &config
        && !cfg.is_v2()
    {
        let mut reasons: Vec<&'static str> = Vec::new();
        if cfg
            .workspace
            .as_ref()
            .is_some_and(|w| w.pcb_version.is_none())
        {
            reasons.push("missing `[workspace].pcb-version`");
        }
        if !cfg.packages.is_empty() {
            reasons.push("uses legacy `[packages]` aliases");
        }
        if cfg.module.is_some() {
            reasons.push("uses legacy `[module]` configuration");
        }

        let src = config_source.as_deref().unwrap_or(pcb_toml_path.as_path());
        let mut msg = format!(
            "Unsupported legacy (V1) pcb manifest at {}\n  \
                This toolchain only supports V2 manifests.",
            src.display()
        );
        if !reasons.is_empty() {
            msg.push_str(&format!("\n  Detected: {}", reasons.join(", ")));
        }
        msg.push_str("\n  Run `pcb migrate` to upgrade this workspace to V2.");
        return Err(anyhow::anyhow!(msg));
    }

    if let Some(cfg) = &config
        && let Some(workspace) = cfg.workspace.as_ref()
    {
        for entry in &workspace.kicad_library {
            validate_kicad_library_config(entry)?;
        }
    }

    let workspace_config = config
        .as_ref()
        .and_then(|c| c.workspace.clone())
        .unwrap_or_default();

    let base_url = match (&workspace_config.repository, &workspace_config.path) {
        (Some(repo), Some(p)) => Some(format!("{}/{}", repo, p)),
        (Some(repo), None) => Some(repo.clone()),
        _ => None,
    };

    let mut packages = BTreeMap::new();
    let mut errors = Vec::new();

    // Only discover member packages if patterns are specified (V2 explicit mode)
    if !workspace_config.members.is_empty() {
        let include_set = build_glob_set(&workspace_config.members)?;
        let exclude_set = if workspace_config.exclude.is_empty() {
            None
        } else {
            Some(build_glob_set(&workspace_config.exclude)?)
        };

        let dirs = walk_directories(
            file_provider,
            &workspace_root,
            &include_set,
            exclude_set.as_ref(),
            &workspace_config.members,
            &mut errors,
        );

        for (dir, rel_path) in dirs {
            let pkg_toml_path = dir.join("pcb.toml");
            if !file_provider.exists(&pkg_toml_path) {
                continue;
            }

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

            if !pkg_config.is_v2() {
                errors.push(DiscoveryError {
                    path: pkg_toml_path,
                    error: "legacy (V1) package manifest is no longer supported; run `pcb migrate`"
                        .to_string(),
                });
                continue;
            }

            if pkg_config.is_workspace() {
                errors.push(DiscoveryError {
                    path: pkg_toml_path,
                    error: "member package cannot have a [workspace] section".to_string(),
                });
                continue;
            }

            let rel_str = rel_path
                .iter()
                .map(|c| c.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            let url = base_url
                .as_ref()
                .map(|base| format!("{}/{}", base, rel_str))
                .unwrap_or(rel_str);

            packages.insert(
                url,
                MemberPackage {
                    rel_path,
                    config: pkg_config,
                    version: None,
                    dirty: false,
                },
            );
        }
    }

    // Add root package if applicable
    // For standalone .zen files with inline manifests, base_url may be None
    // but we still need to add the root package to resolve dependencies
    if let Some(cfg) = &config {
        let has_deps = !cfg.dependencies.is_empty();
        if has_deps || packages.is_empty() {
            // Use base_url if available, otherwise use a deterministic local root URL.
            let url = base_url
                .clone()
                .unwrap_or_else(|| LOCAL_WORKSPACE_ROOT_URL.to_string());
            packages.insert(
                url,
                MemberPackage {
                    rel_path: PathBuf::new(),
                    config: cfg.clone(),
                    version: None,
                    dirty: false,
                },
            );
        }
    }

    // Load lockfile - treat parse errors as hard errors, missing file as None
    let lockfile_path = workspace_root.join("pcb.sum");
    let lockfile = match file_provider.read_file(&lockfile_path) {
        Ok(content) => Some(Lockfile::parse(&content)?),
        Err(crate::FileProviderError::NotFound(_)) => None,
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to read pcb.sum: {}", e));
        }
    };

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
        lockfile,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn test_rejects_invalid_kicad_library_version() {
        let files = HashMap::from([(
            "/repo/pcb.toml".to_string(),
            r#"
[workspace]
pcb-version = "0.3"

[[workspace.kicad_library]]
version = "9"
symbols = "gitlab.com/kicad/libraries/kicad-symbols"
footprints = "gitlab.com/kicad/libraries/kicad-footprints"
"#
            .to_string(),
        )]);
        let provider = InMemoryFileProvider::new(files);

        get_workspace_info(&provider, Path::new("/repo"))
            .expect_err("expected invalid [[workspace.kicad_library]].version to fail parse");
    }

    #[test]
    fn test_member_level_workspace_section_is_discovery_error() {
        let files = HashMap::from([
            (
                "/repo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]
"#
                .to_string(),
            ),
            (
                "/repo/boards/demo/pcb.toml".to_string(),
                r#"
[workspace]
pcb-version = "0.3"

[[workspace.kicad_library]]
version = "9.0.3"
symbols = "gitlab.com/kicad/libraries/kicad-symbols"
footprints = "gitlab.com/kicad/libraries/kicad-footprints"
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
                .contains("member package cannot have a [workspace] section")
        );
    }
}
