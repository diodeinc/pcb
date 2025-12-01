//! HTTP archive download support for package fetching
//!
//! Downloads packages from HTTP tar.gz archives instead of git sparse-checkout.
//! This is faster and simpler for hosts that support archive downloads.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Built-in archive patterns: (host, url_pattern, root_pattern)
/// Variables: {host}, {path}, {repo}, {tag}, {tag_no_v}
const ARCHIVE_PATTERNS: &[(&str, &str, &str)] = &[
    (
        "gitlab.com",
        "https://{host}/{path}/-/archive/{tag}/{repo}-{tag}.tar.gz",
        "{repo}-{tag}",
    ),
    // GitHub pattern (disabled for now - use git sparse checkout instead)
    // (
    //     "github.com",
    //     "https://{host}/{path}/archive/refs/tags/{tag}.tar.gz",
    //     "{repo}-{tag_no_v}",
    // ),
];

/// Get archive pattern for a host: (url_pattern, root_pattern)
pub fn get_archive_pattern(host: &str) -> Option<(&'static str, &'static str)> {
    ARCHIVE_PATTERNS
        .iter()
        .find(|(h, _, _)| *h == host)
        .map(|(_, url, root)| (*url, *root))
}

/// Expand pattern variables
/// Variables: {host}, {path}, {repo}, {tag}, {tag_no_v}
fn expand_pattern(pattern: &str, host: &str, path: &str, tag: &str) -> String {
    let repo = path.rsplit('/').next().unwrap_or(path);
    let tag_no_v = tag.strip_prefix('v').unwrap_or(tag);

    pattern
        .replace("{host}", host)
        .replace("{path}", path)
        .replace("{repo}", repo)
        .replace("{tag}", tag)
        .replace("{tag_no_v}", tag_no_v)
}

/// Download and extract archive to target directory
/// Returns Ok(PathBuf) on success, Err on failure (caller should fallback to git)
pub fn fetch_archive(
    url_pattern: &str,
    root_pattern: &str,
    module_path: &str,
    tag: &str,
    target_dir: &Path,
) -> Result<PathBuf> {
    let (host, path) = module_path
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Invalid module path: {}", module_path))?;

    // Build download URL and archive root
    let url = expand_pattern(url_pattern, host, path, tag);
    let archive_root = expand_pattern(root_pattern, host, path, tag);

    log::debug!("Downloading archive from {}", url);

    // Download archive
    let response = reqwest::blocking::get(&url)?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} from {}", response.status(), url);
    }

    // Extract tar.gz
    let decoder = flate2::read::GzDecoder::new(response);
    let mut archive = tar::Archive::new(decoder);

    // Extract to temp dir first
    let temp_dir = tempfile::tempdir()?;
    archive.unpack(temp_dir.path())?;

    // Move from {temp}/{archive_root}/* to {target_dir}/*
    let src_dir = temp_dir.path().join(&archive_root);
    if !src_dir.exists() {
        anyhow::bail!(
            "Archive root '{}' not found in downloaded archive",
            archive_root
        );
    }

    std::fs::create_dir_all(target_dir)?;
    for entry in std::fs::read_dir(&src_dir)? {
        let entry = entry?;
        let dest = target_dir.join(entry.file_name());
        std::fs::rename(entry.path(), &dest)?;
    }

    log::debug!("Extracted archive to {}", target_dir.display());
    Ok(target_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_pattern_gitlab() {
        let pattern = "https://{host}/{path}/-/archive/{tag}/{repo}-{tag}.tar.gz";
        let result = expand_pattern(
            pattern,
            "gitlab.com",
            "kicad/libraries/kicad-symbols",
            "9.0.2",
        );
        assert_eq!(
            result,
            "https://gitlab.com/kicad/libraries/kicad-symbols/-/archive/9.0.2/kicad-symbols-9.0.2.tar.gz"
        );
    }

    #[test]
    fn test_expand_pattern_github() {
        let pattern = "https://{host}/{path}/archive/refs/tags/{tag}.tar.gz";
        let result = expand_pattern(pattern, "github.com", "diodeinc/stdlib", "v0.2.10");
        assert_eq!(
            result,
            "https://github.com/diodeinc/stdlib/archive/refs/tags/v0.2.10.tar.gz"
        );
    }

    #[test]
    fn test_expand_pattern_root_github() {
        let pattern = "{repo}-{tag_no_v}";
        let result = expand_pattern(pattern, "github.com", "diodeinc/stdlib", "v0.2.10");
        assert_eq!(result, "stdlib-0.2.10");
    }

    #[test]
    fn test_expand_pattern_root_gitlab() {
        let pattern = "{repo}-{tag}";
        let result = expand_pattern(
            pattern,
            "gitlab.com",
            "kicad/libraries/kicad-symbols",
            "9.0.2",
        );
        assert_eq!(result, "kicad-symbols-9.0.2");
    }

    #[test]
    fn test_get_archive_pattern() {
        assert!(get_archive_pattern("gitlab.com").is_some());
        // GitHub is disabled for now
        assert!(get_archive_pattern("github.com").is_none());
        assert!(get_archive_pattern("bitbucket.org").is_none());
    }
}
