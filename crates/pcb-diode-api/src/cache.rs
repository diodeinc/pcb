use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: u32 = 1;

/// A cached value and the time it was last written.
///
/// Freshness is a property of each read, not of the stored entry.
#[derive(Debug)]
pub(crate) struct CacheEntry {
    pub value: Vec<u8>,
    pub updated_at: i64,
}

impl CacheEntry {
    pub fn is_fresh(&self, ttl: Duration, now: i64) -> bool {
        now.checked_sub(self.updated_at)
            .and_then(|age| u64::try_from(age).ok())
            .is_some_and(|age| Duration::from_secs(age) < ttl)
    }
}

/// A namespaced SQLite cache whose writes are committed before returning.
pub(crate) struct WriteThroughCache {
    connection: Connection,
    namespace: String,
}

impl WriteThroughCache {
    pub fn open(namespace: impl Into<String>) -> Result<Self> {
        Self::open_at(cache_path(), namespace)
    }

    pub(crate) fn open_at(path: impl AsRef<Path>, namespace: impl Into<String>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create cache directory {}", parent.display())
            })?;
        }

        let connection = Connection::open(path)
            .with_context(|| format!("Failed to open local cache at {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                namespace TEXT NOT NULL,
                cache_key TEXT NOT NULL,
                value BLOB NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (namespace, cache_key)
            ) WITHOUT ROWID;",
        )?;

        Ok(Self {
            connection,
            namespace: namespace.into(),
        })
    }

    pub fn load(&self, key: &str) -> Result<Option<CacheEntry>> {
        self.connection
            .query_row(
                "SELECT value, updated_at
                 FROM entries
                 WHERE namespace = ?1 AND cache_key = ?2",
                params![self.namespace, key],
                |row| {
                    Ok(CacheEntry {
                        value: row.get(0)?,
                        updated_at: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn store_many(&mut self, entries: &[(String, Vec<u8>)]) -> Result<()> {
        self.store_many_at(entries, unix_now()?)
    }

    fn store_many_at(&mut self, entries: &[(String, Vec<u8>)], updated_at: i64) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO entries (namespace, cache_key, value, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (namespace, cache_key) DO UPDATE SET
                    value = excluded.value,
                    updated_at = excluded.updated_at",
            )?;
            for (key, value) in entries {
                statement.execute(params![self.namespace, key, value, updated_at])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn cache_key(value: &impl Serialize) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(value)?);
    Ok(hex::encode(hasher.finalize()))
}

fn cache_path() -> PathBuf {
    pcb_zen::cache_index::cache_base().join(format!("api_cache_v{SCHEMA_VERSION}.sqlite"))
}

pub(crate) fn unix_now() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_is_chosen_when_an_entry_is_read() {
        let entry = CacheEntry {
            value: Vec::new(),
            updated_at: 1_000,
        };

        assert!(entry.is_fresh(Duration::from_secs(60), 1_059));
        assert!(!entry.is_fresh(Duration::from_secs(60), 1_060));
        assert!(entry.is_fresh(Duration::from_secs(600), 1_599));
    }

    #[test]
    fn writes_are_upserted_and_isolated_by_namespace() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("cache.sqlite");
        let mut first = WriteThroughCache::open_at(&path, "first").unwrap();
        let second = WriteThroughCache::open_at(&path, "second").unwrap();

        first
            .store_many_at(&[("key".to_string(), b"old".to_vec())], 100)
            .unwrap();
        first
            .store_many_at(&[("key".to_string(), b"new".to_vec())], 200)
            .unwrap();

        let entry = first.load("key").unwrap().unwrap();
        assert_eq!(entry.value, b"new");
        assert_eq!(entry.updated_at, 200);
        assert!(second.load("key").unwrap().is_none());
    }

    #[test]
    fn cache_keys_cover_the_complete_input() {
        let request = serde_json::json!({"line": "R1"});
        let other_request = serde_json::json!({"line": "R2"});

        assert_ne!(
            cache_key(&("https://api.example", &request)).unwrap(),
            cache_key(&("https://other.example", &request)).unwrap()
        );
        assert_ne!(
            cache_key(&("https://api.example", &request)).unwrap(),
            cache_key(&("https://api.example", &other_request)).unwrap()
        );
    }
}
