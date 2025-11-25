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
use std::sync::Arc;
use walkdir::WalkDir;

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

    /// Number of declared dependencies
    pub dependency_count: usize,

    /// Number of declared assets
    pub asset_count: usize,
}

/// Summary of board configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardSummary {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zen_path: Option<String>,
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
            BoardSummary {
                name: b.name.clone(),
                description: if b.description.is_empty() {
                    None
                } else {
                    Some(b.description.clone())
                },
                zen_path,
            }
        })
    });

    let dependency_count = config.as_ref().map(|c| c.dependencies.len()).unwrap_or(0);
    let asset_count = config.as_ref().map(|c| c.assets.len()).unwrap_or(0);

    Ok(PackageInfo {
        url: url.to_string(),
        path: dir.to_path_buf(),
        latest_version,
        board,
        dependency_count,
        asset_count,
    })
}

/// Compute the git tag prefix for a package
///
/// Examples:
/// - Root package with no ws.path: "v"
/// - Root package with ws.path="hardware": "hardware/v"
/// - Member at "ti/tps54331" with no ws.path: "ti/tps54331/v"
/// - Member at "ti/tps54331" with ws.path="hardware": "hardware/ti/tps54331/v"
fn compute_tag_prefix(rel_path: Option<&Path>, ws_path: &Option<String>) -> String {
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
        assert_eq!(
            compute_tag_prefix(Some(Path::new("")), &None),
            "v"
        );

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

        assert_eq!(
            find_latest_version(&tags, "v"),
            Some("0.2.1".to_string())
        );
        assert_eq!(
            find_latest_version(&tags, "other/v"),
            Some("1.0.0".to_string())
        );
        assert_eq!(find_latest_version(&tags, "nonexistent/v"), None);
    }
}
