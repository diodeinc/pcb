//! Release upload API client
//!
//! Two-phase upload flow:
//! 1. Stage: POST /api/file/{hash} to get presigned S3 URL, then PUT to S3
//! 2. Create: POST /api/workspaces/{workspace}/releases to finalize the release

use anyhow::{bail, Context, Result};
use base64::Engine;
use reqwest::blocking::{Client, Response};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use crate::auth::get_valid_token;
use crate::get_api_base_url;

/// Response from creating a release.
#[derive(Debug)]
pub struct ReleaseResult {
    pub project_id: String,
    pub commit_sha: Option<String>,
    pub version: Option<String>,
    pub release_id: Option<String>,
}

/// Response from creating a preview.
#[derive(Debug)]
pub struct PreviewResult {
    pub preview_id: String,
    pub preview_url: String,
}

/// Upload a board release archive to the Diode API.
pub fn upload_release(zip_path: &Path, workspace: &str) -> Result<ReleaseResult> {
    let token = get_valid_token()?;
    let base_url = get_api_base_url();

    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;

    let (sha256_hex, sha256_b64) = calculate_sha256(zip_path)?;
    stage_artifact(
        &client,
        &base_url,
        &token,
        zip_path,
        &sha256_hex,
        &sha256_b64,
    )?;
    create_release(&client, &base_url, &token, workspace, &sha256_hex)
}

/// Upload a board preview archive to the Diode API.
pub fn upload_preview(zip_path: &Path) -> Result<PreviewResult> {
    let token = get_valid_token()?;
    let base_url = get_api_base_url();

    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300))
        .build()?;

    let (sha256_hex, sha256_b64) = calculate_sha256(zip_path)?;
    stage_artifact(
        &client,
        &base_url,
        &token,
        zip_path,
        &sha256_hex,
        &sha256_b64,
    )?;
    create_preview(&client, &base_url, &token, &sha256_hex)
}

fn calculate_sha256(path: &Path) -> Result<(String, String)> {
    let mut file = fs::File::open(path)?;
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
    Ok((
        format!("{:x}", hash),
        base64::engine::general_purpose::STANDARD.encode(hash),
    ))
}

fn stage_artifact(
    client: &Client,
    base_url: &str,
    token: &str,
    zip_path: &Path,
    sha256_hex: &str,
    sha256_b64: &str,
) -> Result<()> {
    let url = format!("{}/api/file/{}", base_url, sha256_hex);
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .send()
        .context("Failed to connect to Diode API")?;

    let resp = check_response(resp, |status, msg| match status {
        StatusCode::UNAUTHORIZED => "Authentication failed. Run `pcb login` to sign in.".into(),
        StatusCode::BAD_REQUEST => format!("Invalid request: {msg}"),
        _ => format!("Failed to get upload URL ({status}): {msg}"),
    })?;

    let upload_url = resp.json::<serde_json::Value>()?["uploadUrl"]
        .as_str()
        .context("Missing uploadUrl in response")?
        .to_string();

    let resp = client
        .put(&upload_url)
        .header("Content-Type", "application/zip")
        .header("x-amz-checksum-sha256", sha256_b64)
        .body(fs::read(zip_path)?)
        .send()
        .context("Failed to upload to storage")?;

    check_response(resp, |status, _| match status {
        StatusCode::BAD_REQUEST => "Upload rejected: checksum mismatch".into(),
        _ => format!("Upload to storage failed ({status})"),
    })
    .map(|_| ())
}

fn create_release(
    client: &Client,
    base_url: &str,
    token: &str,
    workspace: &str,
    sha256_hex: &str,
) -> Result<ReleaseResult> {
    let url = format!(
        "{}/api/workspaces/{}/releases",
        base_url,
        urlencoding::encode(workspace)
    );

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "artifactHash": sha256_hex }))
        .send()
        .context("Failed to connect to Diode API")?;

    let resp = check_response(resp, |status, msg| match status {
        StatusCode::UNAUTHORIZED => "Authentication failed. Run `pcb login` to sign in.".into(),
        StatusCode::NOT_FOUND if msg.contains("artifact") => {
            "Staged artifact not found. The upload may have expired.".into()
        }
        StatusCode::NOT_FOUND => format!("Workspace '{workspace}' not found"),
        StatusCode::CONFLICT => "Release version already exists".into(),
        StatusCode::BAD_REQUEST if msg.contains("metadata.json") => {
            "Invalid release archive: missing or malformed metadata.json".into()
        }
        StatusCode::BAD_REQUEST if msg.contains("Checksum") => "Checksum mismatch".into(),
        StatusCode::BAD_REQUEST => format!("Invalid release: {msg}"),
        _ => format!("Failed to create release ({status}): {msg}"),
    })?;

    let json: serde_json::Value = resp.json().context("Invalid response from server")?;

    Ok(ReleaseResult {
        project_id: json["projectId"]
            .as_str()
            .context("Missing projectId in response")?
            .to_string(),
        commit_sha: json["commitSha"].as_str().map(String::from),
        version: json["version"].as_str().map(String::from),
        release_id: json["releaseId"].as_str().map(String::from),
    })
}

fn create_preview(
    client: &Client,
    base_url: &str,
    token: &str,
    sha256_hex: &str,
) -> Result<PreviewResult> {
    let url = format!("{}/api/previews", base_url);

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .json(&serde_json::json!({ "artifactHash": sha256_hex }))
        .send()
        .context("Failed to connect to Diode API")?;

    let resp = check_response(resp, |status, msg| match status {
        StatusCode::UNAUTHORIZED => "Authentication failed. Run `pcb login` to sign in.".into(),
        StatusCode::NOT_FOUND if msg.contains("artifact") => {
            "Staged artifact not found. The upload may have expired.".into()
        }
        StatusCode::BAD_REQUEST if msg.contains("metadata") => {
            "Invalid preview archive: missing metadata.json".into()
        }
        StatusCode::BAD_REQUEST if msg.contains("Checksum") || msg.contains("checksum") => {
            "Checksum mismatch".into()
        }
        StatusCode::BAD_REQUEST => format!("Invalid preview: {msg}"),
        _ => format!("Failed to create preview ({status}): {msg}"),
    })?;

    let json: serde_json::Value = resp.json().context("Invalid response from server")?;
    let preview_id = json["previewId"]
        .as_str()
        .context("Missing previewId in response")?
        .to_string();

    let preview_url = if let Some(url) = json["previewUrl"].as_str() {
        url.to_string()
    } else {
        format!("{}/preview/{}", crate::get_web_base_url(), &preview_id)
    };

    Ok(PreviewResult {
        preview_id,
        preview_url,
    })
}

fn check_response(
    resp: Response,
    format_error: impl FnOnce(StatusCode, &str) -> String,
) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let msg = resp
        .text()
        .ok()
        .and_then(|body| {
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()?
                .get("error")?
                .as_str()
                .map(String::from)
        })
        .unwrap_or_default();
    bail!("{}", format_error(status, &msg))
}
