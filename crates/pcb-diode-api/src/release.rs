//! Release upload API client

use anyhow::{Context, Result};
use base64::Engine;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::auth::get_valid_token;
use crate::get_api_base_url;

/// Upload a board release archive to the Diode API.
pub fn upload_release(zip_path: &Path, workspace: &str) -> Result<()> {
    let token = get_valid_token()?;
    let base_url = get_api_base_url();

    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;

    // Calculate SHA256 hash
    let (sha256_hex, sha256_b64) = {
        let mut file = fs::File::open(zip_path)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let hash = hasher.finalize();
        (
            format!("{:x}", hash),
            base64::engine::general_purpose::STANDARD.encode(hash),
        )
    };

    // Get presigned upload URL
    let url = format!("{}/api/workspaces/{}/releases/upload", base_url, workspace);
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "artifactHash": sha256_hex }))
        .send()
        .context("Failed to request upload URL")?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "Failed to get upload URL ({}): {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }

    let upload_url = resp.json::<serde_json::Value>()?["uploadUrl"]
        .as_str()
        .context("Missing uploadUrl in response")?
        .to_string();

    // Upload to S3
    let resp = client
        .put(&upload_url)
        .header("Content-Type", "application/zip")
        .header("x-amz-checksum-sha256", sha256_b64)
        .body(fs::read(zip_path)?)
        .send()
        .context("Failed to upload release")?;

    if !resp.status().is_success() {
        anyhow::bail!("Upload failed: {}", resp.status());
    }

    Ok(())
}
