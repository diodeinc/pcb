use pcb_zen_core::LoadSpec;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(windows)]
use std::os::windows::fs as win_fs;

// Re-export constants from LoadSpec for backward compatibility
pub use pcb_zen_core::load_spec::{DEFAULT_GITHUB_REV, DEFAULT_GITLAB_REV, DEFAULT_PKG_TAG};

/// Download and cache remote resources (Package, GitHub, GitLab specs).
///
/// This function only handles remote specs - local paths should be resolved
/// by the CoreLoadResolver before reaching this function.
///
/// The returned path is guaranteed to exist on success.
fn materialise_remote(spec: &LoadSpec, workspace_root: Option<&Path>) -> anyhow::Result<PathBuf> {
    match spec {
        LoadSpec::Path { .. } | LoadSpec::WorkspacePath { .. } => {
            // Local specs should not reach here
            anyhow::bail!("materialise_remote only handles remote specs, not local paths")
        }
        LoadSpec::Package { package, tag, path } => {
            let cache_root = cache_dir()?.join("packages").join(package).join(tag);

            // Ensure package tarball is present/unpacked.
            if !cache_root.exists() {
                download_and_unpack_package(package, tag, &cache_root)?;
            }

            let local_path = if path.as_os_str().is_empty() {
                cache_root.clone()
            } else {
                cache_root.join(path)
            };

            if !local_path.exists() {
                anyhow::bail!(
                    "File {} not found in package {}:{}",
                    path.display(),
                    package,
                    tag
                );
            }

            // Expose in .pcb for direct package reference
            if let Some(root) = workspace_root {
                let _ = expose_alias_symlink(root, package, path, &local_path);
            }

            Ok(local_path)
        }
        LoadSpec::Github {
            user,
            repo,
            rev,
            path,
        } => {
            let cache_root = cache_dir()?.join("github").join(user).join(repo).join(rev);

            // Ensure the repo has been fetched & unpacked.
            if !cache_root.exists() {
                download_and_unpack_github_repo(user, repo, rev, &cache_root)?;
            }

            let local_path = cache_root.join(path);
            if !local_path.exists() {
                anyhow::bail!(
                    "Path {} not found inside cached GitHub repo",
                    path.display()
                );
            }
            if let Some(root) = workspace_root {
                let folder_name = format!(
                    "github{}{}{}{}{}",
                    std::path::MAIN_SEPARATOR,
                    user,
                    std::path::MAIN_SEPARATOR,
                    repo,
                    std::path::MAIN_SEPARATOR
                );
                let folder_name = format!("{folder_name}{rev}");
                let _ = expose_alias_symlink(root, &folder_name, path, &local_path);
            }
            Ok(local_path)
        }
        LoadSpec::Gitlab {
            project_path,
            rev,
            path,
        } => {
            let cache_root = cache_dir()?.join("gitlab").join(project_path).join(rev);

            // Ensure the repo has been fetched & unpacked.
            if !cache_root.exists() {
                download_and_unpack_gitlab_repo(project_path, rev, &cache_root)?;
            }

            let local_path = cache_root.join(path);
            if !local_path.exists() {
                anyhow::bail!(
                    "Path {} not found inside cached GitLab repo",
                    path.display()
                );
            }
            if let Some(root) = workspace_root {
                let folder_name = format!(
                    "gitlab{}{}{}",
                    std::path::MAIN_SEPARATOR,
                    project_path,
                    std::path::MAIN_SEPARATOR
                );
                let folder_name = format!("{folder_name}{rev}");
                let _ = expose_alias_symlink(root, &folder_name, path, &local_path);
            }
            Ok(local_path)
        }
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

fn download_and_unpack_package(_package: &str, _tag: &str, _dest_dir: &Path) -> anyhow::Result<()> {
    anyhow::bail!("Package file download not yet implemented")
}

fn download_and_unpack_github_repo(
    user: &str,
    repo: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitHub repo {user}/{repo} @ {rev}");

    // Reject abbreviated commit hashes – we only support full 40-character SHAs or branch/tag names.
    if looks_like_git_sha(rev) && rev.len() < 40 {
        anyhow::bail!(
            "Abbreviated commit hashes ({} characters) are not supported - please use the full 40-character commit SHA or a branch/tag name (got '{}').",
            rev.len(),
            rev
        );
    }

    let effective_rev = rev.to_string();

    // Helper that attempts to clone via the system `git` binary. Returns true on
    // success, false on failure (so we can fall back to other mechanisms).
    let try_git_clone = |remote_url: &str| -> anyhow::Result<bool> {
        // Ensure parent dirs exist so `git clone` can create `dest_dir`.
        if let Some(parent) = dest_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Build the basic clone command.
        let mut cmd = Command::new("git");
        cmd.arg("clone");
        cmd.arg("--depth");
        cmd.arg("1");
        cmd.arg("--quiet"); // Suppress output

        // Decide how to treat the requested revision.
        let rev_is_head = effective_rev.eq_ignore_ascii_case("HEAD");
        let rev_is_sha = looks_like_git_sha(&effective_rev);

        // For branch or tag names we can use the efficient `--branch <name>` clone.
        // For commit SHAs we first perform a regular shallow clone of the default branch
        // and then fetch & checkout the desired commit afterwards.
        if !rev_is_head && !rev_is_sha {
            cmd.arg("--branch");
            cmd.arg(&effective_rev);
            cmd.arg("--single-branch");
        }

        cmd.arg(remote_url);
        cmd.arg(dest_dir);

        // Silence all output
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        log::debug!("Running command: {cmd:?}");
        match cmd.status() {
            Ok(status) if status.success() => {
                if rev_is_head {
                    // Nothing to do – HEAD already checked out.
                    return Ok(true);
                }

                if rev_is_sha {
                    // Fetch the specific commit (shallow) and check it out.
                    let fetch_ok = Command::new("git")
                        .arg("-C")
                        .arg(dest_dir)
                        .arg("fetch")
                        .arg("--quiet")
                        .arg("--depth")
                        .arg("1")
                        .arg("origin")
                        .arg(&effective_rev)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);

                    if !fetch_ok {
                        return Ok(false);
                    }
                }

                // Detach checkout for both commit SHAs and branch/tag when we didn't use --branch.
                let checkout_ok = Command::new("git")
                    .arg("-C")
                    .arg(dest_dir)
                    .arg("checkout")
                    .arg("--quiet")
                    .arg(&effective_rev)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);

                if checkout_ok {
                    return Ok(true);
                }

                // Fall through – treat as failure so other strategies can try.
                Ok(false)
            }
            _ => Ok(false),
        }
    };

    // Strategy 1: system git with HTTPS (respects credential helpers).
    let https_url = format!("https://github.com/{user}/{repo}.git");
    if git_is_available() && try_git_clone(&https_url)? {
        return Ok(());
    }

    // Strategy 2: system git with SSH.
    let ssh_url = format!("git@github.com:{user}/{repo}.git");
    if git_is_available() && try_git_clone(&ssh_url)? {
        return Ok(());
    }

    // Strategy 3: fall back to unauthenticated or token-authenticated codeload tarball.

    // Example tarball URL: https://codeload.github.com/<user>/<repo>/tar.gz/<rev>
    let url = format!("https://codeload.github.com/{user}/{repo}/tar.gz/{effective_rev}");

    // Build a reqwest client so we can attach an Authorization header when needed
    let client = reqwest::blocking::Client::builder()
        .user_agent("diode-star-loader")
        .build()?;

    // Allow users to pass a token for private repositories via env var.
    let token = std::env::var("DIODE_GITHUB_TOKEN")
        .or_else(|_| std::env::var("GITHUB_TOKEN"))
        .ok();

    let mut request = client.get(&url);
    if let Some(t) = token.as_ref() {
        request = request.header("Authorization", format!("token {t}"));
    }

    // GitHub tarball endpoint returns 302 to S3; reqwest follows automatically and
    // does **not** forward the Authorization header (which is fine – S3 URL is
    // pre-signed).  We keep redirects enabled via the default policy.

    let resp = request.send()?;
    if !resp.status().is_success() {
        let code = resp.status();
        if code == reqwest::StatusCode::NOT_FOUND || code == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "Failed to download GitHub repo {user}/{repo} at {rev} (HTTP {code}).\n\
                 Tried clones via HTTPS & SSH, then tarball download.\n\
                 If this repository is private please set an access token in the `GITHUB_TOKEN` environment variable, e.g.:\n\
                     export GITHUB_TOKEN=$(gh auth token)"
            );
        } else {
            anyhow::bail!(
                "Failed to download repo archive {url} (HTTP {code}) after trying git clone."
            );
        }
    }
    let bytes = resp.bytes()?;

    // Decompress tar.gz in-memory.
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);

    // The tarball contains a single top-level directory like <repo>-<rev>/...
    // We extract its contents into dest_dir while stripping the first component.
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

fn download_and_unpack_gitlab_repo(
    project_path: &str,
    rev: &str,
    dest_dir: &Path,
) -> anyhow::Result<()> {
    log::info!("Fetching GitLab repo {project_path} @ {rev}");

    // Reject abbreviated commit hashes – we only support full 40-character SHAs or branch/tag names.
    if looks_like_git_sha(rev) && rev.len() < 40 {
        anyhow::bail!(
            "Abbreviated commit hashes ({} characters) are not supported – please use the full 40-character commit SHA or a branch/tag name (got '{}').",
            rev.len(),
            rev
        );
    }

    let effective_rev = rev.to_string();

    // Helper that attempts to clone via the system `git` binary. Returns true on
    // success, false on failure (so we can fall back to other mechanisms).
    let try_git_clone = |remote_url: &str| -> anyhow::Result<bool> {
        // Ensure parent dirs exist so `git clone` can create `dest_dir`.
        if let Some(parent) = dest_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Build the basic clone command.
        let mut cmd = Command::new("git");
        cmd.arg("clone");
        cmd.arg("--depth");
        cmd.arg("1");
        cmd.arg("--quiet"); // Suppress output

        // Decide how to treat the requested revision.
        let rev_is_head = effective_rev.eq_ignore_ascii_case("HEAD");
        let rev_is_sha = looks_like_git_sha(&effective_rev);

        // For branch or tag names we can use the efficient `--branch <name>` clone.
        // For commit SHAs we first perform a regular shallow clone of the default branch
        // and then fetch & checkout the desired commit afterwards.
        if !rev_is_head && !rev_is_sha {
            cmd.arg("--branch");
            cmd.arg(&effective_rev);
            cmd.arg("--single-branch");
        }

        cmd.arg(remote_url);
        cmd.arg(dest_dir);

        // Silence all output
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        log::debug!("Running command: {cmd:?}");
        match cmd.status() {
            Ok(status) if status.success() => {
                if rev_is_head {
                    // Nothing to do – HEAD already checked out.
                    return Ok(true);
                }

                if rev_is_sha {
                    // Fetch the specific commit (shallow) and check it out.
                    let fetch_ok = Command::new("git")
                        .arg("-C")
                        .arg(dest_dir)
                        .arg("fetch")
                        .arg("--quiet")
                        .arg("--depth")
                        .arg("1")
                        .arg("origin")
                        .arg(&effective_rev)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);

                    if !fetch_ok {
                        return Ok(false);
                    }
                }

                // Detach checkout for both commit SHAs and branch/tag when we didn't use --branch.
                let checkout_ok = Command::new("git")
                    .arg("-C")
                    .arg(dest_dir)
                    .arg("checkout")
                    .arg("--quiet")
                    .arg(&effective_rev)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);

                if checkout_ok {
                    return Ok(true);
                }

                // Fall through – treat as failure so other strategies can try.
                Ok(false)
            }
            _ => Ok(false),
        }
    };

    // Strategy 1: system git with HTTPS (respects credential helpers).
    let https_url = format!("https://gitlab.com/{project_path}.git");
    if git_is_available() && try_git_clone(&https_url)? {
        return Ok(());
    }

    // Strategy 2: system git with SSH.
    let ssh_url = format!("git@gitlab.com:{project_path}.git");
    if git_is_available() && try_git_clone(&ssh_url)? {
        return Ok(());
    }

    // Strategy 3: fall back to unauthenticated or token-authenticated archive tarball.
    // GitLab's archive API: https://gitlab.com/api/v4/projects/{id}/repository/archive?sha={rev}
    // We need to URL-encode the project path (user/repo) for the API
    let encoded_project_path = project_path.replace("/", "%2F");
    let url = format!("https://gitlab.com/api/v4/projects/{encoded_project_path}/repository/archive.tar.gz?sha={effective_rev}");

    // Build a reqwest client so we can attach an Authorization header when needed
    let client = reqwest::blocking::Client::builder()
        .user_agent("diode-star-loader")
        .build()?;

    // Allow users to pass a token for private repositories via env var.
    let token = std::env::var("DIODE_GITLAB_TOKEN")
        .or_else(|_| std::env::var("GITLAB_TOKEN"))
        .ok();

    let mut request = client.get(&url);
    if let Some(t) = token.as_ref() {
        // GitLab uses a different header format
        request = request.header("PRIVATE-TOKEN", t);
    }

    let resp = request.send()?;
    if !resp.status().is_success() {
        let code = resp.status();
        if code == reqwest::StatusCode::NOT_FOUND || code == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "Failed to download GitLab repo {project_path} at {rev} (HTTP {code}).\n\
                 Tried clones via HTTPS & SSH, then archive download.\n\
                 If this repository is private please set an access token in the `GITLAB_TOKEN` environment variable."
            );
        } else {
            anyhow::bail!(
                "Failed to download repo archive {url} (HTTP {code}) after trying git clone."
            );
        }
    }
    let bytes = resp.bytes()?;

    // Decompress tar.gz in-memory.
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(gz);

    // The tarball contains a single top-level directory like <repo>-<rev>-<hash>/...
    // We extract its contents into dest_dir while stripping the first component.
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

// Simple helper that checks whether the `git` executable is available on PATH.
fn git_is_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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

// Determine whether the given revision string looks like a Git commit SHA (short or long).
// We accept hexadecimal strings of length 7–40 (Git allows abbreviated hashes as short as 7).
fn looks_like_git_sha(rev: &str) -> bool {
    if !(7..=40).contains(&rev.len()) {
        return false;
    }
    rev.chars().all(|c| c.is_ascii_hexdigit())
}

/// Default implementation of RemoteFetcher that handles downloading and caching
/// remote resources (GitHub repos, GitLab repos, packages).
#[derive(Debug, Clone)]
pub struct DefaultRemoteFetcher;

impl pcb_zen_core::RemoteFetcher for DefaultRemoteFetcher {
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        workspace_root: Option<&Path>,
    ) -> Result<PathBuf, anyhow::Error> {
        // Use the existing materialise_load function
        materialise_remote(spec, workspace_root)
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
            aliases.get("kicad-symbols"),
            Some(&"@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string())
        );
        assert_eq!(
            aliases.get("stdlib"),
            Some(&"@github/diodeinc/stdlib:HEAD".to_string())
        );
    }

    #[test]
    fn default_aliases_without_workspace() {
        // Test that default aliases work
        let aliases = pcb_zen_core::LoadSpec::default_package_aliases();

        // Test kicad-symbols alias
        assert_eq!(
            aliases.get("kicad-symbols"),
            Some(&"@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string())
        );

        // Test stdlib alias
        assert_eq!(
            aliases.get("stdlib"),
            Some(&"@github/diodeinc/stdlib:HEAD".to_string())
        );

        // Test non-existent alias
        assert_eq!(aliases.get("nonexistent"), None);
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
