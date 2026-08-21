//! Local auto-routing engine backed by FreeRouting (`pcb route --engine
//! freerouting`). KiCad board -> Specctra DSN -> FreeRouting -> SES -> board.
//!
//! Uses FreeRouting's REST API server mode (`self::api`), not its one-shot
//! CLI mode: an upstream bug leaves CLI mode unable to recover partial
//! output on timeout/interrupt, which the API's `GET /jobs/{id}/output`
//! supports.

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use colored::Colorize;
use pcb_kicad::PythonScriptBuilder;
use pcb_ui::prelude::*;

use crate::route::{RouteArgs, format_duration, import_ses};

mod api;
use api::{DEFAULT_TIMEOUT, FreeroutingApiClient, GET_OUTPUT_TIMEOUT, JobOutput, JobState};

const FREEROUTING_VERSION: &str = "2.3.0";

const FREEROUTING_REPO: &str = "freerouting/freerouting";

/// SHA-256 of the `freerouting-{FREEROUTING_VERSION}.jar` release artifact.
const FREEROUTING_JAR_SHA256: &str =
    "3cf18d608437740bc497db6b8ef5888e2e60a08de0def20691d1bad0c0e0ee24";

const FREEROUTING_MAX_PASSES: u32 = 200;

/// Margin added to our poll deadline so FreeRouting's own job_timeout only
/// fires as a backstop. Must exceed `GET_OUTPUT_TIMEOUT`, or FreeRouting's
/// timeout can fire mid-fetch and refuse output.
const JOB_TIMEOUT_SAFETY_MARGIN_SECS: u64 = 150;

/// How often `poll_job` refreshes its salvage cache of in-progress output.
const OUTPUT_CACHE_INTERVAL: Duration = Duration::from_secs(5);

fn freerouting_jar_filename() -> String {
    format!("freerouting-{FREEROUTING_VERSION}.jar")
}

fn freerouting_jar_url() -> String {
    format!(
        "https://github.com/{FREEROUTING_REPO}/releases/download/v{FREEROUTING_VERSION}/freerouting-{FREEROUTING_VERSION}.jar"
    )
}

/// Set by the Ctrl+C handler; only a request — `run_freerouting` owns the
/// `Child` and does the actual killing.
static CANCEL: AtomicBool = AtomicBool::new(false);

/// Set on a second Ctrl+C: skip fetching partial output and kill Java now.
static FORCE_KILL: AtomicBool = AtomicBool::new(false);

/// FreeRouting's pid, so the Ctrl+C handler can kill it directly.
static CHILD_PID: AtomicU32 = AtomicU32::new(0);

pub fn execute(
    args: &RouteArgs,
    board_path: &Path,
    project_path: &Path,
    board_name: &str,
) -> Result<()> {
    // Check Java before the auto-download fallback pays for a big download.
    let expected_hash = hex_decode32(FREEROUTING_JAR_SHA256);
    let explicit_jar = resolve_explicit_freerouting_jar(&expected_hash)?;

    let java_path = resolve_java()?;

    let fr_jar = match explicit_jar {
        Some(path) => path,
        None => find_or_download_freerouting_jar(&expected_hash)?,
    };

    println!(
        "Routing {} via FreeRouting",
        board_path.file_name().unwrap().to_string_lossy().green()
    );
    println!("  JAR: {}", fr_jar.display());

    let work_dir = tempfile::tempdir().context("Failed to create temp working directory")?;
    let work_board = work_dir.path().join(format!("{board_name}.kicad_pcb"));
    std::fs::copy(board_path, &work_board).context("Failed to stage board copy")?;
    // Design rules/net classes live in the project file (KiCad 6+), not the
    // board file, so DSN export and zone fill need it staged too.
    if project_path.exists() {
        std::fs::copy(
            project_path,
            work_dir.path().join(format!("{board_name}.kicad_pro")),
        )
        .context("Failed to stage project file copy")?;
        // Custom design rules live in a sibling .kicad_dru, not the project
        // file itself.
        let dru_path = project_path.with_extension("kicad_dru");
        if dru_path.exists() {
            std::fs::copy(
                &dru_path,
                work_dir.path().join(format!("{board_name}.kicad_dru")),
            )
            .context("Failed to stage design rules file copy")?;
        }
    }
    let dsn_path = work_dir.path().join(format!("{board_name}.dsn"));
    let ses_path = work_dir.path().join(format!("{board_name}.ses"));

    CANCEL.store(false, Ordering::SeqCst);
    FORCE_KILL.store(false, Ordering::SeqCst);
    CHILD_PID.store(0, Ordering::SeqCst);
    if let Err(e) = ctrlc::set_handler(|| {
        if CANCEL.swap(true, Ordering::SeqCst) {
            eprintln!("\n  Force-stopping FreeRouting...");
            FORCE_KILL.store(true, Ordering::SeqCst);
            kill_child_now();
        } else {
            eprintln!("\n  Stopping FreeRouting and fetching the best result so far...");
        }
    }) {
        eprintln!("{} Could not set Ctrl+C handler: {}", "!".yellow(), e);
    }

    let spinner = Spinner::builder("Exporting DSN...").start();
    export_dsn(&work_board, &dsn_path)?;
    spinner.finish();

    // Otherwise CANCEL is only checked inside run_freerouting's poll loop, so
    // Ctrl+C during export would print "Stopping..." then start anyway.
    if CANCEL.load(Ordering::SeqCst) {
        println!("  No routing progress to save. Board left untouched.");
        return Ok(());
    }

    let start_time = Instant::now();
    let outcome = run_freerouting(
        &java_path,
        &fr_jar,
        &dsn_path,
        &ses_path,
        args.timeout as u64 * 60,
    )?;

    if !ses_path.exists() {
        println!("  No routing progress to save. Board left untouched.");
        return Ok(());
    }

    let spinner = Spinner::builder("Importing SES...").start();
    import_ses(&work_board, &ses_path)?;
    spinner.finish();

    // Only now replace the original board, atomically.
    publish_board(&work_board, board_path)?;

    let elapsed = start_time.elapsed();
    println!("  Time:       {}", format_duration(elapsed));
    println!();
    match outcome {
        RunOutcome::Cancelled => println!(
            "Partial result saved to {}",
            board_path.display().to_string().cyan()
        ),
        RunOutcome::Completed => println!(
            "Result saved to {}",
            board_path.display().to_string().cyan()
        ),
        RunOutcome::Terminated => println!(
            "Partial result saved to {} (FreeRouting terminated unexpectedly)",
            board_path.display().to_string().cyan()
        ),
    }

    if !args.no_open {
        let _ = pcb_kicad::open_pcbnew(board_path);
    }

    if outcome == RunOutcome::Terminated {
        anyhow::bail!("FreeRouting terminated unexpectedly; see the log path printed above");
    }

    Ok(())
}

fn publish_board(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Cross-filesystem fallback: copy via a uniquely-named temp file, then
    // publish atomically.
    let dst_dir = dst
        .parent()
        .context("Expected board path to have a parent directory")?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".pcb.route.")
        .suffix(".kicad_pcb")
        .tempfile_in(dst_dir)
        .with_context(|| format!("Failed to create temp file in {}", dst_dir.display()))?;
    let mut src_file =
        std::fs::File::open(src).context("Failed to open routed board for publish")?;
    std::io::copy(&mut src_file, tmp.as_file_mut())
        .context("Failed to stage routed board for publish")?;
    // `tempfile` creates the staging file owner-only (0o600); match the
    // source board's mode so publishing doesn't silently tighten permissions.
    let src_perms = src_file
        .metadata()
        .context("Failed to read source board metadata")?
        .permissions();
    tmp.as_file()
        .set_permissions(src_perms)
        .context("Failed to set published board permissions")?;
    tmp.persist(dst)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| format!("Failed to publish {}", dst.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Java resolution
// ---------------------------------------------------------------------------

/// The pinned jar's `build.gradle` targets this; keep in sync with
/// `FREEROUTING_VERSION`.
const REQUIRED_JAVA_VERSION: u32 = 25;

/// No auto-download of a JRE/JDK — too heavy for a CLI tool; ask the user.
fn resolve_java() -> Result<PathBuf> {
    if is_java_version_sufficient("java") {
        return Ok(PathBuf::from("java"));
    }

    anyhow::bail!(
        "Java {v}+ not found on $PATH. FreeRouting requires a Java {v}+ runtime.\n\
         Install one and try again:\n\
         macOS:   brew install openjdk@{v}\n\
         Linux:   apt install openjdk-{v}-jdk  (or your distro's equivalent)\n\
         Windows: https://adoptium.net/temurin/releases/?version={v}\n\
         Then ensure 'java -version' shows version {v} or later.",
        v = REQUIRED_JAVA_VERSION
    );
}

fn is_java_version_sufficient(java_path: impl AsRef<Path>) -> bool {
    let output = match Command::new(java_path.as_ref()).arg("-version").output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let version_str = match stderr.lines().find(|l| l.contains("version")) {
        Some(s) => s,
        None => return false,
    };
    let major = version_str
        .split('"')
        .nth(1)
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    major >= REQUIRED_JAVA_VERSION
}

// ---------------------------------------------------------------------------
// FreeRouting JAR resolution
// ---------------------------------------------------------------------------

/// `FREEROUTING_JAR` env var. `Ok(None)` means it's not set — caller falls
/// back to the verified cache.
fn resolve_explicit_freerouting_jar(expected_hash: &[u8; 32]) -> Result<Option<PathBuf>> {
    if let Ok(path) = std::env::var("FREEROUTING_JAR") {
        let p = PathBuf::from(&path);
        if p.exists() {
            warn_on_hash_mismatch(&p, expected_hash);
            return Ok(Some(p));
        }
        anyhow::bail!("FreeRouting JAR not found at FREEROUTING_JAR={}", path);
    }

    Ok(None)
}

/// Downloads to (and caches in) `~/.cache/pcb/freerouting/`, verified by SHA-256.
fn find_or_download_freerouting_jar(expected_hash: &[u8; 32]) -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .context(
            "Could not determine a cache directory (check $HOME).\n\
             Set FREEROUTING_JAR to point at a JAR directly.",
        )?
        .join("pcb")
        .join("freerouting");
    let jar_filename = freerouting_jar_filename();
    let cached = cache_dir.join(&jar_filename);

    std::fs::create_dir_all(&cache_dir).context("Failed to create FreeRouting cache dir")?;
    if !restrict_dir_to_owner(&cache_dir) {
        anyhow::bail!(
            "Refusing to use FreeRouting cache dir {} — it's owned by another user.\n\
             This can happen when the system cache/temp dir is shared between users.\n\
             Remove the directory (if you control it) or set FREEROUTING_JAR \
             to point at a JAR directly.",
            cache_dir.display()
        );
    }

    if cached.exists() {
        match sha256_file(&cached) {
            Ok(hash) if hash == *expected_hash => return Ok(cached),
            _ => {
                eprintln!("  Cached JAR is corrupted, re-downloading...");
                let _ = std::fs::remove_file(&cached);
            }
        }
    }

    // Per-process suffix so concurrent `pcb route` invocations don't race the
    // same `.tmp`/`.part` file.
    let tmp_path = cache_dir.join(format!("{jar_filename}.{}.tmp", std::process::id()));
    download_to_file(&freerouting_jar_url(), &tmp_path)?;

    let actual_hash =
        sha256_file(&tmp_path).context("Failed to hash downloaded FreeRouting JAR")?;
    if actual_hash != *expected_hash {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "Downloaded FreeRouting JAR has unexpected SHA-256\n\
             Expected: {}\n\
             Actual:   {}\n\
             The download may have been tampered with or the release has changed.\n\
             Download manually from: https://github.com/{FREEROUTING_REPO}/releases/tag/v{FREEROUTING_VERSION}",
            FREEROUTING_JAR_SHA256,
            hex::encode(actual_hash),
        );
    }

    std::fs::rename(&tmp_path, &cached).context("Failed to move FreeRouting JAR to cache")?;
    println!("  Downloaded to {}", cached.display());

    Ok(cached)
}

/// Chmod `dir` to owner-only, returning `false` (don't reuse it) if it's
/// owned by someone else. No-op / always `true` on non-unix.
#[cfg(unix)]
fn restrict_dir_to_owner(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::metadata(dir) else {
        return false;
    };
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc_geteuid() };
    if meta.uid() != euid {
        return false;
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    true
}

#[cfg(not(unix))]
fn restrict_dir_to_owner(_dir: &Path) -> bool {
    true
}

#[cfg(unix)]
unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(unix)]
unsafe fn libc_geteuid() -> u32 {
    unsafe { geteuid() }
}

// ---------------------------------------------------------------------------
// DSN/SES pipeline
// ---------------------------------------------------------------------------

fn export_dsn(board_path: &Path, dsn_path: &Path) -> Result<()> {
    let script = r#"
import pcbnew
import sys

brd_filename = sys.argv[1]
dsn_filename = sys.argv[2]
brd = pcbnew.LoadBoard(brd_filename)
pcbnew.ExportSpecctraDSN(brd, dsn_filename)
"#;

    PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .arg(dsn_path.to_string_lossy())
        .run()
        .context("Failed to export DSN file from KiCad")?;

    if !dsn_path.exists() {
        anyhow::bail!(
            "DSN export completed but output file not found at {}",
            dsn_path.display()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// FreeRouting process management
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RunOutcome {
    Completed,
    Cancelled,
    Terminated,
}

/// Owns the `Child`; `Drop` kills it on every exit path (success, failure,
/// timeout, Ctrl+C) instead of relying on each return site to do it.
struct FreeroutingServer {
    child: Child,
    base_url: String,
    /// stdout+stderr, piped straight to disk instead of buffered in memory.
    log_path: PathBuf,
}

impl FreeroutingServer {
    fn spawn(java_path: &Path, jar_path: &Path) -> Result<Self> {
        let port = pick_free_port()?;

        // Unique, non-predictable name via `tempfile` — a fixed PID-based path in
        // the shared temp dir could be pre-staked (e.g. as a symlink) by another
        // local user.
        let (log_file, log_path) = tempfile::Builder::new()
            .prefix("pcb-freerouting-")
            .suffix(".log")
            .tempfile()
            .context("Failed to create FreeRouting log file")?
            .keep()
            .context("Failed to persist FreeRouting log file")?;
        let (log_stdout, log_stderr) = pcb_command_runner::log_file_stdio(&log_file)?;

        let mut command = Command::new(java_path);
        command
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(jar_path)
            .arg("--gui.enabled=false")
            .arg("--api_server.enabled=true")
            .arg("--api_server.authentication.enabled=false")
            .arg(format!("--api_server-endpoints=http://127.0.0.1:{port}"))
            .arg("--usage_and_diagnostic_data.disable_analytics=true")
            .stdout(log_stdout)
            .stderr(log_stderr);
        // Otherwise Java shares pcb's foreground process group and gets the
        // terminal's Ctrl+C SIGINT directly, exiting before we fetch output.
        detach_process_group(&mut command);

        let child = command
            .spawn()
            .context("Failed to launch FreeRouting API server (java -jar)")?;

        CHILD_PID.store(child.id(), Ordering::SeqCst);

        Ok(Self {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            log_path,
        })
    }

    fn still_alive(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    /// `path (status)` for error messages — the log itself isn't inlined.
    fn log_summary(&mut self) -> String {
        match self.child.try_wait().ok().flatten() {
            Some(status) => format!("log: {} ({status})", self.log_path.display()),
            None => format!("log: {}", self.log_path.display()),
        }
    }
}

impl Drop for FreeroutingServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        CHILD_PID.store(0, Ordering::SeqCst);
    }
}

/// Puts `command`'s child in its own process group so it doesn't receive
/// signals sent to pcb's foreground process group.
#[cfg(unix)]
fn detach_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn detach_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
fn detach_process_group(_command: &mut Command) {}

/// Called from the Ctrl+C handler on a second press.
#[cfg(unix)]
fn kill_child_now() {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid != 0 {
        // SAFETY: kill(2) with a plain pid and signal cannot fail in a way
        // that matters here.
        unsafe {
            libc_kill(pid as i32, 9);
        }
    }
}

/// Called from the Ctrl+C handler on a second press. Actually kills the
/// process (not just sets `FORCE_KILL`): a second press can land while we're
/// blocked in an output fetch, past the point where the poll loop checks it.
#[cfg(windows)]
fn kill_child_now() {
    let pid = CHILD_PID.load(Ordering::SeqCst);
    if pid != 0 {
        let _ = Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .output();
    }
}

/// Best-effort only; `FORCE_KILL` still reaches `Child::kill()` via the poll loop.
#[cfg(not(any(unix, windows)))]
fn kill_child_now() {}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    unsafe {
        kill(pid, sig);
    }
}

/// TOCTOU race between release and FreeRouting binding it is accepted (same
/// tradeoff as the OAuth callback server elsewhere in this codebase).
fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind ephemeral port for FreeRouting API server")?;
    Ok(listener.local_addr()?.port())
}

fn run_freerouting(
    java_path: &Path,
    jar_path: &Path,
    dsn_path: &Path,
    ses_path: &Path,
    timeout_secs: u64,
) -> Result<RunOutcome> {
    let spinner = Spinner::builder("Starting FreeRouting...").start();

    let mut server = FreeroutingServer::spawn(java_path, jar_path)?;
    let api = FreeroutingApiClient::new(server.base_url.clone())?;
    if let Err(e) = api.wait_ready(Duration::from_secs(15), || server.still_alive()) {
        spinner.error("Failed to start FreeRouting");
        anyhow::bail!("{e}\n{}", server.log_summary());
    }

    let session_id = api
        .create_session()
        .context("Failed to start FreeRouting session")?;
    let job_name = dsn_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "pcb-route".to_string());
    let job_id = api.enqueue_job(&session_id, &job_name)?;

    // Safety net only — our poll loop fetches output and cancels at
    // timeout_secs; this covers the case where that fails to happen.
    api.update_settings(
        &job_id,
        FREEROUTING_MAX_PASSES,
        &format_hms(timeout_secs.max(1) + JOB_TIMEOUT_SAFETY_MARGIN_SECS),
    )?;

    let dsn_bytes = std::fs::read(dsn_path).context("Failed to read DSN file for upload")?;
    let dsn_filename = dsn_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "board.dsn".to_string());
    api.upload_input(&job_id, &dsn_filename, &dsn_bytes)?;
    api.start_job(&job_id)?;
    spinner.finish();

    let spinner = Spinner::builder("Running FreeRouting...").start();
    let start = Instant::now();
    let poll_result = poll_job(&api, &job_id, timeout_secs, &spinner, &mut server)?;
    let elapsed = start.elapsed().as_secs_f64();
    match (poll_result.outcome, poll_result.output.is_some()) {
        (RunOutcome::Completed, true) => {
            spinner.success(format!("FreeRouting finished in {elapsed:.1}s"));
        }
        (RunOutcome::Completed, false) => {
            spinner.success(format!(
                "FreeRouting finished in {elapsed:.1}s (nothing to route)"
            ));
        }
        (RunOutcome::Cancelled, true) => {
            spinner.warning(format!(
                "FreeRouting stopped after {elapsed:.1}s (partial result)"
            ));
        }
        (RunOutcome::Cancelled, false) => {
            spinner.warning(format!(
                "FreeRouting stopped after {elapsed:.1}s with no routing progress"
            ));
        }
        (RunOutcome::Terminated, _) => {
            spinner.error(format!(
                "FreeRouting terminated unexpectedly after {elapsed:.1}s"
            ));
        }
    }

    if let Some(bytes) = poll_result.output {
        std::fs::write(ses_path, bytes).context("Failed to write FreeRouting SES output")?;
    }
    // else: leave ses_path absent — execute() treats that as "nothing to save".

    if poll_result.outcome == RunOutcome::Terminated {
        eprintln!(
            "  {} FreeRouting terminated unexpectedly; see log for details: {}",
            "!".yellow(),
            server.log_path.display()
        );
    } else {
        // No error occurred, so the log has nothing worth keeping.
        let _ = std::fs::remove_file(&server.log_path);
    }

    Ok(poll_result.outcome)
}

struct PollResult {
    outcome: RunOutcome,
    output: Option<Vec<u8>>,
}

/// FreeRouting's `GET /output` returns partial data while `RUNNING`, but
/// unconditionally reports "no output" once a job settles into `CANCELLED` —
/// so a call made right after `cancel_job` (which only requests a stop) can
/// race the transition and lose real progress. We sidestep this by caching
/// output opportunistically while still running, and falling back to that
/// cache if the post-cancel fetch comes back empty.
fn poll_job(
    api: &FreeroutingApiClient,
    job_id: &str,
    timeout_secs: u64,
    spinner: &Spinner,
    server: &mut FreeroutingServer,
) -> Result<PollResult> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(timeout_secs);
    let mut consecutive_errors = 0u32;
    let mut last_printed_pass: Option<u32> = None;
    let mut cached_output: Option<Vec<u8>> = None;
    let mut last_cache_attempt = start;

    loop {
        for _ in 0..10 {
            if FORCE_KILL.load(Ordering::SeqCst) {
                return Ok(PollResult {
                    outcome: RunOutcome::Cancelled,
                    output: None,
                });
            }
            if CANCEL.load(Ordering::SeqCst) {
                return Ok(stop_and_capture(api, job_id, &cached_output));
            }
            thread::sleep(Duration::from_millis(100));
        }

        if Instant::now() > deadline {
            return Ok(stop_and_capture(api, job_id, &cached_output));
        }

        // Hide the pass counter until it's > 0: FreeRouting's initial routing
        // stage can sit at "pass 0" for a long time, indistinguishable from
        // a stuck counter.
        let elapsed = start.elapsed().as_secs();
        spinner.set_message(match last_printed_pass {
            Some(pass) if pass > 0 => format!(
                "Running FreeRouting... {elapsed}s elapsed, pass {pass}/{FREEROUTING_MAX_PASSES}"
            ),
            _ => format!("Running FreeRouting... {elapsed}s elapsed"),
        });

        match api.get_job(job_id) {
            Ok(status) => {
                consecutive_errors = 0;
                if status.current_pass.is_some() && status.current_pass != last_printed_pass {
                    last_printed_pass = status.current_pass;
                }
                // Opportunistically cache output while still RUNNING, on a fixed
                // interval rather than gated on pass changes: FreeRouting can sit
                // on one pass (e.g. an extended pass 0) for a long time, and once
                // the job settles into a terminal state, `/output` can refuse to
                // return partial data. Gated on RUNNING so it doesn't fire an extra
                // fetch right before a terminal-state fetch below.
                if status.state == JobState::Running
                    && Instant::now().duration_since(last_cache_attempt) >= OUTPUT_CACHE_INTERVAL
                {
                    last_cache_attempt = Instant::now();
                    if let Ok(JobOutput::Data(bytes)) = api.get_output(job_id, DEFAULT_TIMEOUT) {
                        cached_output = Some(bytes);
                    }
                }
                match status.state {
                    JobState::Completed => {
                        return Ok(match api.get_output(job_id, GET_OUTPUT_TIMEOUT) {
                            Ok(JobOutput::Data(bytes)) => PollResult {
                                outcome: RunOutcome::Completed,
                                output: Some(bytes),
                            },
                            Ok(JobOutput::NothingToRoute) => PollResult {
                                outcome: RunOutcome::Completed,
                                output: None,
                            },
                            Err(_) => {
                                eprintln!(
                                    "  {} FreeRouting finished but its final output could not be fetched.",
                                    "!".yellow()
                                );
                                PollResult {
                                    outcome: RunOutcome::Cancelled,
                                    output: cached_output,
                                }
                            }
                        });
                    }
                    JobState::Cancelled | JobState::TimedOut => {
                        return Ok(PollResult {
                            outcome: RunOutcome::Cancelled,
                            output: best_effort_output(api, job_id).or(cached_output),
                        });
                    }
                    JobState::Terminated => {
                        return Ok(PollResult {
                            outcome: RunOutcome::Terminated,
                            output: best_effort_output(api, job_id).or(cached_output),
                        });
                    }
                    JobState::Invalid => {
                        anyhow::bail!(
                            "FreeRouting rejected the job input (state: INVALID) — the exported DSN may be malformed"
                        );
                    }
                    JobState::Queued
                    | JobState::ReadyToStart
                    | JobState::Running
                    | JobState::Paused
                    | JobState::Stopping => {}
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= 20 {
                    anyhow::bail!(
                        "Lost contact with FreeRouting API server: {e}\n{}",
                        server.log_summary()
                    );
                }
            }
        }
    }
}

/// Fetches output before cancelling: `/output` unconditionally reports "no
/// output" once a job settles into `CANCELLED`. Falls back to `cached_output`
/// (last output seen while `RUNNING`) if the fetch comes back empty.
fn stop_and_capture(
    api: &FreeroutingApiClient,
    job_id: &str,
    cached_output: &Option<Vec<u8>>,
) -> PollResult {
    let output = best_effort_output(api, job_id).or_else(|| cached_output.clone());
    let _ = api.cancel_job(job_id);
    PollResult {
        outcome: RunOutcome::Cancelled,
        output,
    }
}

fn best_effort_output(api: &FreeroutingApiClient, job_id: &str) -> Option<Vec<u8>> {
    match api.get_output(job_id, GET_OUTPUT_TIMEOUT) {
        Ok(JobOutput::Data(bytes)) => Some(bytes),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// `HH:MM:SS` for FreeRouting's `job_timeout`, at full second precision.
fn format_hms(total_secs: u64) -> String {
    let hh = total_secs / 3600;
    let mm = (total_secs % 3600) / 60;
    let ss = total_secs % 60;
    format!("{hh:02}:{mm:02}:{ss:02}")
}

/// Non-fatal warning for FREEROUTING_JAR: an explicit user override
/// may intentionally point at a patched build, so a mismatch isn't rejected.
fn warn_on_hash_mismatch(path: &Path, expected_hash: &[u8; 32]) {
    match sha256_file(path) {
        Ok(hash) if hash == *expected_hash => {}
        Ok(_) => eprintln!(
            "  {} {} does not match the pinned FreeRouting v{FREEROUTING_VERSION} SHA-256; using it anyway since it was explicitly provided",
            "!".yellow(),
            path.display()
        ),
        Err(e) => eprintln!(
            "  {} Could not verify {}: {e}",
            "!".yellow(),
            path.display()
        ),
    }
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    use sha2::Digest;
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open {} for hashing", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("Failed to read {} while hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

fn hex_decode32(hex: &str) -> [u8; 32] {
    <[u8; 32]>::try_from(
        hex::decode(hex).expect("FREEROUTING_JAR_SHA256 must be a valid 64-char hex constant"),
    )
    .expect("FREEROUTING_JAR_SHA256 must decode to exactly 32 bytes")
}

/// Single attempt; no idle timeout in reqwest's blocking client, so
/// `TOTAL_TIMEOUT` stays generous to not fail slow-but-live connections.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

fn download_to_file(url: &str, dest: &Path) -> Result<()> {
    let part_path = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));

    let result = try_download_to_file(url, &part_path);
    if result.is_err() {
        let _ = std::fs::remove_file(&part_path);
    }
    result?;

    std::fs::rename(&part_path, dest).context("Failed to finalize downloaded file")
}

fn try_download_to_file(url: &str, part_path: &Path) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .build()
        .context("Failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .with_context(|| format!("Failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("Download failed: {url}"))?;

    if let Some(total) = response.content_length()
        && total > MAX_DOWNLOAD_BYTES
    {
        anyhow::bail!(
            "Download of {url} reports {total} bytes, exceeding the {MAX_DOWNLOAD_BYTES}-byte limit"
        );
    }

    let mut file = std::fs::File::create(part_path).context("Failed to create download file")?;

    let bar = match response.content_length() {
        Some(total) if total > 0 => Some(
            pcb_ui::ProgressBar::builder(total)
                .message(format!("Downloading FreeRouting ({FREEROUTING_VERSION})"))
                .template("{msg}  |{bar:40.green/gray}| {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})")
                .start(),
        ),
        _ => None,
    };

    let mut response = response;
    let mut buf = [0u8; 65536];
    let mut written = 0u64;
    loop {
        let n = std::io::Read::read(&mut response, &mut buf)
            .with_context(|| format!("Failed to read response body from {url}"))?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > MAX_DOWNLOAD_BYTES {
            anyhow::bail!(
                "Download of {url} exceeded the {MAX_DOWNLOAD_BYTES}-byte limit; aborting"
            );
        }
        std::io::Write::write_all(&mut file, &buf[..n])
            .context("Failed to write downloaded bytes to disk")?;
        if let Some(bar) = &bar {
            bar.inc(n as u64);
        }
    }
    if let Some(bar) = bar {
        bar.finish();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_java_version_sufficient_returns_false_for_nonexistent_path() {
        assert!(!is_java_version_sufficient("/nonexistent/java"));
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"hello world").unwrap();
        // echo -n "hello world" | sha256sum
        let hash = sha256_file(&path).unwrap();
        assert_eq!(
            hex::encode(hash),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn hex_decode32_round_trips_hex_encode() {
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8);
        assert_eq!(hex_decode32(&hex::encode(bytes)), bytes);
    }

    #[test]
    fn sha256_file_errors_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.txt");
        assert!(sha256_file(&path).is_err());
    }

    #[test]
    fn format_hms_preserves_seconds_not_just_minutes() {
        // Regression: a prior version rounded down to whole minutes
        // (e.g. 90s -> "00:01:00" = 60s), which would let FreeRouting's own
        // job_timeout expire before our poll deadline.
        assert_eq!(format_hms(90), "00:01:30");
        assert_eq!(format_hms(59), "00:00:59");
        assert_eq!(format_hms(3661), "01:01:01");
        assert_eq!(format_hms(0), "00:00:00");
    }

    #[cfg(unix)]
    #[test]
    fn detach_process_group_puts_child_in_its_own_group() {
        let mut command = Command::new("sleep");
        command.arg("5").stdout(std::process::Stdio::null());
        detach_process_group(&mut command);
        let mut child = command.spawn().unwrap();

        // process_group(0) makes the child its own group leader, so its pgid
        // equals its own pid, not ours — this is what keeps it out of the
        // terminal's SIGINT delivery to our foreground group.
        let child_pgid = unsafe { libc_getpgid(child.id() as i32) };
        let our_pgid = unsafe { libc_getpgid(0) };
        assert_eq!(child_pgid, child.id() as i32);
        assert_ne!(child_pgid, our_pgid);

        let _ = Command::new("kill").arg(child.id().to_string()).status();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[test]
    fn kill_child_now_kills_the_stored_pid() {
        let mut child = Command::new("sleep")
            .arg("5")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        CHILD_PID.store(child.id(), Ordering::SeqCst);

        kill_child_now();

        let status = child.wait().unwrap();
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9)
        );
        CHILD_PID.store(0, Ordering::SeqCst);
    }

    #[cfg(unix)]
    unsafe extern "C" {
        fn getpgid(pid: i32) -> i32;
    }

    #[cfg(unix)]
    unsafe fn libc_getpgid(pid: i32) -> i32 {
        unsafe { getpgid(pid) }
    }
}
