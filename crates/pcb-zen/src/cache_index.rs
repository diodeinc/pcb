//! SQLite-based cache index for package metadata

use anyhow::{Context, Result};
use pcb_zen_core::config::Lockfile;
use rusqlite::{params, Connection, OptionalExtension};
use semver::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::git;

/// Bump this when changing table schemas to auto-reset the cache.
/// v3: Added subpath column to assets table for subpath asset dependencies
const SCHEMA_VERSION: i32 = 3;

pub struct CacheIndex {
    conn: Connection,
}

impl CacheIndex {
    pub fn open() -> Result<Self> {
        let path = index_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open cache index at {}", path.display()))?;

        // Enable WAL mode for better concurrent access (especially important on Windows)
        // WAL allows concurrent reads while writing and reduces lock contention
        conn.pragma_update(None, "journal_mode", "WAL")?;

        // Set busy timeout to handle concurrent access
        // This makes SQLite retry for up to 5 seconds if the database is locked
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        let current_version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current_version != SCHEMA_VERSION {
            conn.execute_batch(
                "DROP TABLE IF EXISTS cache_entries;
                 DROP TABLE IF EXISTS packages;
                 DROP TABLE IF EXISTS assets;
                 DROP TABLE IF EXISTS remote_packages;
                 DROP TABLE IF EXISTS commit_metadata;
                 DROP TABLE IF EXISTS branch_commits;",
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS packages (
                module_path TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, version)
            );
            CREATE TABLE IF NOT EXISTS assets (
                module_path TEXT NOT NULL,
                subpath TEXT NOT NULL,
                ref_str TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, subpath, ref_str)
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

        Ok(Self { conn })
    }

    // Packages (dependencies with manifest hash)

    pub fn get_package(&self, module_path: &str, version: &str) -> Option<(String, String)> {
        self.conn
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
        self.conn.execute(
            "INSERT OR REPLACE INTO packages (module_path, version, content_hash, manifest_hash)
             VALUES (?1, ?2, ?3, ?4)",
            params![module_path, version, content_hash, manifest_hash],
        )?;
        Ok(())
    }

    // Assets (no manifest hash, with optional subpath)

    pub fn get_asset(&self, module_path: &str, subpath: &str, ref_str: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT content_hash FROM assets WHERE module_path = ?1 AND subpath = ?2 AND ref_str = ?3",
                params![module_path, subpath, ref_str],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_asset(
        &self,
        module_path: &str,
        subpath: &str,
        ref_str: &str,
        content_hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (module_path, subpath, ref_str, content_hash) VALUES (?1, ?2, ?3, ?4)",
            params![module_path, subpath, ref_str, content_hash],
        )?;
        Ok(())
    }

    // Remote packages (discovered from git tags)

    pub fn find_remote_package(&self, file_url: &str) -> Option<(String, String)> {
        let (repo_url, subpath) = git::split_repo_and_subpath(file_url);
        let without_file = subpath.rsplit_once('/')?.0;

        let mut path = without_file;
        while !path.is_empty() {
            if let Some(version) = self
                .conn
                .query_row(
                    "SELECT latest_version FROM remote_packages WHERE repo_url = ?1 AND package_path = ?2",
                    params![repo_url, path],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .ok()
                .flatten()
            {
                return Some((format!("{}/{}", repo_url, path), version));
            }
            path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        }
        None
    }

    pub fn find_or_discover_remote_package(
        &self,
        file_url: &str,
    ) -> Result<Option<(String, String)>> {
        if let Some(result) = self.find_remote_package(file_url) {
            return Ok(Some(result));
        }

        let (repo_url, subpath) = git::split_repo_and_subpath(file_url);
        if subpath.is_empty() {
            return Ok(None);
        }

        self.discover_remote_packages(repo_url)?;
        Ok(self.find_remote_package(file_url))
    }

    fn discover_remote_packages(&self, repo_url: &str) -> Result<()> {
        let bare_dir = ensure_bare_repo(repo_url)?;
        let tags = git::list_all_tags(&bare_dir)?;

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

        self.conn.execute(
            "DELETE FROM remote_packages WHERE repo_url = ?1",
            params![repo_url],
        )?;
        for (package_path, version) in packages {
            self.conn.execute(
                "INSERT INTO remote_packages (repo_url, package_path, latest_version) VALUES (?1, ?2, ?3)",
                params![repo_url, package_path, version.to_string()],
            )?;
        }

        Ok(())
    }
}

/// Get all available versions for packages in a repository
///
/// Returns a map from package_path (relative to repo) to all available versions,
/// sorted descending (newest first). This fetches/updates the bare repo and parses
/// all version tags.
///
/// For root packages (tags like `v1.0.0`), the package path is an empty string.
/// For nested packages (tags like `path/to/pkg/v1.0.0`), the package path is `path/to/pkg`.
///
/// Example:
/// - repo_url: "github.com/diodeinc/stdlib"
/// - Returns: { "" => [0.4.0, 0.3.2, 0.3.1, ...] } (root package)
///
/// - repo_url: "github.com/diodeinc/registry"
/// - Returns: { "reference/ti/tps54331" => [1.2.0, 1.1.0, 1.0.0], ... }
pub fn get_all_versions_for_repo(repo_url: &str) -> Result<BTreeMap<String, Vec<Version>>> {
    let bare_dir = ensure_bare_repo(repo_url)?;
    let tags = git::list_all_tags(&bare_dir)?;

    let mut packages: BTreeMap<String, Vec<Version>> = BTreeMap::new();
    for tag in tags {
        // Try to parse as nested package tag (path/v1.0.0)
        if let Some((pkg_path, version)) = parse_version_tag(&tag) {
            packages.entry(pkg_path).or_default().push(version);
        } else if let Some(version) = parse_root_version_tag(&tag) {
            // Root package tag (v1.0.0) - empty string as package path
            packages.entry(String::new()).or_default().push(version);
        }
    }

    // Sort versions descending for each package (newest first)
    for versions in packages.values_mut() {
        versions.sort_by(|a, b| b.cmp(a));
    }

    Ok(packages)
}

/// Parse a root package version tag (e.g., "v1.0.0" -> Version)
fn parse_root_version_tag(tag: &str) -> Option<Version> {
    let version_str = tag.strip_prefix('v')?;
    Version::parse(version_str).ok()
}

impl CacheIndex {
    // Commit metadata (for pseudo-version generation)

    pub fn get_commit_metadata(
        &self,
        repo_url: &str,
        commit_hash: &str,
    ) -> Option<(i64, Option<String>)> {
        self.conn
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
        self.conn.execute(
            "INSERT OR REPLACE INTO commit_metadata (repo_url, commit_hash, timestamp, base_version)
             VALUES (?1, ?2, ?3, ?4)",
            params![repo_url, commit_hash, timestamp, base_version],
        )?;
        Ok(())
    }

    // Branch commits (cached branch -> commit mappings)

    pub fn get_branch_commit(&self, repo_url: &str, branch: &str) -> Option<String> {
        self.conn
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
        self.conn.execute(
            "INSERT OR REPLACE INTO branch_commits (repo_url, branch, commit_hash) VALUES (?1, ?2, ?3)",
            params![repo_url, branch, commit_hash],
        )?;
        Ok(())
    }
}

pub fn find_lockfile_entry(file_url: &str, lockfile: &Lockfile) -> Option<(String, String)> {
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

fn index_path() -> PathBuf {
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".pcb/cache/index.sqlite")
}

pub fn cache_base() -> PathBuf {
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".pcb/cache")
}

pub fn ensure_bare_repo(repo_url: &str) -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let bare_dir = home.join(".pcb/bare").join(repo_url);

    if bare_dir.join("HEAD").exists() {
        git::fetch_in_bare_repo(&bare_dir)?;
    } else {
        git::clone_bare_with_fallback(repo_url, &bare_dir)?;
    }

    Ok(bare_dir)
}

fn parse_version_tag(tag: &str) -> Option<(String, Version)> {
    let (pkg_path, version_str) = tag.rsplit_once('/')?;
    let version_str = version_str.strip_prefix('v').unwrap_or(version_str);
    let version = Version::parse(version_str).ok()?;
    Some((pkg_path.to_string(), version))
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
    fn test_packages() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db_path = temp.path().join("index.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE packages (
                module_path TEXT NOT NULL,
                version TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                manifest_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, version)
            );",
        )?;
        let index = CacheIndex { conn };

        assert!(index.get_package("github.com/foo/bar", "1.0.0").is_none());

        index.set_package("github.com/foo/bar", "1.0.0", "hash123", "manifest456")?;

        let (content, manifest) = index.get_package("github.com/foo/bar", "1.0.0").unwrap();
        assert_eq!(content, "hash123");
        assert_eq!(manifest, "manifest456");

        Ok(())
    }

    #[test]
    fn test_assets() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db_path = temp.path().join("index.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE assets (
                module_path TEXT NOT NULL,
                subpath TEXT NOT NULL,
                ref_str TEXT NOT NULL,
                content_hash TEXT NOT NULL,
                PRIMARY KEY (module_path, subpath, ref_str)
            );",
        )?;
        let index = CacheIndex { conn };

        // Whole repo (empty subpath)
        assert!(index
            .get_asset("gitlab.com/kicad/libraries/kicad-footprints", "", "9.0.3")
            .is_none());

        index.set_asset(
            "gitlab.com/kicad/libraries/kicad-footprints",
            "",
            "9.0.3",
            "hash123",
        )?;

        let content = index
            .get_asset("gitlab.com/kicad/libraries/kicad-footprints", "", "9.0.3")
            .unwrap();
        assert_eq!(content, "hash123");

        // Subpath asset
        index.set_asset(
            "gitlab.com/kicad/libraries/kicad-footprints",
            "Resistor_SMD.pretty",
            "9.0.3",
            "subpath_hash",
        )?;

        let subpath_content = index
            .get_asset(
                "gitlab.com/kicad/libraries/kicad-footprints",
                "Resistor_SMD.pretty",
                "9.0.3",
            )
            .unwrap();
        assert_eq!(subpath_content, "subpath_hash");

        // Different subpaths don't conflict
        assert!(index
            .get_asset(
                "gitlab.com/kicad/libraries/kicad-footprints",
                "Capacitor_SMD.pretty",
                "9.0.3"
            )
            .is_none());

        Ok(())
    }

    #[test]
    fn test_remote_packages_lpm() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db_path = temp.path().join("index.sqlite");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE remote_packages (
                repo_url TEXT NOT NULL,
                package_path TEXT NOT NULL,
                latest_version TEXT NOT NULL,
                PRIMARY KEY (repo_url, package_path)
            );",
        )?;

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

        let index = CacheIndex { conn };

        let (path, ver) = index
            .find_remote_package("github.com/diodeinc/registry/components/LED/LED.zen")
            .unwrap();
        assert_eq!(path, "github.com/diodeinc/registry/components/LED");
        assert_eq!(ver, "0.1.0");

        let (path, ver) = index
            .find_remote_package("github.com/diodeinc/registry/components/JST/BM04B/x.zen")
            .unwrap();
        assert_eq!(path, "github.com/diodeinc/registry/components/JST/BM04B");
        assert_eq!(ver, "0.2.0");

        let (path, ver) = index
            .find_remote_package("github.com/diodeinc/registry/components/JST/OTHER/x.zen")
            .unwrap();
        assert_eq!(path, "github.com/diodeinc/registry/components/JST");
        assert_eq!(ver, "0.3.0");

        assert!(index
            .find_remote_package("github.com/diodeinc/registry/modules/foo/bar.zen")
            .is_none());

        Ok(())
    }
}
