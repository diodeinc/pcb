//! HTTP archive download support for package fetching.

use anyhow::Result;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const ZSTD_WINDOW_LOG_MAX: u32 = 31;
const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Download and extract an HTTP `.tar.zst` archive to target directory.
pub fn fetch_http_archive(url: &str, target_dir: &Path) -> Result<PathBuf> {
    log::debug!("Downloading archive from {}", url);

    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} from {}", response.status(), url);
    }

    let mut decoder = zstd::stream::read::Decoder::new(response)?;
    decoder.window_log_max(ZSTD_WINDOW_LOG_MAX)?;
    let reader = BufReader::with_capacity(IO_BUFFER_SIZE, decoder);
    let mut archive = tar::Archive::new(reader);
    std::fs::create_dir_all(target_dir)?;
    archive.unpack(target_dir)?;

    log::debug!("Extracted archive to {}", target_dir.display());
    Ok(target_dir.to_path_buf())
}

/// Download an HTTP file to a specific path.
pub fn fetch_http_file(url: &str, target_path: &Path) -> Result<PathBuf> {
    log::debug!("Downloading file from {}", url);

    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} from {}", response.status(), url);
    }

    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target_path, response.bytes()?)?;

    log::debug!("Wrote file to {}", target_path.display());
    Ok(target_path.to_path_buf())
}
