//! Minimal client for FreeRouting's local REST API (`--api_server.enabled=true`).
//!
//! We use this instead of FreeRouting's one-shot CLI mode (`-de`/`-do`)
//! because the CLI path only writes its `.ses` output once, gated on the
//! job's internal state reaching `COMPLETED` — and a confirmed upstream bug
//! (the `TIMED_OUT` state never promotes to `COMPLETED`) means a timeout or
//! interrupt in CLI mode essentially never produces a usable partial result.
//! The API's `GET /jobs/{id}/output` endpoint explicitly supports returning
//! partial output for a still-running or just-cancelled job, verified
//! against the pinned FreeRouting release.
//!
//! All requests are local loopback calls to a FreeRouting process we spawn
//! ourselves (see `super::mod.rs`), so there's no auth/workspace-context
//! plumbing here — that's what distinguishes this from the DeepPCB client in
//! `pcb-diode-api::routing`, which this module otherwise mirrors in style.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};

/// Per-request headers FreeRouting requires on every `/v1/*` call except
/// `/v1/system/status` — even with `--api_server.authentication.enabled=false`.
/// Neither is actually validated when auth is disabled; they just need to be
/// present. `Freerouting-Profile-ID` is a stable per-invocation UUID we mint
/// once and reuse for every request this process makes.
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
    /// Reported while the server is settling a job we asked to cancel, on
    /// its way to `Cancelled`.
    Stopping,
    /// FreeRouting supports pausing a job; not something we ever request
    /// ourselves, but a real state the API can report.
    Paused,
    Completed,
    Cancelled,
    TimedOut,
    /// The job's input (DSN) could not be processed at all — a terminal
    /// failure state distinct from `Cancelled`/`TimedOut`.
    Invalid,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobStatus {
    pub state: JobState,
    #[serde(default)]
    pub current_pass: Option<u32>,
}

pub enum JobOutput {
    /// Routed data is available (final or partial).
    Data(Vec<u8>),
    /// The job produced no output object at all — either it hasn't started
    /// routing yet, or (the case we actually care about) it had nothing to
    /// route in the first place (e.g. every net was already connected), so
    /// FreeRouting never populated an output artifact even though the job
    /// reached `COMPLETED`.
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

impl FreeroutingApiClient {
    pub fn new(base_url: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            // Every request this client makes targets a FreeRouting process
            // we spawned ourselves on 127.0.0.1 — never an external service.
            // Explicitly bypass any HTTP_PROXY/HTTPS_PROXY the user's
            // environment sets (common behind a corporate proxy), which
            // would otherwise try to route loopback traffic through it and
            // fail outright.
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

    /// Poll `/v1/system/status` (the one endpoint that needs no headers)
    /// until the server responds or `deadline` elapses.
    pub fn wait_ready(&self, deadline: Duration, mut still_alive: impl FnMut() -> Result<bool>) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            if let Ok(resp) = self.client.get(self.url("/v1/system/status")).send()
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
        let resp = self
            .with_headers(self.client.post(self.url("/v1/sessions/create")))
            .send()
            .context("Failed to create FreeRouting session")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to create FreeRouting session", resp));
        }
        let body: CreateSessionResponse = resp
            .json()
            .context("Failed to parse FreeRouting session response")?;
        Ok(body.id)
    }

    pub fn enqueue_job(&self, session_id: &str, name: &str) -> Result<String> {
        let resp = self
            .with_headers(self.client.post(self.url("/v1/jobs/enqueue")))
            .json(&EnqueueJobRequest {
                session_id,
                name,
                priority: "NORMAL",
            })
            .send()
            .context("Failed to enqueue FreeRouting job")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to enqueue FreeRouting job", resp));
        }
        let body: EnqueueJobResponse = resp
            .json()
            .context("Failed to parse FreeRouting job response")?;
        Ok(body.id)
    }

    pub fn update_settings(&self, job_id: &str, max_passes: u32, job_timeout: &str) -> Result<()> {
        let resp = self
            .with_headers(
                self.client
                    .post(self.url(&format!("/v1/jobs/{job_id}/settings"))),
            )
            .json(&UpdateSettingsRequest {
                max_passes,
                job_timeout: job_timeout.to_string(),
            })
            .send()
            .context("Failed to update FreeRouting job settings")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to update FreeRouting job settings", resp));
        }
        Ok(())
    }

    pub fn upload_input(&self, job_id: &str, filename: &str, dsn_bytes: &[u8]) -> Result<()> {
        use base64::Engine;
        let resp = self
            .with_headers(self.client.post(self.url(&format!("/v1/jobs/{job_id}/input"))))
            .json(&UploadInputRequest {
                filename,
                data: base64::engine::general_purpose::STANDARD.encode(dsn_bytes),
            })
            .send()
            .context("Failed to upload DSN input to FreeRouting")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to upload DSN input to FreeRouting", resp));
        }
        Ok(())
    }

    pub fn start_job(&self, job_id: &str) -> Result<()> {
        let resp = self
            .with_headers(self.client.put(self.url(&format!("/v1/jobs/{job_id}/start"))))
            .send()
            .context("Failed to start FreeRouting job")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to start FreeRouting job", resp));
        }
        Ok(())
    }

    pub fn get_job(&self, job_id: &str) -> Result<JobStatus> {
        let resp = self
            .with_headers(self.client.get(self.url(&format!("/v1/jobs/{job_id}"))))
            .send()
            .context("Failed to get FreeRouting job status")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to get FreeRouting job status", resp));
        }
        resp.json().context("Failed to parse FreeRouting job status")
    }

    /// Fetch the job's output (partial or final SES data), decoded to raw
    /// bytes. Returns `NothingToRoute` for the documented "in progress, no
    /// output yet" (204) case as well as the "job had nothing to route"
    /// case, since both mean there's no `.ses` to write.
    pub fn get_output(&self, job_id: &str) -> Result<JobOutput> {
        let resp = self
            .with_headers(self.client.get(self.url(&format!("/v1/jobs/{job_id}/output"))))
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

        // Non-success: FreeRouting reports "no output available" as a 400
        // error rather than an empty/204 response in (at least) two distinct
        // cases we've observed against the pinned release — both meaning
        // "there is nothing to write", not a real failure:
        //   - a completed job that had nothing to route (zero unrouted
        //     nets): "The job hasn't started yet."
        //   - a cancelled job with zero progress before cancellation:
        //     "The job is in state 'CANCELLED' and has no valid output."
        match resp.json::<ApiErrorBody>() {
            Ok(body) if is_no_output_error(&body.error) => Ok(JobOutput::NothingToRoute),
            Ok(body) => anyhow::bail!("Failed to get FreeRouting job output ({status}): {}", body.error),
            Err(_) => anyhow::bail!("Failed to get FreeRouting job output: {status}"),
        }
    }

    /// Best-effort: cancel a running or queued job. Errors are the caller's
    /// choice whether to surface — we always still try `get_output`
    /// afterward regardless of whether this succeeds.
    pub fn cancel_job(&self, job_id: &str) -> Result<()> {
        let resp = self
            .with_headers(self.client.put(self.url(&format!("/v1/jobs/{job_id}/cancel"))))
            .send()
            .context("Failed to cancel FreeRouting job")?;
        if !resp.status().is_success() {
            return Err(api_error("Failed to cancel FreeRouting job", resp));
        }
        Ok(())
    }
}

/// Whether an error body from `GET /jobs/{id}/output` means "no output
/// exists" (expected, not a failure) rather than a genuine error. Matched
/// against the exact wording FreeRouting has been observed to return; if
/// upstream changes this wording, the fallback is a hard error surfaced to
/// the user rather than a silently-wrong "nothing to route" result, which is
/// the safer failure mode.
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

    /// Every state FreeRouting's API can report must deserialize. An
    /// unlisted variant here fails `serde_json::from_str` at the call site
    /// in `get_job`, which `poll_job` mistakes for a lost connection to the
    /// server rather than a normal job state.
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
        ];
        for (json, expected) in cases {
            let actual: JobState =
                serde_json::from_str(json).unwrap_or_else(|e| panic!("failed to parse {json}: {e}"));
            assert_eq!(actual, expected, "mismatch parsing {json}");
        }
    }

    #[test]
    fn job_status_deserializes_with_current_pass() {
        let status: JobStatus =
            serde_json::from_str(r#"{"state":"RUNNING","current_pass":3}"#).unwrap();
        assert_eq!(status.state, JobState::Running);
        assert_eq!(status.current_pass, Some(3));
    }

    #[test]
    fn job_status_deserializes_without_current_pass() {
        let status: JobStatus = serde_json::from_str(r#"{"state":"QUEUED"}"#).unwrap();
        assert_eq!(status.state, JobState::Queued);
        assert_eq!(status.current_pass, None);
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
