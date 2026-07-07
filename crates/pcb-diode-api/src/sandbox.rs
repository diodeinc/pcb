use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::WorkspaceContext;

pub const SANDBOX_LOCK_FILE_PATH: &str = "/home/sandbox/.diode/sandbox-lock.json";
const LOCK_HEARTBEAT_MAX_FAILURES: usize = 3;

#[derive(Clone)]
pub struct SandboxClient {
    api_base_url: String,
    ctx: WorkspaceContext,
    http: Client,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecSyncRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_stdout_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_stderr_bytes: Option<usize>,
}

impl ExecSyncRequest {
    pub fn command(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            shell: None,
            cwd: None,
            env: None,
            timeout_ms: None,
            max_stdout_bytes: None,
            max_stderr_bytes: None,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecSyncOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub timed_out: bool,
    pub truncated: ExecSyncTruncatedFields,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecSyncTruncatedFields {
    pub stdout: bool,
    pub stderr: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SandboxExecRequest {
    argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxExecStatus {
    exit_code: Option<i32>,
    duration_ms: Option<i64>,
    #[serde(default)]
    timed_out: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxListResponse {
    pub path: String,
    pub entries: Vec<SandboxDirEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxDirEntry {
    pub name: String,
    pub path: String,
    #[serde(rename = "type", alias = "kind")]
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
                .build()
                .context("Failed to create sandbox HTTP client")?,
        })
    }

    pub fn exec_sync(&self, sandbox_id: &str, request: ExecSyncRequest) -> Result<ExecSyncOutput> {
        let started_at = Instant::now();
        let max_stdout_bytes = request.max_stdout_bytes;
        let max_stderr_bytes = request.max_stderr_bytes;
        let response = self
            .authenticated(
                self.http
                    .post(self.url(&format!(
                        "/api/sandboxes/{}/exec",
                        encode_segment(sandbox_id)
                    )))
                    .json(&SandboxExecRequest::from(request)),
            )?
            .send()
            .context("Sandbox exec request failed")?;
        let response = self.ensure_success(response)?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .context("Sandbox exec response did not include a Location header")?;
        let exec_id = exec_id_from_location(location)?;

        match self.read_exec_events(
            sandbox_id,
            &exec_id,
            started_at,
            max_stdout_bytes,
            max_stderr_bytes,
        ) {
            Ok(output) => Ok(output),
            Err(err) => {
                let _ = self.delete_exec(sandbox_id, &exec_id);
                Err(err)
            }
        }
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
        self.get_json(&sandbox_fs_url("read", sandbox_id, path)?)
    }

    pub fn read_file(&self, sandbox_id: &str, path: &str) -> Result<Vec<u8>> {
        let url = self.url(&sandbox_fs_url("read", sandbox_id, path)?);
        let response = self
            .authenticated(self.http.get(url))?
            .send()
            .context("Failed to read sandbox file")?;
        let response = self.ensure_success(response)?;
        Ok(response.bytes()?.to_vec())
    }

    pub fn write_file(&self, sandbox_id: &str, path: &str, bytes: &[u8]) -> Result<()> {
        let response = self
            .authenticated(
                self.http
                    .put(self.url(&sandbox_fs_url("write", sandbox_id, path)?)),
            )?
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .context("Sandbox PUT request failed")?;
        let _: serde_json::Value = self
            .ensure_success(response)?
            .json()
            .context("Invalid sandbox response")?;
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
        let response = self
            .authenticated(
                self.http
                    .delete(self.url(&sandbox_fs_url("remove", sandbox_id, path)?)),
            )?
            .send()
            .context("Sandbox DELETE request failed")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        self.ensure_success(response)?;
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

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let response = self
            .authenticated(self.http.get(self.url(path)))?
            .send()
            .context("Sandbox GET request failed")?;
        self.ensure_success(response)?
            .json()
            .context("Invalid sandbox response")
    }

    fn read_exec_events(
        &self,
        sandbox_id: &str,
        exec_id: &str,
        started_at: Instant,
        max_stdout_bytes: Option<usize>,
        max_stderr_bytes: Option<usize>,
    ) -> Result<ExecSyncOutput> {
        let response = self
            .authenticated(self.http.get(self.url(&format!(
                "/api/sandboxes/{}/exec/{}/events?encoding=base64",
                encode_segment(sandbox_id),
                encode_segment(exec_id)
            ))))?
            .send()
            .context("Sandbox exec events request failed")?;
        let response = self.ensure_success(response)?;

        let mut stdout = LimitedBuffer::new(max_stdout_bytes);
        let mut stderr = LimitedBuffer::new(max_stderr_bytes);
        let mut status = None;
        read_sse_events(response, |event, data| -> Result<()> {
            match event {
                Some("stdout") => stdout.append_base64(data)?,
                Some("stderr") => stderr.append_base64(data)?,
                Some("status") => {
                    status = Some(
                        serde_json::from_str::<SandboxExecStatus>(data)
                            .context("Invalid sandbox exec status event")?,
                    );
                }
                _ => {}
            }
            Ok(())
        })?;

        let status = status.context("Sandbox exec ended without a status event")?;
        let stdout_truncated = stdout.truncated;
        let stderr_truncated = stderr.truncated;
        Ok(ExecSyncOutput {
            stdout: stdout.into_string(),
            stderr: stderr.into_string(),
            exit_code: status.exit_code,
            duration_ms: status
                .duration_ms
                .unwrap_or_else(|| started_at.elapsed().as_millis().min(i64::MAX as u128) as i64),
            timed_out: status.timed_out,
            truncated: ExecSyncTruncatedFields {
                stdout: stdout_truncated,
                stderr: stderr_truncated,
            },
        })
    }

    fn delete_exec(&self, sandbox_id: &str, exec_id: &str) -> Result<()> {
        let response = self
            .authenticated(self.http.delete(self.url(&format!(
                "/api/sandboxes/{}/exec/{}",
                encode_segment(sandbox_id),
                encode_segment(exec_id)
            ))))?
            .send()
            .context("Sandbox exec cancel request failed")?;
        self.ensure_success(response)?;
        Ok(())
    }

    fn ensure_success(
        &self,
        response: reqwest::blocking::Response,
    ) -> Result<reqwest::blocking::Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let text = response.text().unwrap_or_default();
        if status == StatusCode::UNAUTHORIZED {
            bail!("Sandbox API request was not authorized. Run `pcb auth login`.");
        }
        bail!("Sandbox API request failed with {status}: {text}");
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

impl ExecSyncRequest {
    pub fn max_stdout(mut self, bytes: usize) -> Self {
        self.max_stdout_bytes = Some(bytes);
        self
    }

    pub fn max_stderr(mut self, bytes: usize) -> Self {
        self.max_stderr_bytes = Some(bytes);
        self
    }
}

impl From<ExecSyncRequest> for SandboxExecRequest {
    fn from(request: ExecSyncRequest) -> Self {
        let shell = request.shell.unwrap_or_else(|| "/bin/bash".to_string());
        Self {
            argv: vec![shell, "-lc".to_string(), request.command],
            cwd: request.cwd,
            env: request.env,
            timeout_ms: request.timeout_ms,
        }
    }
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: Option<usize>,
    truncated: bool,
}

impl LimitedBuffer {
    fn new(limit: Option<usize>) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            truncated: false,
        }
    }

    fn append_base64(&mut self, value: &str) -> Result<()> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .context("Invalid sandbox exec output event")?;
        self.append(&bytes);
        Ok(())
    }

    fn append(&mut self, bytes: &[u8]) {
        let Some(limit) = self.limit else {
            self.bytes.extend_from_slice(bytes);
            return;
        };
        let remaining = limit.saturating_sub(self.bytes.len());
        let keep = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..keep]);
        if keep < bytes.len() {
            self.truncated = true;
        }
    }

    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

fn read_sse_events(
    response: reqwest::blocking::Response,
    mut on_event: impl FnMut(Option<&str>, &str) -> Result<()>,
) -> Result<()> {
    let reader = BufReader::new(response);
    read_sse_events_from_reader(reader, &mut on_event)
}

fn read_sse_events_from_reader(
    reader: impl BufRead,
    on_event: &mut impl FnMut(Option<&str>, &str) -> Result<()>,
) -> Result<()> {
    let mut event: Option<String> = None;
    let mut data = Vec::new();

    for line in reader.lines() {
        let line = line.context("Failed to read sandbox exec event stream")?;
        let line = line.strip_suffix('\r').unwrap_or(&line);
        if line.is_empty() {
            dispatch_sse_event(on_event, &mut event, &mut data)?;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_string());
        }
    }

    dispatch_sse_event(on_event, &mut event, &mut data)
}

fn dispatch_sse_event(
    on_event: &mut impl FnMut(Option<&str>, &str) -> Result<()>,
    event: &mut Option<String>,
    data: &mut Vec<String>,
) -> Result<()> {
    if event.is_none() && data.is_empty() {
        return Ok(());
    }
    let joined = data.join("\n");
    on_event(event.as_deref(), &joined)?;
    *event = None;
    data.clear();
    Ok(())
}

fn exec_id_from_location(location: &str) -> Result<String> {
    location
        .split('/')
        .rfind(|segment| !segment.is_empty())
        .map(ToString::to_string)
        .context("Sandbox exec response Location did not include an id")
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
    let Some(bytes) = read_file_if_exists(client, sandbox_id, SANDBOX_LOCK_FILE_PATH)? else {
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
    let response = client
        .authenticated(client.http.delete(client.url(&sandbox_fs_url(
            "remove",
            sandbox_id,
            SANDBOX_LOCK_FILE_PATH,
        )?)))?
        .send()
        .context("Failed to delete sandbox lock file")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(());
    }
    client.ensure_success(response)?;
    Ok(())
}

fn read_file_if_exists(
    client: &SandboxClient,
    sandbox_id: &str,
    path: &str,
) -> Result<Option<Vec<u8>>> {
    let response = client
        .authenticated(
            client
                .http
                .get(client.url(&sandbox_fs_url("read", sandbox_id, path)?)),
        )?
        .send()
        .context("Failed to read sandbox file")?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let response = client.ensure_success(response)?;
    Ok(Some(response.bytes()?.to_vec()))
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

fn sandbox_fs_url(operation: &str, sandbox_id: &str, path: &str) -> Result<String> {
    Ok(format!(
        "/api/sandboxes/{}/fs/{}?path={}",
        encode_segment(sandbox_id),
        operation,
        encoded_query_path(path)?
    ))
}

fn encoded_query_path(path: &str) -> Result<String> {
    require_safe_absolute_path(path)?;
    Ok(urlencoding::encode(path).into_owned())
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
    fn encodes_absolute_paths_for_query_params() {
        assert_eq!(
            encoded_query_path("/home/sandbox/My Board/main.zen").unwrap(),
            "%2Fhome%2Fsandbox%2FMy%20Board%2Fmain.zen"
        );
    }

    #[test]
    fn builds_query_style_sandbox_fs_urls() {
        assert_eq!(
            sandbox_fs_url(
                "read",
                "sandbox/id",
                "/home/sandbox/registry/components/LT3010EMS8E#PBF/LT3010x.zen"
            )
            .unwrap(),
            "/api/sandboxes/sandbox%2Fid/fs/read?path=%2Fhome%2Fsandbox%2Fregistry%2Fcomponents%2FLT3010EMS8E%23PBF%2FLT3010x.zen"
        );
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(encoded_query_path("relative/path").is_err());
        assert!(encoded_query_path("/home/sandbox/../main.zen").is_err());
        assert!(encoded_query_path("/home//sandbox/main.zen").is_err());
    }

    #[test]
    fn parses_directory_listing_from_fs_read() {
        let listing: SandboxListResponse = serde_json::from_str(
            r#"{
                "path": "/home/sandbox/layout",
                "type": "directory",
                "entries": [
                    {
                        "name": "layout.kicad_pcb",
                        "path": "/home/sandbox/layout/layout.kicad_pcb",
                        "type": "file",
                        "size": 12,
                        "mode": "100644",
                        "mtime": null,
                        "etag": "\"abc\""
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(listing.path, "/home/sandbox/layout");
        assert_eq!(listing.entries[0].kind, "file");
        assert_eq!(listing.entries[0].mtime, None);
    }

    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\"'\"'s'");
    }

    #[test]
    fn maps_exec_sync_request_to_streaming_exec_request() {
        let mut env = BTreeMap::new();
        env.insert("A".to_string(), "B".to_string());

        let request = ExecSyncRequest::command("pwd")
            .cwd("/home/sandbox")
            .env(env.clone())
            .timeout(Duration::from_secs(2));
        let exec = SandboxExecRequest::from(request);

        assert_eq!(exec.argv, vec!["/bin/bash", "-lc", "pwd"]);
        assert_eq!(exec.cwd.as_deref(), Some("/home/sandbox"));
        assert_eq!(exec.env, Some(env));
        assert_eq!(exec.timeout_ms, Some(2000));
    }

    #[test]
    fn parses_streaming_exec_events() {
        let body = concat!(
            "event: stdout\n",
            "data: aGVs\n",
            "\n",
            "event: stdout\n",
            "data: bG8=\n",
            "\n",
            "event: stderr\n",
            "data: ZXJy\n",
            "\n",
            "event: status\n",
            "data: {\"exitCode\":0,\"durationMs\":24,\"timedOut\":false,\"canceled\":false}\n",
            "\n",
        );
        let mut stdout = LimitedBuffer::new(Some(4));
        let mut stderr = LimitedBuffer::new(None);
        let mut status = None;

        let reader = std::io::Cursor::new(body);
        read_sse_events_from_reader(reader, &mut |event, data| -> Result<()> {
            match event {
                Some("stdout") => stdout.append_base64(data)?,
                Some("stderr") => stderr.append_base64(data)?,
                Some("status") => status = Some(serde_json::from_str::<SandboxExecStatus>(data)?),
                _ => {}
            }
            Ok(())
        })
        .unwrap();

        assert_eq!(stdout.into_string(), "hell");
        assert_eq!(stderr.into_string(), "err");
        assert!(status.is_some());
    }
}
