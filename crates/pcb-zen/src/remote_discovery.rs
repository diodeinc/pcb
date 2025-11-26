//! Remote package discovery for auto-deps
//!
//! Discovers packages in remote repositories by fetching and parsing git tags.
//! Used when a URL import doesn't match any workspace member or lockfile entry.

use anyhow::{Context, Result};
use semver::Version;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use pcb_zen_core::config::Lockfile;

/// Index of published packages in a remote repository
///
/// Maps package_path (relative to repo root) -> latest version
/// e.g., "components/LED" -> "0.1.0"
#[derive(Debug, Default)]
pub struct RemotePackageIndex {
    /// Package path -> latest version
    packages: BTreeMap<String, String>,
}

impl RemotePackageIndex {
    /// Find the longest matching package for a file path
    ///
    /// e.g., "components/X/Y/Z.zen" matches "components/X/Y" before "components/X"
    ///
    /// Returns (package_path, version) where both are owned by self.
    pub fn find_longest_match(&self, file_path: &str) -> Option<(&str, &str)> {
        // Strip the filename first
        let without_file = file_path.rsplit_once('/')?.0;

        // Iterate in reverse (BTreeMap is sorted, so longer paths come later alphabetically
        // within the same prefix, but we need true LPM). Try progressively shorter prefixes.
        let mut path = without_file;
        while !path.is_empty() {
            if let Some((package_path, version)) = self.packages.get_key_value(path) {
                return Some((package_path.as_str(), version.as_str()));
            }
            // Strip the last path component
            path = match path.rsplit_once('/') {
                Some((prefix, _)) => prefix,
                None => break,
            };
        }
        None
    }
}

/// Cache of remote package indices, persisted across the build session
#[derive(Debug, Default)]
pub struct RemoteIndexCache {
    /// repo_url -> package index
    indices: HashMap<String, RemotePackageIndex>,
}

impl RemoteIndexCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or build the package index for a repository
    ///
    /// First time: clones bare repo and parses all tags
    /// Subsequent: fetches new tags and re-parses
    pub fn get_or_build(&mut self, repo_url: &str) -> Result<&RemotePackageIndex> {
        if !self.indices.contains_key(repo_url) {
            let index = build_remote_package_index(repo_url)?;
            self.indices.insert(repo_url.to_string(), index);
        }
        Ok(&self.indices[repo_url])
    }
}

/// Find the longest matching lockfile entry for a file URL
///
/// e.g., "github.com/diodeinc/registry/components/X/Y/Z.zen"
///    -> Some(("github.com/diodeinc/registry/components/X/Y", "0.1.0"))
///
/// This is the fast path - no git operations needed.
pub fn find_matching_lockfile_entry(
    file_url: &str,
    lockfile: &Lockfile,
) -> Option<(String, String)> {
    // Strip the filename first
    let without_file = file_url.rsplit_once('/')?.0;

    // Try progressively shorter prefixes
    let mut path = without_file;
    while !path.is_empty() {
        // Check if any lockfile entry matches this path
        if let Some(entry) = lockfile.iter().find(|e| e.module_path == path) {
            return Some((entry.module_path.clone(), entry.version.clone()));
        }
        // Strip the last path component
        path = match path.rsplit_once('/') {
            Some((prefix, _)) => prefix,
            None => break,
        };
    }
    None
}

/// Find the longest matching remote package for a file URL
///
/// e.g., "github.com/diodeinc/registry/components/X/Y/Z.zen"
///    -> Some(("github.com/diodeinc/registry/components/X/Y", "0.1.0"))
///
/// This is the slow path - requires git operations (cached per repo).
pub fn find_matching_remote_package(
    file_url: &str,
    cache: &mut RemoteIndexCache,
) -> Result<Option<(String, String)>> {
    // Extract repo URL and subpath
    let (repo_url, subpath) = split_repo_and_subpath(file_url);

    if subpath.is_empty() {
        // URL is just the repo itself, no package discovery needed
        return Ok(None);
    }

    // Get or build the package index for this repo
    let index = cache.get_or_build(repo_url)?;

    // Find longest matching package within the subpath
    if let Some((package_path, version)) = index.find_longest_match(subpath) {
        // Reconstruct full module path
        let full_module_path = format!("{}/{}", repo_url, package_path);
        return Ok(Some((full_module_path, version.to_string())));
    }

    Ok(None)
}

/// Build a package index by fetching and parsing git tags from a remote repository
fn build_remote_package_index(repo_url: &str) -> Result<RemotePackageIndex> {
    let bare_repo = ensure_bare_repo(repo_url)?;

    // List all tags
    let tags = list_tags(&bare_repo)?;

    // Parse tags into package -> version map, keeping only the latest version
    let mut packages: BTreeMap<String, Version> = BTreeMap::new();

    for tag in tags {
        if let Some((package_path, version)) = parse_version_tag(&tag) {
            // Keep only the latest version for each package
            packages
                .entry(package_path)
                .and_modify(|existing| {
                    if version > *existing {
                        *existing = version.clone();
                    }
                })
                .or_insert(version);
        }
    }

    // Convert to string versions
    let packages = packages
        .into_iter()
        .map(|(path, version)| (path, version.to_string()))
        .collect();

    Ok(RemotePackageIndex { packages })
}

/// Ensure a bare repo exists for the given URL, fetching updates if needed
fn ensure_bare_repo(repo_url: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let bare_dir = home.join(".pcb").join("bare").join(repo_url);

    if bare_dir.join("HEAD").exists() {
        // Bare repo exists, fetch updates
        fetch_tags(&bare_dir)?;
    } else {
        // Clone bare repo (tags only via shallow clone)
        clone_bare_for_tags(repo_url, &bare_dir)?;
    }

    Ok(bare_dir)
}

/// Clone a bare repository optimized for tag discovery
fn clone_bare_for_tags(repo_url: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or(dest))
        .with_context(|| format!("Failed to create directory for {}", dest.display()))?;

    let https_url = format!("https://{}.git", repo_url);
    let ssh_url = format_ssh_url(repo_url);

    // Try HTTPS first
    let status = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg("--filter=blob:none")
        .arg("--quiet")
        .arg(&https_url)
        .arg(dest)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(s) = status {
        if s.success() {
            return Ok(());
        }
    }

    // Fallback to SSH
    let status = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg("--filter=blob:none")
        .arg("--quiet")
        .arg(&ssh_url)
        .arg(dest)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Failed to clone bare repo for {}", repo_url)
    }
}

/// Fetch tags from remote (updates existing bare repo)
fn fetch_tags(bare_repo: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(bare_repo)
        .arg("fetch")
        .arg("origin")
        .arg("--tags")
        .arg("--force")
        .arg("--prune-tags")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("Failed to run git fetch in {}", bare_repo.display()))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Failed to fetch tags in {}", bare_repo.display())
    }
}

/// List all tags in a bare repository
fn list_tags(bare_repo: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(bare_repo)
        .arg("tag")
        .arg("-l")
        .output()
        .with_context(|| format!("Failed to run git tag in {}", bare_repo.display()))?;

    if !output.status.success() {
        anyhow::bail!("Failed to list tags in {}", bare_repo.display());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.to_string())
        .collect())
}

/// Parse a version tag into (package_path, version)
///
/// Supported formats:
/// - "v0.1.0" -> ("", v0.1.0) for root package
/// - "components/LED/v0.1.0" -> ("components/LED", v0.1.0)
/// - "components/X/Y/v1.2.3" -> ("components/X/Y", v1.2.3)
fn parse_version_tag(tag: &str) -> Option<(String, Version)> {
    // Find the version suffix (vX.Y.Z)
    let parts: Vec<&str> = tag.rsplitn(2, '/').collect();

    let (version_str, package_path) = if parts.len() == 2 {
        // Has path prefix: "components/LED/v0.1.0"
        (parts[0], parts[1])
    } else {
        // Root package: "v0.1.0"
        (parts[0], "")
    };

    // Parse version (strip 'v' prefix if present)
    let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
    let version = Version::parse(version_str).ok()?;

    Some((package_path.to_string(), version))
}

/// Extract repository boundary and subpath from a URL
///
/// For GitHub: github.com/user/repo is the boundary
/// Everything after is the subpath
fn split_repo_and_subpath(url: &str) -> (&str, &str) {
    let parts: Vec<&str> = url.split('/').collect();

    if parts.is_empty() {
        return (url, "");
    }

    let host = parts[0];

    // For GitHub: repo is host/user/repo (first 3 segments)
    if host == "github.com" && parts.len() > 3 {
        let boundary_len = parts[..3].join("/").len();
        let repo_url = &url[..boundary_len];
        let subpath = &url[boundary_len + 1..]; // Skip the '/' separator
        (repo_url, subpath)
    } else {
        // GitLab and others: treat entire path as repo (no subpath)
        (url, "")
    }
}

/// Convert module path to SSH URL format
fn format_ssh_url(module_path: &str) -> String {
    let parts: Vec<&str> = module_path.splitn(2, '/').collect();
    if parts.len() == 2 {
        let host = parts[0];
        let path = parts[1];
        format!("git@{}:{}.git", host, path)
    } else {
        format!("https://{}.git", module_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_tag() {
        // Root package
        let (path, version) = parse_version_tag("v0.1.0").unwrap();
        assert_eq!(path, "");
        assert_eq!(version, Version::new(0, 1, 0));

        // Nested package
        let (path, version) = parse_version_tag("components/LED/v1.2.3").unwrap();
        assert_eq!(path, "components/LED");
        assert_eq!(version, Version::new(1, 2, 3));

        // Deeply nested
        let (path, version) = parse_version_tag("components/JST/BM04B/v0.1.0").unwrap();
        assert_eq!(path, "components/JST/BM04B");
        assert_eq!(version, Version::new(0, 1, 0));

        // Invalid version
        assert!(parse_version_tag("not-a-version").is_none());
        assert!(parse_version_tag("components/foo/bar").is_none());
    }

    #[test]
    fn test_split_repo_and_subpath() {
        // GitHub with subpath
        let (repo, sub) = split_repo_and_subpath("github.com/diodeinc/registry/components/LED");
        assert_eq!(repo, "github.com/diodeinc/registry");
        assert_eq!(sub, "components/LED");

        // GitHub root
        let (repo, sub) = split_repo_and_subpath("github.com/diodeinc/stdlib");
        assert_eq!(repo, "github.com/diodeinc/stdlib");
        assert_eq!(sub, "");

        // Deep subpath
        let (repo, sub) =
            split_repo_and_subpath("github.com/diodeinc/registry/components/X/Y/Z.zen");
        assert_eq!(repo, "github.com/diodeinc/registry");
        assert_eq!(sub, "components/X/Y/Z.zen");
    }

    #[test]
    fn test_remote_package_index_lpm() {
        let mut index = RemotePackageIndex::default();
        index
            .packages
            .insert("components/LED".to_string(), "0.1.0".to_string());
        index.packages.insert(
            "components/JST/BM04B".to_string(),
            "0.2.0".to_string(),
        );
        index
            .packages
            .insert("components/JST".to_string(), "0.3.0".to_string());

        // Exact match
        let (path, ver) = index.find_longest_match("components/LED/LED.zen").unwrap();
        assert_eq!(path, "components/LED");
        assert_eq!(ver, "0.1.0");

        // LPM: prefer longer match
        let (path, ver) = index
            .find_longest_match("components/JST/BM04B/BM04B.zen")
            .unwrap();
        assert_eq!(path, "components/JST/BM04B");
        assert_eq!(ver, "0.2.0");

        // Falls back to shorter match
        let (path, ver) = index
            .find_longest_match("components/JST/OTHER/other.zen")
            .unwrap();
        assert_eq!(path, "components/JST");
        assert_eq!(ver, "0.3.0");

        // No match
        assert!(index.find_longest_match("modules/foo/bar.zen").is_none());
    }
}
