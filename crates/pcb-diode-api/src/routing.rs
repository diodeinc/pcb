//! DeepPCB routing API client
//!
//! Provides functions for cloud-based PCB auto-routing via the DeepPCB service.

use anyhow::{Context, Result};
use reqwest::blocking::{Client, multipart};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

use crate::auth::get_valid_token;
use crate::get_api_base_url;

/// Request to start a routing job
pub struct StartRoutingRequest {
    /// Project identifier (used for grouping/billing)
    pub project_id: String,
    /// Timeout in minutes (default: 20, max: 60)
    pub timeout: Option<u32>,
}

/// Status of a routing job
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RoutingStatus {
    Queued,
    InProgress,
    Complete,
    Error,
}

/// Statistics about the routing progress
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingStats {
    pub total_nets: u32,
    pub nets_completed: u32,
    pub air_wires_total: u32,
    pub air_wires_connected: u32,
    pub vias: u32,
    pub wire_length: f64,
    pub revision_number: u32,
    pub processing_time: Option<String>,
}

/// Information about a routing job
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingJob {
    pub id: String,
    pub project_id: String,
    pub status: RoutingStatus,
    #[serde(default)]
    pub converged: bool,
    #[serde(default)]
    pub anomalies: Vec<serde_json::Value>,
    pub stats: Option<RoutingStats>,
    pub cost_per_minute: Option<f64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: Option<String>,
    pub timeout: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct StartRoutingResponse {
    id: String,
}

fn create_client() -> Result<Client> {
    let user_agent = format!("diode-pcb/{}", env!("CARGO_PKG_VERSION"));
    Client::builder()
        .user_agent(user_agent)
        .timeout(Duration::from_secs(120))
        .build()
        .context("Failed to create HTTP client")
}

/// Start a new routing job by uploading KiCad files
///
/// # Arguments
/// * `board_path` - Path to the .kicad_pcb file
/// * `project_path` - Path to the .kicad_pro file
/// * `request` - Routing request parameters
///
/// # Returns
/// The job ID for tracking the routing progress
pub fn start_routing(
    board_path: &Path,
    project_path: &Path,
    request: &StartRoutingRequest,
) -> Result<String> {
    let token = get_valid_token()?;
    let api_url = get_api_base_url();
    let url = format!("{}/api/routing", api_url);

    // Build multipart form
    let mut form = multipart::Form::new()
        .text("projectId", request.project_id.clone())
        .file("kicadBoardFile", board_path)
        .context("Failed to attach board file")?
        .file("kicadProjectFile", project_path)
        .context("Failed to attach project file")?;

    if let Some(timeout) = request.timeout {
        form = form.text("timeout", timeout.to_string());
    }

    let client = create_client()?;
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .multipart(form)
        .send()
        .context("Failed to send routing request")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("Failed to start routing ({}): {}", status, body);
    }

    let result: StartRoutingResponse = response
        .json()
        .context("Failed to parse routing response")?;

    Ok(result.id)
}

/// Get the current status of a routing job
///
/// # Arguments
/// * `job_id` - The job ID returned from `start_routing`
///
/// # Returns
/// Current job status including routing statistics
pub fn get_routing_status(job_id: &str) -> Result<RoutingJob> {
    let token = get_valid_token()?;
    let api_url = get_api_base_url();
    let url = format!("{}/api/routing/{}", api_url, job_id);

    let client = create_client()?;
    let response = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to get routing status")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("Failed to get routing status ({}): {}", status, body);
    }

    let job: RoutingJob = response.json().context("Failed to parse routing status")?;

    Ok(job)
}

/// Download the current best routing result as a SES file
///
/// # Arguments
/// * `job_id` - The job ID returned from `start_routing`
///
/// # Returns
/// The SES file contents as bytes
pub fn download_routing_result(job_id: &str) -> Result<Vec<u8>> {
    let token = get_valid_token()?;
    let api_url = get_api_base_url();
    let url = format!("{}/api/routing/{}/download?format=ses", api_url, job_id);

    let client = create_client()?;
    let response = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to download routing result")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("Failed to download routing result ({}): {}", status, body);
    }

    let bytes = response.bytes().context("Failed to read routing result")?;

    Ok(bytes.to_vec())
}

/// Stop a routing job early
///
/// The best result found so far remains available for download.
pub fn stop_routing(job_id: &str) -> Result<()> {
    let token = get_valid_token()?;
    let api_url = get_api_base_url();
    let url = format!("{}/api/routing/{}/stop", api_url, job_id);

    let client = create_client()?;
    let response = client
        .post(&url)
        .bearer_auth(&token)
        .send()
        .context("Failed to stop routing")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("Failed to stop routing ({}): {}", status, body);
    }

    Ok(())
}
