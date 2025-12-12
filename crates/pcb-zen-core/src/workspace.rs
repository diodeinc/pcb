//! Workspace discovery and member package types.
//!
//! Provides cross-platform workspace discovery using FileProvider abstraction.
//! Native code can enrich with git tag versions after discovery.

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{find_workspace_root, Lockfile, PcbToml, WorkspaceConfig};
use crate::FileProvider;

/// A discovered member package in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberPackage {
    /// Package directory (absolute path)
    pub dir: PathBuf,
    /// Package directory relative to workspace root
    pub rel_path: PathBuf,
    /// Parsed pcb.toml config
    pub config: PcbToml,
    /// Latest published version from git tags (None if unpublished or not computed)
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
    /// Root pcb.toml config (if present)
    pub config: Option<PcbToml>,
    /// Discovered member packages keyed by URL
    pub packages: BTreeMap<String, MemberPackage>,
    /// Optional lockfile
    #[serde(skip)]
    pub lockfile: Option<Lockfile>,
    /// Discovery errors
    pub errors: Vec<DiscoveryError>,
}

impl WorkspaceInfo {
    /// Check if this workspace is V2 mode
    pub fn is_v2(&self) -> bool {
        self.config.as_ref().is_some_and(|c| c.is_v2())
    }

    /// Get workspace config section (with defaults if not present)
    pub fn workspace_config(&self) -> WorkspaceConfig {
        self.config
            .as_ref()
            .and_then(|c| c.workspace.clone())
            .unwrap_or_default()
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

/// Recursively walk directories using FileProvider
fn walk_directories<F: FileProvider>(
    file_provider: &F,
    root: &Path,
    include_set: &GlobSet,
    exclude_set: Option<&GlobSet>,
    errors: &mut Vec<DiscoveryError>,
) -> Vec<(PathBuf, PathBuf)> {
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

            let Ok(rel_path) = entry.strip_prefix(root) else {
                continue;
            };

            // Normalize to forward slashes for glob matching
            let rel_str = rel_path
                .iter()
                .map(|c| c.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            // Check include patterns - must match to be considered
            if !include_set.is_match(&rel_str) {
                stack.push(entry);
                continue;
            }

            // Check exclude patterns
            if let Some(exclude) = exclude_set {
                if exclude.is_match(&rel_str) {
                    continue;
                }
            }

            result.push((entry.clone(), rel_path.to_path_buf()));
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
    let workspace_root = find_workspace_root(file_provider, start_path);
    let pcb_toml_path = workspace_root.join("pcb.toml");

    // Load root config
    let config: Option<PcbToml> = if file_provider.exists(&pcb_toml_path) {
        Some(PcbToml::from_file(file_provider, &pcb_toml_path)?)
    } else if start_path.extension().is_some_and(|ext| ext == "zen") {
        let zen_content = file_provider.read_file(start_path)?;
        match PcbToml::from_zen_content(&zen_content) {
            Some(Ok(cfg)) => Some(cfg),
            Some(Err(e)) => return Err(e),
            None => None,
        }
    } else {
        None
    };

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
            &mut errors,
        );

        for (dir, rel_path) in dirs {
            let pkg_toml_path = dir.join("pcb.toml");
            if !file_provider.exists(&pkg_toml_path) {
                continue;
            }

            let pkg_config = match file_provider.read_file(&pkg_toml_path) {
                Ok(content) => match PcbToml::parse(&content) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        errors.push(DiscoveryError {
                            path: pkg_toml_path,
                            error: e.to_string(),
                        });
                        continue;
                    }
                },
                Err(e) => {
                    errors.push(DiscoveryError {
                        path: pkg_toml_path,
                        error: e.to_string(),
                    });
                    continue;
                }
            };

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
                    dir,
                    rel_path,
                    config: pkg_config,
                    version: None,
                },
            );
        }
    }

    // Add root package if applicable
    // For standalone .zen files with inline manifests, base_url may be None
    // but we still need to add the root package to resolve dependencies
    if let Some(cfg) = &config {
        let has_deps = !cfg.dependencies.is_empty() || !cfg.assets.is_empty();
        if has_deps || packages.is_empty() {
            // Use base_url if available, otherwise use workspace root path as synthetic URL
            let url = base_url
                .clone()
                .unwrap_or_else(|| workspace_root.to_string_lossy().into_owned());
            packages.insert(
                url,
                MemberPackage {
                    dir: workspace_root.clone(),
                    rel_path: PathBuf::new(),
                    config: cfg.clone(),
                    version: None,
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
        if let Some(board) = &mut pkg.config.board {
            if board.path.is_none() {
                board.path = find_single_zen_file(file_provider, &pkg.dir);
            }
        }
    }

    Ok(WorkspaceInfo {
        root: workspace_root,
        config,
        packages,
        lockfile,
        errors,
    })
}
