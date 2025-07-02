use once_cell::sync::Lazy;
use pcb_zen_core::{FileProvider, LoadResolver};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

#[cfg(unix)]
use std::os::unix::fs as unix_fs;
#[cfg(windows)]
use std::os::windows::fs as win_fs;

/// Default tag that is assumed when the caller does not specify one in a
/// package spec, e.g. `@mypkg/utils.zen`.
pub(crate) const DEFAULT_PKG_TAG: &str = "latest";

/// Default git revision that is assumed when the caller omits one in a GitHub
/// spec, e.g. `@github/user/repo/path.zen`.
pub(crate) const DEFAULT_GITHUB_REV: &str = "HEAD";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LoadSpec {
    Package {
        package: String,
        tag: String,
        path: PathBuf,
    },
    Github {
        user: String,
        repo: String,
        rev: String,
        path: PathBuf,
    },
}

/// Parse the raw string passed to `load()` into a [`LoadSpec`].
///
/// The supported grammar is:
///
/// • **Package reference** – `"@<package>[:<tag>]/<optional/path>"`.
///   If `<tag>` is omitted the [`DEFAULT_PKG_TAG`] (currently `"latest"`) is
///   assumed.
///   Example: `"@stdlib:1.2.3/math.zen"` or `"@stdlib/math.zen"`.
///
/// • **GitHub repository** –
///   `"@github/<user>/<repo>[:<rev>]/<path>"`.
///   If `<rev>` is omitted the special value [`DEFAULT_GITHUB_REV`] (currently
///   `"HEAD"`) is assumed.
///   The `<rev>` component can be a branch name, tag, or a short/long commit
///   SHA (7–40 hexadecimal characters).
///   Example: `"@github/foo/bar:abc123/scripts/build.zen".
///
/// The function does not touch the filesystem – it only performs syntactic
/// parsing.
pub fn parse_load_spec(s: &str) -> Option<LoadSpec> {
    if let Some(rest) = s.strip_prefix("@github/") {
        // GitHub: @github/user/repo:rev/path  (must come before generic "@pkg" handling)
        let mut user_repo_rev_and_path = rest.splitn(3, '/');
        let user = user_repo_rev_and_path.next().unwrap_or("").to_string();
        let repo_and_rev = user_repo_rev_and_path.next().unwrap_or("");
        let remaining_path = user_repo_rev_and_path.next().unwrap_or("");

        let (repo, rev) = if let Some((repo, rev)) = repo_and_rev.split_once(':') {
            (repo.to_string(), rev.to_string())
        } else {
            (repo_and_rev.to_string(), DEFAULT_GITHUB_REV.to_string())
        };

        Some(LoadSpec::Github {
            user,
            repo,
            rev,
            path: PathBuf::from(remaining_path),
        })
    } else if let Some(rest) = s.strip_prefix('@') {
        // Generic package: @<pkg>[:<tag>]/optional/path
        // rest looks like "pkg[:tag]/path..." or just "pkg"/"pkg:tag"
        let mut parts = rest.splitn(2, '/');
        let pkg_and_tag = parts.next().unwrap_or("");
        let rel_path = parts.next().unwrap_or("");

        let (package, tag) = if let Some((pkg, tag)) = pkg_and_tag.split_once(':') {
            (pkg.to_string(), tag.to_string())
        } else {
            (pkg_and_tag.to_string(), DEFAULT_PKG_TAG.to_string())
        };

        Some(LoadSpec::Package {
            package,
            tag,
            path: PathBuf::from(rel_path),
        })
    } else {
        None
    }
}

/// Ensure that the resource referenced by `spec` exists on the **local**
/// filesystem and return its absolute path.
///
/// * **Local** specs are returned unchanged.
/// * **Package** and **GitHub** specs are fetched (and cached) under the
///   user's cache directory on first use. Subsequent invocations will reuse
///   the cached copy.
///
/// The returned path is guaranteed to exist on success.
pub fn materialise_load(spec: &LoadSpec, workspace_root: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let LoadSpec::Package { package, tag, path } = spec {
        if let Some(root) = workspace_root {
            if let Some(target) = lookup_package_alias(root, package) {
                // Build new load string by appending any extra path the caller asked for.
                let mut new_spec_str = target.clone();
                if !path.as_os_str().is_empty() {
                    new_spec_str = format!(
                        "{}/{}",
                        new_spec_str.trim_end_matches('/'),
                        path.to_string_lossy()
                    );
                }

                // If caller explicitly specified a tag (non-default) we warn and ignore –
                // alias definitions should embed the desired tag.
                if tag != DEFAULT_PKG_TAG {
                    log::debug!("ignoring tag '{tag}' on alias '{package}'");
                }

                let new_spec = parse_load_spec(&new_spec_str).ok_or_else(|| {
                    anyhow::anyhow!("Failed to parse load spec: {}", new_spec_str)
                })?;

                // Recurse to resolve/load and obtain the concrete local path.
                let resolved_path = materialise_load(&new_spec, workspace_root)?;

                // Attempt to expose in .pcb folder via symlink.
                if let Err(e) = expose_alias_symlink(root, package, path, &resolved_path) {
                    log::debug!("failed to create alias symlink: {e}");
                }

                return Ok(resolved_path);
            }
        }
        // No alias match – proceed with normal package handling below, but ensure we expose a symlink afterwards.
    }

    match spec {
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

            // Expose in .pcb for direct package reference (non-alias)
            if let Some(root) = workspace_root {
                let rel_path = path.clone();
                let _ = expose_alias_symlink(root, package, &rel_path, &local_path);
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
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────

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
        let dir = base.join("diode_star");
        if std::fs::create_dir_all(&dir).is_ok() {
            return Ok(dir);
        }
        // If we failed to create the directory (e.g. permission denied) we fall through
        // to the temporary directory fallback below instead of erroring out immediately.
    }

    // 3. As a last resort fall back to a writable path under the system temp directory. While
    //    this is not cached across runs, it ensures functionality in locked-down CI systems.
    let dir = std::env::temp_dir().join("diode_star_cache");
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

/// Walk up the directory tree starting at `start` until a directory containing
/// `pcb.toml` is found. Returns `Some(PathBuf)` pointing at that directory or
/// `None` if we reach the filesystem root without finding one.
pub fn find_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        // For files we search from their parent directory.
        start.parent()
    } else {
        Some(start)
    };

    while let Some(dir) = current {
        if dir.join("pcb.toml").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

// Package alias helpers

/// Thread-safe cache: workspace root → alias map.
static ALIAS_CACHE: Lazy<
    Mutex<std::collections::HashMap<PathBuf, std::collections::HashMap<String, String>>>,
> = Lazy::new(|| Mutex::new(std::collections::HashMap::new()));

/// Return the package alias map for the given workspace root. Parsed once and cached.
fn package_aliases(root: &Path) -> std::collections::HashMap<String, String> {
    let mut guard = ALIAS_CACHE.lock().expect("alias cache poisoned");
    if let Some(map) = guard.get(root) {
        return map.clone();
    }

    let mut map = std::collections::HashMap::new();
    let toml_path = root.join("pcb.toml");
    if let Ok(contents) = std::fs::read_to_string(&toml_path) {
        // Deserialize only the [packages] table to avoid large structs.
        #[derive(Debug, Deserialize)]
        struct PkgRoot {
            packages: Option<std::collections::HashMap<String, String>>,
        }

        if let Ok(parsed) = toml::from_str::<PkgRoot>(&contents) {
            if let Some(pkgs) = parsed.packages {
                map = pkgs;
            }
        }
    }

    guard.insert(root.to_path_buf(), map.clone());
    map
}

/// Look up an alias (package name) in workspace root. Returns mapped string if present.
fn lookup_package_alias(root: &Path, alias: &str) -> Option<String> {
    package_aliases(root).get(alias).cloned()
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

// Add unit tests for parse_load_spec
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_without_tag() {
        let spec = parse_load_spec("@stdlib/math.zen");
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
        let spec = parse_load_spec("@stdlib:1.2.3");
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
        let spec = parse_load_spec("@github/foo/bar:abc123/scripts/build.zen");
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
        let spec = parse_load_spec("@github/foo/bar/scripts/build.zen");
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
        let spec = parse_load_spec("@github/foo/bar:main");
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
        let spec = parse_load_spec(&input);
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
}

/// Load resolver that handles loading files from remote dependencies.
#[derive(Debug, Clone)]
pub struct RemoteLoadResolver;

impl LoadResolver for RemoteLoadResolver {
    fn resolve_path(
        &self,
        _file_provider: &dyn FileProvider,
        load_path: &str,
        current_file: &std::path::Path,
    ) -> Result<std::path::PathBuf, anyhow::Error> {
        // Parse the load spec
        if let Some(spec) = parse_load_spec(load_path) {
            // Find workspace root starting from the current file
            let workspace_root = find_workspace_root(current_file);

            // Materialize the load (download if needed)
            materialise_load(&spec, workspace_root.as_deref())
        } else {
            anyhow::bail!("Invalid load spec: {}", load_path);
        }
    }
}
