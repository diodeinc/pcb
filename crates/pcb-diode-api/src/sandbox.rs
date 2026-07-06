use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::WorkspaceContext;

pub const SANDBOX_LOCK_FILE_PATH: &str = "/home/sandbox/.diode/sandbox-lock.json";
const LOCK_HEARTBEAT_MAX_FAILURES: usize = 3;

/// Re-mint the sandbox access token when it is within this margin of expiry.
/// Tokens live ~20 minutes but a `pcb open` session (KiCad + lock heartbeats)
/// can outlive any fixed lifetime.
const ACCESS_TOKEN_REFRESH_MARGIN_SECS: i64 = 120;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const FILE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EXEC_STREAM_RECONNECT_ATTEMPTS: usize = 5;
const EXEC_STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// A dropped events connection can leave its reader lease held server-side
/// until the data plane notices the dead socket via its keep-alive writes
/// (~10-20s), during which reconnects get 409. Wait out a good multiple of
/// that before giving up.
const EXEC_STREAM_BUSY_ATTEMPTS: usize = 15;
/// Cap accumulated exec output, mirroring the truncation the old exec_sync
/// API applied server-side. Anything pcb parses (layout result JSON) is far
/// below this; a runaway command must not balloon a sync session's memory.
const EXEC_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// The data plane sends SSE keep-alives every 10s; a much longer read stall
/// means the connection is dead and we should reconnect.
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct SandboxClient {
    api_base_url: String,
    ctx: WorkspaceContext,
    http: Client,
    access: Arc<Mutex<BTreeMap<String, SandboxAccess>>>,
}

/// Short-lived, sandbox-scoped credentials minted by the API. All sandbox
/// file/exec traffic goes directly to `data_plane_url` with `token`; the API
/// bearer token is only used for minting.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAccess {
    token: String,
    expires_at: i64,
    data_plane_url: String,
}

impl SandboxAccess {
    fn expires_soon(&self) -> bool {
        Utc::now().timestamp() + ACCESS_TOKEN_REFRESH_MARGIN_SECS >= self.expires_at
    }

    fn is_expired(&self) -> bool {
        Utc::now().timestamp() >= self.expires_at
    }
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExecStatus {
    exit_code: Option<i32>,
    duration_ms: u64,
    timed_out: bool,
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
    lease_id: String,
    ttl_seconds: i64,
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
                // Long-running exec event streams must not be cut off by the
                // blocking client's 30s default whole-request timeout; every
                // request sets an explicit per-request timeout instead.
                .timeout(None)
                .build()
                .context("Failed to create sandbox HTTP client")?,
            access: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn exec_sync(&self, sandbox_id: &str, request: ExecSyncRequest) -> Result<ExecSyncOutput> {
        let exec_id = self.create_exec(sandbox_id, &request)?;
        let result = self.collect_exec_output(sandbox_id, &exec_id);
        if result.is_err()
            && let Err(err) = self.cancel_exec(sandbox_id, &exec_id)
        {
            log::debug!("Failed to cancel sandbox exec {exec_id}: {err:#}");
        }
        result
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
        let response = self.read_response(sandbox_id, path)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = ensure_data_plane_success(response)?;
        Ok(Some(response.bytes()?.to_vec()))
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

    pub fn mkdir_p(&self, sandbox_id: &str, path: &str) -> Result<()> {
        require_safe_absolute_path(path)?;
        self.exec_sync_success(
            sandbox_id,
            ExecSyncRequest::command(format!("mkdir -p -- {}", shell_quote(path)))
                .timeout(Duration::from_secs(30)),
        )?;
        Ok(())
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

        let lease_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let lock = SandboxLockFile {
            kind: options.kind,
            holder: options.holder,
            lease_id: lease_id.clone(),
            hostname: options.hostname,
            message: options.message,
            started_at: now,
            updated_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_seconds),
            ttl_seconds,
        };
        acquire_lock_file(self, sandbox_id, &lock, options.force_reclaim_stale)?;

        let state = Arc::new(SandboxLockState {
            client: self.clone(),
            sandbox_id: sandbox_id.to_string(),
            lease_id,
            ttl_seconds,
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

    fn collect_exec_output(&self, sandbox_id: &str, exec_id: &str) -> Result<ExecSyncOutput> {
        let mut state = ExecStreamState::default();
        let mut drops_without_progress = 0;
        let mut busy_attempts = 0;
        loop {
            let last_seen = state.last_event_id;
            match self.stream_exec_events(sandbox_id, exec_id, &mut state)? {
                StreamOutcome::Done(status) => {
                    return Ok(ExecSyncOutput {
                        stdout: String::from_utf8_lossy(&state.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&state.stderr).into_owned(),
                        exit_code: status.exit_code,
                        duration_ms: status.duration_ms.min(i64::MAX as u64) as i64,
                        timed_out: status.timed_out,
                    });
                }
                StreamOutcome::Dropped => {
                    busy_attempts = 0;
                    if state.last_event_id != last_seen {
                        drops_without_progress = 0;
                    }
                    drops_without_progress += 1;
                    if drops_without_progress >= EXEC_STREAM_RECONNECT_ATTEMPTS {
                        bail!(
                            "Sandbox exec event stream disconnected before the command completed"
                        );
                    }
                    thread::sleep(EXEC_STREAM_RECONNECT_DELAY);
                }
                // The previous connection's reader lease has not been reaped
                // yet; this clears on its own, so wait it out on a separate,
                // more patient budget than stream drops.
                StreamOutcome::Busy => {
                    busy_attempts += 1;
                    if busy_attempts >= EXEC_STREAM_BUSY_ATTEMPTS {
                        bail!("Sandbox exec event stream stayed locked by a previous reader");
                    }
                    thread::sleep(EXEC_STREAM_RECONNECT_DELAY);
                }
            }
        }
    }

    /// Stream one events connection. `Dropped` means the stream ended before
    /// the status event and the caller may reconnect (resuming after
    /// `state.last_event_id`).
    fn stream_exec_events(
        &self,
        sandbox_id: &str,
        exec_id: &str,
        state: &mut ExecStreamState,
    ) -> Result<StreamOutcome> {
        let mut events_endpoint =
            format!("/exec/{}/events?encoding=base64", encode_segment(exec_id));
        if let Some(after) = state.last_event_id {
            events_endpoint.push_str(&format!("&after={after}"));
        }
        let response = self.data_plane_request(sandbox_id, |http, base| {
            // The blocking client applies the request timeout to each body
            // read, so this acts as a read-idle guard on the event stream.
            http.get(sandbox_endpoint_url(base, sandbox_id, &events_endpoint))
                .timeout(READ_IDLE_TIMEOUT)
        })?;
        if response.status() == StatusCode::CONFLICT {
            return Ok(StreamOutcome::Busy);
        }
        let response = ensure_data_plane_success(response)?;

        let mut events = SseReader::new(BufReader::new(response));
        loop {
            let event = match events.next_event() {
                Ok(Some(event)) => event,
                Ok(None) => return Ok(StreamOutcome::Dropped),
                Err(err) => {
                    log::warn!("Sandbox exec event stream was interrupted: {err}");
                    return Ok(StreamOutcome::Dropped);
                }
            };
            if let Some(id) = event.id.as_deref().and_then(|id| id.parse().ok()) {
                state.last_event_id = Some(id);
            }
            match event.event.as_deref() {
                Some("stdout") => append_capped(&mut state.stdout, &decode_exec_data(&event.data)?),
                Some("stderr") => append_capped(&mut state.stderr, &decode_exec_data(&event.data)?),
                Some("status") => {
                    let status = serde_json::from_str(&event.data)
                        .context("Invalid sandbox exec status event")?;
                    return Ok(StreamOutcome::Done(status));
                }
                _ => {}
            }
        }
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
    /// once if the token was rejected (expired mid-session).
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
        // workers, the lock heartbeat) share one refresh instead of
        // stampeding the API; they all need the same token anyway.
        let mut cache = self
            .access
            .lock()
            .map_err(|_| anyhow!("sandbox access cache lock poisoned"))?;
        if let Some(access) = cache.get(sandbox_id)
            && !access.expires_soon()
        {
            return Ok(access.clone());
        }
        match self.mint_access(sandbox_id) {
            Ok(access) => {
                cache.insert(sandbox_id.to_string(), access.clone());
                Ok(access)
            }
            // The refresh margin fires two minutes early, so a failed
            // refresh is not fatal while the current token is still valid.
            Err(err) => match cache.get(sandbox_id) {
                Some(access) if !access.is_expired() => {
                    log::warn!(
                        "Failed to refresh sandbox access token; reusing the current one: {err:#}"
                    );
                    Ok(access.clone())
                }
                _ => Err(err),
            },
        }
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

#[derive(Default)]
struct ExecStreamState {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    last_event_id: Option<u64>,
}

enum StreamOutcome {
    Done(ExecStatus),
    Dropped,
    Busy,
}

fn append_capped(buffer: &mut Vec<u8>, bytes: &[u8]) {
    let remaining = EXEC_MAX_OUTPUT_BYTES.saturating_sub(buffer.len());
    buffer.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
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

fn decode_exec_data(data: &str) -> Result<Vec<u8>> {
    BASE64
        .decode(data.trim())
        .context("Invalid base64 in sandbox exec event")
}

#[derive(Debug, PartialEq, Eq)]
struct SseEvent {
    id: Option<String>,
    event: Option<String>,
    data: String,
}

/// Minimal SSE reader for the sandbox exec event stream. Comment lines
/// (keep-alives) are skipped; events without data are ignored; a partial
/// event cut off at EOF is dropped (the caller reconnects with `after`).
struct SseReader<R> {
    reader: R,
}

impl<R: BufRead> SseReader<R> {
    fn new(reader: R) -> Self {
        Self { reader }
    }

    fn next_event(&mut self) -> std::io::Result<Option<SseEvent>> {
        let mut id = None;
        let mut event = None;
        let mut data: Vec<String> = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            if line.is_empty() {
                if data.is_empty() {
                    id = None;
                    event = None;
                    continue;
                }
                return Ok(Some(SseEvent {
                    id,
                    event,
                    data: data.join("\n"),
                }));
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };
            match field {
                "id" => id = Some(value.to_string()),
                "event" => event = Some(value.to_string()),
                "data" => data.push(value.to_string()),
                _ => {}
            }
        }
    }
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

fn heartbeat_once(state: &SandboxLockState) -> Result<LockHeartbeat> {
    let Some(current) = read_lock_file(&state.client, &state.sandbox_id)
        .context("Failed to refresh sandbox lock")?
    else {
        return Ok(LockHeartbeat::Lost);
    };
    if current.lease_id != state.lease_id {
        return Ok(LockHeartbeat::Lost);
    }

    let now = Utc::now();
    let lock = SandboxLockFile {
        updated_at: now,
        expires_at: now + chrono::Duration::seconds(state.ttl_seconds),
        ttl_seconds: state.ttl_seconds,
        ..current
    };
    write_lock_file(&state.client, &state.sandbox_id, &lock)
        .context("Failed to refresh sandbox lock")?;
    Ok(LockHeartbeat::Active)
}

fn release_once(state: &SandboxLockState) -> Result<()> {
    if let Some(current) = read_lock_file(&state.client, &state.sandbox_id)
        .context("Failed to release sandbox lock")?
        && current.lease_id != state.lease_id
    {
        return Ok(());
    }
    delete_lock_file(&state.client, &state.sandbox_id).context("Failed to release sandbox lock")
}

fn acquire_lock_file(
    client: &SandboxClient,
    sandbox_id: &str,
    lock: &SandboxLockFile,
    force_reclaim_stale: bool,
) -> Result<()> {
    client.mkdir_p(sandbox_id, "/home/sandbox/.diode")?;
    if let Some(existing) = read_lock_file(client, sandbox_id)?
        && (!force_reclaim_stale || !existing.is_stale())
    {
        bail!(
            "Sandbox is already locked: existing lock is active ({})",
            existing.holder
        );
    }

    write_lock_file(client, sandbox_id, lock)
}

impl SandboxLockFile {
    fn is_stale(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

fn read_lock_file(client: &SandboxClient, sandbox_id: &str) -> Result<Option<SandboxLockFile>> {
    let Some(bytes) = client.read_file_optional(sandbox_id, SANDBOX_LOCK_FILE_PATH)? else {
        return Ok(None);
    };
    let lock = serde_json::from_slice(&bytes).context("Failed to parse sandbox lock file")?;
    Ok(Some(lock))
}

fn write_lock_file(client: &SandboxClient, sandbox_id: &str, lock: &SandboxLockFile) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(lock).context("Failed to encode sandbox lock file")?;
    client.write_file(sandbox_id, SANDBOX_LOCK_FILE_PATH, &bytes)
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

    fn read_all_events(input: &str) -> Vec<SseEvent> {
        let mut reader = SseReader::new(input.as_bytes());
        let mut events = Vec::new();
        while let Some(event) = reader.next_event().unwrap() {
            events.push(event);
        }
        events
    }

    #[test]
    fn parses_sse_events() {
        let events = read_all_events(
            "id: 1\nevent: stdout\ndata: aGVsbG8=\n\nid: 2\nevent: status\ndata: {\"exitCode\":0}\n\n",
        );
        assert_eq!(
            events,
            vec![
                SseEvent {
                    id: Some("1".to_string()),
                    event: Some("stdout".to_string()),
                    data: "aGVsbG8=".to_string(),
                },
                SseEvent {
                    id: Some("2".to_string()),
                    event: Some("status".to_string()),
                    data: "{\"exitCode\":0}".to_string(),
                },
            ]
        );
    }

    #[test]
    fn sse_skips_keep_alive_comments_and_blank_lines() {
        let events = read_all_events(":keep-alive\n\n\n:\n\nid: 7\nevent: stderr\ndata: b2g=\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("7"));
        assert_eq!(events[0].event.as_deref(), Some("stderr"));
        assert_eq!(events[0].data, "b2g=");
    }

    #[test]
    fn sse_handles_crlf_and_multi_line_data() {
        let events = read_all_events("event: status\r\ndata: {\r\ndata: }\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\n}");
    }

    #[test]
    fn sse_drops_partial_event_at_eof() {
        let events = read_all_events("id: 3\nevent: stdout\ndata: aGVsbG8=");
        assert!(events.is_empty());
    }

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
    fn deserializes_exec_status() {
        let status: ExecStatus = serde_json::from_str(
            "{\"exitCode\":null,\"durationMs\":1200,\"timedOut\":true,\"canceled\":false}",
        )
        .unwrap();
        assert_eq!(status.exit_code, None);
        assert_eq!(status.duration_ms, 1200);
        assert!(status.timed_out);
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
    fn caps_accumulated_exec_output() {
        let mut buffer = vec![0u8; EXEC_MAX_OUTPUT_BYTES - 2];
        append_capped(&mut buffer, b"abcde");
        assert_eq!(buffer.len(), EXEC_MAX_OUTPUT_BYTES);
        assert_eq!(&buffer[EXEC_MAX_OUTPUT_BYTES - 2..], b"ab");
        append_capped(&mut buffer, b"xyz");
        assert_eq!(buffer.len(), EXEC_MAX_OUTPUT_BYTES);
    }

    #[test]
    fn access_token_refresh_margin() {
        let fresh = SandboxAccess {
            token: "tok".to_string(),
            expires_at: Utc::now().timestamp() + 600,
            data_plane_url: "https://example.com".to_string(),
        };
        assert!(!fresh.expires_soon());
        let expiring = SandboxAccess {
            expires_at: Utc::now().timestamp() + 60,
            ..fresh.clone()
        };
        assert!(expiring.expires_soon());
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
