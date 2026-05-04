//! Download registry index from API server + S3

pub use crate::download_support::{DownloadProgress, DownloadSource};
use crate::download_support::{
    ProgressReader, StderrProgressReader, ensure_parent_dir, http_client,
    save_local_version as save_shared_local_version, write_decoded_index,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::sync::mpsc::Sender;

const REGISTRY_INDEX_ROUTE: &str = "/api/registry/index";

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryIndexMetadata {
    pub url: String,
    pub sha256: String,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
    #[serde(rename = "expiresAt")]
    #[allow(dead_code)]
    pub expires_at: String,
}

impl RegistryIndexMetadata {
    /// Stable token for local freshness checks.
    pub fn version_token(&self) -> Result<String> {
        crate::download_support::sha256_version_token(&self.sha256, "registry index")
    }
}

pub fn load_local_version(db_path: &Path) -> Option<String> {
    crate::download_support::load_local_version(db_path)
}

pub fn save_local_version(db_path: &Path, version: &str) -> Result<()> {
    save_shared_local_version(db_path, version, "registry")
}

/// Fetch registry index metadata without downloading the file
pub fn fetch_registry_index_metadata() -> Result<RegistryIndexMetadata> {
    let token = crate::auth::get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{REGISTRY_INDEX_ROUTE}");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("API error from {url}"))?;

    resp.json()
        .context("Failed to parse registry index metadata")
}

fn download_index_response(
    client: &reqwest::blocking::Client,
    index_url: &str,
) -> Result<reqwest::blocking::Response> {
    client
        .get(index_url)
        .send()
        .context("Failed to download registry index")?
        .error_for_status()
        .context("S3 returned error when downloading registry index")
}

/// Result of checking registry access
pub enum RegistryAccessResult {
    /// Access allowed, includes metadata for download
    Allowed(RegistryIndexMetadata),
    /// Access forbidden (non-admin user)
    Forbidden,
}

/// Check if registry index download is allowed (for preflight check)
/// Returns metadata if allowed, or Forbidden if user lacks admin access
pub fn check_registry_access() -> Result<RegistryAccessResult> {
    let token = crate::auth::get_valid_token()
        .context("Authentication required. Run `pcb auth` to log in.")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{REGISTRY_INDEX_ROUTE}");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?;

    match resp.status() {
        reqwest::StatusCode::FORBIDDEN => Ok(RegistryAccessResult::Forbidden),
        reqwest::StatusCode::UNAUTHORIZED => {
            anyhow::bail!("Authentication expired. Run `pcb auth` to log in again.")
        }
        status if status.is_success() => {
            let metadata: RegistryIndexMetadata = resp
                .json()
                .context("Failed to parse registry index metadata")?;
            Ok(RegistryAccessResult::Allowed(metadata))
        }
        _ => {
            resp.error_for_status()
                .with_context(|| format!("API error from {url}"))?;
            unreachable!()
        }
    }
}

/// Download registry index with progress reporting via channel
///
/// If `prefetched_metadata` is provided, it will be used instead of fetching from the API.
pub fn download_registry_index_with_progress(
    dest_path: &Path,
    progress_tx: &Sender<DownloadProgress>,
    is_update: bool,
    prefetched_metadata: Option<&RegistryIndexMetadata>,
) -> Result<()> {
    let send_progress = |pct: Option<u8>, done: bool, error: Option<String>| {
        let _ = progress_tx.send(DownloadProgress {
            source: DownloadSource::Registry,
            pct,
            done,
            error,
            is_update,
        });
    };

    send_progress(None, false, None);

    let client = http_client()?;

    let index_metadata = if let Some(meta) = prefetched_metadata {
        meta.clone()
    } else {
        fetch_registry_index_metadata().map_err(|e| {
            let msg = format!("Failed to fetch registry index URL: {e}");
            send_progress(None, true, Some(msg.clone()));
            anyhow::anyhow!(msg)
        })?
    };

    ensure_parent_dir(dest_path, "registry")?;

    let response = match download_index_response(&client, &index_metadata.url) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to download registry index: {e}");
            send_progress(None, true, Some(msg.clone()));
            anyhow::bail!(msg);
        }
    };

    let total_size = response.content_length();

    // Wrap response in a progress-tracking reader, then decompress with zstd
    let progress_reader = ProgressReader::new(response, total_size, &send_progress);
    write_decoded_index(dest_path, progress_reader, "registry index")?;

    let version_token = index_metadata.version_token()?;
    let _ = save_local_version(dest_path, &version_token);

    send_progress(Some(100), true, None);
    Ok(())
}

/// Download registry index (blocking, prints to stderr)
pub fn download_registry_index(dest_path: &Path) -> Result<()> {
    let token = crate::auth::get_valid_token()
        .context("Authentication required. Run `pcb auth login` first.")?;

    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{REGISTRY_INDEX_ROUTE}");

    eprintln!("Fetching registry index URL...");
    let index_metadata: RegistryIndexMetadata = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to fetch registry index URL")?
        .error_for_status()
        .context("API returned error when fetching registry index URL")?
        .json()
        .context("Failed to parse registry index response")?;

    ensure_parent_dir(dest_path, "registry")?;

    eprintln!("Downloading parts.db.zst...");
    let response = download_index_response(&client, &index_metadata.url)?;

    let total_size = response.content_length();

    // Wrap response in a progress-printing reader, then decompress with zstd
    let progress_reader = StderrProgressReader::new(response, total_size, "parts.db.zst");
    write_decoded_index(dest_path, progress_reader, "registry index")?;
    eprintln!();

    let version_token = index_metadata.version_token()?;
    save_local_version(dest_path, &version_token)?;

    eprintln!("Registry index downloaded successfully.");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshResult {
    UpToDate,
    Downloaded,
}

/// Refresh the local registry index when the server-side version changes.
pub fn refresh_registry_index_if_stale(dest_path: &Path) -> Result<RefreshResult> {
    let meta = fetch_registry_index_metadata()?;
    let remote_version = meta.version_token()?;
    let local_version = load_local_version(dest_path);

    if dest_path.exists() && local_version.as_deref() == Some(remote_version.as_str()) {
        return Ok(RefreshResult::UpToDate);
    }

    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let _ = progress_rx;
    download_registry_index_with_progress(dest_path, &progress_tx, true, Some(&meta))?;
    Ok(RefreshResult::Downloaded)
}
