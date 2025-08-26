use log::debug;
use pcb_zen_core::{LoadSpec, RefKind, RemoteRefMeta};
use std::collections::hash_map::Entry;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(windows)]
use std::os::windows::fs as win_fs;

use crate::git;

// Re-export constants from LoadSpec for backward compatibility
pub use pcb_zen_core::load_spec::{DEFAULT_GITHUB_REV, DEFAULT_GITLAB_REV, DEFAULT_PKG_TAG};

/// Resolve file path within cache and create convenience symlinks for Git repos
fn ensure_symlinks(
    spec: &LoadSpec,
    workspace_root: &Path,
    cache_root: &Path,
) -> anyhow::Result<PathBuf> {
    let path = spec.path();
    let local_path = if path.as_os_str().is_empty() {
        cache_root.to_path_buf()
    } else {
        cache_root.join(path)
    };

    if local_path.exists() {
        // Create convenience symlinks for Git repos (not packages)
        match spec {
            LoadSpec::Github {
                user, repo, rev, ..
            } => {
                let folder_name = format!(
                    "github{}{}{}{}{}{}",
                    std::path::MAIN_SEPARATOR,
                    user,
                    std::path::MAIN_SEPARATOR,
                    repo,
                    std::path::MAIN_SEPARATOR,
                    rev
                );
                let _ = expose_alias_symlink(workspace_root, &folder_name, path, &local_path);
            }
            LoadSpec::Gitlab {
                project_path, rev, ..
            } => {
                let folder_name = format!(
                    "gitlab{}{}{}{}",
                    std::path::MAIN_SEPARATOR,
                    project_path,
                    std::path::MAIN_SEPARATOR,
                    rev
                );
                let _ = expose_alias_symlink(workspace_root, &folder_name, path, &local_path);
            }
            LoadSpec::Package { package, .. } => {
                // Packages use simple alias symlinks
                let _ = expose_alias_symlink(workspace_root, package, path, &local_path);
            }
            _ => {}
        }
    }
    Ok(local_path)
}

/// Classify a remote Git repository to determine reference type
fn classify_remote(cache_root: &Path, spec: &LoadSpec) -> Option<RemoteRefMeta> {
    let expected_ref = match spec {
        LoadSpec::Github { rev, .. } | LoadSpec::Gitlab { rev, .. } => rev,
        _ => unreachable!(),
    };

    let sha1 = git::rev_parse_head(cache_root)?;
    let kind = {
        let tag_sha1 = git::rev_parse(cache_root, &format!("{expected_ref}^{{commit}}"));
        if git::tag_exists(cache_root, expected_ref) && tag_sha1 == Some(sha1.clone()) {
            debug!("Tag {expected_ref} exists, and it's at HEAD");
            RefKind::Tag
        } else if expected_ref.len() > 7 && sha1.starts_with(expected_ref) {
            debug!("Hash matches {expected_ref} ref");
            RefKind::Commit
        } else {
            debug!("{expected_ref} is unstable");
            RefKind::Unstable
        }
    };

    Some(RemoteRefMeta {
        commit_sha1: sha1,
        commit_sha256: None,
        kind,
    })
}

/// Ensure a cache directory exists atomically, downloading if necessary.
fn ensure_cached_atomically(
    cache_root: &Path,
    download_fn: impl FnOnce(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if cache_root.exists() {
        return Ok(());
    }

    // Ensure parent directory exists
    if let Some(parent) = cache_root.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Create temp directory in same parent as final cache directory
    let temp_dir = tempfile::tempdir_in(cache_root.parent().unwrap())?;

    // Download to temp directory
    download_fn(temp_dir.path())?;

    // Atomically move temp directory to final location
    match std::fs::rename(temp_dir.path(), cache_root) {
        Ok(()) => Ok(()),
        Err(_) if cache_root.exists() => {
            // Another thread won the race - that's fine
            // TempDir will clean itself up on drop
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// Ensure the remote is cached and return the root directory of the checked-out revision.
/// Returns the directory containing the checked-out repository or unpacked package.
/// Uses atomic directory creation to prevent race conditions when multiple tests run in parallel.
pub fn ensure_remote_cached(spec: &LoadSpec) -> anyhow::Result<PathBuf> {
    match spec {
        LoadSpec::Github {
            user, repo, rev, ..
        } => {
            let cache_root = cache_dir()?.join("github").join(user).join(repo).join(rev);
            ensure_cached_atomically(&cache_root, |temp_dir| {
                download_and_unpack_github_repo(user, repo, rev, temp_dir)
            })?;
            Ok(cache_root)
        }
        LoadSpec::Gitlab {
            project_path, rev, ..
        } => {
            let cache_root = cache_dir()?.join("gitlab").join(project_path).join(rev);
            ensure_cached_atomically(&cache_root, |temp_dir| {
                download_and_unpack_gitlab_repo(project_path, rev, temp_dir)
            })?;
            Ok(cache_root)
        }
        _ => anyhow::bail!("ensure_remote_cached only handles remote specs"),
    }
}

pub fn cache_dir() -> anyhow::Result<PathBuf> {
    // 1. Allow callers to force an explicit location via env var. This is handy in CI
    //    environments where the default XDG cache directory may be read-only or owned
    //    by a different user (e.g. when running inside a rootless container).
    if let Ok(custom) = std::env::var("DIODE_STAR_CACHE_DIR") {
        let path = PathBuf::from(custom);
        std::fs::create_dir_all(&path)?;
        return Ok(path);
    }

    // 2. Attempt to use the standard per-user cache directory reported by the `dirs` crate.
    if let Some(base) = dirs::cache_dir() {
        let dir = base.join("pcb");
        if std::fs::create_dir_all(&dir).is_ok() {
            return Ok(dir);
        }
        // If we failed to create the directory (e.g. permission denied) we fall through
        // to the temporary directory fallback below instead of erroring out immediately.
    }

    // 3. As a last resort fall back to a writable path under the system temp directory. While
    //    this is not cached across runs, it ensures functionality in locked-down CI systems.
    let dir = std::env::temp_dir().join("pcb_cache");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn try_clone_and_fetch_commit(remote_url: &str, rev: &str, dest_dir: &Path) -> anyhow::Result<()> {
    log::debug!("Cloning default branch then fetching commit: {remote_url} @ {rev}");
    git::clone_default_branch(remote_url, dest_dir)?;
    git::fetch_commit(dest_dir, rev)?;
    git::checkout_revision(dest_dir, rev)
}

/// Try Git clone with 2-pass strategy for multiple URLs
fn try_git_clone(clone_urls: &[String], rev: &str, dest_dir: &Path) -> anyhow::Result<bool> {
    if !git::is_available() {
        return Ok(false);
    }

    for remote_url in clone_urls {
        // Ensure parent dirs exist so `git clone` can create `dest_dir`.
        if let Some(parent) = dest_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Pass 1: Try to clone as branch/tag (most efficient)
        log::debug!("Trying branch/tag clone: {remote_url} @ {rev}");
        if git::clone_as_branch_or_tag(remote_url, rev, dest_dir).is_ok() {
            return Ok(true);
        }

        // Pass 2: Clone default branch, then fetch specific commit
        if try_clone_and_fetch_commit(remote_url, rev, dest_dir).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn download_and_unpack_github_repo(
    user: &str,
    repo: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitHub repo {user}/{repo} @ {rev}");

    // Try Git clone strategies first
    let clone_urls = [
        format!("https://github.com/{user}/{repo}.git"),
        format!("git@github.com:{user}/{repo}.git"),
    ];

    if try_git_clone(&clone_urls, rev, dest_dir)? {
        return Ok(());
    }

    // Fallback: GitHub's codeload tarball API
    let url = format!("https://codeload.github.com/{user}/{repo}/tar.gz/{rev}");
    let client = reqwest::blocking::Client::builder()
        .user_agent("diode-star-loader")
        .build()?;

    let token = std::env::var("DIODE_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok();

    let mut request = client.get(&url);
    if let Some(t) = token.as_ref() {
        request = request.header("Authorization", format!("Bearer {t}"));
    }

    let resp = request.send()?;
    if !resp.status().is_success() {
        let code = resp.status();
        if code == reqwest::StatusCode::NOT_FOUND || code == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "Failed to download GitHub repo {user}/{repo} at {rev} (HTTP {code}).\n\
                 Tried clones via HTTPS & SSH, then tarball download.\n\
                 If this repository is private, set an access token in GITHUB_TOKEN."
            );
        } else {
            anyhow::bail!("Failed to download GitHub repo {user}/{repo} at {rev} (HTTP {code})");
        }
    }

    let bytes = resp.bytes()?;
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);

    // Extract contents while stripping the top-level folder
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let mut comps = path.components();
        comps.next(); // strip top-level folder
        let stripped: PathBuf = comps.as_path().to_path_buf();
        let out_path = dest_dir.join(stripped);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out_path)?;
    }
    Ok(())
}

pub fn download_and_unpack_gitlab_repo(
    project_path: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitLab repo {project_path} @ {rev}");

    // Try Git clone strategies first
    let clone_urls = [
        format!("https://gitlab.com/{project_path}.git"),
        format!("git@gitlab.com:{project_path}.git"),
    ];

    if try_git_clone(&clone_urls, rev, dest_dir)? {
        return Ok(());
    }

    // Fallback: GitLab's archive API
    let encoded_project_path = project_path.replace('/', "%2F");
    let url = format!("https://gitlab.com/api/v4/projects/{encoded_project_path}/repository/archive.tar.gz?sha={rev}");

    let client = reqwest::blocking::Client::builder()
        .user_agent("diode-star-loader")
        .build()?;

    let token = std::env::var("DIODE_GITLAB_TOKEN")
        .or_else(|_| std::env::var("GITLAB_TOKEN"))
        .ok();

    let mut request = client.get(&url);
    if let Some(t) = token.as_ref() {
        request = request.header("PRIVATE-TOKEN", t);
    }

    let resp = request.send()?;
    if !resp.status().is_success() {
        let code = resp.status();
        if code == reqwest::StatusCode::NOT_FOUND || code == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "Failed to download GitLab repo {project_path} at {rev} (HTTP {code}).\n\
                 Tried clones via HTTPS & SSH, then archive download.\n\
                 If this repository is private, set an access token in GITLAB_TOKEN."
            );
        } else {
            anyhow::bail!("Failed to download GitLab repo {project_path} at {rev} (HTTP {code})");
        }
    }

    let bytes = resp.bytes()?;
    let gz = flate2::read::GzDecoder::new(bytes.as_ref());
    let mut archive = tar::Archive::new(gz);

    // Extract contents while stripping the top-level folder
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let mut comps = path.components();
        comps.next(); // strip top-level folder
        let stripped: PathBuf = comps.as_path().to_path_buf();
        let out_path = dest_dir.join(stripped);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out_path)?;
    }
    Ok(())
}

// Create a symlink inside `<workspace>/.pcb/<alias>/<sub_path>` pointing to `target`.
fn expose_alias_symlink(
    workspace_root: &Path,
    alias: &str,
    sub_path: &Path,
    target: &Path,
) -> anyhow::Result<()> {
    let dest_base = workspace_root.join(".pcb").join("cache").join(alias);
    let dest = if sub_path.as_os_str().is_empty() {
        dest_base.clone()
    } else {
        dest_base.join(sub_path)
    };

    if dest.exists() {
        return Ok(()); // already linked/copied
    }

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    {
        unix_fs::symlink(target, &dest)?;
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            win_fs::symlink_dir(target, &dest)?;
        } else {
            win_fs::symlink_file(target, &dest)?;
        }
    }
    Ok(())
}

/// Default implementation of RemoteFetcher that handles downloading and caching
/// remote resources (GitHub repos, GitLab repos, packages).
#[derive(Debug)]
pub struct DefaultRemoteFetcher {
    metadata_cache: std::sync::Mutex<
        std::collections::HashMap<pcb_zen_core::RemoteRef, pcb_zen_core::RemoteRefMeta>,
    >,
}

impl Default for DefaultRemoteFetcher {
    fn default() -> Self {
        Self {
            metadata_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl pcb_zen_core::RemoteFetcher for DefaultRemoteFetcher {
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        // Step 1: Ensure remote is cached (downloads if needed)
        let cache_root = ensure_remote_cached(spec)?;

        // Step 2: Resolve specific file path within cache and create symlinks
        let file_path = ensure_symlinks(spec, workspace_root, &cache_root)?;

        // Step 3: Classify Git repository and cache metadata (only if not already cached)
        let remote_ref = spec
            .remote_ref()
            .expect("remote specs should always have remote_ref");
        let mut cache = self.metadata_cache.lock().unwrap();
        if let Entry::Vacant(entry) = cache.entry(remote_ref) {
            if let Some(metadata) = classify_remote(&cache_root, spec) {
                entry.insert(metadata);
            }
        }

        Ok(file_path)
    }

    fn remote_ref_meta(
        &self,
        remote_ref: &pcb_zen_core::RemoteRef,
    ) -> Option<pcb_zen_core::RemoteRefMeta> {
        self.metadata_cache.lock().unwrap().get(remote_ref).cloned()
    }
}
// Add unit tests for LoadSpec::parse
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_without_tag() {
        let spec = LoadSpec::parse("@stdlib/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: DEFAULT_PKG_TAG.to_string(),
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn parses_package_with_tag_and_root_path() {
        let spec = LoadSpec::parse("@stdlib:1.2.3");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "1.2.3".to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    fn parses_github_with_rev_and_path() {
        let spec = LoadSpec::parse("@github/foo/bar:abc123/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn parses_github_without_rev() {
        let spec = LoadSpec::parse("@github/foo/bar/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: DEFAULT_GITHUB_REV.to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn parses_github_repo_root_with_rev() {
        let spec = LoadSpec::parse("@github/foo/bar:main");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: "main".to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    fn parses_github_repo_root_with_long_commit() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let input = format!("@github/foo/bar:{sha}");
        let spec = LoadSpec::parse(&input);
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: sha.to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    fn parses_gitlab_with_rev_and_path() {
        let spec = LoadSpec::parse("@gitlab/foo/bar:abc123/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn parses_gitlab_without_rev() {
        let spec = LoadSpec::parse("@gitlab/foo/bar/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: DEFAULT_GITLAB_REV.to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn parses_gitlab_repo_root_with_rev() {
        let spec = LoadSpec::parse("@gitlab/foo/bar:main");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: "main".to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    fn parses_gitlab_repo_root_with_long_commit() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let input = format!("@gitlab/foo/bar:{sha}");
        let spec = LoadSpec::parse(&input);
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: sha.to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    fn parses_gitlab_nested_groups_with_rev() {
        let spec = LoadSpec::parse("@gitlab/kicad/libraries/kicad-symbols:main/Device.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "kicad/libraries/kicad-symbols".to_string(),
                rev: "main".to_string(),
                path: PathBuf::from("Device.kicad_sym"),
            })
        );
    }

    #[test]
    fn parses_gitlab_simple_without_rev_with_file_path() {
        // Without revision, first 2 parts are project
        let spec = LoadSpec::parse("@gitlab/user/repo/src/main.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "user/repo".to_string(),
                rev: DEFAULT_GITLAB_REV.to_string(),
                path: PathBuf::from("src/main.zen"),
            })
        );
    }

    #[test]
    fn parses_gitlab_nested_groups_no_file() {
        let spec = LoadSpec::parse("@gitlab/kicad/libraries/kicad-symbols:v7.0.0");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "kicad/libraries/kicad-symbols".to_string(),
                rev: "v7.0.0".to_string(),
                path: PathBuf::new(),
            })
        );
    }

    #[test]
    #[ignore]
    fn downloads_github_repo_by_commit_tarball() {
        // This test performs a real network request to GitHub. It is ignored by default and
        // can be run explicitly with `cargo test -- --ignored`.
        use tempfile::tempdir;

        // Public, tiny repository & commit known to exist for years.
        let user = "octocat";
        let repo = "Hello-World";
        // Commit from Octocat's canonical example repository that is present in the
        // public API and codeload tarballs.
        let rev = "7fd1a60b01f91b314f59955a4e4d4e80d8edf11d";

        let tmp = tempdir().expect("create temp dir");
        let dest = tmp.path().join("repo");

        // Attempt to fetch solely via HTTPS tarball (git may not be available in CI).
        download_and_unpack_github_repo(user, repo, rev, &dest)
            .expect("download and unpack GitHub tarball");

        // Ensure some expected file exists. The Hello-World repo always contains a README.
        let readme_exists = dest.join("README").exists() || dest.join("README.md").exists();
        assert!(
            readme_exists,
            "expected README file to exist in extracted repo"
        );
    }

    #[test]
    fn default_package_aliases() {
        // Test that default aliases are available
        let aliases = pcb_zen_core::LoadSpec::default_package_aliases();

        assert_eq!(
            aliases.get("kicad-symbols").map(|a| &a.target),
            Some(&"@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string())
        );
        assert_eq!(
            aliases.get("stdlib").map(|a| &a.target),
            Some(&"@github/diodeinc/stdlib:HEAD".to_string())
        );
    }

    #[test]
    fn default_aliases_without_workspace() {
        // Test that default aliases work
        let aliases = pcb_zen_core::LoadSpec::default_package_aliases();

        // Test kicad-symbols alias
        assert_eq!(
            aliases.get("kicad-symbols").map(|a| &a.target),
            Some(&"@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string())
        );

        // Test stdlib alias
        assert_eq!(
            aliases.get("stdlib").map(|a| &a.target),
            Some(&"@github/diodeinc/stdlib:HEAD".to_string())
        );

        // Test non-existent alias
        assert!(aliases.get("nonexistent").is_none());
    }

    #[test]
    fn alias_with_custom_tag_override() {
        // Test that custom tags override the default alias tags

        // Test 1: Package alias with tag override
        let spec = LoadSpec::parse("@stdlib:zen/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "zen".to_string(),
                path: PathBuf::from("math.zen"),
            })
        );

        // Test 2: Verify that default tag is used when not specified
        let spec = LoadSpec::parse("@stdlib/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: DEFAULT_PKG_TAG.to_string(),
                path: PathBuf::from("math.zen"),
            })
        );

        // Test 3: KiCad symbols with custom version
        let spec = LoadSpec::parse("@kicad-symbols:8.0.0/Device.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "kicad-symbols".to_string(),
                tag: "8.0.0".to_string(),
                path: PathBuf::from("Device.kicad_sym"),
            })
        );
    }
}
