use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};

const CACHE_VERSION: &[u8] = b"pcb-bom-match-v1";
const CACHE_TTL_SECS: i64 = 10 * 60;

#[derive(Debug)]
pub(crate) struct CachedResponse {
    pub response_json: String,
    fetched_at: i64,
}

impl CachedResponse {
    pub fn is_fresh(&self, now: i64) -> bool {
        now.checked_sub(self.fetched_at)
            .is_some_and(|age| (0..CACHE_TTL_SECS).contains(&age))
    }
}

pub(crate) struct BomMatchCache {
    connection: Connection,
}

impl BomMatchCache {
    pub fn open() -> Result<Self> {
        Self::open_at(cache_path())
    }

    pub(crate) fn open_at(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create BOM cache directory {}", parent.display())
            })?;
        }

        let connection = Connection::open(path)
            .with_context(|| format!("Failed to open BOM cache at {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS responses (
                cache_key TEXT PRIMARY KEY,
                response_json TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            ) WITHOUT ROWID;",
        )?;

        Ok(Self { connection })
    }

    pub fn load(&self, key: &str) -> Result<Option<CachedResponse>> {
        self.connection
            .query_row(
                "SELECT response_json, fetched_at FROM responses WHERE cache_key = ?1",
                params![key],
                |row| {
                    Ok(CachedResponse {
                        response_json: row.get(0)?,
                        fetched_at: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn store(&self, key: &str, response_json: &str, fetched_at: i64) -> Result<()> {
        self.connection.execute(
            "INSERT INTO responses (cache_key, response_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET
                response_json = excluded.response_json,
                fetched_at = excluded.fetched_at",
            params![key, response_json, fetched_at],
        )?;
        Ok(())
    }
}

pub(crate) fn cache_key(url: &str, request: &Value) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_VERSION);
    hasher.update([0]);
    hasher.update(url.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(request)?);
    Ok(hex::encode(hasher.finalize()))
}

fn cache_path() -> PathBuf {
    pcb_zen::cache_index::cache_base().join("bom_match_v1.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_includes_endpoint_and_request() {
        let request = serde_json::json!({"designBom": []});
        let other_request = serde_json::json!({"designBom": [{"path": "R1"}]});

        assert_ne!(
            cache_key("https://api.example/api/boms/match", &request).unwrap(),
            cache_key("https://other.example/api/boms/match", &request).unwrap()
        );
        assert_ne!(
            cache_key("https://api.example/api/boms/match", &request).unwrap(),
            cache_key("https://api.example/api/boms/match", &other_request).unwrap()
        );
    }
}
