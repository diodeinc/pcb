//! Embedding generation and caching for semantic search
//!
//! This module handles:
//! - AWS credentials fetching and caching (disk-based)
//! - Embedding cache using SQLite (reuses existing schema)
//! - Bedrock Titan embeddings API calls with SigV4 signing

use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;

use crate::auth::get_valid_token;
use crate::get_api_base_url;

// ============================================================================
// AWS Credentials and Bedrock
// ============================================================================

const MODEL_ID: &str = "amazon.titan-embed-text-v2:0";
const EMBEDDING_DIMS: usize = 1024;

/// AWS credentials for Bedrock API access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
    pub region: String,
    pub model_id: String,
    pub expiration: String,
}

impl AwsCredentials {
    /// Check if credentials are expired or will expire within 5 minutes
    pub fn is_expired(&self) -> bool {
        let Ok(expiration) = DateTime::parse_from_rfc3339(&self.expiration) else {
            return true;
        };
        let now = Utc::now();
        let buffer = chrono::Duration::minutes(5);
        expiration < now + buffer
    }
}

/// Global HTTP client for API calls
static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client")
});

/// Cached AWS credentials (in memory for quick access, backed by disk)
static AWS_CREDS_CACHE: Lazy<Mutex<Option<AwsCredentials>>> = Lazy::new(|| Mutex::new(None));

/// Get the path to AWS credentials cache file
fn aws_creds_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".pcb").join("aws-credentials.toml"))
}

/// Get the path to embedding cache database
fn embedding_cache_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join(".pcb")
        .join("registry")
        .join("embedding_cache.db"))
}

/// Load AWS credentials from disk cache
fn load_aws_creds_from_disk() -> Result<Option<AwsCredentials>> {
    let path = aws_creds_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    let creds: AwsCredentials = toml::from_str(&contents)?;
    if creds.is_expired() {
        return Ok(None);
    }
    Ok(Some(creds))
}

/// Save AWS credentials to disk cache
fn save_aws_creds_to_disk(creds: &AwsCredentials) -> Result<()> {
    let path = aws_creds_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string(creds)?;
    AtomicFile::new(&path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(contents.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write cached AWS creds: {err}"))?;
    Ok(())
}

/// Fetch fresh AWS credentials from Diode API
fn fetch_aws_creds_from_api() -> Result<AwsCredentials> {
    let token = get_valid_token()?;

    let api_url = get_api_base_url();
    let url = format!("{}/api/bedrock/embed/credentials", api_url);

    let response = HTTP_CLIENT
        .post(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to fetch AWS credentials")?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Failed to fetch AWS credentials: {} {}",
            response.status(),
            response.text().unwrap_or_default()
        );
    }

    #[derive(Deserialize)]
    struct ApiResponse {
        region: String,
        #[serde(rename = "modelId")]
        model_id: String,
        credentials: ApiCredentials,
    }

    #[derive(Deserialize)]
    struct ApiCredentials {
        #[serde(rename = "accessKeyId")]
        access_key_id: String,
        #[serde(rename = "secretAccessKey")]
        secret_access_key: String,
        #[serde(rename = "sessionToken")]
        session_token: String,
        expiration: String,
    }

    let api_resp: ApiResponse = response.json().context("Failed to parse AWS credentials")?;

    Ok(AwsCredentials {
        access_key_id: api_resp.credentials.access_key_id,
        secret_access_key: api_resp.credentials.secret_access_key,
        session_token: api_resp.credentials.session_token,
        region: api_resp.region,
        model_id: api_resp.model_id,
        expiration: api_resp.credentials.expiration,
    })
}

/// Get valid AWS credentials (from cache or fetch new)
pub fn get_aws_credentials() -> Result<AwsCredentials> {
    // Check memory cache first
    {
        let cache = AWS_CREDS_CACHE.lock().unwrap();
        if let Some(ref creds) = *cache
            && !creds.is_expired()
        {
            return Ok(creds.clone());
        }
    }

    // Check disk cache
    if let Some(creds) = load_aws_creds_from_disk()? {
        let mut cache = AWS_CREDS_CACHE.lock().unwrap();
        *cache = Some(creds.clone());
        return Ok(creds);
    }

    // Fetch fresh credentials
    let creds = fetch_aws_creds_from_api()?;
    save_aws_creds_to_disk(&creds)?;

    let mut cache = AWS_CREDS_CACHE.lock().unwrap();
    *cache = Some(creds.clone());

    Ok(creds)
}

/// Normalize query for consistent hashing
fn normalize_query(query: &str) -> String {
    query.trim().to_lowercase()
}

/// Compute MD5 hash of normalized query (matches existing cache schema)
fn hash_query(normalized: &str) -> String {
    format!("{:x}", md5::compute(normalized.as_bytes()))
}

/// Open the embedding cache database
fn open_embedding_cache() -> Result<Connection> {
    let path = embedding_cache_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(&path).context("Failed to open embedding cache")?;

    // Create table if not exists (matches existing schema)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache (
            model TEXT NOT NULL,
            blob_hash TEXT NOT NULL,
            embedding BLOB NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (model, blob_hash)
        )",
        [],
    )?;

    Ok(conn)
}

/// Look up embedding in cache
fn lookup_cache(conn: &Connection, blob_hash: &str) -> Result<Option<[f32; EMBEDDING_DIMS]>> {
    let mut stmt =
        conn.prepare("SELECT embedding FROM cache WHERE model = ?1 AND blob_hash = ?2")?;

    let result: Option<Vec<u8>> = stmt.query_row([MODEL_ID, blob_hash], |row| row.get(0)).ok();

    match result {
        Some(bytes) => {
            if bytes.len() != EMBEDDING_DIMS * 4 {
                return Ok(None);
            }
            let mut embedding = [0f32; EMBEDDING_DIMS];
            for (i, chunk) in bytes.chunks_exact(4).enumerate() {
                embedding[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
            Ok(Some(embedding))
        }
        None => Ok(None),
    }
}

/// Store embedding in cache
fn store_cache(
    conn: &Connection,
    blob_hash: &str,
    embedding: &[f32; EMBEDDING_DIMS],
) -> Result<()> {
    let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO cache (model, blob_hash, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![MODEL_ID, blob_hash, bytes, now],
    )?;

    Ok(())
}

/// Call Bedrock Titan embeddings API
fn call_bedrock_api(text: &str, creds: &AwsCredentials) -> Result<[f32; EMBEDDING_DIMS]> {
    let host = format!("bedrock-runtime.{}.amazonaws.com", creds.region);
    // AWS SigV4 requires URI-encoding the path for signing, even though
    // the colon is technically valid. The canonical URI must be encoded,
    // but the actual HTTP request can use the unencoded path.
    let url = format!("https://{}/model/{}/invoke", host, creds.model_id);
    // For signing, encode the colon in the path
    let canonical_uri = format!("/model/{}/invoke", creds.model_id.replace(':', "%3A"));

    // Request body
    let body = serde_json::json!({
        "inputText": text,
        "dimensions": EMBEDDING_DIMS,
        "normalize": true
    });
    let body_str = serde_json::to_string(&body)?;

    // AWS SigV4 signing
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let datetime = chrono::DateTime::from_timestamp(now as i64, 0)
        .unwrap()
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let date = &datetime[..8];

    let content_hash = hex::encode(Sha256::digest(body_str.as_bytes()));
    let canonical_headers = format!(
        "content-type:application/json\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\n",
        host, datetime, creds.session_token
    );
    let signed_headers = "content-type;host;x-amz-date;x-amz-security-token";

    let canonical_request = format!(
        "POST\n{}\n\n{}\n{}\n{}",
        canonical_uri, canonical_headers, signed_headers, content_hash
    );

    // String to sign
    let scope = format!("{}/{}/bedrock/aws4_request", date, creds.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        datetime,
        scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    // Signing key
    fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
        use sha2::Sha256;

        let mut ipad = vec![0x36u8; 64];
        let mut opad = vec![0x5cu8; 64];

        let key = if key.len() > 64 {
            Sha256::digest(key).to_vec()
        } else {
            key.to_vec()
        };

        for (i, &b) in key.iter().enumerate() {
            ipad[i] ^= b;
            opad[i] ^= b;
        }

        let mut inner = Sha256::new();
        inner.update(&ipad);
        inner.update(data);
        let inner_hash = inner.finalize();

        let mut outer = Sha256::new();
        outer.update(&opad);
        outer.update(inner_hash);
        outer.finalize().to_vec()
    }

    let k_date = hmac_sha256(
        format!("AWS4{}", creds.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, creds.region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"bedrock");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    // Authorization header
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        creds.access_key_id, scope, signed_headers, signature
    );

    // Make request
    let response = HTTP_CLIENT
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Amz-Date", &datetime)
        .header("X-Amz-Security-Token", &creds.session_token)
        .header("Authorization", &authorization)
        .body(body_str)
        .send()
        .context("Failed to call Bedrock API")?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        anyhow::bail!("Bedrock API error: {} {}", status, text);
    }

    #[derive(Deserialize)]
    struct BedrockResponse {
        embedding: Vec<f32>,
    }

    let resp: BedrockResponse = response
        .json()
        .context("Failed to parse Bedrock response")?;

    if resp.embedding.len() != EMBEDDING_DIMS {
        anyhow::bail!(
            "Unexpected embedding dimensions: {} (expected {})",
            resp.embedding.len(),
            EMBEDDING_DIMS
        );
    }

    let mut embedding = [0f32; EMBEDDING_DIMS];
    embedding.copy_from_slice(&resp.embedding);

    Ok(embedding)
}

/// Get embedding for a query, using cache when available
pub fn get_query_embedding(query: &str) -> Result<[f32; EMBEDDING_DIMS]> {
    let normalized = normalize_query(query);
    if normalized.is_empty() {
        anyhow::bail!("Query is empty");
    }

    let blob_hash = hash_query(&normalized);

    // Try cache first
    let conn = open_embedding_cache()?;
    if let Some(embedding) = lookup_cache(&conn, &blob_hash)? {
        return Ok(embedding);
    }

    // Get credentials and call API
    let creds = get_aws_credentials()?;
    let embedding = call_bedrock_api(&normalized, &creds)?;

    // Cache result
    store_cache(&conn, &blob_hash, &embedding)?;

    Ok(embedding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_query() {
        assert_eq!(normalize_query("  Hello World  "), "hello world");
        assert_eq!(normalize_query("STM32G431"), "stm32g431");
    }

    #[test]
    fn test_hash_query() {
        let hash = hash_query("hello world");
        assert_eq!(hash.len(), 32);
    }
}
