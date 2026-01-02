//! Download registry index from API server + S3

use anyhow::{Context, Result};
use fslock::LockFile;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// Create an HTTP client with proper User-Agent (required by our API gateway)
fn http_client() -> Result<Client> {
    Client::builder()
        .user_agent("pcb-registry/1.0")
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

impl AuthTokens {
    fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.expires_at - now < 300
    }
}

fn get_auth_file_path() -> Result<PathBuf> {
    let home_dir = dirs::home_dir().context("Failed to get home directory")?;
    let pcb_dir = home_dir.join(".pcb");
    fs::create_dir_all(&pcb_dir)?;
    Ok(pcb_dir.join("auth.toml"))
}

fn load_tokens() -> Result<Option<AuthTokens>> {
    let path = get_auth_file_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&contents)?))
}

fn save_tokens(tokens: &AuthTokens) -> Result<()> {
    let contents = toml::to_string(tokens)?;
    let auth_path = get_auth_file_path()?;
    let temp_path = auth_path.with_extension("toml.tmp");
    fs::write(&temp_path, &contents)?;
    fs::rename(&temp_path, &auth_path)?;
    Ok(())
}

#[derive(Serialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

fn refresh_tokens() -> Result<AuthTokens> {
    let lock_path = get_auth_file_path()?.with_extension("toml.lock");
    let mut lock = LockFile::open(&lock_path)?;
    lock.lock()?;

    let tokens = load_tokens()?.context("No tokens to refresh")?;
    if !tokens.is_expired() {
        return Ok(tokens);
    }

    let api_url = get_api_base_url();
    let url = format!("{}/api/auth/refresh", api_url);

    let response = http_client()?
        .post(&url)
        .json(&RefreshRequest {
            refresh_token: tokens.refresh_token.clone(),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Token refresh failed: {}", response.status());
    }

    let refresh_response: RefreshResponse = response.json()?;
    let new_tokens = AuthTokens {
        access_token: refresh_response.access_token,
        refresh_token: refresh_response.refresh_token,
        expires_at: refresh_response.expires_at,
    };

    save_tokens(&new_tokens)?;
    Ok(new_tokens)
}

fn get_valid_token() -> Result<String> {
    let tokens =
        load_tokens()?.context("Not authenticated. Run `pcb auth login` to authenticate.")?;

    if tokens.is_expired() {
        let new_tokens = refresh_tokens()?;
        return Ok(new_tokens.access_token);
    }

    Ok(tokens.access_token)
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
    let tmp = path.with_extension("version.tmp");
    fs::write(&tmp, version)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Fetch registry index metadata without downloading the file
pub fn fetch_registry_index_metadata() -> Result<RegistryIndexMetadata> {
    let token = get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = get_api_base_url();
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

/// Download registry index with progress reporting via channel
pub fn download_registry_index_with_progress(
    dest_path: &Path,
    progress_tx: &Sender<DownloadProgress>,
    is_update: bool,
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

    let token = match get_valid_token() {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("Auth required: {}", e);
            send_progress(None, true, Some(msg.clone()));
            anyhow::bail!(msg);
        }
    };

    let client = http_client()?;
    let api_url = get_api_base_url();

    let index_metadata: RegistryIndexMetadata = match client
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

    let temp_path = dest_path.with_extension("db.tmp");
    let mut file = File::create(&temp_path).context("Failed to create temp file")?;

    // Wrap response in a progress-tracking reader, then decompress with zstd
    let progress_reader = ProgressReader::new(response, total_size, &send_progress);
    let mut decoder =
        zstd::stream::Decoder::new(progress_reader).context("Failed to create zstd decoder")?;

    io::copy(&mut decoder, &mut file).context("Failed to decompress and write index")?;

    file.flush()?;
    drop(file);

    fs::rename(&temp_path, dest_path).context("Failed to move downloaded file into place")?;

    let _ = save_local_version(dest_path, &index_metadata.sha256);

    send_progress(Some(100), true, None);
    Ok(())
}

/// Download registry index (blocking, prints to stderr)
pub fn download_registry_index(dest_path: &Path) -> Result<()> {
    let token =
        get_valid_token().context("Authentication required. Run `pcb auth login` first.")?;

    let client = http_client()?;
    let api_url = get_api_base_url();

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

    let temp_path = dest_path.with_extension("db.tmp");
    let mut file = File::create(&temp_path).context("Failed to create temp file")?;

    // Wrap response in a progress-printing reader, then decompress with zstd
    let progress_reader = StderrProgressReader::new(response, total_size);
    let mut decoder =
        zstd::stream::Decoder::new(progress_reader).context("Failed to create zstd decoder")?;

    io::copy(&mut decoder, &mut file).context("Failed to decompress and write index")?;
    eprintln!();

    file.flush()?;
    drop(file);

    fs::rename(&temp_path, dest_path).context("Failed to move downloaded file into place")?;

    eprintln!("Registry index downloaded successfully.");
    Ok(())
}
