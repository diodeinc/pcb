use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::CONTENT_TYPE;
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
    pub kind: String,
    pub size: Option<u64>,
    pub mode: String,
    pub mtime: String,
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
        self.post_json(
            &format!("/api/sandboxes/{}/exec_sync", encode_segment(sandbox_id)),
            &request,
        )
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
        self.get_json(&format!(
            "/api/sandboxes/{}/fs/list{}",
            encode_segment(sandbox_id),
            encoded_absolute_path(path)?
        ))
    }

    pub fn read_file(&self, sandbox_id: &str, path: &str) -> Result<Vec<u8>> {
        let url = self.url(&format!(
            "/api/sandboxes/{}/fs/file{}",
            encode_segment(sandbox_id),
            encoded_absolute_path(path)?
        ));
        let response = self
            .authenticated(self.http.get(url))?
            .send()
            .context("Failed to read sandbox file")?;
        let response = self.ensure_success(response)?;
        Ok(response.bytes()?.to_vec())
    }

    pub fn write_file(&self, sandbox_id: &str, path: &str, bytes: &[u8]) -> Result<()> {
        let response = self
            .authenticated(self.http.put(self.url(&format!(
                "/api/sandboxes/{}/fs/write{}",
                encode_segment(sandbox_id),
                encoded_absolute_path(path)?
            ))))?
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

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let response = self
            .authenticated(self.http.get(self.url(path)))?
            .send()
            .context("Sandbox GET request failed")?;
        self.ensure_success(response)?
            .json()
            .context("Invalid sandbox response")
    }

    fn post_json<T: for<'de> Deserialize<'de>, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self
            .authenticated(self.http.post(self.url(path)))?
            .json(body)
            .send()
            .context("Sandbox POST request failed")?;
        self.ensure_success(response)?
            .json()
            .context("Invalid sandbox response")
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
        .authenticated(client.http.delete(client.url(&format!(
            "/api/sandboxes/{}/fs/file{}",
            encode_segment(sandbox_id),
            encoded_absolute_path(SANDBOX_LOCK_FILE_PATH)?
        ))))?
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
        .authenticated(client.http.get(client.url(&format!(
            "/api/sandboxes/{}/fs/file{}",
            encode_segment(sandbox_id),
            encoded_absolute_path(path)?
        ))))?
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

fn encoded_absolute_path(path: &str) -> Result<String> {
    require_safe_absolute_path(path)?;
    Ok(path
        .split('/')
        .skip(1)
        .map(encode_segment)
        .fold(String::new(), |mut output, segment| {
            output.push('/');
            output.push_str(&segment);
            output
        }))
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
    fn encodes_absolute_paths_segment_by_segment() {
        assert_eq!(
            encoded_absolute_path("/home/sandbox/My Board/main.zen").unwrap(),
            "/home/sandbox/My%20Board/main.zen"
        );
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(encoded_absolute_path("relative/path").is_err());
        assert!(encoded_absolute_path("/home/sandbox/../main.zen").is_err());
        assert!(encoded_absolute_path("/home//sandbox/main.zen").is_err());
    }

    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\"'\"'s'");
    }
}
