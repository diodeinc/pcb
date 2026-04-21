//! SQLite-based cache index for package metadata

use anyhow::{Context, Result};
use pcb_ui::Spinner;
use pcb_zen_core::FileProvider;
use pcb_zen_core::config::split_repo_and_subpath;
use pcb_zen_core::embedded_stdlib::compute_stdlib_dir_hash;
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, params};
use semver::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::git;
use crate::tags;

/// Bump this when changing table schemas. Encoded in the filename so a new
/// version just creates a fresh file — no migration logic needed.
const SCHEMA_VERSION: i32 = 4;

pub struct CacheIndex {
    pool: Pool<SqliteConnectionManager>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePackage {
    pub module_path: String,
    pub version: String,
}

impl CacheIndex {
    pub fn open() -> Result<Self> {
        let path = index_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let manager = SqliteConnectionManager::file(&path).with_init(|c| {
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            c.pragma_update(None, "journal_mode", "WAL")
        });
        let pool = Pool::builder()
            .max_size(8)
            .error_handler(Box::new(r2d2::NopErrorHandler))
            .build(manager)
            .with_context(|| format!("Failed to create connection pool at {}", path.display()))?;

        let conn = pool.get()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS packages (
                module_path TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, version)
            );
            CREATE TABLE IF NOT EXISTS remote_packages (
                repo_url TEXT NOT NULL,
                package_path TEXT NOT NULL,
                latest_version TEXT NOT NULL,
                PRIMARY KEY (repo_url, package_path)
            );
            CREATE TABLE IF NOT EXISTS commit_metadata (
                repo_url TEXT NOT NULL,
                commit_hash TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                base_version TEXT,
                PRIMARY KEY (repo_url, commit_hash)
            );
            CREATE TABLE IF NOT EXISTS branch_commits (
                repo_url TEXT NOT NULL,
                branch TEXT NOT NULL,
                commit_hash TEXT NOT NULL,
                PRIMARY KEY (repo_url, branch)
            );",
        )?;
        drop(conn);

        Ok(Self { pool })
    }

    fn conn(&self) -> PooledConnection<SqliteConnectionManager> {
        self.pool.get().expect("failed to get connection from pool")
    }

    // Packages (dependencies with manifest hash)

    pub fn get_package(&self, module_path: &str, version: &str) -> Option<(String, String)> {
        self.conn()
            .query_row(
                "SELECT content_hash, manifest_hash FROM packages WHERE module_path = ?1 AND version = ?2",
                params![module_path, version],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_package(
        &self,
        module_path: &str,
        version: &str,
        content_hash: &str,
        manifest_hash: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO packages (module_path, version, content_hash, manifest_hash)
             VALUES (?1, ?2, ?3, ?4)",
            params![module_path, version, content_hash, manifest_hash],
        )?;
        Ok(())
    }

    // Remote packages (discovered from git tags)

    fn find_remote_package_cached(&self, file_url: &str) -> Option<RemotePackage> {
        let (repo_url, subpath) = split_repo_and_subpath(file_url);
        let without_file = subpath.rsplit_once('/')?.0;

        let conn = self.conn();
        let mut path = without_file;
        while !path.is_empty() {
            if let Some(version) = conn
                .query_row(
                    "SELECT latest_version FROM remote_packages WHERE repo_url = ?1 AND package_path = ?2",
                    params![repo_url, path],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            {
                return Some(RemotePackage {
                    module_path: format!("{}/{}", repo_url, path),
                    version,
                });
            }
            path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        }
        None
    }

    pub fn find_remote_package(&self, file_url: &str) -> Result<Option<RemotePackage>> {
        if let Some(result) = self.find_remote_package_cached(file_url) {
            return Ok(Some(result));
        }

        let (repo_url, subpath) = split_repo_and_subpath(file_url);
        if subpath.is_empty() {
            return Ok(None);
        }

        self.discover_remote_packages(repo_url)?;
        Ok(self.find_remote_package_cached(file_url))
    }

    fn discover_remote_packages(&self, repo_url: &str) -> Result<()> {
        let bare_dir = ensure_bare_repo(repo_url)?;
        let tags = git::list_all_tags(&bare_dir)?;

        let mut packages: BTreeMap<String, Version> = BTreeMap::new();
        for tag in tags {
            if let Some((pkg_path, version)) = tags::parse_tag(&tag) {
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

        let conn = self.conn();
        conn.execute(
            "DELETE FROM remote_packages WHERE repo_url = ?1",
            params![repo_url],
        )?;
        for (package_path, version) in packages {
            conn.execute(
                "INSERT INTO remote_packages (repo_url, package_path, latest_version) VALUES (?1, ?2, ?3)",
                params![repo_url, package_path, version.to_string()],
            )?;
        }

        Ok(())
    }
}

impl CacheIndex {
    // Commit metadata (for pseudo-version generation)

    pub fn get_commit_metadata(
        &self,
        repo_url: &str,
        commit_hash: &str,
    ) -> Option<(i64, Option<String>)> {
        self.conn()
            .query_row(
                "SELECT timestamp, base_version FROM commit_metadata WHERE repo_url = ?1 AND commit_hash = ?2",
                params![repo_url, commit_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_commit_metadata(
        &self,
        repo_url: &str,
        commit_hash: &str,
        timestamp: i64,
        base_version: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO commit_metadata (repo_url, commit_hash, timestamp, base_version)
             VALUES (?1, ?2, ?3, ?4)",
            params![repo_url, commit_hash, timestamp, base_version],
        )?;
        Ok(())
    }

    // Branch commits (cached branch -> commit mappings)

    pub fn get_branch_commit(&self, repo_url: &str, branch: &str) -> Option<String> {
        self.conn()
            .query_row(
                "SELECT commit_hash FROM branch_commits WHERE repo_url = ?1 AND branch = ?2",
                params![repo_url, branch],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_branch_commit(&self, repo_url: &str, branch: &str, commit_hash: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO branch_commits (repo_url, branch, commit_hash) VALUES (?1, ?2, ?3)",
            params![repo_url, branch, commit_hash],
        )?;
        Ok(())
    }

    pub fn clear_branch_commits(&self) -> Result<()> {
        self.conn().execute("DELETE FROM branch_commits", [])?;
        Ok(())
    }
}

fn index_path() -> PathBuf {
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(format!(".pcb/cache/index_v{SCHEMA_VERSION}.sqlite"))
}

pub fn cache_base() -> PathBuf {
    pcb_zen_core::DefaultFileProvider::new().cache_dir()
}

/// Ensure the embedded stdlib is materialized into the workspace stdlib location.
///
/// Materializes to `<workspace>/.pcb/stdlib`, replacing the directory only when
/// its canonical content hash differs from the embedded stdlib hash.
pub fn ensure_stdlib_materialized(workspace_root: &std::path::Path) -> Result<PathBuf> {
    let target = pcb_zen_core::workspace_stdlib_root(workspace_root);
    let _lock = git::lock_dir(&target)?;

    let expected_hash = pcb_zen_core::embedded_stdlib::embedded_stdlib_hash();
    let current_hash = if target.exists() {
        compute_stdlib_dir_hash(&target).ok()
    } else {
        None
    };
    if current_hash.as_deref() == Some(expected_hash) {
        return Ok(target);
    }

    if target.exists() {
        std::fs::remove_dir_all(&target)
            .or_else(|err| {
                if err.kind() == std::io::ErrorKind::NotADirectory {
                    std::fs::remove_file(&target)
                } else {
                    Err(err)
                }
            })
            .with_context(|| format!("Failed to replace stdlib at {}", target.display()))?;
    }
    pcb_zen_core::embedded_stdlib::extract_embedded_stdlib(&target)?;

    let refreshed_hash = compute_stdlib_dir_hash(&target)
        .with_context(|| format!("Failed to hash materialized stdlib at {}", target.display()))?;
    if refreshed_hash != expected_hash {
        anyhow::bail!(
            "Materialized stdlib hash mismatch: expected {}, got {}",
            expected_hash,
            refreshed_hash
        );
    }

    Ok(target)
}

/// Ensure the workspace cache link exists.
///
/// Creates <workspace_root>/.pcb/cache as a symlink to ~/.pcb/cache.
/// On Windows, falls back to a junction when symlink creation requires
/// privileges that the current process does not have.
/// This provides stable workspace-relative paths in generated files.
pub fn ensure_workspace_cache_symlink(workspace_root: &std::path::Path) -> Result<()> {
    let home_dir = dirs::home_dir().expect("Cannot determine home directory");

    // Skip if workspace_root is home directory - would create self-symlink
    if workspace_root == home_dir {
        return Ok(());
    }

    let workspace_cache = workspace_root.join(".pcb/cache");
    let home_cache = cache_base();

    // Ensure directories exist
    std::fs::create_dir_all(workspace_root.join(".pcb"))?;
    std::fs::create_dir_all(&home_cache)?;

    // Check if already a correct symlink or Windows junction.
    if cache_link_points_to(&workspace_cache, &home_cache) {
        return Ok(());
    }

    // Remove whatever exists at the path
    remove_workspace_cache_entry(&workspace_cache)?;

    create_workspace_cache_link(&home_cache, &workspace_cache)?;

    Ok(())
}

fn paths_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    if left == right {
        return true;
    }

    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn cache_link_points_to(workspace_cache: &std::path::Path, home_cache: &std::path::Path) -> bool {
    if let Ok(target) = std::fs::read_link(workspace_cache)
        && paths_equal(&target, home_cache)
    {
        return true;
    }

    #[cfg(windows)]
    {
        if junction::exists(workspace_cache).unwrap_or(false)
            && let Ok(target) = junction::get_target(workspace_cache)
        {
            return paths_equal(&target, home_cache);
        }
    }

    false
}

fn remove_workspace_cache_entry(workspace_cache: &std::path::Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(workspace_cache) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    #[cfg(windows)]
    {
        if junction::exists(workspace_cache).unwrap_or(false) {
            return junction::delete(workspace_cache);
        }
    }

    if metadata.file_type().is_symlink() {
        return std::fs::remove_file(workspace_cache)
            .or_else(|_| std::fs::remove_dir(workspace_cache));
    }

    if metadata.is_dir() {
        std::fs::remove_dir_all(workspace_cache)
    } else {
        std::fs::remove_file(workspace_cache)
    }
}

#[cfg(unix)]
fn create_workspace_cache_link(
    home_cache: &std::path::Path,
    workspace_cache: &std::path::Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(home_cache, workspace_cache)
}

#[cfg(windows)]
fn create_workspace_cache_link(
    home_cache: &std::path::Path,
    workspace_cache: &std::path::Path,
) -> std::io::Result<()> {
    match std::os::windows::fs::symlink_dir(home_cache, workspace_cache) {
        Ok(()) => Ok(()),
        Err(err)
            if err.raw_os_error() == Some(1314)
                || err.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            let _ = remove_workspace_cache_entry(workspace_cache);
            junction::create(home_cache, workspace_cache)
        }
        Err(err) => Err(err),
    }
}

pub fn ensure_bare_repo(repo_url: &str) -> Result<PathBuf> {
    let bare_dir = bare_repo_dir(repo_url)?;

    let _lock = git::lock_dir(&bare_dir)?;
    let spinner = Spinner::builder(format!("Fetching {repo_url}")).start();
    let result = if bare_dir.join("HEAD").exists() {
        git::fetch_in_bare_repo(&bare_dir)
    } else {
        git::clone_bare_with_fallback(repo_url, &bare_dir)
    };
    if let Err(err) = result {
        spinner.error(format!("Failed to fetch {repo_url}"));
        return Err(err);
    }
    spinner.finish();

    Ok(bare_dir)
}

pub fn bare_repo_dir(repo_url: &str) -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".pcb/bare").join(repo_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_index(db_path: &std::path::Path, schema: &str) -> CacheIndex {
        let manager = SqliteConnectionManager::file(db_path).with_init(|c| {
            c.pragma_update(None, "journal_mode", "WAL")?;
            c.busy_timeout(std::time::Duration::from_secs(5))
        });
        let pool = Pool::builder().max_size(4).build(manager).unwrap();
        pool.get().unwrap().execute_batch(schema).unwrap();
        CacheIndex { pool }
    }

    #[test]
    fn test_packages() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db_path = temp.path().join("index.sqlite");
        let index = test_index(
            &db_path,
            "CREATE TABLE packages (
                module_path TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, version)
            );",
        );

        assert!(index.get_package("github.com/foo/bar", "1.0.0").is_none());

        index.set_package("github.com/foo/bar", "1.0.0", "hash123", "manifest456")?;

        let (content, manifest) = index.get_package("github.com/foo/bar", "1.0.0").unwrap();
        assert_eq!(content, "hash123");
        assert_eq!(manifest, "manifest456");

        Ok(())
    }

    #[test]
    fn test_remote_packages_lpm() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db_path = temp.path().join("index.sqlite");
        let index = test_index(
            &db_path,
            "CREATE TABLE remote_packages (
                repo_url TEXT NOT NULL,
                package_path TEXT NOT NULL,
                latest_version TEXT NOT NULL,
                PRIMARY KEY (repo_url, package_path)
            );",
        );

        let conn = index.conn();
        conn.execute(
            "INSERT INTO remote_packages VALUES (?1, ?2, ?3)",
            params!["github.com/diodeinc/registry", "components/LED", "0.1.0"],
        )?;
        conn.execute(
            "INSERT INTO remote_packages VALUES (?1, ?2, ?3)",
            params![
                "github.com/diodeinc/registry",
                "components/JST/BM04B",
                "0.2.0"
            ],
        )?;
        conn.execute(
            "INSERT INTO remote_packages VALUES (?1, ?2, ?3)",
            params!["github.com/diodeinc/registry", "components/JST", "0.3.0"],
        )?;
        drop(conn);

        let dep = index
            .find_remote_package("github.com/diodeinc/registry/components/LED/LED.zen")?
            .unwrap();
        assert_eq!(
            dep.module_path,
            "github.com/diodeinc/registry/components/LED"
        );
        assert_eq!(dep.version, "0.1.0");

        let dep = index
            .find_remote_package("github.com/diodeinc/registry/components/JST/BM04B/x.zen")?
            .unwrap();
        assert_eq!(
            dep.module_path,
            "github.com/diodeinc/registry/components/JST/BM04B"
        );
        assert_eq!(dep.version, "0.2.0");

        let dep = index
            .find_remote_package("github.com/diodeinc/registry/components/JST/OTHER/x.zen")?
            .unwrap();
        assert_eq!(
            dep.module_path,
            "github.com/diodeinc/registry/components/JST"
        );
        assert_eq!(dep.version, "0.3.0");

        // Verify cache miss without triggering remote discovery/network.
        assert!(
            index
                .find_remote_package_cached("github.com/diodeinc/registry/modules/foo/bar.zen")
                .is_none()
        );

        Ok(())
    }
}
