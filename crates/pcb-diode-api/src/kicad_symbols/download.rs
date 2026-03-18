//! Download KiCad symbols index from API server + CDN

use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

const KICAD_SYMBOLS_INDEX_ROUTE: &str = "/api/symbols/kicad/index";

/// Create an HTTP client with proper User-Agent (required by our API gateway)
fn http_client() -> Result<Client> {
    let user_agent = format!("diode-pcb/{}", env!("CARGO_PKG_VERSION"));
    Client::builder()
        .user_agent(user_agent)
        .build()
        .context("Failed to build HTTP client")
}

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub pct: Option<u8>,
    pub done: bool,
    pub error: Option<String>,
    /// True if this is a background update (vs initial download)
    pub is_update: bool,
}

/// A reader wrapper that tracks download progress
struct ProgressReader<'a, R> {
    inner: R,
    downloaded: u64,
    total_size: Option<u64>,
    last_pct: u8,
    send_progress: &'a dyn Fn(Option<u8>, bool, Option<String>),
}

impl<'a, R> ProgressReader<'a, R> {
    fn new(
        inner: R,
        total_size: Option<u64>,
        send_progress: &'a dyn Fn(Option<u8>, bool, Option<String>),
    ) -> Self {
        Self {
            inner,
            downloaded: 0,
            total_size,
            last_pct: 0,
            send_progress,
        }
    }
}

impl<R: io::Read> io::Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.downloaded += bytes_read as u64;

        if let Some(total) = self.total_size {
            let pct = (self.downloaded as f64 / total as f64 * 100.0) as u8;
            if pct != self.last_pct {
                (self.send_progress)(Some(pct), false, None);
                self.last_pct = pct;
            }
        }

        Ok(bytes_read)
    }
}

/// A reader wrapper that prints download progress to stderr (for blocking CLI use)
struct StderrProgressReader<R> {
    inner: R,
    downloaded: u64,
    total_size: Option<u64>,
    last_pct: u32,
}

impl<R> StderrProgressReader<R> {
    fn new(inner: R, total_size: Option<u64>) -> Self {
        Self {
            inner,
            downloaded: 0,
            total_size,
            last_pct: 0,
        }
    }
}

impl<R: io::Read> io::Read for StderrProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.downloaded += bytes_read as u64;

        if let Some(total) = self.total_size {
            let pct = (self.downloaded as f64 / total as f64 * 100.0) as u32;
            if pct != self.last_pct {
                eprint!("\rDownloading symbols.db.zst... {}%", pct);
                self.last_pct = pct;
            }
        }

        Ok(bytes_read)
    }
}

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

fn version_file_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("db.version")
}

fn ensure_parent_dir(dest_path: &Path) -> Result<()> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).context("Failed to create KiCad symbols directory")?;
    }
    Ok(())
}

fn download_index_response(
    client: &Client,
    index_url: &str,
) -> Result<reqwest::blocking::Response> {
    client
        .get(index_url)
        .send()
        .context("Failed to download KiCad symbols index")?
        .error_for_status()
        .context("CDN returned error when downloading KiCad symbols index")
}

fn write_decoded_index<R: io::Read>(dest_path: &Path, reader: R) -> Result<()> {
    let mut decoder =
        zstd::stream::Decoder::new(reader).context("Failed to create zstd decoder")?;
    AtomicFile::new(dest_path, OverwriteBehavior::AllowOverwrite)
        .write(|file| {
            io::copy(&mut decoder, file).map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("Failed to decompress and write KiCad symbols index: {err}"),
                )
            })?;
            file.flush()
        })
        .context("Failed to move downloaded KiCad symbols file into place")
}

pub fn load_local_version(db_path: &Path) -> Option<String> {
    let path = version_file_path(db_path);
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn save_local_version(db_path: &Path, version: &str) -> Result<()> {
    let path = version_file_path(db_path);
    AtomicFile::new(&path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(version.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write local KiCad symbols version: {err}"))?;
    Ok(())
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

    ensure_parent_dir(dest_path)?;

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
    write_decoded_index(dest_path, progress_reader)?;

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

    ensure_parent_dir(dest_path)?;

    eprintln!("Downloading symbols.db.zst...");
    let response = download_index_response(&client, &index_metadata.url)?;

    let total_size = response.content_length();
    let progress_reader = StderrProgressReader::new(response, total_size);
    write_decoded_index(dest_path, progress_reader)?;
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
