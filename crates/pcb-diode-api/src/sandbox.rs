use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex, TryLockError,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{
    CONTENT_TYPE, ETAG, HeaderMap, HeaderName, HeaderValue, IF_MATCH, IF_NONE_MATCH, LOCATION,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::WorkspaceContext;

pub const SANDBOX_LOCK_FILE_PATH: &str = "/home/sandbox/.diode/sandbox-lock.json";
const LOCK_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const FILE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Commands run job-shaped: create the exec, poll its state, then read the
/// output files it left behind. Polling is stateless and idempotent, so a
/// dropped connection costs one retry instead of a resume protocol.
const EXEC_POLL_INITIAL_DELAY: Duration = Duration::from_millis(150);
const EXEC_POLL_MAX_DELAY: Duration = Duration::from_secs(2);
const EXEC_POLL_MAX_CONSECUTIVE_FAILURES: usize = 3;
/// Grace beyond the command's own timeout before polling gives up; the data
/// plane kills the command at its timeout, so this only covers slow polls.
const EXEC_POLL_GRACE: Duration = Duration::from_secs(60);
/// Matches the data plane's default command timeout (SANDBOXD_DEFAULT_TIMEOUT_MS).
const EXEC_DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Refuse to download exec output files larger than this; anything pcb
/// parses (layout result JSON) is far below it, and a runaway command must
/// not balloon a sync session's memory.
const EXEC_MAX_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
/// Where wrapped commands leave their output files inside the sandbox.
const EXEC_OUTPUT_DIR: &str = "/tmp/.pcb-exec";

#[derive(Clone)]
pub struct SandboxClient {
    api_base_url: String,
    ctx: WorkspaceContext,
    http: Client,
    connections: Arc<Mutex<BTreeMap<String, Arc<SandboxConnection>>>>,
    scope: RequestScope,
}

#[derive(Clone)]
enum RequestScope {
    Unleased,
    Editing(Arc<SandboxLockState>),
    Cleanup(Instant),
}

/// Provider-neutral transport details minted atomically by the API.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxConnection {
    http: SandboxHttpConnection,
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
struct SandboxHttpConnection {
    endpoint: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
}

fn sandbox_headers(values: &BTreeMap<String, String>) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        let name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("Invalid sandbox connection header name: {name}"))?;
        let mut value = HeaderValue::from_bytes(value.as_bytes())
            .context("Invalid sandbox connection header value")?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}

fn validate_sandbox_endpoint(api_base_url: &str, endpoint: &str) -> Result<()> {
    let endpoint = reqwest::Url::parse(endpoint).context("Invalid sandbox connection endpoint")?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        bail!("Sandbox connection endpoint must be an HTTP(S) URL with a host");
    }

    let api = reqwest::Url::parse(api_base_url).context("Invalid Diode API URL")?;
    if api.scheme() == "https" && endpoint.scheme() != "https" {
        bail!("Sandbox connection endpoint cannot downgrade an HTTPS API connection");
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecSyncRequest {
    #[serde(rename = "cmd")]
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
}

impl ExecSyncRequest {
    pub fn command(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            cwd: None,
            env: None,
            timeout_ms: None,
        }
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(timeout.as_millis().min(i64::MAX as u128) as i64);
        self
    }

    pub fn env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = Some(env);
        self
    }
}

#[derive(Debug, Clone)]
pub struct ExecSyncOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub timed_out: bool,
}

/// `GET /exec/{id}` — the exec's current state, polled until it leaves
/// `running`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecInfo {
    state: String,
    duration_ms: u64,
    exit_code: Option<i32>,
    timed_out: bool,
}

impl ExecInfo {
    fn is_finished(&self) -> bool {
        self.state != "running"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxListResponse {
    pub path: String,
    pub entries: Vec<SandboxDirEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxDirEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: Option<u64>,
    pub mode: String,
    pub mtime: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SandboxLockOptions {
    pub holder: String,
    pub hostname: Option<String>,
    pub message: Option<String>,
    pub kind: String,
    pub ttl: Duration,
    pub heartbeat_interval: Duration,
    pub force_reclaim_stale: bool,
}

impl SandboxLockOptions {
    pub fn local_edit(holder: impl Into<String>) -> Self {
        Self {
            holder: holder.into(),
            hostname: local_hostname(),
            message: Some("This sandbox is open for local editing.".to_string()),
            kind: "local-edit".to_string(),
            ttl: Duration::from_secs(90),
            heartbeat_interval: Duration::from_secs(5),
            force_reclaim_stale: true,
        }
    }
}

fn local_hostname() -> Option<String> {
    if let Ok(value) = std::env::var("HOSTNAME")
        && !value.trim().is_empty()
    {
        return Some(value.trim().to_string());
    }
    if let Ok(value) = std::env::var("COMPUTERNAME")
        && !value.trim().is_empty()
    {
        return Some(value.trim().to_string());
    }
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

struct SandboxLockState {
    client: SandboxClient,
    sandbox_id: String,
    /// The lock contents we wrote; heartbeats re-write it with fresh
    /// timestamps.
    template: SandboxLockFile,
    /// Etag and conservative monotonic expiry of our last successful write.
    lease: Mutex<(String, Instant)>,
    running: AtomicBool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxLockFile {
    kind: String,
    holder: String,
    lease_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    ttl_seconds: i64,
}

pub struct SandboxLockGuard {
    state: Arc<SandboxLockState>,
    heartbeat_thread: Option<JoinHandle<()>>,
}

impl SandboxClient {
    pub fn new(ctx: WorkspaceContext) -> Result<Self> {
        crate::auth::get_api_token_with_context(&ctx)?;
        let api_base_url = ctx.api_base_url().trim_end_matches('/').to_string();
        Ok(Self {
            api_base_url,
            ctx,
            connections: Arc::default(),
            scope: RequestScope::Unleased,
            http: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                // Minted headers are scoped to the returned endpoint. Never
                // forward them to a redirect target.
                .redirect(reqwest::redirect::Policy::none())
                // Every request sets an explicit per-request timeout instead
                // of relying on the blocking client's 30s default.
                .timeout(None)
                .build()
                .context("Failed to create sandbox HTTP client")?,
        })
    }

    /// Run a command job-shaped: create the exec (with its output redirected
    /// to files, since the data plane's event queue backpressures the command
    /// after 1 MiB unless someone streams it), poll until it finishes, then
    /// read the output files.
    pub fn exec_sync(&self, sandbox_id: &str, request: ExecSyncRequest) -> Result<ExecSyncOutput> {
        let output_id = Uuid::new_v4().simple().to_string();
        let deadline = Duration::from_millis(
            request
                .timeout_ms
                .unwrap_or(EXEC_DEFAULT_TIMEOUT.as_millis() as i64) as u64,
        ) + EXEC_POLL_GRACE;
        let request = ExecSyncRequest {
            command: wrapped_command(&request.command, &output_id),
            ..request
        };

        let exec_id = self.create_exec(sandbox_id, &request)?;
        let info = match self.poll_exec(sandbox_id, &exec_id, deadline) {
            Ok(info) => info,
            Err(err) => {
                if let Err(cancel_err) = self.cancel_exec(sandbox_id, &exec_id) {
                    log::debug!("Failed to cancel sandbox exec {exec_id}: {cancel_err:#}");
                }
                return Err(err);
            }
        };

        Ok(ExecSyncOutput {
            stdout: self.fetch_exec_output(sandbox_id, &output_id, "out")?,
            stderr: self.fetch_exec_output(sandbox_id, &output_id, "err")?,
            exit_code: info.exit_code,
            duration_ms: info.duration_ms.min(i64::MAX as u64) as i64,
            timed_out: info.timed_out,
        })
    }

    pub fn exec_sync_success(
        &self,
        sandbox_id: &str,
        request: ExecSyncRequest,
    ) -> Result<ExecSyncOutput> {
        let output = self.exec_sync(sandbox_id, request)?;
        ensure_exec_success(&output)?;
        Ok(output)
    }

    pub fn list(&self, sandbox_id: &str, path: &str) -> Result<SandboxListResponse> {
        let response = self.read_response(sandbox_id, path)?;
        ensure_data_plane_success(response)?
            .json()
            .context("Invalid sandbox directory listing")
    }

    pub fn read_file(&self, sandbox_id: &str, path: &str) -> Result<Vec<u8>> {
        self.read_file_optional(sandbox_id, path)?
            .with_context(|| format!("Sandbox file not found: {path}"))
    }

    /// Read a file, returning `None` if the file (or sandbox) is gone.
    fn read_file_optional(&self, sandbox_id: &str, path: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .read_file_with_etag(sandbox_id, path)?
            .map(|(bytes, _)| bytes))
    }

    fn read_file_with_etag(
        &self,
        sandbox_id: &str,
        path: &str,
    ) -> Result<Option<(Vec<u8>, Option<String>)>> {
        let response = self.read_response(sandbox_id, path)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = ensure_data_plane_success(response)?;
        let etag = header_string(response.headers(), ETAG);
        Ok(Some((response.bytes()?.to_vec(), etag)))
    }

    pub fn write_file(&self, sandbox_id: &str, path: &str, bytes: &[u8]) -> Result<()> {
        require_safe_absolute_path(path)?;
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.put(sandbox_fs_url(base, "/fs/write", path))
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(bytes.to_vec())
                .timeout(FILE_TRANSFER_TIMEOUT)
        })?;
        let _: serde_json::Value = ensure_data_plane_success(response)?
            .json()
            .context("Invalid sandbox write response")?;
        Ok(())
    }

    /// Compare-and-swap write: the data plane checks the precondition
    /// against the file's current etag, making lock acquire/refresh atomic.
    fn write_lock_file(
        &self,
        sandbox_id: &str,
        template: &SandboxLockFile,
        precondition: &WritePrecondition,
    ) -> Result<ConditionalWrite> {
        let mut expires_at = Instant::now();
        let response = self.data_plane_request(sandbox_id, |http, base| {
            // Connection renewal can wait. Start the lease only once we can send it.
            let now = Utc::now();
            expires_at = Instant::now() + Duration::from_secs(template.ttl_seconds as u64);
            let lock = SandboxLockFile {
                updated_at: now,
                expires_at: now + chrono::Duration::seconds(template.ttl_seconds),
                ..template.clone()
            };
            let request = http
                .put(sandbox_fs_url(base, "/fs/write", SANDBOX_LOCK_FILE_PATH))
                .header(CONTENT_TYPE, "application/octet-stream")
                .json(&lock)
                .timeout(LOCK_REQUEST_TIMEOUT);
            match precondition {
                WritePrecondition::CreateOnly => request.header(IF_NONE_MATCH, "*"),
                WritePrecondition::Match(etag) => request.header(IF_MATCH, etag.clone()),
            }
        })?;
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Ok(ConditionalWrite::PreconditionFailed);
        }
        let response = ensure_data_plane_success(response)?;
        let etag = header_string(response.headers(), ETAG)
            .context("Sandbox write response is missing an ETag header")?;
        Ok(ConditionalWrite::Written(etag, expires_at))
    }

    pub fn remove(&self, sandbox_id: &str, path: &str) -> Result<()> {
        require_safe_absolute_path(path)?;
        self.exec_sync_success(
            sandbox_id,
            ExecSyncRequest::command(format!("rm -f -- {}", shell_quote(path)))
                .timeout(Duration::from_secs(30)),
        )?;
        Ok(())
    }

    pub fn acquire_lock(
        &self,
        sandbox_id: &str,
        options: SandboxLockOptions,
    ) -> Result<SandboxLockGuard> {
        let ttl_seconds = duration_secs_i64(options.ttl, "lock ttl")?;
        let heartbeat_seconds =
            duration_secs_i64(options.heartbeat_interval, "lock heartbeat interval")?;
        if heartbeat_seconds >= ttl_seconds {
            bail!("lock heartbeat interval must be shorter than the ttl");
        }

        let now = Utc::now();
        let lock = SandboxLockFile {
            kind: options.kind,
            holder: options.holder,
            lease_id: Uuid::new_v4().to_string(),
            hostname: options.hostname,
            message: options.message,
            started_at: now,
            updated_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_seconds),
            ttl_seconds,
        };
        let lease = acquire_lock_file(self, sandbox_id, &lock, options.force_reclaim_stale)?;

        let state = Arc::new(SandboxLockState {
            client: self.clone(),
            sandbox_id: sandbox_id.to_string(),
            template: lock,
            lease: Mutex::new(lease),
            running: AtomicBool::new(true),
        });
        let thread_state = Arc::clone(&state);
        let heartbeat_thread = thread::spawn(move || {
            heartbeat_loop(thread_state, options.heartbeat_interval);
        });

        Ok(SandboxLockGuard {
            state,
            heartbeat_thread: Some(heartbeat_thread),
        })
    }

    fn read_response(&self, sandbox_id: &str, path: &str) -> Result<reqwest::blocking::Response> {
        require_safe_absolute_path(path)?;
        self.data_plane_request(sandbox_id, |http, base| {
            http.get(sandbox_fs_url(base, "/fs/read", path))
                .timeout(FILE_TRANSFER_TIMEOUT)
        })
    }

    fn create_exec(&self, sandbox_id: &str, request: &ExecSyncRequest) -> Result<String> {
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.post(sandbox_endpoint_url(base, "/exec"))
                .json(request)
                .timeout(DEFAULT_REQUEST_TIMEOUT)
        })?;
        let response = ensure_data_plane_success(response)?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("Sandbox exec response is missing a Location header")?;
        exec_id_from_location(location)
    }

    /// Poll the exec until it finishes. Transient poll failures are retried
    /// (polling is stateless), so a network blip mid-command costs nothing.
    fn poll_exec(&self, sandbox_id: &str, exec_id: &str, deadline: Duration) -> Result<ExecInfo> {
        let started = std::time::Instant::now();
        let mut delay = EXEC_POLL_INITIAL_DELAY;
        let mut consecutive_failures = 0;
        loop {
            match self.exec_info(sandbox_id, exec_id) {
                Ok(info) if info.is_finished() => return Ok(info),
                Ok(_) => consecutive_failures = 0,
                Err(err) => {
                    consecutive_failures += 1;
                    if consecutive_failures >= EXEC_POLL_MAX_CONSECUTIVE_FAILURES {
                        return Err(err);
                    }
                    log::warn!("Failed to poll sandbox exec {exec_id}; retrying: {err:#}");
                }
            }
            if started.elapsed() > deadline {
                bail!("Sandbox command did not finish within {deadline:?}");
            }
            self.wait(delay)?;
            delay = (delay * 2).min(EXEC_POLL_MAX_DELAY);
        }
    }

    fn exec_info(&self, sandbox_id: &str, exec_id: &str) -> Result<ExecInfo> {
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.get(sandbox_endpoint_url(
                base,
                &format!("/exec/{}", encode_segment(exec_id)),
            ))
            .timeout(DEFAULT_REQUEST_TIMEOUT)
        })?;
        ensure_data_plane_success(response)?
            .json()
            .context("Invalid sandbox exec info")
    }

    /// Read one of the output files a wrapped command left behind, refusing
    /// to download runaway output.
    fn fetch_exec_output(&self, sandbox_id: &str, output_id: &str, kind: &str) -> Result<String> {
        let path = exec_output_path(output_id, kind);
        let Some(stat) = self.stat(sandbox_id, &path)? else {
            return Ok(String::new());
        };
        if stat.size == 0 {
            return Ok(String::new());
        }
        if stat.size > EXEC_MAX_OUTPUT_BYTES {
            return Ok(format!(
                "(sandbox command produced {} bytes of {kind} output; not downloaded)",
                stat.size
            ));
        }
        let bytes = self.read_file(sandbox_id, &path)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn stat(&self, sandbox_id: &str, path: &str) -> Result<Option<SandboxStat>> {
        require_safe_absolute_path(path)?;
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.get(sandbox_fs_url(base, "/fs/stat", path))
                .timeout(DEFAULT_REQUEST_TIMEOUT)
        })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure_data_plane_success(response)?
            .json()
            .map(Some)
            .context("Invalid sandbox stat response")
    }

    fn cancel_exec(&self, sandbox_id: &str, exec_id: &str) -> Result<()> {
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.delete(sandbox_endpoint_url(
                base,
                &format!("/exec/{}", encode_segment(exec_id)),
            ))
            .timeout(DEFAULT_REQUEST_TIMEOUT)
        })?;
        ensure_data_plane_success(response)?;
        Ok(())
    }

    fn data_plane_request<F>(
        &self,
        sandbox_id: &str,
        build: F,
    ) -> Result<reqwest::blocking::Response>
    where
        F: FnOnce(&Client, &str) -> reqwest::blocking::RequestBuilder,
    {
        let connection = self.connection(sandbox_id)?;
        let headers = sandbox_headers(&connection.http.headers)?;
        let mut request = build(&self.http, &connection.http.endpoint).headers(headers);
        if let RequestScope::Cleanup(deadline) = self.scope {
            request = request.timeout(deadline.saturating_duration_since(Instant::now()));
        }
        // Recheck after renewal, immediately before submitting work. Keep the
        // transfer's own timeout: heartbeats can extend the lease while it runs.
        self.request_timeout(DEFAULT_REQUEST_TIMEOUT)?;
        let response = request.send();
        if response.as_ref().map_or(true, |response| {
            matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) || response.status().is_server_error()
        }) {
            let mut connections = self.connections.lock().unwrap();
            // A late failure must not discard a newer connection.
            if connections
                .get(sandbox_id)
                .is_some_and(|current| Arc::ptr_eq(current, &connection))
            {
                connections.remove(sandbox_id);
            }
        }
        // Never replay a submitted request: commands and writes may have taken effect.
        response.context("Sandbox request failed")
    }

    fn connection(&self, sandbox_id: &str) -> Result<Arc<SandboxConnection>> {
        // Pre-lease waits may last twenty minutes; a sync lease or cleanup
        // deadline always bounds this further through request_timeout.
        let deadline = Instant::now() + Duration::from_secs(20 * 60);
        // Only renewal holds this lock, never file transfers. Clones (including
        // the lock heartbeat) reuse the same endpoint + credentials together.
        let mut connections = loop {
            self.request_timeout(deadline.saturating_duration_since(Instant::now()))?;
            match self.connections.try_lock() {
                Ok(connections) => break connections,
                Err(TryLockError::WouldBlock) => self.wait(Duration::from_millis(50))?,
                Err(TryLockError::Poisoned(_)) => bail!("Sandbox connection cache poisoned"),
            }
        };
        if let Some(connection) = connections.get(sandbox_id)
            && connection
                .expires_at
                .is_none_or(|expiry| expiry > Utc::now().timestamp() + 60)
        {
            return Ok(connection.clone());
        }
        let url = self.url(&format!(
            "/api/sandboxes/{}/access-token",
            encode_segment(sandbox_id)
        ));
        let mut delay = Duration::ZERO;
        loop {
            self.wait(delay.min(deadline.saturating_duration_since(Instant::now())))?;
            delay = (delay * 2).clamp(Duration::from_millis(200), Duration::from_secs(4));
            let remaining =
                self.request_timeout(deadline.saturating_duration_since(Instant::now()))?;
            // Blocking HTTP cannot be cancelled mid-request. Keep lease renewal
            // attempts short so shutdown can join the heartbeat promptly.
            let timeout = if matches!(self.scope, RequestScope::Editing(_)) {
                LOCK_REQUEST_TIMEOUT
            } else {
                DEFAULT_REQUEST_TIMEOUT
            };
            let response = match self
                .authenticated(self.http.post(&url))?
                .timeout(timeout.min(remaining))
                .send()
            {
                Ok(response) => response,
                Err(err) if err.is_timeout() || err.is_connect() || err.is_request() => {
                    log::warn!("Failed to mint sandbox connection; retrying: {err}");
                    continue;
                }
                Err(err) => return Err(err).context("Failed to mint sandbox connection"),
            };
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED {
                bail!("Sandbox API request was not authorized. Run `pcb auth login`.");
            }
            if status == StatusCode::NOT_FOUND {
                bail!("Sandbox {sandbox_id} was not found or you do not have access to it");
            }
            if status.is_success() {
                let connection: SandboxConnection = response
                    .json()
                    .context("Invalid sandbox connection response")?;
                validate_sandbox_endpoint(&self.api_base_url, &connection.http.endpoint)?;
                sandbox_headers(&connection.http.headers)?;
                let connection = Arc::new(connection);
                connections.insert(sandbox_id.to_owned(), connection.clone());
                return Ok(connection);
            }
            let text = response
                .text()
                .context("Failed to read sandbox connection error")?;
            let retry = status == StatusCode::SERVICE_UNAVAILABLE
                && serde_json::from_str::<serde_json::Value>(&text).is_ok_and(|error| {
                    matches!(
                        error["code"].as_str(),
                        Some("SANDBOX_BUSY" | "SANDBOX_UPDATING")
                    )
                });
            if !retry {
                bail!("Failed to mint sandbox connection ({status}): {text}");
            }
        }
    }

    fn request_timeout(&self, timeout: Duration) -> Result<Duration> {
        let deadline = match &self.scope {
            RequestScope::Unleased => Instant::now() + timeout,
            RequestScope::Editing(state) => {
                if !state.running.load(Ordering::SeqCst) {
                    bail!("Sandbox sync stopped");
                }
                state.lease.lock().unwrap().1
            }
            RequestScope::Cleanup(deadline) => *deadline,
        };
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .min(timeout);
        if remaining.is_zero() {
            bail!("Sandbox request deadline or editor lease expired");
        }
        Ok(remaining)
    }

    fn wait(&self, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            thread::sleep(
                self.request_timeout(remaining)?
                    .min(Duration::from_millis(50)),
            );
        }
        Ok(())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base_url, path)
    }

    fn authenticated(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::RequestBuilder> {
        crate::auth::apply_api_auth_with_context(&self.ctx, request)
    }
}

/// `GET /fs/stat` — only the size is needed (to guard output downloads).
#[derive(Debug, Clone, Deserialize)]
struct SandboxStat {
    size: u64,
}

enum WritePrecondition {
    /// `If-None-Match: *` — write only if the file does not exist yet.
    CreateOnly,
    /// `If-Match: <etag>` — write only if the file is unchanged.
    Match(String),
}

enum ConditionalWrite {
    /// The write landed; holds its new etag and conservative lease expiry.
    Written(String, Instant),
    /// 412 — another writer changed or created the file first.
    PreconditionFailed,
}

fn header_string(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Wrap a command so its output lands in files instead of the exec event
/// queue (which backpressures the command after 1 MiB unless streamed).
/// Output files from earlier runs are pruned opportunistically.
fn wrapped_command(command: &str, output_id: &str) -> String {
    format!(
        "mkdir -p {dir} && find {dir} -type f -mmin +240 -delete 2>/dev/null; exec >{out} 2>{err}; {command}",
        dir = EXEC_OUTPUT_DIR,
        out = exec_output_path(output_id, "out"),
        err = exec_output_path(output_id, "err"),
    )
}

fn exec_output_path(output_id: &str, kind: &str) -> String {
    format!("{EXEC_OUTPUT_DIR}/{output_id}.{kind}")
}

fn sandbox_endpoint_url(connection_endpoint: &str, endpoint: &str) -> String {
    let query_start = connection_endpoint
        .find('?')
        .unwrap_or(connection_endpoint.len());
    let (base, query) = connection_endpoint.split_at(query_start);
    format!("{}{}{}", base.trim_end_matches('/'), endpoint, query)
}

fn sandbox_fs_url(connection_endpoint: &str, endpoint: &str, path: &str) -> String {
    let url = sandbox_endpoint_url(connection_endpoint, endpoint);
    let separator = if !url.contains('?') {
        "?"
    } else if url.ends_with(['?', '&']) {
        ""
    } else {
        "&"
    };
    format!("{url}{separator}path={}", encode_segment(path))
}

fn ensure_data_plane_success(
    response: reqwest::blocking::Response,
) -> Result<reqwest::blocking::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().unwrap_or_default();
    bail!("Sandbox request failed with {status}: {text}");
}

fn exec_id_from_location(location: &str) -> Result<String> {
    let path = match location.split_once("://") {
        Some((_, rest)) => rest.find('/').map(|index| &rest[index..]).unwrap_or(""),
        None => location,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .context("Sandbox exec Location header did not include an exec id")
}

impl SandboxLockGuard {
    /// Stop submitting new work when the editor lease expires or sync stops.
    /// Requests already in flight retain their own timeouts.
    pub fn client(&self) -> SandboxClient {
        SandboxClient {
            scope: RequestScope::Editing(Arc::clone(&self.state)),
            ..self.state.client.clone()
        }
    }

    /// A one-way stop callback suitable for a Ctrl-C handler.
    pub fn stop_handler(&self) -> impl Fn() + Send + 'static {
        let state = Arc::clone(&self.state);
        move || state.running.store(false, Ordering::SeqCst)
    }

    pub fn is_stopped(&self) -> bool {
        !self.state.running.load(Ordering::SeqCst)
    }

    pub fn is_active(&self) -> bool {
        !self.is_stopped() && Instant::now() < self.state.lease.lock().unwrap().1
    }

    pub fn release(mut self) -> Result<()> {
        self.release_inner()
    }

    fn release_inner(&mut self) -> Result<()> {
        let Some(thread) = self.heartbeat_thread.take() else {
            return Ok(());
        };
        self.state.running.store(false, Ordering::SeqCst);
        thread.thread().unpark();
        let _ = thread.join();
        release_once(&self.state)
    }
}

impl Drop for SandboxLockGuard {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

fn heartbeat_loop(state: Arc<SandboxLockState>, interval: Duration) {
    let client = SandboxClient {
        scope: RequestScope::Editing(Arc::clone(&state)),
        ..state.client.clone()
    };
    while let Ok(remaining) = client.request_timeout(interval) {
        thread::park_timeout(remaining);
        let etag = state.lease.lock().unwrap().0.clone();
        match client.write_lock_file(
            &state.sandbox_id,
            &state.template,
            &WritePrecondition::Match(etag),
        ) {
            Ok(ConditionalWrite::Written(etag, expires_at)) => {
                *state.lease.lock().unwrap() = (etag, expires_at);
            }
            Ok(ConditionalWrite::PreconditionFailed) => {
                // Someone else changed the lock. Stop without deleting it.
                state.lease.lock().unwrap().1 = Instant::now();
                break;
            }
            Err(err) if state.running.load(Ordering::SeqCst) => {
                log::warn!("Sandbox lock heartbeat failed: {err:#}");
            }
            Err(_) => break,
        }
    }
}

fn release_once(state: &SandboxLockState) -> Result<()> {
    let expires_at = state.lease.lock().unwrap().1;
    if Instant::now() >= expires_at {
        return Ok(());
    }
    // Shutdown must not start another twenty-minute maintenance wait. If cleanup
    // cannot finish, the remote lease expires naturally.
    let client = SandboxClient {
        scope: RequestScope::Cleanup(expires_at.min(Instant::now() + LOCK_REQUEST_TIMEOUT)),
        ..state.client.clone()
    };
    if let Some((current, _)) =
        read_lock_file(&client, &state.sandbox_id).context("Failed to release sandbox lock")?
        && current.lease_id != state.template.lease_id
    {
        return Ok(());
    }
    delete_lock_file(&client, &state.sandbox_id).context("Failed to release sandbox lock")
}

/// Take the lock atomically: create-only when no lock exists, or a
/// compare-and-swap overwrite of a stale one. Returns the etag of our write.
/// The data plane's fs/write creates parent directories itself, so the lock
/// directory needs no separate mkdir on fresh sandboxes.
fn acquire_lock_file(
    client: &SandboxClient,
    sandbox_id: &str,
    lock: &SandboxLockFile,
    force_reclaim_stale: bool,
) -> Result<(String, Instant)> {
    let precondition = match read_lock_file(client, sandbox_id)? {
        None => WritePrecondition::CreateOnly,
        Some((existing, etag)) => {
            if !force_reclaim_stale || !existing.is_stale() {
                bail!(
                    "Sandbox is already locked: existing lock is active ({})",
                    existing.holder
                );
            }
            WritePrecondition::Match(etag)
        }
    };
    match client.write_lock_file(sandbox_id, lock, &precondition)? {
        ConditionalWrite::Written(etag, expires_at) => Ok((etag, expires_at)),
        ConditionalWrite::PreconditionFailed => {
            bail!("Sandbox is already locked: another client just acquired it")
        }
    }
}

impl SandboxLockFile {
    fn is_stale(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

fn read_lock_file(
    client: &SandboxClient,
    sandbox_id: &str,
) -> Result<Option<(SandboxLockFile, String)>> {
    let Some((bytes, etag)) = client.read_file_with_etag(sandbox_id, SANDBOX_LOCK_FILE_PATH)?
    else {
        return Ok(None);
    };
    let etag = etag.context("Sandbox lock read response is missing an ETag header")?;
    let lock = serde_json::from_slice(&bytes).context("Failed to parse sandbox lock file")?;
    Ok(Some((lock, etag)))
}

fn delete_lock_file(client: &SandboxClient, sandbox_id: &str) -> Result<()> {
    // The data plane has no fs delete endpoint; `rm -f` also succeeds when
    // the lock file is already gone.
    client.remove(sandbox_id, SANDBOX_LOCK_FILE_PATH)
}

fn ensure_exec_success(output: &ExecSyncOutput) -> Result<()> {
    if output.timed_out {
        bail!("Sandbox command timed out");
    }
    if output.exit_code != Some(0) {
        bail!(
            "Sandbox command failed with exit code {:?}: {}",
            output.exit_code,
            output.stderr.trim()
        );
    }
    Ok(())
}

fn duration_secs_i64(duration: Duration, label: &str) -> Result<i64> {
    let seconds = duration.as_secs();
    if seconds == 0 {
        bail!("{label} must be at least one second");
    }
    i64::try_from(seconds).map_err(|_| anyhow!("{label} is too large"))
}

fn require_safe_absolute_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        bail!("sandbox path must be absolute: {path}");
    }
    for segment in path.split('/').skip(1) {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\') {
            bail!("unsafe sandbox path: {path}");
        }
    }
    Ok(())
}

fn encode_segment(segment: &str) -> String {
    urlencoding::encode(segment).into_owned()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_exec_id_from_location() {
        assert_eq!(exec_id_from_location("/exec/abc123").unwrap(), "abc123");
        assert_eq!(exec_id_from_location("/exec/abc123/").unwrap(), "abc123");
        assert_eq!(
            exec_id_from_location(
                "https://sandbox.api.diode.computer/sandboxes/sbx_1/exec/e-9?x=1"
            )
            .unwrap(),
            "e-9"
        );
        assert!(exec_id_from_location("").is_err());
    }

    #[test]
    fn deserializes_exec_info() {
        let info: ExecInfo = serde_json::from_str(
            "{\"id\":\"e-1\",\"state\":\"exited\",\"durationMs\":1200,\"exitCode\":null,\"timedOut\":true,\"canceled\":false}",
        )
        .unwrap();
        assert!(info.is_finished());
        assert_eq!(info.exit_code, None);
        assert_eq!(info.duration_ms, 1200);
        assert!(info.timed_out);

        let running: ExecInfo = serde_json::from_str(
            "{\"id\":\"e-1\",\"state\":\"running\",\"durationMs\":5,\"exitCode\":null,\"timedOut\":false,\"canceled\":false}",
        )
        .unwrap();
        assert!(!running.is_finished());
    }

    #[test]
    fn deserializes_directory_listing() {
        let listing: SandboxListResponse = serde_json::from_str(
            "{\"path\":\"/home/sandbox\",\"type\":\"directory\",\"entries\":[{\"name\":\"main.zen\",\"path\":\"/home/sandbox/main.zen\",\"type\":\"file\",\"size\":42,\"mode\":\"0644\",\"mtime\":\"2026-07-04T00:00:00Z\",\"etag\":\"\\\"abc\\\"\"},{\"name\":\"layout\",\"path\":\"/home/sandbox/layout\",\"type\":\"directory\",\"size\":0,\"mode\":\"0755\"}]}",
        )
        .unwrap();
        assert_eq!(listing.path, "/home/sandbox");
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.entries[0].kind, "file");
        assert_eq!(listing.entries[0].size, Some(42));
        assert_eq!(listing.entries[1].kind, "directory");
        assert_eq!(listing.entries[1].mtime, None);
    }

    #[test]
    fn wraps_commands_with_output_redirection() {
        let wrapped = wrapped_command("pcb layout 'main.zen'", "abc123");
        assert!(wrapped.ends_with("; pcb layout 'main.zen'"));
        assert!(wrapped.contains("exec >/tmp/.pcb-exec/abc123.out 2>/tmp/.pcb-exec/abc123.err"));
    }

    #[test]
    fn builds_sandbox_endpoint_urls() {
        assert_eq!(
            sandbox_endpoint_url(
                "https://sandbox.api.diode.computer/connections/sbx-1/?route=foo%2Fbar&sig=a%2Bb",
                "/fs/read"
            ),
            "https://sandbox.api.diode.computer/connections/sbx-1/fs/read?route=foo%2Fbar&sig=a%2Bb"
        );
        assert_eq!(
            sandbox_fs_url(
                "http://localhost:8080/routed/sandbox?route=foo%2Fbar&sig=a%2Bb",
                "/fs/read",
                "/home/sandbox/My Board/main.zen"
            ),
            "http://localhost:8080/routed/sandbox/fs/read?route=foo%2Fbar&sig=a%2Bb&path=%2Fhome%2Fsandbox%2FMy%20Board%2Fmain.zen"
        );
    }

    #[test]
    fn marks_sandbox_headers_sensitive() {
        let headers = sandbox_headers(&BTreeMap::from([(
            "x-provider-credential".to_string(),
            "secret".to_string(),
        )]))
        .unwrap();
        assert_eq!(headers["x-provider-credential"], "secret");
        assert!(headers["x-provider-credential"].is_sensitive());
        assert!(
            sandbox_headers(&BTreeMap::from([(
                "bad header".to_string(),
                "secret".to_string()
            )]))
            .is_err()
        );
    }

    #[test]
    fn validates_sandbox_endpoint_security() {
        assert!(
            validate_sandbox_endpoint(
                "https://api.diode.computer",
                "https://provider.example/sandbox"
            )
            .is_ok()
        );
        assert!(
            validate_sandbox_endpoint("http://localhost:3001", "http://localhost:9000/sandbox")
                .is_ok()
        );
        assert!(
            validate_sandbox_endpoint(
                "https://api.diode.computer",
                "http://provider.example/sandbox"
            )
            .is_err()
        );
        assert!(
            validate_sandbox_endpoint("https://api.diode.computer", "file:///tmp/not-a-sandbox")
                .is_err()
        );
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(require_safe_absolute_path("relative/path").is_err());
        assert!(require_safe_absolute_path("/home/sandbox/../main.zen").is_err());
        assert!(require_safe_absolute_path("/home//sandbox/main.zen").is_err());
        assert!(require_safe_absolute_path("/home/sandbox/My Board/main.zen").is_ok());
    }

    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\"'\"'s'");
    }
}
