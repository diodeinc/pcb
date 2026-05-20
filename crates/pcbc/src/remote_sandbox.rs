use anyhow::{Context, Result, bail};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pcb_diode_api::{
    ExecSyncRequest, SandboxClient, SandboxFileUri, SandboxLockGuard, SandboxLockOptions,
};
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError},
};
use std::thread;
use std::time::Duration;
use uuid::Uuid;

use crate::layout::{LayoutArgs, LayoutOutputFormat};
use crate::open::OpenArgs;

const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
const REMOTE_LAYOUT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

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
    let local = sync_layout_down(&client, &uri, &result)?;
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
        sync_remote_pcb_file_down(&client, &uri)?
    } else {
        let layout_args = LayoutArgs {
            file: PathBuf::from(&uri.sandbox_path),
            config: Vec::new(),
            no_open: true,
            offline: args.offline,
            temp: false,
            check: false,
            suppress: Vec::new(),
            locked: args.locked,
            no_sync: true,
            format: LayoutOutputFormat::Human,
        };
        status.set_message("Running pcb layout in sandbox...");
        let result = run_remote_layout(&client, &uri, &layout_args)?;
        status.set_message("Downloading layout from sandbox...");
        sync_layout_down(&client, &uri, &result)?
    };

    open_layout_and_sync(&client, &uri, &local, lock, status)
}

struct LocalLayout {
    remote_layout_dir: String,
    local_layout_dir: PathBuf,
    pcb_file: PathBuf,
}

#[derive(Default)]
struct SyncStats {
    uploaded: usize,
    removed: usize,
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
    let running = install_shutdown_flag()?;
    status.set_message(format!("Opening {}...", local.pcb_file.display()));
    let mut session = pcb_kicad::open_pcbnew_session(&local.pcb_file)?;
    let watcher = LocalLayoutWatcher::new(&local.local_layout_dir)?;
    status.set_message(format!(
        "Watching {} for KiCad changes...",
        local.local_layout_dir.display()
    ));

    loop {
        if session.try_wait()?.is_some() || !running.load(Ordering::SeqCst) {
            break;
        }
        if !lock.is_active() {
            bail!(
                "Sandbox lock was reclaimed while KiCad was open; stopped syncing remote changes"
            );
        }
        if watcher.changed_with_timeout(WATCH_POLL_INTERVAL)? {
            status.set_message("Syncing local changes to sandbox...");
            let stats = sync_layout_up(client, uri, local)?;
            status.set_message(format!(
                "Synced {} uploaded, {} removed. Watching for more changes...",
                stats.uploaded, stats.removed
            ));
        }
    }

    if lock.is_active() {
        status.set_message("Final sync to sandbox...");
        let stats = sync_layout_up(client, uri, local)?;
        status.success(format!(
            "Final sync complete ({} uploaded, {} removed)",
            stats.uploaded, stats.removed
        ));
    } else {
        status.finish();
    }
    lock.release()?;
    Ok(())
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
    if args.locked {
        command.push("--locked".to_string());
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

fn sync_remote_pcb_file_down(client: &SandboxClient, uri: &SandboxFileUri) -> Result<LocalLayout> {
    let result = RemoteLayoutResult {
        layout_dir: Some(remote_parent_dir(&uri.sandbox_path)?),
        pcb_file: Some(uri.sandbox_path.clone()),
    };
    sync_layout_down(client, uri, &result)
}

fn sync_layout_down(
    client: &SandboxClient,
    uri: &SandboxFileUri,
    result: &RemoteLayoutResult,
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
    let local_layout_dir = local_layout_cache_dir(uri, &remote_layout_dir)?;
    if local_layout_dir.exists() {
        fs::remove_dir_all(&local_layout_dir).with_context(|| {
            format!(
                "Failed to remove local layout cache {}",
                local_layout_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&local_layout_dir)?;
    sync_remote_dir_down(
        client,
        &uri.sandbox_id,
        &remote_layout_dir,
        &local_layout_dir,
    )?;

    let relative_pcb = remote_relative_path(&remote_layout_dir, &remote_pcb_file)?;
    let pcb_file = local_layout_dir.join(relative_pcb);
    Ok(LocalLayout {
        remote_layout_dir,
        local_layout_dir,
        pcb_file,
    })
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

fn local_layout_cache_dir(uri: &SandboxFileUri, remote_layout_dir: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Failed to find home directory")?;
    let key = format!("{}:{}:{}", uri.host, uri.sandbox_id, remote_layout_dir);
    let id = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes());
    Ok(home
        .join(".pcb")
        .join("sandbox-layouts")
        .join(sanitize_path_component(&uri.host))
        .join(sanitize_path_component(&uri.sandbox_id))
        .join(id.to_string()))
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
