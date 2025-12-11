//! Unified tag and version parsing/creation utilities.
//!
//! All version tags follow the format `{path}/v{version}` where:
//! - `{path}` is the package path (can be empty for root packages)
//! - `{version}` is a semver version like `1.2.3`
//!
//! When parsing, the `v` prefix is optional for flexibility.
//! When creating tags, the `v` prefix is always included.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use path_slash::PathExt;
use semver::Version;

use crate::cache_index::ensure_bare_repo;
use crate::git;

/// Parse a version string, with or without 'v' prefix.
///
/// # Examples
/// ```
/// use pcb_zen::tags::parse_version;
/// use semver::Version;
/// assert_eq!(parse_version("1.2.3"), Some(Version::new(1, 2, 3)));
/// assert_eq!(parse_version("v1.2.3"), Some(Version::new(1, 2, 3)));
/// ```
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    Version::parse(s).ok()
}

/// Parse a tag like `path/to/pkg/v1.2.3` or `path/to/pkg/1.2.3`.
///
/// Returns `(package_path, Version)` where package_path does not include
/// the version suffix.
///
/// # Examples
/// ```
/// use pcb_zen::tags::parse_tag;
/// use semver::Version;
/// let (path, ver) = parse_tag("components/LED/v1.2.3").unwrap();
/// assert_eq!(path, "components/LED");
/// assert_eq!(ver, Version::new(1, 2, 3));
/// ```
pub fn parse_tag(tag: &str) -> Option<(String, Version)> {
    let (pkg_path, version_str) = tag.rsplit_once('/')?;
    let version = parse_version(version_str)?;
    Some((pkg_path.to_string(), version))
}

/// Parse a root package tag like `v1.2.3` or `1.2.3`.
///
/// Used for packages at the repository root that don't have a path prefix.
pub fn parse_root_tag(tag: &str) -> Option<Version> {
    // Root tags should not contain slashes
    if tag.contains('/') {
        return None;
    }
    parse_version(tag)
}

/// Compute the git tag prefix for a package.
///
/// The prefix ends with `v` so you can append a version directly.
///
/// # Examples
/// - `rel_path=None, ws_path=None` -> `"v"`
/// - `rel_path=Some("foo"), ws_path=None` -> `"foo/v"`
/// - `rel_path=None, ws_path=Some("pkg")` -> `"pkg/v"`
/// - `rel_path=Some("foo"), ws_path=Some("pkg")` -> `"pkg/foo/v"`
pub fn compute_tag_prefix(rel_path: Option<&Path>, ws_path: Option<&str>) -> String {
    let rel_str = rel_path
        .map(|p| p.to_slash_lossy().into_owned())
        .unwrap_or_default();

    match (ws_path, rel_str.is_empty()) {
        (Some(p), true) => format!("{p}/v"),
        (Some(p), false) => format!("{p}/{rel_str}/v"),
        (None, true) => "v".to_string(),
        (None, false) => format!("{rel_str}/v"),
    }
}

/// Build a full tag name from a prefix and version.
///
/// The prefix should come from `compute_tag_prefix()` and already ends with `v`.
pub fn build_tag_name(prefix: &str, version: &Version) -> String {
    format!("{prefix}{version}")
}

/// Find the latest semver version from tags matching a prefix.
///
/// The prefix should end with `v` (from `compute_tag_prefix()`).
pub fn find_latest_version(tags: &[String], prefix: &str) -> Option<Version> {
    tags.iter()
        .filter_map(|tag| {
            let version_str = tag.strip_prefix(prefix)?;
            parse_version(version_str)
        })
        .max()
}

/// Find the latest tag (full tag string) matching a prefix.
///
/// Returns the complete tag name, not just the version.
pub fn find_latest_tag(tags: &[String], prefix: &str) -> Option<String> {
    tags.iter()
        .filter_map(|tag| {
            let version_str = tag.strip_prefix(prefix)?;
            let version = parse_version(version_str)?;
            Some((tag.clone(), version))
        })
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(tag, _)| tag)
}

/// Get all available versions for packages in a repository.
///
/// Returns a map from package_path (relative to repo) to all available versions,
/// sorted descending (newest first). This fetches/updates the bare repo and parses
/// all version tags.
///
/// For root packages (tags like `v1.0.0`), the package path is an empty string.
/// For nested packages (tags like `path/to/pkg/v1.0.0`), the package path is `path/to/pkg`.
pub fn get_all_versions_for_repo(repo_url: &str) -> Result<BTreeMap<String, Vec<Version>>> {
    let bare_dir = ensure_bare_repo(repo_url)?;
    let tags = git::list_all_tags(&bare_dir)?;

    let mut packages: BTreeMap<String, Vec<Version>> = BTreeMap::new();
    for tag in tags {
        if let Some((pkg_path, version)) = parse_tag(&tag) {
            packages.entry(pkg_path).or_default().push(version);
        } else if let Some(version) = parse_root_tag(&tag) {
            packages.entry(String::new()).or_default().push(version);
        }
    }

    // Sort versions descending for each package (newest first)
    for versions in packages.values_mut() {
        versions.sort_by(|a, b| b.cmp(a));
    }

    Ok(packages)
}

/// Compute the semver family for a version.
///
/// For 0.x versions, the minor version determines the family (0.2.x and 0.3.x are different).
/// For 1.x+ versions, the major version determines the family.
pub fn semver_family(v: &Version) -> String {
    if v.major == 0 {
        format!("v0.{}", v.minor)
    } else {
        format!("v{}", v.major)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(parse_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(parse_version("0.1.0"), Some(Version::new(0, 1, 0)));
        assert_eq!(parse_version("v0.1.0"), Some(Version::new(0, 1, 0)));
        assert_eq!(parse_version("invalid"), None);
        assert_eq!(parse_version("vv1.0.0"), None);
    }

    #[test]
    fn test_parse_tag() {
        let (path, ver) = parse_tag("components/LED/v1.2.3").unwrap();
        assert_eq!(path, "components/LED");
        assert_eq!(ver, Version::new(1, 2, 3));

        let (path, ver) = parse_tag("foo/1.0.0").unwrap();
        assert_eq!(path, "foo");
        assert_eq!(ver, Version::new(1, 0, 0));

        assert!(parse_tag("v1.2.3").is_none()); // No path component
        assert!(parse_tag("invalid").is_none());
    }

    #[test]
    fn test_parse_root_tag() {
        assert_eq!(parse_root_tag("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(parse_root_tag("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(parse_root_tag("path/v1.2.3"), None); // Has path
        assert_eq!(parse_root_tag("invalid"), None);
    }

    #[test]
    fn test_compute_tag_prefix() {
        assert_eq!(compute_tag_prefix(None, None), "v");
        assert_eq!(compute_tag_prefix(Some(Path::new("foo")), None), "foo/v");
        assert_eq!(compute_tag_prefix(None, Some("pkg")), "pkg/v");
        assert_eq!(
            compute_tag_prefix(Some(Path::new("foo/bar")), Some("pkg")),
            "pkg/foo/bar/v"
        );
    }

    #[test]
    fn test_build_tag_name() {
        assert_eq!(
            build_tag_name("foo/v", &Version::new(1, 2, 3)),
            "foo/v1.2.3"
        );
        assert_eq!(build_tag_name("v", &Version::new(0, 1, 0)), "v0.1.0");
    }

    #[test]
    fn test_find_latest_version() {
        let tags = vec![
            "foo/v1.0.0".to_string(),
            "foo/v1.2.0".to_string(),
            "foo/v1.1.0".to_string(),
            "bar/v2.0.0".to_string(),
        ];
        assert_eq!(
            find_latest_version(&tags, "foo/v"),
            Some(Version::new(1, 2, 0))
        );
        assert_eq!(
            find_latest_version(&tags, "bar/v"),
            Some(Version::new(2, 0, 0))
        );
        assert_eq!(find_latest_version(&tags, "baz/v"), None);
    }

    #[test]
    fn test_find_latest_tag() {
        let tags = vec![
            "foo/v1.0.0".to_string(),
            "foo/v1.2.0".to_string(),
            "foo/v1.1.0".to_string(),
        ];
        assert_eq!(
            find_latest_tag(&tags, "foo/v"),
            Some("foo/v1.2.0".to_string())
        );
    }

    #[test]
    fn test_semver_family() {
        assert_eq!(semver_family(&Version::new(0, 1, 0)), "v0.1");
        assert_eq!(semver_family(&Version::new(0, 2, 5)), "v0.2");
        assert_eq!(semver_family(&Version::new(1, 0, 0)), "v1");
        assert_eq!(semver_family(&Version::new(2, 5, 3)), "v2");
    }
}
