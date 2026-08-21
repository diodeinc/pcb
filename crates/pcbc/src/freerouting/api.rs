//! Client for FreeRouting's local REST API (`--api_server.enabled=true`).
//!
//! Output must be fetched before cancelling a job — `GET /jobs/{id}/output`
//! reports nothing once the job settles into `CANCELLED`.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};

pub struct FreeroutingApiClient {
    client: Client,
    base_url: String,
    env_host: String,
    profile_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    Queued,
    ReadyToStart,
    Running,
    Stopping,
    Paused,
    Completed,
    Cancelled,
    TimedOut,
    Invalid,
    Terminated,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobStatus {
    pub state: JobState,
    #[serde(default)]
    pub current_pass: Option<u32>,
}

pub enum JobOutput {
    Data(Vec<u8>),
    NothingToRoute,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: String,
}

#[derive(Deserialize)]
struct CreateSessionResponse {
    id: String,
}

#[derive(Serialize)]
struct EnqueueJobRequest<'a> {
    session_id: &'a str,
    name: &'a str,
    priority: &'a str,
}

#[derive(Deserialize)]
struct EnqueueJobResponse {
    id: String,
}

#[derive(Serialize)]
struct UpdateSettingsRequest {
    max_passes: u32,
    job_timeout: String,
}

#[derive(Serialize)]
struct UploadInputRequest<'a> {
    filename: &'a str,
    data: String,
}

#[derive(Deserialize)]
struct JobOutputResponse {
    data: String,
}

#[derive(Deserialize)]
struct DrcReport {
    #[serde(default)]
    unconnected_items: Vec<serde_json::Value>,
}

pub const GET_OUTPUT_TIMEOUT: Duration = Duration::from_secs(120);

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

impl FreeroutingApiClient {
    pub fn new(base_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .no_proxy()
            .build()
            .context("Failed to create FreeRouting API HTTP client")?;
        Ok(Self {
            client,
            base_url,
            env_host: format!("pcb/{}", env!("CARGO_PKG_VERSION")),
            profile_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    fn with_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
            .header("Freerouting-Environment-Host", &self.env_host)
            .header("Freerouting-Profile-ID", &self.profile_id)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.client.get(self.url(path))
    }

    fn post(&self, path: &str) -> RequestBuilder {
        self.client.post(self.url(path))
    }

    fn put(&self, path: &str) -> RequestBuilder {
        self.client.put(self.url(path))
    }

    fn send(&self, builder: RequestBuilder, action: &str) -> Result<Response> {
        let resp = self
            .with_headers(builder)
            .send()
            .with_context(|| action.to_string())?;
        if !resp.status().is_success() {
            return Err(api_error(action, resp));
        }
        Ok(resp)
    }

    pub fn wait_ready(
        &self,
        deadline: Duration,
        mut still_alive: impl FnMut() -> Result<bool>,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            if let Ok(resp) = self.get("/v1/system/status").send()
                && resp.status().is_success()
            {
                return Ok(());
            }
            if !still_alive()? {
                anyhow::bail!("FreeRouting API server exited during startup");
            }
            if start.elapsed() > deadline {
                anyhow::bail!(
                    "FreeRouting API server did not become ready within {:?}",
                    deadline
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn create_session(&self) -> Result<String> {
        let resp = self.send(
            self.post("/v1/sessions/create"),
            "Failed to create FreeRouting session",
        )?;
        let body: CreateSessionResponse = resp
            .json()
            .context("Failed to parse FreeRouting session response")?;
        Ok(body.id)
    }

    pub fn enqueue_job(&self, session_id: &str, name: &str) -> Result<String> {
        let resp = self.send(
            self.post("/v1/jobs/enqueue").json(&EnqueueJobRequest {
                session_id,
                name,
                priority: "NORMAL",
            }),
            "Failed to enqueue FreeRouting job",
        )?;
        let body: EnqueueJobResponse = resp
            .json()
            .context("Failed to parse FreeRouting job response")?;
        Ok(body.id)
    }

    pub fn update_settings(&self, job_id: &str, max_passes: u32, job_timeout: &str) -> Result<()> {
        self.send(
            self.post(&format!("/v1/jobs/{job_id}/settings"))
                .json(&UpdateSettingsRequest {
                    max_passes,
                    job_timeout: job_timeout.to_string(),
                }),
            "Failed to update FreeRouting job settings",
        )?;
        Ok(())
    }

    pub fn upload_input(&self, job_id: &str, filename: &str, dsn_bytes: &[u8]) -> Result<()> {
        use base64::Engine;
        self.send(
            self.post(&format!("/v1/jobs/{job_id}/input"))
                .json(&UploadInputRequest {
                    filename,
                    data: base64::engine::general_purpose::STANDARD.encode(dsn_bytes),
                }),
            "Failed to upload DSN input to FreeRouting",
        )?;
        Ok(())
    }

    pub fn start_job(&self, job_id: &str) -> Result<()> {
        self.send(
            self.put(&format!("/v1/jobs/{job_id}/start")),
            "Failed to start FreeRouting job",
        )?;
        Ok(())
    }

    pub fn get_job(&self, job_id: &str) -> Result<JobStatus> {
        let resp = self.send(
            self.get(&format!("/v1/jobs/{job_id}")),
            "Failed to get FreeRouting job status",
        )?;
        resp.json()
            .context("Failed to parse FreeRouting job status")
    }

    pub fn get_output(&self, job_id: &str, timeout: Duration) -> Result<JobOutput> {
        let resp = self
            .with_headers(
                self.get(&format!("/v1/jobs/{job_id}/output"))
                    .timeout(timeout),
            )
            .send()
            .context("Failed to get FreeRouting job output")?;

        let status = resp.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(JobOutput::NothingToRoute);
        }
        if status.is_success() {
            let body: JobOutputResponse = resp
                .json()
                .context("Failed to parse FreeRouting output response")?;
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&body.data)
                .context("Failed to decode FreeRouting output as base64")?;
            return Ok(JobOutput::Data(bytes));
        }

        match resp.json::<ApiErrorBody>() {
            Ok(body) if is_no_output_error(&body.error) => Ok(JobOutput::NothingToRoute),
            Ok(body) => anyhow::bail!(
                "Failed to get FreeRouting job output ({status}): {}",
                body.error
            ),
            Err(_) => anyhow::bail!("Failed to get FreeRouting job output: {status}"),
        }
    }

    pub fn get_unrouted_count(&self, job_id: &str) -> Result<usize> {
        let resp = self.send(
            self.get(&format!("/v1/jobs/{job_id}/drc"))
                .timeout(GET_OUTPUT_TIMEOUT),
            "Failed to get FreeRouting DRC report",
        )?;
        let report: DrcReport = resp
            .json()
            .context("Failed to parse FreeRouting DRC report")?;
        Ok(report.unconnected_items.len())
    }

    pub fn cancel_job(&self, job_id: &str) -> Result<()> {
        self.send(
            self.put(&format!("/v1/jobs/{job_id}/cancel")),
            "Failed to cancel FreeRouting job",
        )?;
        Ok(())
    }
}

fn is_no_output_error(error: &str) -> bool {
    let error = error.to_lowercase();
    error.contains("hasn't started") || error.contains("no valid output")
}

fn api_error(context: &str, response: Response) -> anyhow::Error {
    let status = response.status();
    match response.json::<ApiErrorBody>() {
        Ok(body) if !body.error.is_empty() => {
            anyhow::anyhow!("{context} ({status}): {}", body.error)
        }
        _ => anyhow::anyhow!("{context}: {status}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_state_deserializes_all_known_variants() {
        let cases = [
            ("\"QUEUED\"", JobState::Queued),
            ("\"READY_TO_START\"", JobState::ReadyToStart),
            ("\"RUNNING\"", JobState::Running),
            ("\"STOPPING\"", JobState::Stopping),
            ("\"PAUSED\"", JobState::Paused),
            ("\"COMPLETED\"", JobState::Completed),
            ("\"CANCELLED\"", JobState::Cancelled),
            ("\"TIMED_OUT\"", JobState::TimedOut),
            ("\"INVALID\"", JobState::Invalid),
            ("\"TERMINATED\"", JobState::Terminated),
        ];
        for (json, expected) in cases {
            let actual: JobState = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("failed to parse {json}: {e}"));
            assert_eq!(actual, expected, "mismatch parsing {json}");
        }
    }

    #[test]
    fn drc_report_counts_unconnected_items() {
        let report: DrcReport =
            serde_json::from_str(r#"{"unconnected_items":[{},{}],"violations":[]}"#).unwrap();
        assert_eq!(report.unconnected_items.len(), 2);
    }

    #[test]
    fn is_no_output_error_matches_observed_freerouting_wording() {
        assert!(is_no_output_error("The job hasn't started yet."));
        assert!(is_no_output_error(
            "The job is in state 'CANCELLED' and has no valid output."
        ));
        assert!(!is_no_output_error("Internal server error"));
    }
}
