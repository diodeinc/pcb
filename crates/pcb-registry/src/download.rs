//! Download registry index from API server + S3

use anyhow::{Context, Result};
use fslock::LockFile;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub pct: Option<u8>,
    pub done: bool,
    pub error: Option<String>,
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

    let response = Client::new()
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

#[derive(Debug, Deserialize)]
struct RegistryIndexResponse {
    url: String,
    #[serde(rename = "expiresAt")]
    #[allow(dead_code)]
    expires_at: String,
}

/// Download registry index with progress reporting via channel
pub fn download_registry_index_with_progress(
    dest_path: &Path,
    progress_tx: &Sender<DownloadProgress>,
) -> Result<()> {
    let send_progress = |pct: Option<u8>, done: bool, error: Option<String>| {
        let _ = progress_tx.send(DownloadProgress { pct, done, error });
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

    let client = Client::new();
    let api_url = get_api_base_url();

    let index_response: RegistryIndexResponse = match client
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
        .get(&index_response.url)
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
    let mut downloaded: u64 = 0;

    let temp_path = dest_path.with_extension("db.tmp");
    let mut file = File::create(&temp_path).context("Failed to create temp file")?;

    let mut reader = response;
    let mut buffer = [0u8; 8192];
    let mut last_pct: u8 = 0;

    loop {
        let bytes_read = io::Read::read(&mut reader, &mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;

        if let Some(total) = total_size {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u8;
            if pct != last_pct {
                send_progress(Some(pct), false, None);
                last_pct = pct;
            }
        }
    }

    file.flush()?;
    drop(file);

    fs::rename(&temp_path, dest_path).context("Failed to move downloaded file into place")?;

    send_progress(Some(100), true, None);
    Ok(())
}

/// Download registry index (blocking, prints to stderr)
pub fn download_registry_index(dest_path: &Path) -> Result<()> {
    let token =
        get_valid_token().context("Authentication required. Run `pcb auth login` first.")?;

    let client = Client::new();
    let api_url = get_api_base_url();

    eprintln!("Fetching registry index URL...");
    let index_response: RegistryIndexResponse = client
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

    eprintln!("Downloading parts.db...");
    let response = client
        .get(&index_response.url)
        .send()
        .context("Failed to download registry index from S3")?
        .error_for_status()
        .context("S3 returned error when downloading registry index")?;

    let total_size = response.content_length();
    let mut downloaded: u64 = 0;

    let temp_path = dest_path.with_extension("db.tmp");
    let mut file = File::create(&temp_path).context("Failed to create temp file")?;

    let mut reader = response;
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = io::Read::read(&mut reader, &mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        file.write_all(&buffer[..bytes_read])?;
        downloaded += bytes_read as u64;

        if let Some(total) = total_size {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            eprint!("\rDownloading parts.db... {}%", pct);
        }
    }
    eprintln!();

    file.flush()?;
    drop(file);

    fs::rename(&temp_path, dest_path).context("Failed to move downloaded file into place")?;

    eprintln!("Registry index downloaded successfully.");
    Ok(())
}
