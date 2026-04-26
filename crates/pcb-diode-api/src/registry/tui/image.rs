//! Image decoding utilities for TUI

use anyhow::{Context, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use image::GenericImageView;
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

const IMAGE_WORKER_COUNT: usize = 4;
const IMAGE_LOADING_DELAY_MS: u64 = 150;

/// Whether the terminal supports image display
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageProtocol {
    Supported,
    None,
}

impl ImageProtocol {
    pub fn detect() -> Self {
        // Check for Ghostty or Kitty terminal
        if let Ok(term) = std::env::var("TERM")
            && (term.contains("kitty") || term.contains("ghostty"))
        {
            return ImageProtocol::Supported;
        }
        // Ghostty also sets TERM_PROGRAM
        if let Ok(prog) = std::env::var("TERM_PROGRAM")
            && prog.to_lowercase().contains("ghostty")
        {
            return ImageProtocol::Supported;
        }
        ImageProtocol::None
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, ImageProtocol::Supported)
    }
}

/// Decode image bytes into a renderable protocol
pub fn decode_image(bytes: &[u8], picker: &Picker) -> Option<StatefulProtocol> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(picker.new_resize_protocol(img))
}

pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::load_from_memory(bytes)
        .ok()
        .map(|img| img.dimensions())
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ImageKey {
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct ImageRequest {
    pub key: ImageKey,
}

#[derive(Debug, Clone)]
pub enum ImageResult {
    Ready(Vec<u8>),
    Missing,
    Failed,
}

pub type ImageResponse = (ImageKey, ImageResult);

enum ImageState {
    Pending { started_at: Instant },
    Ready(Arc<Vec<u8>>),
    Missing,
}

pub struct ImageStore {
    by_key: HashMap<ImageKey, ImageState>,
}

impl ImageStore {
    pub fn new() -> Self {
        Self {
            by_key: HashMap::new(),
        }
    }

    pub fn queue_request(&mut self, key: ImageKey, started_at: Instant) -> Option<ImageRequest> {
        match self.by_key.get(&key) {
            Some(ImageState::Pending { .. } | ImageState::Ready(_) | ImageState::Missing) => None,
            None => {
                self.by_key
                    .insert(key.clone(), ImageState::Pending { started_at });
                Some(ImageRequest { key })
            }
        }
    }

    pub fn apply_response(&mut self, response: ImageResponse) {
        let (key, result) = response;
        match result {
            ImageResult::Ready(bytes) => {
                self.by_key.insert(key, ImageState::Ready(Arc::new(bytes)));
            }
            ImageResult::Missing => {
                self.by_key.insert(key, ImageState::Missing);
            }
            ImageResult::Failed => {
                self.by_key.remove(&key);
            }
        }
    }

    pub fn lookup(&self, key: &ImageKey) -> (Option<Arc<Vec<u8>>>, bool) {
        match self.by_key.get(key) {
            Some(ImageState::Ready(bytes)) => (Some(bytes.clone()), false),
            Some(ImageState::Pending { started_at }) => (
                None,
                started_at.elapsed() > Duration::from_millis(IMAGE_LOADING_DELAY_MS),
            ),
            Some(ImageState::Missing) | None => (None, false),
        }
    }
}

pub fn spawn_image_workers(
    req_rx: Receiver<ImageRequest>,
    resp_tx: Sender<ImageResponse>,
) -> Vec<JoinHandle<()>> {
    let req_rx = Arc::new(Mutex::new(req_rx));

    (0..IMAGE_WORKER_COUNT)
        .map(|_| {
            let req_rx = Arc::clone(&req_rx);
            let resp_tx = resp_tx.clone();
            thread::spawn(move || {
                let client = match Client::builder().timeout(Duration::from_secs(30)).build() {
                    Ok(client) => client,
                    Err(err) => {
                        log::warn!("Failed to build registry image HTTP client: {err}");
                        return;
                    }
                };

                loop {
                    let request = match req_rx
                        .lock()
                        .expect("image worker receiver poisoned")
                        .recv()
                    {
                        Ok(request) => request,
                        Err(_) => break,
                    };

                    let result = match load_or_fetch_registry_image(&client, &request.key.sha256) {
                        Ok(result) => result,
                        Err(err) => {
                            log::warn!("Registry image fetch failed: {err}");
                            ImageResult::Failed
                        }
                    };

                    let _ = resp_tx.send((request.key, result));
                }
            })
        })
        .collect()
}

fn load_or_fetch_registry_image(client: &Client, sha256: &str) -> Result<ImageResult> {
    let sha256 = sha256.to_ascii_lowercase();
    if !is_valid_sha256(&sha256) {
        bail!("invalid registry image hash: {sha256}");
    }

    if let Some(bytes) = read_valid_cached_image(&sha256)? {
        return Ok(ImageResult::Ready(bytes));
    }

    let token = crate::auth::get_valid_token().context("image auth failed")?;
    let Some(url) = resolve_registry_image_url(client, &token, &sha256)? else {
        return Ok(ImageResult::Missing);
    };

    let bytes = download_image_bytes(client, &url)?;
    let actual_sha256 = hex::encode(Sha256::digest(&bytes));
    if actual_sha256 != sha256 {
        bail!("registry image hash mismatch: expected {sha256}, got {actual_sha256}");
    }

    write_cached_image(&sha256, &bytes)?;
    Ok(ImageResult::Ready(bytes))
}

fn read_valid_cached_image(sha256: &str) -> Result<Option<Vec<u8>>> {
    let path = registry_image_cache_path(sha256);
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .with_context(|| format!("failed to read cached image {}", path.display()))?;
    if hex::encode(Sha256::digest(&bytes)) == sha256 {
        Ok(Some(bytes))
    } else {
        log::warn!(
            "Ignoring cached registry image with hash mismatch: {}",
            path.display()
        );
        let _ = fs::remove_file(&path);
        Ok(None)
    }
}

fn write_cached_image(sha256: &str, bytes: &[u8]) -> Result<()> {
    let path = registry_image_cache_path(sha256);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create registry image cache directory {}",
                parent.display()
            )
        })?;
    }

    AtomicFile::new(&path, OverwriteBehavior::AllowOverwrite)
        .write(|file| {
            file.write_all(bytes)?;
            file.flush()
        })
        .map_err(|err| anyhow::anyhow!("failed to write cached registry image: {err}"))?;
    Ok(())
}

fn registry_image_cache_path(sha256: &str) -> PathBuf {
    let prefix = sha256.get(..2).unwrap_or("xx");
    pcb_zen::cache_index::cache_base()
        .join("registry")
        .join("images")
        .join(prefix)
        .join(sha256)
}

#[derive(Debug, Deserialize)]
struct SignedImageResponse {
    url: String,
}

fn resolve_registry_image_url(
    client: &Client,
    token: &str,
    sha256: &str,
) -> Result<Option<String>> {
    let endpoint = format!("{}/api/registry/images/{sha256}", crate::get_api_base_url());
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .context("failed to resolve registry image URL")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }

    if !response.status().is_success() {
        bail!("registry image URL resolver returned {}", response.status());
    }

    let signed: SignedImageResponse = response
        .json()
        .context("failed to decode registry image URL response")?;
    Ok(Some(signed.url))
}

fn download_image_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .context("failed to download registry image")?;

    if !response.status().is_success() {
        bail!("registry image download returned {}", response.status());
    }

    Ok(response
        .bytes()
        .context("failed to read registry image bytes")?
        .to_vec())
}

fn is_valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
