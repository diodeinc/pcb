use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const CACHE_NAMESPACE: &str = "pcb-bom-match-v1";

#[derive(Debug)]
pub(crate) struct CacheIdentity {
    pub key: String,
    request_json: String,
}

#[derive(Debug)]
pub(crate) struct CachedResponse {
    pub response_json: String,
    pub fetched_at: i64,
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
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS bom_match_responses (
                cache_key TEXT PRIMARY KEY,
                request_json TEXT NOT NULL,
                response_json TEXT NOT NULL,
                response_sha256 TEXT NOT NULL,
                fetched_at INTEGER NOT NULL
            ) WITHOUT ROWID;",
        )?;

        Ok(Self { connection })
    }

    pub fn load(&self, identity: &CacheIdentity) -> Result<Option<CachedResponse>> {
        let row = self
            .connection
            .query_row(
                "SELECT request_json, response_json, response_sha256, fetched_at
                 FROM bom_match_responses WHERE cache_key = ?1",
                params![identity.key],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;

        let Some((request_json, response_json, response_sha256, fetched_at)) = row else {
            return Ok(None);
        };

        anyhow::ensure!(
            request_json == identity.request_json,
            "BOM cache key matched a different request"
        );
        anyhow::ensure!(
            response_sha256 == sha256(response_json.as_bytes()),
            "BOM cache response checksum mismatch"
        );

        Ok(Some(CachedResponse {
            response_json,
            fetched_at,
        }))
    }

    pub fn store(
        &self,
        identity: &CacheIdentity,
        response_json: &str,
        fetched_at: i64,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO bom_match_responses (
                cache_key, request_json, response_json, response_sha256, fetched_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(cache_key) DO UPDATE SET
                request_json = excluded.request_json,
                response_json = excluded.response_json,
                response_sha256 = excluded.response_sha256,
                fetched_at = excluded.fetched_at",
            params![
                identity.key,
                identity.request_json,
                response_json,
                sha256(response_json.as_bytes()),
                fetched_at,
            ],
        )?;
        Ok(())
    }
}

pub(crate) fn cache_identity(url: &str, request: &Value) -> Result<CacheIdentity> {
    let envelope = serde_json::json!({
        "cacheNamespace": CACHE_NAMESPACE,
        "method": "POST",
        "url": url,
        "request": request,
    });
    let request_json = serde_json::to_string(&canonicalize(&envelope))?;
    Ok(CacheIdentity {
        key: sha256(request_json.as_bytes()),
        request_json,
    })
}

fn cache_path() -> PathBuf {
    pcb_zen::cache_index::cache_base().join("bom_match_v1.sqlite")
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        _ => value.clone(),
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_canonicalizes_object_keys_but_preserves_array_order() {
        let first = serde_json::json!({
            "designBom": [{"path": "root.R1", "mpn": "R-1"}],
            "regions": ["US", "GLOBAL"],
        });
        let reordered: Value = serde_json::from_str(
            r#"{"regions":["US","GLOBAL"],"designBom":[{"mpn":"R-1","path":"root.R1"}]}"#,
        )
        .unwrap();
        let reversed_regions = serde_json::json!({
            "designBom": [{"path": "root.R1", "mpn": "R-1"}],
            "regions": ["GLOBAL", "US"],
        });

        let first = cache_identity("https://api.example/api/boms/match", &first).unwrap();
        let reordered = cache_identity("https://api.example/api/boms/match", &reordered).unwrap();
        let reversed =
            cache_identity("https://api.example/api/boms/match", &reversed_regions).unwrap();

        assert_eq!(first.key, reordered.key);
        assert_ne!(first.key, reversed.key);
    }

    #[test]
    fn cache_key_includes_endpoint() {
        let request = serde_json::json!({"designBom": []});
        let production = cache_identity("https://api.example/api/boms/match", &request).unwrap();
        let sandbox =
            cache_identity("https://api.sandbox.example/api/boms/match", &request).unwrap();

        assert_ne!(production.key, sandbox.key);
    }

    #[test]
    fn cache_round_trips_and_rejects_corrupted_responses() {
        let tempdir = tempfile::tempdir().unwrap();
        let cache = BomMatchCache::open_at(tempdir.path().join("bom.sqlite")).unwrap();
        let identity = cache_identity(
            "https://api.example/api/boms/match",
            &serde_json::json!({"designBom": []}),
        )
        .unwrap();

        cache.store(&identity, r#"{"results":[]}"#, 123).unwrap();
        let cached = cache.load(&identity).unwrap().unwrap();
        assert_eq!(cached.response_json, r#"{"results":[]}"#);
        assert_eq!(cached.fetched_at, 123);

        cache
            .connection
            .execute(
                "UPDATE bom_match_responses SET response_json = 'corrupted'",
                [],
            )
            .unwrap();
        assert!(cache.load(&identity).is_err());
    }
}
