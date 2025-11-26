//! Remote package discovery for auto-deps
//!
//! Discovers packages in remote repositories by fetching and parsing git tags.
//! Used when a URL import doesn't match any workspace member or lockfile entry.

use anyhow::Result;
use semver::Version;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::git;
use pcb_zen_core::config::Lockfile;

/// Index of published packages in a remote repository
#[derive(Debug, Default)]
struct RemotePackageIndex {
    /// Package path -> latest version (e.g., "components/LED" -> "0.1.0")
    packages: BTreeMap<String, String>,
}

impl RemotePackageIndex {
    /// Find the longest matching package for a file path (LPM)
    fn find_longest_match(&self, file_path: &str) -> Option<(&str, &str)> {
        let without_file = file_path.rsplit_once('/')?.0;

        let mut path = without_file;
        while !path.is_empty() {
            if let Some((package_path, version)) = self.packages.get_key_value(path) {
                return Some((package_path.as_str(), version.as_str()));
            }
            path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        }
        None
    }
}

/// Cache of remote package indices (per build session)
#[derive(Debug, Default)]
pub struct RemoteIndexCache {
    indices: HashMap<String, RemotePackageIndex>,
}

impl RemoteIndexCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn get_or_build(&mut self, repo_url: &str) -> Result<&RemotePackageIndex> {
        if !self.indices.contains_key(repo_url) {
            let index = build_remote_package_index(repo_url)?;
            self.indices.insert(repo_url.to_string(), index);
        }
        Ok(&self.indices[repo_url])
    }
}

/// Find matching lockfile entry via LPM (fast path - no git)
pub fn find_matching_lockfile_entry(
    file_url: &str,
    lockfile: &Lockfile,
) -> Option<(String, String)> {
    let without_file = file_url.rsplit_once('/')?.0;

    let mut path = without_file;
    while !path.is_empty() {
        if let Some(entry) = lockfile.iter().find(|e| e.module_path == path) {
            return Some((entry.module_path.clone(), entry.version.clone()));
        }
        path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    }
    None
}

/// Find matching remote package via LPM (slow path - git tags, cached)
pub fn find_matching_remote_package(
    file_url: &str,
    cache: &mut RemoteIndexCache,
) -> Result<Option<(String, String)>> {
    let (repo_url, subpath) = split_repo_and_subpath(file_url);

    if subpath.is_empty() {
        return Ok(None);
    }

    let index = cache.get_or_build(repo_url)?;

    Ok(index
        .find_longest_match(subpath)
        .map(|(pkg_path, version)| (format!("{}/{}", repo_url, pkg_path), version.to_string())))
}

/// Build package index from git tags
fn build_remote_package_index(repo_url: &str) -> Result<RemotePackageIndex> {
    let bare_repo = ensure_bare_repo(repo_url)?;
    let tags = git::list_all_tags(&bare_repo)?;

    let mut packages: BTreeMap<String, Version> = BTreeMap::new();
    for tag in tags {
        if let Some((pkg_path, version)) = parse_version_tag(&tag) {
            packages
                .entry(pkg_path)
                .and_modify(|v| {
                    if version > *v {
                        *v = version.clone()
                    }
                })
                .or_insert(version);
        }
    }

    Ok(RemotePackageIndex {
        packages: packages
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect(),
    })
}

/// Ensure bare repo exists at ~/.pcb/bare/{repo_url}, fetching updates if needed
fn ensure_bare_repo(repo_url: &str) -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let bare_dir = home.join(".pcb").join("bare").join(repo_url);

    if bare_dir.join("HEAD").exists() {
        git::fetch_in_bare_repo(&bare_dir)?;
    } else {
        git::clone_bare_with_fallback(repo_url, &bare_dir)?;
    }

    Ok(bare_dir)
}

/// Parse version tag: "components/LED/v0.1.0" -> ("components/LED", 0.1.0)
fn parse_version_tag(tag: &str) -> Option<(String, Version)> {
    let (pkg_path, version_str) = tag.rsplit_once('/')?;
    let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
    let version = Version::parse(version_str).ok()?;
    Some((pkg_path.to_string(), version))
}

/// Split URL into repo and subpath: "github.com/user/repo/path/file" -> ("github.com/user/repo", "path/file")
fn split_repo_and_subpath(url: &str) -> (&str, &str) {
    let parts: Vec<&str> = url.split('/').collect();
    if parts.first() == Some(&"github.com") && parts.len() > 3 {
        let boundary = parts[..3].join("/").len();
        (&url[..boundary], &url[boundary + 1..])
    } else {
        (url, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version_tag() {
        let (path, ver) = parse_version_tag("components/LED/v1.2.3").unwrap();
        assert_eq!(path, "components/LED");
        assert_eq!(ver, Version::new(1, 2, 3));

        assert!(parse_version_tag("not-a-version").is_none());
    }

    #[test]
    fn test_split_repo_and_subpath() {
        let (repo, sub) = split_repo_and_subpath("github.com/diodeinc/registry/components/LED");
        assert_eq!(repo, "github.com/diodeinc/registry");
        assert_eq!(sub, "components/LED");

        let (repo, sub) = split_repo_and_subpath("github.com/diodeinc/stdlib");
        assert_eq!(repo, "github.com/diodeinc/stdlib");
        assert_eq!(sub, "");
    }

    #[test]
    fn test_lpm() {
        let mut index = RemotePackageIndex::default();
        index
            .packages
            .insert("components/LED".into(), "0.1.0".into());
        index
            .packages
            .insert("components/JST/BM04B".into(), "0.2.0".into());
        index
            .packages
            .insert("components/JST".into(), "0.3.0".into());

        let (p, v) = index.find_longest_match("components/LED/LED.zen").unwrap();
        assert_eq!((p, v), ("components/LED", "0.1.0"));

        let (p, v) = index
            .find_longest_match("components/JST/BM04B/x.zen")
            .unwrap();
        assert_eq!((p, v), ("components/JST/BM04B", "0.2.0"));

        let (p, v) = index
            .find_longest_match("components/JST/OTHER/x.zen")
            .unwrap();
        assert_eq!((p, v), ("components/JST", "0.3.0"));

        assert!(index.find_longest_match("modules/foo/bar.zen").is_none());
    }
}
