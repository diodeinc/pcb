use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, LOCATION};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::WorkspaceContext;

pub const SANDBOX_LOCK_FILE_PATH: &str = "/home/sandbox/.diode/sandbox-lock.json";
const LOCK_HEARTBEAT_MAX_FAILURES: usize = 3;

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
    access: Arc<Mutex<BTreeMap<String, SandboxAccess>>>,
}

/// Short-lived, sandbox-scoped credentials minted by the API. All sandbox
/// file/exec traffic goes directly to `data_plane_url` with `token`; the API
/// bearer token is only used for minting. Expiry is handled reactively: a
/// 401/403 from the data plane invalidates the cache and re-mints, so there
/// is no clock-based refresh logic to get wrong.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAccess {
    token: String,
    data_plane_url: String,
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
    /// Etag of our last lock write — the compare-and-swap token for the next
    /// heartbeat. A 412 means someone else took the lock.
    etag: Mutex<String>,
    stop: AtomicBool,
    active: AtomicBool,
    released: AtomicBool,
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
    stop_tx: Option<Sender<()>>,
}

impl SandboxClient {
    pub fn new(ctx: WorkspaceContext) -> Result<Self> {
        crate::auth::get_api_token_with_context(&ctx)?;
        let api_base_url = ctx.api_base_url().trim_end_matches('/').to_string();
        Ok(Self {
            api_base_url,
            ctx,
            http: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                // Every request sets an explicit per-request timeout instead
                // of relying on the blocking client's 30s default.
                .timeout(None)
                .build()
                .context("Failed to create sandbox HTTP client")?,
            access: Arc::new(Mutex::new(BTreeMap::new())),
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
            http.put(sandbox_fs_url(base, sandbox_id, "/fs/write", path))
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
    fn write_file_if(
        &self,
        sandbox_id: &str,
        path: &str,
        bytes: &[u8],
        precondition: &WritePrecondition,
    ) -> Result<ConditionalWrite> {
        require_safe_absolute_path(path)?;
        let response = self.data_plane_request(sandbox_id, |http, base| {
            let request = http
                .put(sandbox_fs_url(base, sandbox_id, "/fs/write", path))
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(bytes.to_vec())
                .timeout(FILE_TRANSFER_TIMEOUT);
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
        Ok(ConditionalWrite::Written(etag))
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
        let etag = acquire_lock_file(self, sandbox_id, &lock, options.force_reclaim_stale)?;

        let state = Arc::new(SandboxLockState {
            client: self.clone(),
            sandbox_id: sandbox_id.to_string(),
            template: lock,
            etag: Mutex::new(etag),
            stop: AtomicBool::new(false),
            active: AtomicBool::new(true),
            released: AtomicBool::new(false),
        });
        let thread_state = Arc::clone(&state);
        let (stop_tx, stop_rx) = mpsc::channel();
        let heartbeat_thread = thread::spawn(move || {
            heartbeat_loop(thread_state, options.heartbeat_interval, stop_rx);
        });

        Ok(SandboxLockGuard {
            state,
            heartbeat_thread: Some(heartbeat_thread),
            stop_tx: Some(stop_tx),
        })
    }

    fn read_response(&self, sandbox_id: &str, path: &str) -> Result<reqwest::blocking::Response> {
        require_safe_absolute_path(path)?;
        self.data_plane_request(sandbox_id, |http, base| {
            http.get(sandbox_fs_url(base, sandbox_id, "/fs/read", path))
                .timeout(FILE_TRANSFER_TIMEOUT)
        })
    }

    fn create_exec(&self, sandbox_id: &str, request: &ExecSyncRequest) -> Result<String> {
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.post(sandbox_endpoint_url(base, sandbox_id, "/exec"))
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
            thread::sleep(delay);
            delay = (delay * 2).min(EXEC_POLL_MAX_DELAY);
        }
    }

    fn exec_info(&self, sandbox_id: &str, exec_id: &str) -> Result<ExecInfo> {
        let response = self.data_plane_request(sandbox_id, |http, base| {
            http.get(sandbox_endpoint_url(
                base,
                sandbox_id,
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
            http.get(sandbox_fs_url(base, sandbox_id, "/fs/stat", path))
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
                sandbox_id,
                &format!("/exec/{}", encode_segment(exec_id)),
            ))
            .timeout(DEFAULT_REQUEST_TIMEOUT)
        })?;
        ensure_data_plane_success(response)?;
        Ok(())
    }

    /// Send a data-plane request with the sandbox-scoped token, re-minting
    /// once if the token was rejected (expired mid-session). The orchestrator
    /// authorizes before forwarding anything to the sandbox, so a 401/403
    /// means the request was never executed and the retry cannot double-apply
    /// a write.
    fn data_plane_request<F>(
        &self,
        sandbox_id: &str,
        build: F,
    ) -> Result<reqwest::blocking::Response>
    where
        F: Fn(&Client, &str) -> reqwest::blocking::RequestBuilder,
    {
        let response = self.send_data_plane(sandbox_id, &build)?;
        let status = response.status();
        if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
            return Ok(response);
        }
        log::debug!("Sandbox data-plane request was rejected ({status}); re-minting access token");
        self.invalidate_access(sandbox_id);
        self.send_data_plane(sandbox_id, &build)
    }

    fn send_data_plane<F>(&self, sandbox_id: &str, build: &F) -> Result<reqwest::blocking::Response>
    where
        F: Fn(&Client, &str) -> reqwest::blocking::RequestBuilder,
    {
        let access = self.access(sandbox_id)?;
        build(&self.http, &access.data_plane_url)
            .bearer_auth(&access.token)
            .send()
            .context("Sandbox request failed")
    }

    fn access(&self, sandbox_id: &str) -> Result<SandboxAccess> {
        // The mutex is held across the mint so concurrent callers (sync
        // workers, the lock heartbeat) share one mint instead of stampeding
        // the API; they all need the same token anyway.
        let mut cache = self
            .access
            .lock()
            .map_err(|_| anyhow!("sandbox access cache lock poisoned"))?;
        if let Some(access) = cache.get(sandbox_id) {
            return Ok(access.clone());
        }
        let access = self.mint_access(sandbox_id)?;
        cache.insert(sandbox_id.to_string(), access.clone());
        Ok(access)
    }

    fn invalidate_access(&self, sandbox_id: &str) {
        if let Ok(mut cache) = self.access.lock() {
            cache.remove(sandbox_id);
        }
    }

    fn mint_access(&self, sandbox_id: &str) -> Result<SandboxAccess> {
        let url = self.url(&format!(
            "/api/sandboxes/{}/access-token",
            encode_segment(sandbox_id)
        ));
        let response = self
            .authenticated(self.http.post(url))?
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .send()
            .context("Failed to mint sandbox access token")?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED {
            bail!("Sandbox API request was not authorized. Run `pcb auth login`.");
        }
        if status == StatusCode::NOT_FOUND {
            bail!("Sandbox {sandbox_id} was not found or you do not have access to it");
        }
        if !status.is_success() {
            let text = response.text().unwrap_or_default();
            bail!("Failed to mint sandbox access token ({status}): {text}");
        }
        response
            .json()
            .context("Invalid sandbox access token response")
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
    /// The write landed; holds the file's new etag.
    Written(String),
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

fn sandbox_endpoint_url(data_plane_url: &str, sandbox_id: &str, endpoint: &str) -> String {
    format!(
        "{}/sandboxes/{}{}",
        data_plane_url.trim_end_matches('/'),
        encode_segment(sandbox_id),
        endpoint
    )
}

fn sandbox_fs_url(data_plane_url: &str, sandbox_id: &str, endpoint: &str, path: &str) -> String {
    format!(
        "{}?path={}",
        sandbox_endpoint_url(data_plane_url, sandbox_id, endpoint),
        encode_segment(path)
    )
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
    pub fn is_active(&self) -> bool {
        self.state.active.load(Ordering::SeqCst) && !self.state.stop.load(Ordering::SeqCst)
    }

    pub fn release(mut self) -> Result<()> {
        self.release_inner()
    }

    fn release_inner(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(thread) = self.heartbeat_thread.take() {
            let _ = thread.join();
        }

        if self.state.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let result = release_once(&self.state);
        self.state.active.store(false, Ordering::SeqCst);
        result
    }
}

impl Drop for SandboxLockGuard {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

enum LockHeartbeat {
    Active,
    Lost,
}

fn heartbeat_loop(state: Arc<SandboxLockState>, interval: Duration, stop_rx: Receiver<()>) {
    let mut failures = 0;
    while !state.stop.load(Ordering::SeqCst) {
        match stop_rx.recv_timeout(interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if state.stop.load(Ordering::SeqCst) {
            break;
        }
        match heartbeat_once(&state) {
            Ok(LockHeartbeat::Active) => failures = 0,
            Ok(LockHeartbeat::Lost) => {
                state.active.store(false, Ordering::SeqCst);
                break;
            }
            Err(err) => {
                failures += 1;
                if failures >= LOCK_HEARTBEAT_MAX_FAILURES {
                    log::warn!(
                        "Sandbox lock heartbeat failed {LOCK_HEARTBEAT_MAX_FAILURES} times; marking lock inactive: {err:#}"
                    );
                    state.active.store(false, Ordering::SeqCst);
                    break;
                }
            }
        }
    }
}

/// Refresh the lock with a compare-and-swap write against the etag of our
/// previous write. A 412 means another client took the lock.
fn heartbeat_once(state: &SandboxLockState) -> Result<LockHeartbeat> {
    let now = Utc::now();
    let lock = SandboxLockFile {
        updated_at: now,
        expires_at: now + chrono::Duration::seconds(state.template.ttl_seconds),
        ..state.template.clone()
    };
    let etag = state
        .etag
        .lock()
        .map_err(|_| anyhow!("sandbox lock etag poisoned"))?
        .clone();
    let outcome = state
        .client
        .write_file_if(
            &state.sandbox_id,
            SANDBOX_LOCK_FILE_PATH,
            &encode_lock_file(&lock)?,
            &WritePrecondition::Match(etag),
        )
        .context("Failed to refresh sandbox lock")?;
    match outcome {
        ConditionalWrite::Written(new_etag) => {
            *state
                .etag
                .lock()
                .map_err(|_| anyhow!("sandbox lock etag poisoned"))? = new_etag;
            Ok(LockHeartbeat::Active)
        }
        ConditionalWrite::PreconditionFailed => Ok(LockHeartbeat::Lost),
    }
}

fn release_once(state: &SandboxLockState) -> Result<()> {
    if let Some((current, _)) = read_lock_file(&state.client, &state.sandbox_id)
        .context("Failed to release sandbox lock")?
        && current.lease_id != state.template.lease_id
    {
        return Ok(());
    }
    delete_lock_file(&state.client, &state.sandbox_id).context("Failed to release sandbox lock")
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
) -> Result<String> {
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
    match client.write_file_if(
        sandbox_id,
        SANDBOX_LOCK_FILE_PATH,
        &encode_lock_file(lock)?,
        &precondition,
    )? {
        ConditionalWrite::Written(etag) => Ok(etag),
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

fn encode_lock_file(lock: &SandboxLockFile) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(lock).context("Failed to encode sandbox lock file")
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
            sandbox_endpoint_url("https://sandbox.api.diode.computer/", "sbx 1", "/fs/read"),
            "https://sandbox.api.diode.computer/sandboxes/sbx%201/fs/read"
        );
        assert_eq!(
            sandbox_fs_url(
                "http://localhost:8080",
                "sbx_1",
                "/fs/read",
                "/home/sandbox/My Board/main.zen"
            ),
            "http://localhost:8080/sandboxes/sbx_1/fs/read?path=%2Fhome%2Fsandbox%2FMy%20Board%2Fmain.zen"
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
