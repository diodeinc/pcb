//! Download registry index from API server + S3

use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

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
                eprint!("\rDownloading parts.db.zst... {}%", pct);
                self.last_pct = pct;
            }
        }

        Ok(bytes_read)
    }
}

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

fn version_file_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("db.version")
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
        .map_err(|err| anyhow::anyhow!("Failed to write local registry version: {err}"))?;
    Ok(())
}

/// Fetch registry index metadata without downloading the file
pub fn fetch_registry_index_metadata() -> Result<RegistryIndexMetadata> {
    let token = crate::auth::get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{}/api/registry/index", api_url);

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {} failed", url))?
        .error_for_status()
        .with_context(|| format!("API error from {}", url))?;

    resp.json()
        .context("Failed to parse registry index metadata")
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
    let url = format!("{}/api/registry/index", api_url);

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {} failed", url))?;

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
                .with_context(|| format!("API error from {}", url))?;
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
            pct,
            done,
            error,
            is_update,
        });
    };

    send_progress(None, false, None);

    let client = http_client()?;

    // Use pre-fetched metadata or fetch from API
    let index_metadata: RegistryIndexMetadata = if let Some(meta) = prefetched_metadata {
        meta.clone()
    } else {
        let token = match crate::auth::get_valid_token() {
            Ok(t) => t,
            Err(e) => {
                let msg = format!("Auth required: {}", e);
                send_progress(None, true, Some(msg.clone()));
                anyhow::bail!(msg);
            }
        };

        let api_url = crate::get_api_base_url();
        match client
            .get(format!("{}/api/registry/index", api_url))
            .bearer_auth(&token)
            .send()
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.json() {
                Ok(j) => j,
                Err(e) => {
                    let msg = format!("Failed to parse index response: {}", e);
                    send_progress(None, true, Some(msg.clone()));
                    anyhow::bail!(msg);
                }
            },
            Err(e) => {
                let msg = format!("Failed to fetch index URL: {}", e);
                send_progress(None, true, Some(msg.clone()));
                anyhow::bail!(msg);
            }
        }
    };

    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).context("Failed to create registry directory")?;
    }

    let response = match client
        .get(&index_metadata.url)
        .send()
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to download from S3: {}", e);
            send_progress(None, true, Some(msg.clone()));
            anyhow::bail!(msg);
        }
    };

    let total_size = response.content_length();

    // Wrap response in a progress-tracking reader, then decompress with zstd
    let progress_reader = ProgressReader::new(response, total_size, &send_progress);
    let mut decoder =
        zstd::stream::Decoder::new(progress_reader).context("Failed to create zstd decoder")?;
    AtomicFile::new(dest_path, OverwriteBehavior::AllowOverwrite)
        .write(|file| {
            io::copy(&mut decoder, file).map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("Failed to decompress and write index: {err}"),
                )
            })?;
            file.flush()
        })
        .context("Failed to move downloaded file into place")?;

    let _ = save_local_version(dest_path, &index_metadata.sha256);

    send_progress(Some(100), true, None);
    Ok(())
}

/// Download registry index (blocking, prints to stderr)
pub fn download_registry_index(dest_path: &Path) -> Result<()> {
    let token = crate::auth::get_valid_token()
        .context("Authentication required. Run `pcb auth login` first.")?;

    let client = http_client()?;
    let api_url = crate::get_api_base_url();

    eprintln!("Fetching registry index URL...");
    let index_metadata: RegistryIndexMetadata = client
        .get(format!("{}/api/registry/index", api_url))
        .bearer_auth(&token)
        .send()
        .context("Failed to fetch registry index URL")?
        .error_for_status()
        .context("API returned error when fetching registry index URL")?
        .json()
        .context("Failed to parse registry index response")?;

    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent).context("Failed to create registry directory")?;
    }

    eprintln!("Downloading parts.db.zst...");
    let response = client
        .get(&index_metadata.url)
        .send()
        .context("Failed to download registry index from S3")?
        .error_for_status()
        .context("S3 returned error when downloading registry index")?;

    let total_size = response.content_length();

    // Wrap response in a progress-printing reader, then decompress with zstd
    let progress_reader = StderrProgressReader::new(response, total_size);
    let mut decoder =
        zstd::stream::Decoder::new(progress_reader).context("Failed to create zstd decoder")?;
    AtomicFile::new(dest_path, OverwriteBehavior::AllowOverwrite)
        .write(|file| {
            io::copy(&mut decoder, file).map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("Failed to decompress and write index: {err}"),
                )
            })?;
            file.flush()
        })
        .context("Failed to move downloaded file into place")?;
    eprintln!();

    eprintln!("Registry index downloaded successfully.");
    Ok(())
}
