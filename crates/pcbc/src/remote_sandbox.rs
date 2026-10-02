use anyhow::{Context, Result, bail, ensure};
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
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use crate::layout::{LayoutArgs, LayoutOutputFormat};
use crate::open::OpenArgs;
use crate::recovery_dialog::{RecoveryChoice, URL_LAUNCHER_ENV};
use crate::sandbox_uri::is_kicad_sch_path;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
// The sandbox data plane caps exec timeouts at 15 minutes (SANDBOXD_MAX_TIMEOUT_MS).
const REMOTE_LAYOUT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const LOCAL_LAYOUT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const SYNC_RETRY_ATTEMPTS: usize = 3;
const SYNC_RETRY_DELAY: Duration = Duration::from_millis(500);
const SESSION_MANIFEST: &str = ".pcb-sync-session.json";
const MAX_SYNC_DEPTH: usize = 32;
const MAX_SYNC_ENTRIES: usize = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteLayoutResult {
    layout_dir: Option<String>,
    pcb_file: Option<String>,
}

pub fn execute_layout(uri: SandboxFileUri, args: LayoutArgs) -> Result<()> {
    let should_open = !args.no_open && !args.check;

    let client = sandbox_client(&uri)?;
    let lock = client.acquire_lock(
        &uri.sandbox_id,
        lock_options("pcb layout", "This sandbox is running pcb layout locally."),
    )?;
    let client = lock.client();
    install_shutdown_handler(&lock)?;

    let status = pcb_ui::Spinner::builder("Running pcb layout in sandbox...").start();
    let result = run_remote_layout(&client, &uri, &args)?;
    status.set_message("Downloading layout from sandbox...");
    let restore_status = should_open.then_some(&status);
    let Some(local) = sync_layout_down(&client, &uri, &result, restore_status)? else {
        return finish_cancelled(lock, status);
    };
    if should_open {
        return open_layout_and_sync(&uri, &local, lock, status);
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
    let client = lock.client();
    install_shutdown_handler(&lock)?;

    let status = pcb_ui::Spinner::builder("Downloading KiCad files from sandbox...").start();
    let local = if crate::sandbox_uri::is_remote_kicad_pcb_file(&uri)
        || is_kicad_sch_path(Path::new(&uri.sandbox_path))
    {
        sync_remote_kicad_file_down(&client, &uri, Some(&status))?
    } else {
        let layout_args = LayoutArgs {
            file: PathBuf::from(&uri.sandbox_path),
            config: Vec::new(),
            no_open: true,
            offline: args.offline,
            check: false,
            suppress: Vec::new(),
            no_sync: true,
            sync_footprints: false,
            format: LayoutOutputFormat::Human,
            skip_bom_hydration: false,
        };
        status.set_message("Running pcb layout in sandbox...");
        let result = run_remote_layout(&client, &uri, &layout_args)?;
        status.set_message("Downloading layout from sandbox...");
        sync_layout_down(&client, &uri, &result, Some(&status))?
    };
    let Some(local) = local else {
        return finish_cancelled(lock, status);
    };

    open_layout_and_sync(&uri, &local, lock, status)
}

/// Wind down cleanly after the user cancels the recovery prompt.
fn finish_cancelled(lock: SandboxLockGuard, status: pcb_ui::Spinner) -> Result<()> {
    lock.release()?;
    status.finish();
    Ok(())
}

struct LocalLayout {
    cache_root: PathBuf,
    remote_layout_dir: String,
    local_layout_dir: PathBuf,
    pcb_file: PathBuf,
    sync_session: Option<SyncSession>,
    restored_from_recovery: bool,
    attached_editor_pid: Option<u32>,
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

enum EditorSession {
    Spawned(pcb_kicad::KiCadSession),
    Attached { pid: u32, file: PathBuf },
}

impl EditorSession {
    fn id(&self) -> u32 {
        match self {
            Self::Spawned(session) => session.id(),
            Self::Attached { pid, .. } => *pid,
        }
    }

    fn is_running(&mut self) -> Result<bool> {
        match self {
            Self::Spawned(session) => Ok(session.try_wait()?.is_none()),
            Self::Attached { pid, file } => Ok(editor_process_is_running(*pid, file)),
        }
    }
}

#[derive(Debug)]
enum RecoverableSession {
    Ready(SyncSession),
    EditorOpen { session: SyncSession, pid: u32 },
}

impl RecoverableSession {
    fn updated_at(&self) -> DateTime<Utc> {
        match self {
            Self::Ready(session) | Self::EditorOpen { session, .. } => session.manifest.updated_at,
        }
    }
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
                "Sandbox editor lease expired or was reclaimed; stopped syncing remote changes"
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
    #[serde(default)]
    editor_pid: Option<u32>,
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
    uri: &SandboxFileUri,
    local: &LocalLayout,
    lock: SandboxLockGuard,
    status: pcb_ui::Spinner,
) -> Result<()> {
    let mut sync_session = local
        .sync_session
        .clone()
        .context("Remote sync session was not initialized")?;
    // Start watching before recovery upload so saves made in an already-open
    // KiCad window during the upload are queued for the sync loop.
    let watcher = LocalLayoutWatcher::new(&local.local_layout_dir)?;
    restore_recovered_layout_if_needed(&lock.client(), uri, local, &mut sync_session, &status)
        .with_context(|| {
            format!(
                "Failed to restore local recovery file {}",
                local.pcb_file.display()
            )
        })?;
    status.set_message(match local.attached_editor_pid {
        Some(_) => format!("Resuming sync for {}...", local.pcb_file.display()),
        None => format!("Opening {}...", local.pcb_file.display()),
    });
    let mut session = match local.attached_editor_pid {
        Some(pid) => EditorSession::Attached {
            pid,
            file: local.pcb_file.clone(),
        },
        None => match if is_kicad_sch_path(&local.pcb_file) {
            pcb_kicad::open_eeschema_session(&local.pcb_file)
        } else {
            pcb_kicad::open_pcbnew_session(&local.pcb_file)
        } {
            Ok(session) => EditorSession::Spawned(session),
            Err(err) => {
                sync_session.mark_complete()?;
                return Err(err);
            }
        },
    };
    sync_session.mark_active(session.id())?;
    status.set_message(format!(
        "Watching {} for KiCad changes...",
        local.local_layout_dir.display()
    ));

    match run_local_sync_loop(uri, local, &lock, &mut session, &watcher, &status) {
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
            // Leave KiCad open so an interrupted sync cannot discard unsaved
            // edits or close another concurrent editor session.
            let release_result = lock.release();
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

fn run_local_sync_loop(
    uri: &SandboxFileUri,
    local: &LocalLayout,
    lock: &SandboxLockGuard,
    session: &mut EditorSession,
    watcher: &LocalLayoutWatcher,
    status: &pcb_ui::Spinner,
) -> SyncOutcome {
    let client = lock.client();
    loop {
        if lock.is_stopped() {
            return recoverable_outcome(
                RecoverableStopReason::SyncFailed,
                anyhow::anyhow!("Sync interrupted; local changes have been preserved"),
            );
        }
        match session.is_running() {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) => return recoverable_outcome(RecoverableStopReason::SyncFailed, err),
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
            let stats = match sync_layout_up_with_retry(&client, uri, local) {
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
        match sync_layout_up_with_retry(&client, uri, local) {
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

fn install_shutdown_handler(lock: &SandboxLockGuard) -> Result<()> {
    ctrlc::set_handler(lock.stop_handler()).context("Failed to set Ctrl-C handler")
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
    if args.sync_footprints {
        command.push("--sync-footprints".to_string());
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
            .timeout(REMOTE_LAYOUT_TIMEOUT),
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

fn sync_remote_kicad_file_down(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    status: Option<&pcb_ui::Spinner>,
) -> Result<Option<LocalLayout>> {
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
) -> Result<Option<LocalLayout>> {
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
    let mut attached_editor_pid = None;
    if let Some(status) = restore_status
        && let Some(recovery) = latest_recoverable_session(&cache_root, &relative_pcb)?
    {
        match recovery {
            RecoverableSession::Ready(session) => {
                match prompt_restore_recovery(status, &session)? {
                    Some(RecoveryChoice::Restore) => recovered_session = Some(session),
                    Some(RecoveryChoice::Discard) => {
                        mark_recoverable_sessions_prompt_seen(&cache_root, &relative_pcb)
                    }
                    Some(RecoveryChoice::Cancel) => return Ok(None),
                    None => {}
                }
            }
            RecoverableSession::EditorOpen { session, pid } => {
                if !prompt_resume_existing_editor(status, &session)? {
                    return Ok(None);
                }
                recovered_session = Some(session);
                attached_editor_pid = Some(pid);
            }
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
                is_kicad_sch_path(&relative_pcb),
            )?;
            let pcb_file = local_layout_dir.join(&relative_pcb);
            ensure!(
                pcb_file.is_file(),
                "Remote KiCad file was not downloaded: {remote_pcb_file}"
            );
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

    Ok(Some(LocalLayout {
        cache_root,
        remote_layout_dir,
        local_layout_dir,
        pcb_file,
        sync_session,
        restored_from_recovery,
        attached_editor_pid,
    }))
}

fn restore_recovered_layout_if_needed(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
    sync_session: &mut SyncSession,
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
    mark_recoverable_sessions_prompt_seen(
        &local.cache_root,
        local.pcb_file.strip_prefix(&local.local_layout_dir)?,
    );
    sync_session.mark_prompt_seen()?;
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
    schematic: bool,
) -> Result<()> {
    fs::create_dir_all(local_dir)?;
    let files = remote_sync_files(client, sandbox_id, remote_dir, schematic)?;
    files.par_iter().try_for_each(|relative| -> Result<()> {
        let bytes = client.read_file(sandbox_id, &remote_sync_path(remote_dir, relative))?;
        let local_path = local_dir.join(relative);
        fs::create_dir_all(local_path.parent().context("Sync file has no parent")?)?;
        fs::write(&local_path, bytes)
            .with_context(|| format!("Failed to write {}", local_path.display()))
    })
}

// Schematic sync is bounded to the selected file's directory, never follows
// links, and mirrors only KiCad source/settings files. Board sync stays flat.
fn remote_sync_files(
    client: &SandboxClient,
    sandbox_id: &str,
    root: &str,
    schematic: bool,
) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    let mut entries_seen = 0;
    while let Some(dir) = pending.pop() {
        for entry in client
            .list(sandbox_id, &remote_sync_path(root, &dir))?
            .entries
        {
            entries_seen += 1;
            let relative = dir.join(&entry.name);
            ensure!(
                safe_sync_component(&entry.name),
                "Unsafe sandbox entry name: {}",
                entry.name
            );
            ensure!(
                entry.path == remote_sync_path(root, &relative),
                "Sandbox entry escaped its directory: {}",
                entry.path
            );
            check_sync_bounds(&relative, entries_seen)?;
            if skip_sync_entry(&entry.name, schematic) {
                continue;
            }
            match entry.kind.as_str() {
                "directory" if schematic => pending.push(relative),
                "file" if sync_file_name(&entry.name, schematic) => {
                    files.insert(relative);
                }
                "file" | "directory" => {}
                // sandboxd labels symlinks and other non-regular entries "other".
                // Refuse rather than letting a later upload write through one.
                _ if schematic => bail!("Cannot sync non-regular sandbox entry: {}", entry.path),
                _ => {}
            }
        }
    }
    Ok(files)
}

fn local_sync_files(root: &Path, schematic: bool) -> Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    let entries = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .follow_links(false)
        .max_depth(Some(if schematic { MAX_SYNC_DEPTH + 1 } else { 1 }))
        .filter_entry(move |entry| {
            entry.depth() == 0 || !skip_sync_entry(&entry.file_name().to_string_lossy(), schematic)
        })
        .build();
    for (index, entry) in entries.enumerate() {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let name = entry
            .file_name()
            .to_str()
            .context("KiCad sync filename is not UTF-8")?;
        ensure!(
            safe_sync_component(name),
            "Unsafe local sync filename: {name}"
        );
        let relative = entry.path().strip_prefix(root)?;
        check_sync_bounds(relative, index)?;
        let kind = entry.file_type().context("Missing sync file type")?;
        ensure!(
            !schematic || kind.is_file() || kind.is_dir(),
            "Cannot sync non-regular local entry: {}",
            entry.path().display()
        );
        if kind.is_file() && sync_file_name(name, schematic) {
            files.insert(relative.to_path_buf());
        }
    }
    Ok(files)
}

fn remote_sync_path(root: &str, relative: &Path) -> String {
    if relative.as_os_str().is_empty() {
        return root.to_string();
    }
    format!(
        "{}/{}",
        root.trim_end_matches('/'),
        relative.to_string_lossy().replace('\\', "/")
    )
}

fn safe_sync_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':', '\0'])
}

fn check_sync_bounds(path: &Path, entries: usize) -> Result<()> {
    ensure!(
        path.components().count() <= MAX_SYNC_DEPTH && entries <= MAX_SYNC_ENTRIES,
        "KiCad sync exceeds the {MAX_SYNC_DEPTH}-level / {MAX_SYNC_ENTRIES}-entry project limit"
    );
    Ok(())
}

fn skip_sync_entry(name: &str, schematic: bool) -> bool {
    should_skip_sync_name(name)
        || (schematic && (name.starts_with('.') || name.ends_with("-backups")))
}

fn sync_file_name(name: &str, schematic: bool) -> bool {
    !schematic
        || matches!(name, "sym-lib-table" | "fp-lib-table")
        || matches!(
            Path::new(name).extension().and_then(|ext| ext.to_str()),
            Some("kicad_sch" | "kicad_pro" | "kicad_prl" | "kicad_sym" | "kicad_mod" | "kicad_wks")
        )
}

fn sync_layout_up(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    local: &LocalLayout,
) -> Result<SyncStats> {
    let schematic = is_kicad_sch_path(&local.pcb_file);
    let files = local_sync_files(&local.local_layout_dir, schematic)?;
    // Finish both inventories before any mutation: traversal failures must not
    // turn a partial listing into deletions, or write through remote symlinks.
    let remote_files =
        remote_sync_files(client, &uri.sandbox_id, &local.remote_layout_dir, schematic)?;

    files.par_iter().try_for_each(|relative| -> Result<()> {
        let local_path = local.local_layout_dir.join(relative);
        let bytes = fs::read(&local_path).with_context(|| {
            format!("Failed to read local layout file {}", local_path.display())
        })?;
        client.write_file(
            &uri.sandbox_id,
            &remote_sync_path(&local.remote_layout_dir, relative),
            &bytes,
        )
    })?;

    let stale_remote: Vec<_> = remote_files.difference(&files).collect();

    stale_remote.par_iter().try_for_each(|relative| {
        client.remove(
            &uri.sandbox_id,
            &remote_sync_path(&local.remote_layout_dir, relative),
        )
    })?;

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
                editor_pid: None,
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

    fn mark_active(&mut self, editor_pid: u32) -> Result<()> {
        self.manifest.state = SyncSessionState::Active;
        self.manifest.stop_reason = None;
        self.manifest.editor_pid = Some(editor_pid);
        self.manifest.updated_at = Utc::now();
        self.save()
    }

    fn mark_complete(&mut self) -> Result<()> {
        self.manifest.state = SyncSessionState::Complete;
        self.manifest.stop_reason = None;
        self.manifest.editor_pid = None;
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

fn latest_recoverable_session(
    cache_root: &Path,
    target: &Path,
) -> Result<Option<RecoverableSession>> {
    latest_recoverable_session_with(cache_root, target, editor_process_is_running)
}

fn latest_recoverable_session_with(
    cache_root: &Path,
    target: &Path,
    editor_is_running: impl Fn(u32, &Path) -> bool,
) -> Result<Option<RecoverableSession>> {
    if !cache_root.exists() {
        return Ok(None);
    }
    let mut latest: Option<RecoverableSession> = None;
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
        if !session_matches_target(&session.manifest, target) {
            continue;
        }
        let live_editor_pid = if matches!(
            session.manifest.state,
            SyncSessionState::Active | SyncSessionState::Recoverable
        ) {
            session
                .manifest
                .editor_pid
                .filter(|pid| editor_is_running(*pid, &session.manifest.layout_file))
        } else {
            None
        };
        let recovery = match live_editor_pid {
            Some(pid) => RecoverableSession::EditorOpen { session, pid },
            None if is_recovery_candidate(&session.manifest) => RecoverableSession::Ready(session),
            None => continue,
        };
        if latest
            .as_ref()
            .is_none_or(|current| recovery.updated_at() > current.updated_at())
        {
            latest = Some(recovery);
        }
    }
    Ok(latest)
}

fn prompt_resume_existing_editor(status: &pcb_ui::Spinner, session: &SyncSession) -> Result<bool> {
    let board = session
        .manifest
        .layout_file
        .file_name()
        .unwrap_or(session.manifest.layout_file.as_os_str())
        .to_string_lossy();
    let message = format!(
        "{board} is already open in KiCad, but remote sync stopped.\n\n\
         Resume syncing the open local design to the sandbox?"
    );
    if std::env::var_os(URL_LAUNCHER_ENV).is_some() {
        return status.suspend(|| {
            pcb_native_dialog::ActionDialog {
                title: "Resume KiCad sync?",
                message: &message,
                action_label: "Resume Sync",
            }
            .show()
        });
    }
    if !crate::tty::is_interactive() {
        eprintln!(
            "KiCad is still open for {}. Re-run interactively to resume sync.",
            session.manifest.layout_file.display()
        );
        return Ok(false);
    }
    status.suspend(|| {
        Ok(Confirm::new(&message)
            .with_default(true)
            .prompt()
            .unwrap_or(false))
    })
}

fn prompt_restore_recovery(
    status: &pcb_ui::Spinner,
    session: &SyncSession,
) -> Result<Option<RecoveryChoice>> {
    if !crate::tty::is_interactive() {
        if std::env::var_os(URL_LAUNCHER_ENV).is_some() {
            return status.suspend(|| {
                crate::recovery_dialog::choose(&session.manifest.layout_file).map(Some)
            });
        }
        eprintln!(
            "Found local recovery file for this remote design at {}. Re-run interactively to restore it.",
            session.manifest.layout_file.display()
        );
        return Ok(None);
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
    Ok(Some(if status.suspend(ask) {
        RecoveryChoice::Restore
    } else {
        RecoveryChoice::Discard
    }))
}

fn mark_recoverable_sessions_prompt_seen(cache_root: &Path, target: &Path) {
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
        if session_matches_target(&session.manifest, target)
            && is_recovery_candidate(&session.manifest)
            && let Err(err) = session.mark_prompt_seen()
        {
            log::warn!(
                "Failed to mark local recovery prompt seen at {}: {err:#}",
                session.manifest_path.display()
            );
        }
    }
}

fn session_matches_target(manifest: &SyncSessionManifest, target: &Path) -> bool {
    manifest
        .layout_file
        .strip_prefix(&manifest.local_layout_dir)
        .ok()
        == Some(target)
}

fn is_recovery_candidate(manifest: &SyncSessionManifest) -> bool {
    matches!(
        manifest.state,
        SyncSessionState::Active | SyncSessionState::Recoverable
    ) && !manifest.prompt_seen
        && manifest.layout_file.is_file()
}

/// Best-effort check that `pid` is still the editor process this session
/// launched. Check both editor kind and exact target, including the dedicated
/// `open -n -W` launcher on macOS, rather than accepting any KiCad process.
#[cfg(unix)]
fn editor_process_is_running(pid: u32, file: &Path) -> bool {
    let Ok(name) = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let Ok(args) = std::process::Command::new("ps")
        .args(["-ww", "-o", "args=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    name.status.success()
        && args.status.success()
        && is_editor_process(
            &String::from_utf8_lossy(&name.stdout),
            &String::from_utf8_lossy(&args.stdout),
            file,
        )
}

#[cfg(windows)]
fn editor_process_is_running(pid: u32, file: &Path) -> bool {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("powershell.exe");
    command.creation_flags(CREATE_NO_WINDOW).args([
        "-NoProfile", "-NonInteractive", "-Command",
        &format!("[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); Get-CimInstance Win32_Process -Filter 'ProcessId = {pid}' | Select-Object Name, CommandLine | ConvertTo-Json -Compress"),
    ]);
    command.output().is_ok_and(|output| {
        output.status.success()
            && serde_json::from_slice::<serde_json::Value>(&output.stdout).is_ok_and(|process| {
                is_editor_process(
                    process["Name"].as_str().unwrap_or_default(),
                    process["CommandLine"].as_str().unwrap_or_default(),
                    file,
                )
            })
    })
}

#[cfg(not(any(unix, windows)))]
fn editor_process_is_running(_pid: u32, _file: &Path) -> bool {
    false
}

#[cfg(any(unix, windows))]
fn is_editor_process(raw: &str, command: &str, file: &Path) -> bool {
    let name = raw.trim().trim_matches('"');
    let name = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    let editor = if is_kicad_sch_path(file) {
        "eeschema"
    } else {
        "pcbnew"
    };
    let right_editor = name == editor
        || name == format!("{editor}.exe")
        || (name == "open" && command.contains(&format!("/{editor}.app")));
    right_editor
        && command
            .trim()
            .split_once(file.to_string_lossy().as_ref())
            .is_some_and(|(before, after)| {
                (before.ends_with(' ') || before.ends_with('"'))
                    && (after.is_empty() || after.starts_with(' ') || after.starts_with('"'))
            })
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
        // Incomplete sessions can contain newer nested sheets or an editor
        // whose lock lives below the root. Never age out unsynced recovery.
        if let Ok(session) = SyncSession::load(path.clone())
            && (is_recovery_candidate(&session.manifest)
                || session.manifest.editor_pid.is_some_and(|pid| {
                    editor_process_is_running(pid, &session.manifest.layout_file)
                }))
        {
            continue;
        }
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
    let relative = path
        .strip_prefix(base)
        .with_context(|| format!("{remote_path} is not inside {remote_base}"))?;
    ensure!(!relative.as_os_str().is_empty() && relative.components().all(|part| matches!(part, Component::Normal(name) if name.to_str().is_some_and(safe_sync_component))),
        "Unsafe relative KiCad path: {}", relative.display());
    Ok(relative.to_path_buf())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session_manifest(
        cache_root: &Path,
        state: SyncSessionState,
        editor_pid: Option<u32>,
        prompt_seen: bool,
    ) {
        let local_layout_dir = cache_root.join("session");
        fs::create_dir(&local_layout_dir).unwrap();
        let layout_file = local_layout_dir.join("layout.kicad_pcb");
        fs::write(&layout_file, "").unwrap();
        let uri = SandboxFileUri::parse(
            "diode://api.diode.computer/sandboxes/test/fs/read?path=/layout.kicad_pcb",
        )
        .unwrap();
        let mut session = SyncSession::create(&uri, "/", local_layout_dir, layout_file).unwrap();
        session.manifest.state = state;
        session.manifest.editor_pid = editor_pid;
        session.manifest.prompt_seen = prompt_seen;
        session.save().unwrap();
    }

    #[test]
    fn finds_restored_open_editor_session_for_sync_resume() {
        let cache = tempfile::tempdir().unwrap();
        let editor_pid = 42;
        write_session_manifest(
            cache.path(),
            SyncSessionState::Active,
            Some(editor_pid),
            true,
        );

        let recovery = latest_recoverable_session_with(
            cache.path(),
            Path::new("layout.kicad_pcb"),
            |pid, _| pid == editor_pid,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            recovery,
            RecoverableSession::EditorOpen { pid, .. } if pid == editor_pid
        ));
    }

    #[test]
    fn recycled_editor_pid_does_not_block_recovery() {
        let cache = tempfile::tempdir().unwrap();
        // A live pid that is not a KiCad editor process: pid reuse must not
        // permanently block the board.
        write_session_manifest(
            cache.path(),
            SyncSessionState::Recoverable,
            Some(std::process::id()),
            false,
        );

        let recovery =
            latest_recoverable_session_with(cache.path(), Path::new("layout.kicad_pcb"), |_, _| {
                false
            })
            .unwrap()
            .unwrap();
        assert!(matches!(recovery, RecoverableSession::Ready(_)));
    }

    #[test]
    fn matches_editor_kind_and_exact_target() {
        let sch = Path::new("/root.kicad_sch");
        let pcb = Path::new("/root.kicad_pcb");
        for (name, args, target, expected) in [
            ("eeschema", "eeschema /root.kicad_sch", sch, true),
            ("pcbnew.exe", "pcbnew.exe /root.kicad_pcb", pcb, true),
            (
                "/usr/bin/open\n",
                "open -n -W -a /eeschema.app /root.kicad_sch",
                sch,
                true,
            ),
            (
                "open",
                "open -n -W -a /pcbnew.app /root.kicad_sch",
                sch,
                false,
            ),
            ("pcbnew", "pcbnew /root.kicad_sch", sch, false),
            ("eeschema", "eeschema /other.kicad_sch", sch, false),
            ("eeschema", "eeschema /root.kicad_sch.other", sch, false),
        ] {
            assert_eq!(is_editor_process(name, args, target), expected, "{args}");
        }
    }

    #[test]
    fn recovery_and_discard_are_target_specific_in_a_shared_directory() {
        let cache = tempfile::tempdir().unwrap();
        write_session_manifest(cache.path(), SyncSessionState::Recoverable, Some(42), false);
        for target in ["layout.kicad_sch", "other.kicad_pcb"] {
            assert!(
                latest_recoverable_session_with(cache.path(), Path::new(target), |_, _| true)
                    .unwrap()
                    .is_none()
            );
            mark_recoverable_sessions_prompt_seen(cache.path(), Path::new(target));
            assert!(
                !SyncSession::load(cache.path().join("session"))
                    .unwrap()
                    .manifest
                    .prompt_seen
            );
        }
        mark_recoverable_sessions_prompt_seen(cache.path(), Path::new("layout.kicad_pcb"));
        assert!(
            SyncSession::load(cache.path().join("session"))
                .unwrap()
                .manifest
                .prompt_seen
        );
    }

    #[test]
    fn nested_inventory_is_filtered_bounded_and_does_not_follow_links() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "root.kicad_sch",
            "root.kicad_pro",
            "sheets/a.kicad_sch",
            "sheets/deep/b.kicad_sch",
            "sheets/_autosave-a.kicad_sch",
            "sheets/a.kicad_sch.tmp",
            "sheets/a.kicad_sch.lck",
            "sheets/notes.txt",
            "root-backups/old.kicad_sch",
            ".git/hidden.kicad_sch",
        ] {
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "test").unwrap();
        }
        assert_eq!(
            local_sync_files(root.path(), true).unwrap(),
            [
                "root.kicad_sch",
                "root.kicad_pro",
                "sheets/a.kicad_sch",
                "sheets/deep/b.kicad_sch"
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect()
        );
        assert_eq!(local_sync_files(root.path(), false).unwrap().len(), 2);
        let at_limit: PathBuf = std::iter::repeat_n("d", MAX_SYNC_DEPTH).collect();
        assert!(check_sync_bounds(&at_limit, MAX_SYNC_ENTRIES).is_ok());
        assert!(check_sync_bounds(&at_limit.join("d"), 1).is_err());
        assert!(check_sync_bounds(Path::new("a"), MAX_SYNC_ENTRIES + 1).is_err());
        // The walker must fail on a missing root or excess depth, not return a
        // partial inventory that could delete remote files.
        assert!(local_sync_files(&root.path().join("missing"), true).is_err());
        fs::create_dir_all(root.path().join(&at_limit)).unwrap();
        assert!(local_sync_files(root.path(), true).is_ok());
        fs::write(root.path().join(&at_limit).join("deep.kicad_sch"), "").unwrap();
        assert!(local_sync_files(root.path(), true).is_err());
        fs::remove_dir_all(root.path().join("d")).unwrap();
        assert!(remote_relative_path("/layout", "/layout/../other.kicad_sch").is_err());
        assert!(!safe_sync_component("../outside.kicad_sch"));
        assert!(!safe_sync_component("C:\\outside.kicad_sch"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("sheets"), root.path().join("link"))
                .unwrap();
            assert!(local_sync_files(root.path(), true).is_err());
        }
    }
}
