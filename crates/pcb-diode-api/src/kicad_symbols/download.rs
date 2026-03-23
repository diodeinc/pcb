//! Download KiCad symbols index from API server + CDN

pub use crate::download_support::DownloadProgress;
use crate::download_support::{
    DownloadSource, ProgressReader, StderrProgressReader, ensure_parent_dir, http_client,
    save_local_version as save_shared_local_version, write_decoded_index,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::sync::mpsc::Sender;

const KICAD_SYMBOLS_INDEX_ROUTE: &str = "/api/symbols/kicad/index";

#[derive(Debug, Clone, Deserialize)]
pub struct KicadSymbolsIndexMetadata {
    pub url: String,
    pub sha256: String,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
    #[serde(rename = "expiresAt")]
    #[allow(dead_code)]
    pub expires_at: String,
}

impl KicadSymbolsIndexMetadata {
    /// Stable token for local freshness checks.
    pub fn version_token(&self) -> Result<String> {
        let sha256 = self.sha256.trim();
        if !sha256.is_empty() {
            return Ok(sha256.to_string());
        }

        anyhow::bail!("KiCad symbols index metadata missing sha256")
    }
}

pub fn load_local_version(db_path: &Path) -> Option<String> {
    crate::download_support::load_local_version(db_path)
}

pub fn save_local_version(db_path: &Path, version: &str) -> Result<()> {
    save_shared_local_version(db_path, version, "KiCad symbols")
}

fn download_index_response(
    client: &reqwest::blocking::Client,
    index_url: &str,
) -> Result<reqwest::blocking::Response> {
    client
        .get(index_url)
        .send()
        .context("Failed to download KiCad symbols index")?
        .error_for_status()
        .context("CDN returned error when downloading KiCad symbols index")
}

/// Fetch KiCad symbols index metadata without downloading the file.
pub fn fetch_kicad_symbols_index_metadata() -> Result<KicadSymbolsIndexMetadata> {
    let token = crate::auth::get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{KICAD_SYMBOLS_INDEX_ROUTE}");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("API error from {url}"))?;

    resp.json()
        .context("Failed to parse KiCad symbols index metadata")
}

/// Result of checking KiCad symbols index access.
pub enum KicadSymbolsAccessResult {
    /// Access allowed, includes metadata for download.
    Allowed(KicadSymbolsIndexMetadata),
    /// Access forbidden.
    Forbidden,
}

/// Check if KiCad symbols index download is allowed.
pub fn check_kicad_symbols_access() -> Result<KicadSymbolsAccessResult> {
    let token = crate::auth::get_valid_token()
        .context("Authentication required. Run `pcb auth` to log in.")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{KICAD_SYMBOLS_INDEX_ROUTE}");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?;

    match resp.status() {
        reqwest::StatusCode::FORBIDDEN => Ok(KicadSymbolsAccessResult::Forbidden),
        reqwest::StatusCode::UNAUTHORIZED => {
            anyhow::bail!("Authentication expired. Run `pcb auth` to log in again.")
        }
        status if status.is_success() => {
            let metadata: KicadSymbolsIndexMetadata = resp
                .json()
                .context("Failed to parse KiCad symbols index metadata")?;
            Ok(KicadSymbolsAccessResult::Allowed(metadata))
        }
        _ => {
            resp.error_for_status()
                .with_context(|| format!("API error from {url}"))?;
            unreachable!()
        }
    }
}

/// Download KiCad symbols index with progress reporting via channel.
///
/// If `prefetched_metadata` is provided, it will be used instead of fetching from the API.
pub fn download_kicad_symbols_index_with_progress(
    dest_path: &Path,
    progress_tx: &Sender<DownloadProgress>,
    is_update: bool,
    prefetched_metadata: Option<&KicadSymbolsIndexMetadata>,
) -> Result<()> {
    let send_progress = |pct: Option<u8>, done: bool, error: Option<String>| {
        let _ = progress_tx.send(DownloadProgress {
            source: DownloadSource::KicadSymbols,
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
        fetch_kicad_symbols_index_metadata().map_err(|e| {
            let msg = format!("Failed to fetch KiCad symbols index URL: {e}");
            send_progress(None, true, Some(msg.clone()));
            anyhow::anyhow!(msg)
        })?
    };

    ensure_parent_dir(dest_path, "KiCad symbols")?;

    let response = match download_index_response(&client, &index_metadata.url) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to download KiCad symbols index: {e}");
            send_progress(None, true, Some(msg.clone()));
            anyhow::bail!(msg);
        }
    };

    let total_size = response.content_length();
    let progress_reader = ProgressReader::new(response, total_size, &send_progress);
    write_decoded_index(dest_path, progress_reader, "KiCad symbols index")?;

    let version_token = index_metadata.version_token()?;
    let _ = save_local_version(dest_path, &version_token);

    send_progress(Some(100), true, None);
    Ok(())
}

/// Download KiCad symbols index (blocking, prints to stderr).
pub fn download_kicad_symbols_index(dest_path: &Path) -> Result<()> {
    let token = crate::auth::get_valid_token()
        .context("Authentication required. Run `pcb auth login` first.")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{KICAD_SYMBOLS_INDEX_ROUTE}");

    eprintln!("Fetching KiCad symbols index URL...");
    let index_metadata: KicadSymbolsIndexMetadata = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to fetch KiCad symbols index URL")?
        .error_for_status()
        .context("API returned error when fetching KiCad symbols index URL")?
        .json()
        .context("Failed to parse KiCad symbols index response")?;

    ensure_parent_dir(dest_path, "KiCad symbols")?;

    eprintln!("Downloading symbols.db.zst...");
    let response = download_index_response(&client, &index_metadata.url)?;

    let total_size = response.content_length();
    let progress_reader = StderrProgressReader::new(response, total_size, "symbols.db.zst");
    write_decoded_index(dest_path, progress_reader, "KiCad symbols index")?;
    eprintln!();

    let version_token = index_metadata.version_token()?;
    save_local_version(dest_path, &version_token)?;

    eprintln!("KiCad symbols index downloaded successfully.");
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshResult {
    UpToDate,
    Downloaded,
}

/// Refresh the local KiCad symbols index when the remote version token changes.
pub fn refresh_kicad_symbols_index_if_stale(dest_path: &Path) -> Result<RefreshResult> {
    let meta = fetch_kicad_symbols_index_metadata()?;
    let remote_version = meta.version_token()?;
    let local_version = load_local_version(dest_path);

    if dest_path.exists() && local_version.as_deref() == Some(remote_version.as_str()) {
        return Ok(RefreshResult::UpToDate);
    }

    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let _ = progress_rx;
    download_kicad_symbols_index_with_progress(dest_path, &progress_tx, true, Some(&meta))?;
    Ok(RefreshResult::Downloaded)
}

#[cfg(test)]
mod tests {
    use super::{KicadSymbolsIndexMetadata, load_local_version, save_local_version};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn version_token_prefers_sha256() {
        let metadata = KicadSymbolsIndexMetadata {
            url: "https://example.com/symbols.db.zst".to_string(),
            sha256: "abc123".to_string(),
            last_modified: "2026-03-18T15:17:20.000Z".to_string(),
            expires_at: "2026-03-18T16:18:53.256Z".to_string(),
        };

        assert_eq!(metadata.version_token().unwrap(), "abc123");
    }

    #[test]
    fn version_token_requires_sha256() {
        let metadata = KicadSymbolsIndexMetadata {
            url: "https://example.com/symbols.db.zst".to_string(),
            sha256: "".to_string(),
            last_modified: "2026-03-18T15:17:20.000Z".to_string(),
            expires_at: "2026-03-18T16:18:53.256Z".to_string(),
        };

        assert_eq!(
            metadata.version_token().unwrap_err().to_string(),
            "KiCad symbols index metadata missing sha256"
        );
    }

    #[test]
    fn version_sidecar_round_trips() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("pcb-kicad-symbols-test-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("symbols.db");

        save_local_version(&db_path, "sha256-token").unwrap();
        assert_eq!(
            load_local_version(&db_path).as_deref(),
            Some("sha256-token")
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
