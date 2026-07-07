use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use inquire::Confirm;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pcb_diode_api::{
    ExecSyncRequest, SandboxClient, SandboxFileUri, SandboxLockGuard, SandboxLockOptions,
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError},
};
use std::thread;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use crate::layout::{LayoutArgs, LayoutOutputFormat};
use crate::open::OpenArgs;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
const REMOTE_LAYOUT_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const LOCAL_LAYOUT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const SYNC_RETRY_ATTEMPTS: usize = 3;
const SYNC_RETRY_DELAY: Duration = Duration::from_millis(500);
const SESSION_MANIFEST: &str = ".pcb-sync-session.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteLayoutResult {
    layout_dir: Option<String>,
    pcb_file: Option<String>,
}

pub fn execute_layout(uri: SandboxFileUri, args: LayoutArgs) -> Result<()> {
    if args.temp {
        bail!("Remote sandbox layout does not support --temp");
    }
    let should_open = !args.no_open && !args.check;

    let client = sandbox_client(&uri)?;
    let lock = client.acquire_lock(
        &uri.sandbox_id,
        lock_options("pcb layout", "This sandbox is running pcb layout locally."),
    )?;

    let status = pcb_ui::Spinner::builder("Running pcb layout in sandbox...").start();
    let result = run_remote_layout(&client, &uri, &args)?;
    status.set_message("Downloading layout from sandbox...");
    let restore_status = should_open.then_some(&status);
    let local = sync_layout_down(&client, &uri, &result, restore_status)?;
    if should_open {
        return open_layout_and_sync(&client, &uri, &local, lock, status);
    }

    lock.release()?;
    status.success(format!(
        "Remote layout synced to {}",
        local.pcb_file.display()
    ));
    Ok(())
}

pub fn execute_open(uri: SandboxFileUri, args: OpenArgs) -> Result<()> {
    let client = sandbox_client(&uri)?;
    let lock = client.acquire_lock(
        &uri.sandbox_id,
        lock_options("pcb open", "This sandbox is open in KiCad locally."),
    )?;

    let status = pcb_ui::Spinner::builder("Downloading layout from sandbox...").start();
    let local = if crate::sandbox_uri::is_remote_kicad_pcb_file(&uri) {
        sync_remote_pcb_file_down(&client, &uri, Some(&status))?
    } else {
        let layout_args = LayoutArgs {
            file: PathBuf::from(&uri.sandbox_path),
            config: Vec::new(),
            no_open: true,
            offline: args.offline,
            temp: false,
            check: false,
            suppress: Vec::new(),
            no_sync: true,
            format: LayoutOutputFormat::Human,
        };
        status.set_message("Running pcb layout in sandbox...");
        let result = run_remote_layout(&client, &uri, &layout_args)?;
        status.set_message("Downloading layout from sandbox...");
        sync_layout_down(&client, &uri, &result, Some(&status))?
    };

    open_layout_and_sync(&client, &uri, &local, lock, status)
}

struct LocalLayout {
    remote_layout_dir: String,
    local_layout_dir: PathBuf,
    pcb_file: PathBuf,
    sync_session: Option<SyncSession>,
    restored_from_recovery: bool,
}

#[derive(Default)]
struct SyncStats {
    uploaded: usize,
    removed: usize,
}

enum SyncOutcome {
    Clean(SyncStats),
    Recoverable {
        reason: RecoverableStopReason,
        error: anyhow::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecoverableStopReason {
    LockReclaimed,
    SyncFailed,
}

impl RecoverableStopReason {
    fn message(self) -> &'static str {
        match self {
            Self::LockReclaimed => {
                "Sandbox lock was reclaimed while KiCad was open; stopped syncing remote changes"
            }
            Self::SyncFailed => "Remote layout sync failed",
        }
    }
}

#[derive(Debug, Clone)]
struct SyncSession {
    manifest_path: PathBuf,
    manifest: SyncSessionManifest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncSessionManifest {
    version: u32,
    uri: String,
    remote_layout_dir: String,
    local_layout_dir: PathBuf,
    layout_file: PathBuf,
    state: SyncSessionState,
    stop_reason: Option<RecoverableStopReason>,
    prompt_seen: bool,
    started_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SyncSessionState {
    Active,
    Complete,
    Recoverable,
}

fn sandbox_client(uri: &SandboxFileUri) -> Result<SandboxClient> {
    SandboxClient::new(pcb_diode_api::WorkspaceContext::from_api_base_url(
        uri.api_base_url(),
    ))
}

fn lock_options(holder: &str, message: &str) -> SandboxLockOptions {
    SandboxLockOptions {
        message: Some(message.to_string()),
        ..SandboxLockOptions::local_edit(holder)
    }
}

fn open_layout_and_sync(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
    lock: SandboxLockGuard,
    status: pcb_ui::Spinner,
) -> Result<()> {
    let mut sync_session = local
        .sync_session
        .clone()
        .context("Remote sync session was not initialized")?;
    restore_recovered_layout_if_needed(client, uri, local, &status).with_context(|| {
        format!(
            "Failed to restore local recovery file {}",
            local.pcb_file.display()
        )
    })?;
    sync_session.mark_active()?;
    let running = install_shutdown_flag()?;
    status.set_message(format!("Opening {}...", local.pcb_file.display()));
    let watcher = LocalLayoutWatcher::new(&local.local_layout_dir)?;
    let mut session = match pcb_kicad::open_pcbnew_session(&local.pcb_file) {
        Ok(session) => session,
        Err(err) => {
            sync_session.mark_complete()?;
            return Err(err);
        }
    };
    status.set_message(format!(
        "Watching {} for KiCad changes...",
        local.local_layout_dir.display()
    ));

    match run_local_sync_loop(
        client,
        uri,
        local,
        &lock,
        &mut session,
        &watcher,
        &running,
        &status,
    ) {
        SyncOutcome::Clean(stats) => {
            sync_session.mark_complete()?;
            lock.release()?;
            status.success(format!(
                "Final sync complete ({} uploaded, {} removed). Local recovery file: {}",
                stats.uploaded,
                stats.removed,
                local.pcb_file.display()
            ));
            Ok(())
        }
        SyncOutcome::Recoverable { reason, error } => {
            sync_session.mark_recoverable(reason)?;
            status.error(format!(
                "Remote sync stopped; local recovery file is {}",
                local.pcb_file.display()
            ));
            let terminate_result = session.terminate();
            let release_result = lock.release();
            if let Err(terminate_err) = terminate_result {
                return Err(error).with_context(|| {
                    format!(
                        "Remote sync stopped. Local recovery file: {}. Also failed to quit KiCad: {terminate_err:#}",
                        local.pcb_file.display()
                    )
                });
            }
            if let Err(release_err) = release_result {
                return Err(error).with_context(|| {
                    format!(
                        "Remote sync stopped. Local recovery file: {}. Also failed to release the sandbox lock: {release_err:#}",
                        local.pcb_file.display()
                    )
                });
            }
            Err(error).with_context(|| {
                format!(
                    "Remote sync stopped. Local recovery file: {}",
                    local.pcb_file.display()
                )
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_local_sync_loop(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
    lock: &SandboxLockGuard,
    session: &mut pcb_kicad::PcbnewSession,
    watcher: &LocalLayoutWatcher,
    running: &AtomicBool,
    status: &pcb_ui::Spinner,
) -> SyncOutcome {
    loop {
        match session.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => return recoverable_outcome(RecoverableStopReason::SyncFailed, err),
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if !lock.is_active() {
            return recoverable_outcome(
                RecoverableStopReason::LockReclaimed,
                anyhow::anyhow!(RecoverableStopReason::LockReclaimed.message()),
            );
        }
        let changed = match watcher.changed_with_timeout(WATCH_POLL_INTERVAL) {
            Ok(changed) => changed,
            Err(err) => return recoverable_outcome(RecoverableStopReason::SyncFailed, err),
        };
        if changed {
            status.set_message("Syncing local changes to sandbox...");
            let stats = match sync_layout_up_with_retry(client, uri, local) {
                Ok(stats) => stats,
                Err(err) => return recoverable_outcome(RecoverableStopReason::SyncFailed, err),
            };
            status.set_message(format!(
                "Synced {} uploaded, {} removed. Watching for more changes...",
                stats.uploaded, stats.removed
            ));
        }
    }

    if lock.is_active() {
        status.set_message("Final sync to sandbox...");
        match sync_layout_up_with_retry(client, uri, local) {
            Ok(stats) => SyncOutcome::Clean(stats),
            Err(err) => recoverable_outcome(RecoverableStopReason::SyncFailed, err),
        }
    } else {
        recoverable_outcome(
            RecoverableStopReason::LockReclaimed,
            anyhow::anyhow!(RecoverableStopReason::LockReclaimed.message()),
        )
    }
}

fn recoverable_outcome(reason: RecoverableStopReason, error: anyhow::Error) -> SyncOutcome {
    SyncOutcome::Recoverable { reason, error }
}

struct LocalLayoutWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
}

impl LocalLayoutWatcher {
    fn new(path: &Path) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })
        .context("Failed to create local layout watcher")?;
        watcher
            .watch(path, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch {}", path.display()))?;
        Ok(Self {
            _watcher: watcher,
            rx,
        })
    }

    fn changed_with_timeout(&self, timeout: Duration) -> Result<bool> {
        let mut changed = match self.rx.recv_timeout(timeout) {
            Ok(event) => relevant_watch_event(event?)?,
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => bail!("Local layout watcher stopped"),
        };

        thread::sleep(WATCH_DEBOUNCE);
        for event in self.rx.try_iter() {
            changed |= relevant_watch_event(event?)?;
        }
        Ok(changed)
    }
}

fn relevant_watch_event(event: notify::Event) -> Result<bool> {
    if matches!(event.kind, EventKind::Access(_)) {
        return Ok(false);
    }
    Ok(event.paths.iter().any(|path| !should_skip_sync_path(path)))
}

fn install_shutdown_flag() -> Result<Arc<AtomicBool>> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    ctrlc::set_handler(move || {
        flag.store(false, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;
    Ok(running)
}

fn run_remote_layout(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    args: &LayoutArgs,
) -> Result<RemoteLayoutResult> {
    let mut command = vec![
        "pcb".to_string(),
        "layout".to_string(),
        "--no-open".to_string(),
        "-f".to_string(),
        "json".to_string(),
    ];
    if args.no_sync {
        command.push("--no-sync".to_string());
    }
    if args.offline {
        command.push("--offline".to_string());
    }
    if args.check {
        command.push("--check".to_string());
    }
    for config in &args.config {
        command.push("--config".to_string());
        command.push(config.clone());
    }
    for suppress in &args.suppress {
        command.push("--suppress".to_string());
        command.push(suppress.clone());
    }
    command.push(uri.sandbox_path.clone());

    let output = client.exec_sync_success(
        &uri.sandbox_id,
        ExecSyncRequest::command(shell_command(&command))
            .cwd(remote_parent_dir(&uri.sandbox_path)?)
            .timeout(REMOTE_LAYOUT_TIMEOUT)
            .max_stdout(2 * 1024 * 1024)
            .max_stderr(2 * 1024 * 1024),
    )?;

    parse_remote_layout_result(&output.stdout)
}

fn parse_remote_layout_result(stdout: &str) -> Result<RemoteLayoutResult> {
    let json = stdout.trim();
    if json.is_empty() {
        bail!("Remote layout command did not return layout result JSON");
    }
    let result: RemoteLayoutResult =
        serde_json::from_str(json).with_context(|| "Invalid remote layout result JSON")?;
    if result.layout_dir.is_none() || result.pcb_file.is_none() {
        bail!("Remote board does not define a layout path");
    }
    Ok(result)
}

fn sync_remote_pcb_file_down(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    status: Option<&pcb_ui::Spinner>,
) -> Result<LocalLayout> {
    let result = RemoteLayoutResult {
        layout_dir: Some(remote_parent_dir(&uri.sandbox_path)?),
        pcb_file: Some(uri.sandbox_path.clone()),
    };
    sync_layout_down(client, uri, &result, status)
}

fn sync_layout_down(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    result: &RemoteLayoutResult,
    restore_status: Option<&pcb_ui::Spinner>,
) -> Result<LocalLayout> {
    let remote_layout_dir = result
        .layout_dir
        .as_ref()
        .context("Remote layout result is missing layoutDir")?
        .to_string();
    let remote_pcb_file = result
        .pcb_file
        .as_ref()
        .context("Remote layout result is missing pcbFile")?
        .to_string();
    let cache_root = local_layout_cache_root(uri, &remote_layout_dir)?;
    let relative_pcb = remote_relative_path(&remote_layout_dir, &remote_pcb_file)?;

    let mut recovered_session = None;
    if let Some(status) = restore_status
        && let Some(session) = latest_recoverable_session(&cache_root)?
        && let Some(restore) = prompt_restore_recovery(status, &session)
    {
        mark_recoverable_sessions_prompt_seen(&cache_root);
        if restore {
            recovered_session = Some(session);
        }
    }

    let (local_layout_dir, pcb_file, sync_session, restored_from_recovery) =
        if let Some(session) = recovered_session {
            let local_layout_dir = session.manifest.local_layout_dir.clone();
            let pcb_file = session.manifest.layout_file.clone();
            (local_layout_dir, pcb_file, Some(session), true)
        } else {
            let local_layout_dir = new_local_layout_session_dir(&cache_root);
            fs::create_dir_all(&local_layout_dir)?;
            sync_remote_dir_down(
                client,
                &uri.sandbox_id,
                &remote_layout_dir,
                &local_layout_dir,
            )?;
            let pcb_file = local_layout_dir.join(&relative_pcb);
            let sync_session = if restore_status.is_some() {
                Some(SyncSession::create(
                    uri,
                    &remote_layout_dir,
                    local_layout_dir.clone(),
                    pcb_file.clone(),
                )?)
            } else {
                None
            };
            (local_layout_dir, pcb_file, sync_session, false)
        };

    Ok(LocalLayout {
        remote_layout_dir,
        local_layout_dir,
        pcb_file,
        sync_session,
        restored_from_recovery,
    })
}

fn restore_recovered_layout_if_needed(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
    status: &pcb_ui::Spinner,
) -> Result<()> {
    if !local.restored_from_recovery {
        return Ok(());
    }
    status.set_message(format!(
        "Restoring local recovery file {} to sandbox...",
        local.pcb_file.display()
    ));
    let stats = sync_layout_up_with_retry(client, uri, local)?;
    status.set_message(format!(
        "Restored recovery file ({} uploaded, {} removed).",
        stats.uploaded, stats.removed
    ));
    Ok(())
}

fn sync_remote_dir_down(
    client: &SandboxClient,
    sandbox_id: &str,
    remote_dir: &str,
    local_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(local_dir)?;
    // Only mirror the direct-child files of the layout directory; KiCad needs
    // the .kicad_pcb / .kicad_pro / .kicad_prl + friends that live alongside
    // the board, not arbitrary nested assets (3D models, fp-info-cache, etc.).
    let files: Vec<_> = client
        .list(sandbox_id, remote_dir)?
        .entries
        .into_iter()
        .filter(|entry| entry.kind == "file" && !should_skip_sync_name(&entry.name))
        .collect();

    files.par_iter().try_for_each(|entry| -> Result<()> {
        let bytes = client.read_file(sandbox_id, &entry.path)?;
        let local_path = local_dir.join(&entry.name);
        fs::write(&local_path, bytes)
            .with_context(|| format!("Failed to write {}", local_path.display()))
    })
}

fn sync_layout_up(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
) -> Result<SyncStats> {
    // Only sync direct-child files of the layout dir, mirroring the
    // download side. KiCad keeps the .kicad_pcb / .kicad_pro / .kicad_prl
    // + friends flat in the board directory.
    let mut local_names = BTreeSet::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(&local.local_layout_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if should_skip_sync_name(name_str) {
            continue;
        }
        let remote_path = format!(
            "{}/{}",
            local.remote_layout_dir.trim_end_matches('/'),
            name_str
        );
        local_names.insert(name_str.to_string());
        files.push((entry.path(), remote_path));
    }

    files
        .par_iter()
        .try_for_each(|(local_path, remote_path)| -> Result<()> {
            let bytes = fs::read(local_path).with_context(|| {
                format!("Failed to read local layout file {}", local_path.display())
            })?;
            client.write_file(&uri.sandbox_id, remote_path, &bytes)
        })?;

    let stale_remote: Vec<_> = client
        .list(&uri.sandbox_id, &local.remote_layout_dir)?
        .entries
        .into_iter()
        .filter(|entry| {
            entry.kind == "file"
                && !should_skip_sync_name(&entry.name)
                && !local_names.contains(&entry.name)
        })
        .collect();

    stale_remote
        .par_iter()
        .try_for_each(|entry| client.remove(&uri.sandbox_id, &entry.path))?;

    Ok(SyncStats {
        uploaded: files.len(),
        removed: stale_remote.len(),
    })
}

fn sync_layout_up_with_retry(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
) -> Result<SyncStats> {
    for attempt in 1..=SYNC_RETRY_ATTEMPTS {
        match sync_layout_up(client, uri, local) {
            Ok(stats) => return Ok(stats),
            Err(err) if attempt < SYNC_RETRY_ATTEMPTS => {
                log::warn!(
                    "Remote layout sync attempt {attempt}/{SYNC_RETRY_ATTEMPTS} failed: {err:#}"
                );
                thread::sleep(SYNC_RETRY_DELAY);
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("sync retry loop always returns")
}

fn local_layout_cache_root(uri: &SandboxFileUri, remote_layout_dir: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    let key = format!("{}:{}:{}", uri.host, uri.sandbox_id, remote_layout_dir);
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
    let cache_root = home
        .join(".pcb")
        .join("sandbox-layouts")
        .join(sanitize_path_component(&uri.host))
        .join(sanitize_path_component(&uri.sandbox_id))
        .join(id.to_string());
    if let Err(err) = prune_old_layout_sessions(&cache_root, LOCAL_LAYOUT_RETENTION) {
        log::warn!(
            "Failed to prune old local sandbox layout sessions in {}: {err:#}",
            cache_root.display()
        );
    }
    Ok(cache_root)
}

fn new_local_layout_session_dir(cache_root: &Path) -> PathBuf {
    let session_id = format!("{}-{}", Utc::now().format("%Y%m%dT%H%M%SZ"), Uuid::new_v4());
    cache_root.join(session_id)
}

impl SyncSession {
    fn create(
        uri: &SandboxFileUri,
        remote_layout_dir: &str,
        local_layout_dir: PathBuf,
        layout_file: PathBuf,
    ) -> Result<Self> {
        let now = Utc::now();
        let mut session = Self {
            manifest_path: local_layout_dir.join(SESSION_MANIFEST),
            manifest: SyncSessionManifest {
                version: 1,
                uri: sandbox_uri_string(uri),
                remote_layout_dir: remote_layout_dir.to_string(),
                local_layout_dir,
                layout_file,
                state: SyncSessionState::Active,
                stop_reason: None,
                prompt_seen: false,
                started_at: now,
                updated_at: now,
            },
        };
        session.save()?;
        Ok(session)
    }

    fn load(local_layout_dir: PathBuf) -> Result<Self> {
        let manifest_path = local_layout_dir.join(SESSION_MANIFEST);
        let bytes = fs::read(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
        Ok(Self {
            manifest_path,
            manifest,
        })
    }

    fn mark_active(&mut self) -> Result<()> {
        self.manifest.state = SyncSessionState::Active;
        self.manifest.stop_reason = None;
        self.manifest.updated_at = Utc::now();
        self.save()
    }

    fn mark_complete(&mut self) -> Result<()> {
        self.manifest.state = SyncSessionState::Complete;
        self.manifest.stop_reason = None;
        self.manifest.prompt_seen = true;
        self.manifest.updated_at = Utc::now();
        self.save()
    }

    fn mark_recoverable(&mut self, reason: RecoverableStopReason) -> Result<()> {
        self.manifest.state = SyncSessionState::Recoverable;
        self.manifest.stop_reason = Some(reason);
        self.manifest.prompt_seen = false;
        self.manifest.updated_at = Utc::now();
        self.save()
    }

    fn mark_prompt_seen(&mut self) -> Result<()> {
        self.manifest.prompt_seen = true;
        self.manifest.updated_at = Utc::now();
        self.save()
    }

    fn save(&mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.manifest)
            .context("Failed to encode local sync session manifest")?;
        fs::write(&self.manifest_path, bytes)
            .with_context(|| format!("Failed to write {}", self.manifest_path.display()))
    }
}

fn latest_recoverable_session(cache_root: &Path) -> Result<Option<SyncSession>> {
    if !cache_root.exists() {
        return Ok(None);
    }
    let mut latest: Option<SyncSession> = None;
    for entry in fs::read_dir(cache_root)
        .with_context(|| format!("Failed to read {}", cache_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Ok(session) = SyncSession::load(entry.path()) else {
            continue;
        };
        if !is_recovery_candidate(&session.manifest) {
            continue;
        }
        if latest
            .as_ref()
            .is_none_or(|current| session.manifest.updated_at > current.manifest.updated_at)
        {
            latest = Some(session);
        }
    }
    Ok(latest)
}

fn prompt_restore_recovery(status: &pcb_ui::Spinner, session: &SyncSession) -> Option<bool> {
    if !crate::tty::is_interactive() {
        eprintln!(
            "Found local recovery file for this remote layout at {}. Re-run interactively to restore it.",
            session.manifest.layout_file.display()
        );
        return None;
    }
    let prompt = format!(
        "Restore previous local KiCad recovery file from {}?",
        session.manifest.layout_file.display()
    );
    let ask = || {
        Confirm::new(&prompt)
            .with_default(true)
            .prompt()
            .unwrap_or(false)
    };
    Some(status.suspend(ask))
}

fn mark_recoverable_sessions_prompt_seen(cache_root: &Path) {
    let entries = match fs::read_dir(cache_root) {
        Ok(entries) => entries,
        Err(err) => {
            if cache_root.exists() {
                log::warn!(
                    "Failed to mark recovery prompts seen in {}: {err:#}",
                    cache_root.display()
                );
            }
            return;
        }
    };

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(mut session) = SyncSession::load(entry.path()) else {
            continue;
        };
        if is_recovery_candidate(&session.manifest)
            && let Err(err) = session.mark_prompt_seen()
        {
            log::warn!(
                "Failed to mark local recovery prompt seen at {}: {err:#}",
                session.manifest_path.display()
            );
        }
    }
}

fn is_recovery_candidate(manifest: &SyncSessionManifest) -> bool {
    matches!(
        manifest.state,
        SyncSessionState::Active | SyncSessionState::Recoverable
    ) && !manifest.prompt_seen
        && manifest.layout_file.is_file()
}

fn prune_old_layout_sessions(cache_root: &Path, retention: Duration) -> Result<()> {
    if !cache_root.exists() {
        return Ok(());
    }
    let cutoff = SystemTime::now()
        .checked_sub(retention)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    for entry in fs::read_dir(cache_root)
        .with_context(|| format!("Failed to read {}", cache_root.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if session_contains_lock_file(&path)? {
            continue;
        }
        if let Some(modified) = newest_direct_child_mtime(&path)?
            && modified < cutoff
        {
            fs::remove_dir_all(&path)
                .with_context(|| format!("Failed to remove old layout cache {}", path.display()))?;
        }
    }
    Ok(())
}

fn session_contains_lock_file(path: &Path) -> Result<bool> {
    for entry in fs::read_dir(path).with_context(|| format!("Failed to read {}", path.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.ends_with(".lck") || name.ends_with(".lock") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn newest_direct_child_mtime(path: &Path) -> Result<Option<SystemTime>> {
    let mut newest = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok();
    for entry in fs::read_dir(path).with_context(|| format!("Failed to read {}", path.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        newest = Some(match newest {
            Some(current) => current.max(modified),
            None => modified,
        });
    }
    Ok(newest)
}

fn remote_parent_dir(path: &str) -> Result<String> {
    let parent = Path::new(path)
        .parent()
        .context("Remote path must have a parent directory")?;
    Ok(parent.to_string_lossy().to_string())
}

fn remote_relative_path(remote_base: &str, remote_path: &str) -> Result<PathBuf> {
    let base = Path::new(remote_base);
    let path = Path::new(remote_path);
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .with_context(|| format!("{remote_path} is not inside {remote_base}"))
}

fn sandbox_uri_string(uri: &SandboxFileUri) -> String {
    uri.to_read_uri_string()
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "_".to_string()
    } else {
        sanitized
    }
}

fn should_skip_sync_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(should_skip_sync_name)
}

fn should_skip_sync_name(name: &str) -> bool {
    name == ".DS_Store"
        || name == SESSION_MANIFEST
        || name.ends_with('~')
        // KiCad layout logs are disposable, and prod rejects raw GETs for *.log paths.
        || name.ends_with(".log")
        || name.ends_with(".lck")
        || name.ends_with(".lock")
        || name.starts_with("_autosave-")
}

fn shell_command(args: &[String]) -> String {
    args.iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
