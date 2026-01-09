use pcb_zen_core::LoadSpec;
use std::path::{Path, PathBuf};

use crate::git;

// Re-export constants from LoadSpec for backward compatibility
pub use pcb_zen_core::load_spec::{DEFAULT_GITHUB_REV, DEFAULT_GITLAB_REV, DEFAULT_PKG_TAG};

/// Resolve file path within cache.
fn resolve_cache_path(spec: &LoadSpec, cache_root: &Path) -> PathBuf {
    let path = spec.path();
    if path.as_os_str().is_empty() {
        cache_root.to_path_buf()
    } else {
        cache_root.join(path)
    }
}

/// Ensure the remote is cached and return the root directory of the checked-out revision.
/// Returns the directory containing the checked-out repository or unpacked package.
/// For Git repos, uses worktrees for efficient multi-version support.
pub fn ensure_remote_cached(spec: &LoadSpec) -> anyhow::Result<PathBuf> {
    match spec {
        LoadSpec::Github {
            user, repo, rev, ..
        } => {
            let cache_root = cache_dir()?.join("github").join(user).join(repo).join(rev);
            download_and_unpack_github_repo(user, repo, rev, &cache_root)?;
            Ok(cache_root)
        }
        LoadSpec::Gitlab {
            project_path, rev, ..
        } => {
            let cache_root = cache_dir()?.join("gitlab").join(project_path).join(rev);
            download_and_unpack_gitlab_repo(project_path, rev, &cache_root)?;
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

/// Ensure a git repository is available as a worktree using a shared bare repository.
/// This approach minimizes disk space and network usage when multiple refs from the
/// same repository are needed.
fn ensure_git_worktree(
    remote_urls: &[String],
    repo_root: &Path,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    // Fast path: if worktree exists, return immediately
    // This is safe because we use atomic rename during creation
    if dest_dir.exists() {
        return Ok(());
    }

    if !git::is_available() {
        anyhow::bail!("Git is required but not available on this system");
    }

    let bare_repo = repo_root.join(".repo");
    let lock_file = repo_root.join(".repo.lock");

    // Acquire lock for this repository to prevent race conditions
    std::fs::create_dir_all(repo_root)?;
    let mut _lock = fslock::LockFile::open(&lock_file)?;
    _lock.lock()?;

    // Double-check after acquiring lock (another process may have created it)
    if dest_dir.exists() {
        return Ok(());
    }

    // Ensure bare repository exists
    if !bare_repo.exists() {
        let mut last_error = None;
        for remote_url in remote_urls {
            log::debug!("Cloning bare repository: {remote_url}");

            // Try with filter first (network optimization), fall back without (for file:// URLs)
            let result = git::clone_bare_with_filter(remote_url, &bare_repo).or_else(|e| {
                log::debug!("Clone with filter failed: {e}, trying without filter");
                git::clone_bare(remote_url, &bare_repo)
            });

            match result {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(e) => {
                    log::debug!("Failed to clone from {remote_url}: {e}");
                    last_error = Some(e);
                }
            }
        }

        if let Some(e) = last_error {
            return Err(e);
        }
    }

    // Fetch updates (best effort)
    log::debug!("Fetching updates in bare repository");
    let _ = git::fetch_in_bare_repo(&bare_repo);

    // Prune stale worktree metadata before creating new worktree
    log::debug!("Pruning stale worktrees");
    let _ = git::prune_worktrees(&bare_repo);

    // Create worktree to a temp directory first, then atomically rename
    // This ensures other processes don't see a partially-created worktree
    let temp_dir = repo_root.join(format!(".tmp-{}", uuid::Uuid::new_v4()));

    log::debug!("Creating worktree for {rev}");
    git::create_worktree(&bare_repo, &temp_dir, rev)?;

    // Atomically rename temp directory to final location
    std::fs::rename(&temp_dir, dest_dir)?;

    Ok(())
}

pub fn download_and_unpack_github_repo(
    user: &str,
    repo: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitHub repo {user}/{repo} @ {rev}");

    let remote_urls = [
        format!("https://github.com/{user}/{repo}.git"),
        format!("git@github.com:{user}/{repo}.git"),
    ];

    // Get the repository root (parent of the rev-specific directory)
    let repo_root = dest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid destination directory"))?;

    ensure_git_worktree(&remote_urls, repo_root, rev, dest_dir)
        .map_err(|_| anyhow::anyhow!("Failed to fetch GitHub repo {user}/{repo}@{rev}"))
}

pub fn download_and_unpack_gitlab_repo(
    project_path: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitLab repo {project_path} @ {rev}");

    let remote_urls = [
        format!("https://gitlab.com/{project_path}.git"),
        format!("git@gitlab.com:{project_path}.git"),
    ];

    // Get the repository root (parent of the rev-specific directory)
    let repo_root = dest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid destination directory"))?;

    ensure_git_worktree(&remote_urls, repo_root, rev, dest_dir)
        .map_err(|_| anyhow::anyhow!("Failed to fetch GitLab repo {project_path}@{rev}"))
}

/// Default implementation of RemoteFetcher that handles downloading and caching
/// remote resources (GitHub repos, GitLab repos, packages).
#[derive(Debug, Default)]
pub struct DefaultRemoteFetcher {}

impl pcb_zen_core::RemoteFetcher for DefaultRemoteFetcher {
    fn fetch_remote(&self, spec: &LoadSpec) -> Result<PathBuf, anyhow::Error> {
        // Ensure remote is cached (downloads if needed) and resolve file path
        let cache_root = ensure_remote_cached(spec)?;
        Ok(resolve_cache_path(spec, &cache_root))
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
    fn default_package_aliases() {
        // Test that default aliases are available
        let aliases = pcb_zen_core::LoadSpec::default_package_aliases();

        assert_eq!(
            aliases.get("kicad-symbols").map(|a| &a.target),
            Some(&"@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string())
        );
        assert_eq!(
            aliases.get("stdlib").map(|a| &a.target),
            Some(&format!(
                "@github/diodeinc/stdlib:v{}",
                pcb_zen_core::STDLIB_VERSION
            ))
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

        // Test stdlib alias (now uses pinned version instead of HEAD)
        assert_eq!(
            aliases.get("stdlib").map(|a| &a.target),
            Some(&format!(
                "@github/diodeinc/stdlib:v{}",
                pcb_zen_core::STDLIB_VERSION
            ))
        );

        // Test non-existent alias
        assert!(!aliases.contains_key("nonexistent"));
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
