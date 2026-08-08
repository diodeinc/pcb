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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// get_output serializes the whole in-progress board server-side, so cache
/// refreshes are throttled to this interval instead of every ~1s poll tick.
const OUTPUT_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

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

pub fn execute(
    args: &RouteArgs,
    board_path: &Path,
    project_path: &Path,
    board_name: &str,
) -> Result<()> {
    // Java check happens before the PATH search / auto-download fallback so
    // a machine without Java doesn't pay for a multi-megabyte download first.
    let expected_hash = hex_decode32(FREEROUTING_JAR_SHA256);
    let explicit_jar = resolve_explicit_freerouting_jar(args.fr_jar.as_deref(), &expected_hash)?;

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
    if let Err(e) = ctrlc::set_handler(|| {
        eprintln!("\n  Stopping FreeRouting and fetching the best result so far...");
        CANCEL.store(true, Ordering::SeqCst);
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
        args.fr_timeout * 60,
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
    if outcome == RunOutcome::Cancelled {
        println!(
            "Partial result saved to {}",
            board_path.display().to_string().cyan()
        );
    } else {
        println!(
            "Result saved to {}",
            board_path.display().to_string().cyan()
        );
    }

    if !args.no_open {
        let _ = pcb_kicad::open_pcbnew(board_path);
    }

    Ok(())
}

fn publish_board(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    let tmp = dst.with_extension("kicad_pcb.tmp");
    std::fs::copy(src, &tmp).context("Failed to stage routed board for publish")?;
    std::fs::rename(&tmp, dst).context("Failed to publish routed board")?;
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

/// `--fr-jar` flag (priority 1) or `FREEROUTING_JAR` env var (priority 2).
/// `Ok(None)` means neither is set — caller falls back to PATH/auto-download.
fn resolve_explicit_freerouting_jar(
    provided: Option<&Path>,
    expected_hash: &[u8; 32],
) -> Result<Option<PathBuf>> {
    if let Some(path) = provided {
        if path.exists() {
            warn_on_hash_mismatch(path, expected_hash);
            return Ok(Some(path.to_path_buf()));
        }
        anyhow::bail!(
            "FreeRouting JAR not found at --fr-jar path: {}",
            path.display()
        );
    }

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

/// Priority: `freerouting.jar` on `$PATH`, then auto-download to
/// `~/.cache/pcb/freerouting/`.
fn find_or_download_freerouting_jar(expected_hash: &[u8; 32]) -> Result<PathBuf> {
    // Unlike --fr-jar/FREEROUTING_JAR, a PATH match is discovered implicitly,
    // so it's still hash-checked rather than handed straight to `java -jar`.
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("freerouting.jar");
            if !candidate.exists() {
                continue;
            }
            match sha256_file(&candidate) {
                Ok(hash) if hash == *expected_hash => return Ok(candidate),
                _ => eprintln!(
                    "  {} {} does not match the pinned FreeRouting v{FREEROUTING_VERSION} SHA-256, ignoring",
                    "!".yellow(),
                    candidate.display()
                ),
            }
        }
    }

    // Fall back to a uid-scoped temp subdir when dirs::cache_dir() is
    // unavailable — a fixed shared path could be pre-staked by another user.
    let cache_dir = match dirs::cache_dir() {
        Some(dir) => dir.join("pcb").join("freerouting"),
        None => process_temp_dir()
            .join(temp_cache_subdir())
            .join("freerouting"),
    };
    let jar_filename = freerouting_jar_filename();
    let cached = cache_dir.join(&jar_filename);

    std::fs::create_dir_all(&cache_dir).context("Failed to create FreeRouting cache dir")?;
    if !restrict_dir_to_owner(&cache_dir) {
        anyhow::bail!(
            "Refusing to use FreeRouting cache dir {} — it's owned by another user.\n\
             This can happen when the system cache/temp dir is shared between users.\n\
             Remove the directory (if you control it) or set --fr-jar/FREEROUTING_JAR \
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
    download_to_file(&freerouting_jar_url(), &tmp_path, 3)?;

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

fn process_temp_dir() -> PathBuf {
    std::env::temp_dir()
}

/// Per-uid subdirectory under `process_temp_dir()` so different local users
/// never share the fallback cache path. `restrict_dir_to_owner` is defense
/// in depth on top of this, not the primary mechanism.
#[cfg(unix)]
fn temp_cache_subdir() -> String {
    // SAFETY: geteuid takes no arguments and cannot fail.
    let euid = unsafe { libc_geteuid() };
    format!("pcb-{euid}")
}

#[cfg(not(unix))]
fn temp_cache_subdir() -> String {
    "pcb".to_string()
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
}

/// Owns the `Child`; `Drop` kills it on every exit path (success, failure,
/// timeout, Ctrl+C) instead of relying on each return site to do it.
struct FreeroutingServer {
    child: Child,
    base_url: String,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl FreeroutingServer {
    fn spawn(java_path: &Path, jar_path: &Path) -> Result<Self> {
        let port = pick_free_port()?;

        let mut child = Command::new(java_path)
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(jar_path)
            .arg("--gui.enabled=false")
            .arg("--api_server.enabled=true")
            .arg("--api_server.authentication.enabled=false")
            .arg(format!("--api_server-endpoints=http://127.0.0.1:{port}"))
            .arg("--usage_and_diagnostic_data.disable_analytics=true")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to launch FreeRouting API server (java -jar)")?;

        let stdout = capture_stream(child.stdout.take().unwrap());
        let stderr = capture_stream(child.stderr.take().unwrap());

        Ok(Self {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            stdout,
            stderr,
        })
    }

    fn still_alive(&mut self) -> Result<bool> {
        Ok(self.child.try_wait()?.is_none())
    }

    fn log(&self) -> String {
        format!(
            "{}{}",
            self.stdout.lock().unwrap(),
            self.stderr.lock().unwrap()
        )
    }
}

impl Drop for FreeroutingServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// TOCTOU race between release and FreeRouting binding it is accepted (same
/// tradeoff as the OAuth callback server elsewhere in this codebase).
fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind ephemeral port for FreeRouting API server")?;
    Ok(listener.local_addr()?.port())
}

fn capture_stream(mut stream: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let buf2 = buf.clone();
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf2
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(_) => break,
            }
        }
    });
    buf
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
        let log = server.log();
        if log.trim().is_empty() {
            return Err(e);
        }
        anyhow::bail!("{e}\n{}", log.trim());
    }

    let session_id = api
        .create_session()
        .context("Failed to start FreeRouting session")?;
    let job_name = dsn_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "pcb-route".to_string());
    let job_id = api.enqueue_job(&session_id, &job_name)?;

    // Full HH:MM:SS so FreeRouting's job_timeout matches our poll deadline
    // exactly, rather than rounding down and timing out early.
    api.update_settings(
        &job_id,
        FREEROUTING_MAX_PASSES,
        &format_hms(timeout_secs.max(1)),
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
    let poll_result = poll_job(&api, &job_id, timeout_secs, &spinner)?;
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
    }

    if let Some(bytes) = poll_result.output {
        std::fs::write(ses_path, bytes).context("Failed to write FreeRouting SES output")?;
    }
    // else: leave ses_path absent — execute() treats that as "nothing to save".

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
) -> Result<PollResult> {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(timeout_secs);
    let mut consecutive_errors = 0u32;
    let mut last_printed_pass: Option<u32> = None;
    let mut last_known_output: Option<Vec<u8>> = None;
    let mut last_output_refresh: Option<Instant> = None;

    loop {
        for _ in 0..10 {
            if CANCEL.load(Ordering::SeqCst) {
                let _ = api.cancel_job(job_id);
                return Ok(PollResult {
                    outcome: RunOutcome::Cancelled,
                    output: best_effort_output(api, job_id).or(last_known_output),
                });
            }
            thread::sleep(Duration::from_millis(100));
        }

        if Instant::now() > deadline {
            let _ = api.cancel_job(job_id);
            return Ok(PollResult {
                outcome: RunOutcome::Cancelled,
                output: best_effort_output(api, job_id).or(last_known_output),
            });
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
                match status.state {
                    JobState::Completed => {
                        // Unlike Cancelled/TimedOut below, a failed fetch here
                        // must downgrade to Cancelled rather than silently
                        // reporting Completed with a stale cached snapshot.
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
                                    "  {} FreeRouting finished but its final output could not be fetched; saving last known progress instead.",
                                    "!".yellow()
                                );
                                PollResult {
                                    outcome: RunOutcome::Cancelled,
                                    output: last_known_output,
                                }
                            }
                        });
                    }
                    JobState::Cancelled | JobState::TimedOut => {
                        // FreeRouting stopped on its own (e.g. internal
                        // job_timeout), so we never cached output beforehand.
                        return Ok(PollResult {
                            outcome: RunOutcome::Cancelled,
                            output: best_effort_output(api, job_id).or(last_known_output),
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
                    | JobState::Stopping => {
                        // Refresh the cache now, while the API is guaranteed
                        // to serve it, instead of waiting until we're racing
                        // a cancellation (see OUTPUT_REFRESH_INTERVAL).
                        let should_refresh = last_output_refresh
                            .is_none_or(|t| t.elapsed() >= OUTPUT_REFRESH_INTERVAL);
                        if should_refresh {
                            if let Ok(JobOutput::Data(bytes)) =
                                api.get_output(job_id, DEFAULT_TIMEOUT)
                            {
                                last_known_output = Some(bytes);
                            }
                            last_output_refresh = Some(Instant::now());
                        }
                    }
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= 20 {
                    if last_known_output.is_some() {
                        eprintln!(
                            "  {} Lost contact with FreeRouting API server: {e}",
                            "!".yellow()
                        );
                        eprintln!("  Saving last known routing progress.");
                        return Ok(PollResult {
                            outcome: RunOutcome::Cancelled,
                            output: last_known_output,
                        });
                    }
                    anyhow::bail!("Lost contact with FreeRouting API server: {e}");
                }
            }
        }
    }
}

fn best_effort_output(api: &FreeroutingApiClient, job_id: &str) -> Option<Vec<u8>> {
    match api.get_output(job_id, DEFAULT_TIMEOUT) {
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

/// Non-fatal warning for --fr-jar/FREEROUTING_JAR: an explicit user override
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

/// Retries transient failures (network errors, 5xx); 4xx is treated as
/// permanent.
fn download_to_file(url: &str, dest: &Path, max_attempts: u32) -> Result<()> {
    let part_path = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));

    for attempt in 1..=max_attempts {
        match try_download_to_file(url, &part_path) {
            Ok(()) => {
                std::fs::rename(&part_path, dest).context("Failed to finalize downloaded file")?;
                return Ok(());
            }
            Err(DownloadError::Permanent(e)) => return Err(e),
            Err(DownloadError::Transient(e)) => {
                let _ = std::fs::remove_file(&part_path);
                if attempt < max_attempts {
                    let delay = Duration::from_secs(2u64.pow(attempt));
                    eprintln!(
                        "  Download failed (attempt {attempt}/{max_attempts}), retrying in {}s: {e}",
                        delay.as_secs()
                    );
                    thread::sleep(delay);
                } else {
                    return Err(e.context(format!("Download failed after {max_attempts} attempts")));
                }
            }
        }
    }

    unreachable!()
}

enum DownloadError {
    /// Not worth retrying (e.g. HTTP 4xx, or we already wrote a bad file).
    Permanent(anyhow::Error),
    /// Worth retrying (network error, HTTP 5xx).
    Transient(anyhow::Error),
}

fn try_download_to_file(url: &str, part_path: &Path) -> Result<(), DownloadError> {
    let response = reqwest::blocking::get(url).map_err(|e| DownloadError::Transient(e.into()))?;

    let status = response.status();
    if status.is_client_error() {
        return Err(DownloadError::Permanent(anyhow::anyhow!(
            "download failed with client error {status}: {url}"
        )));
    }
    let mut response = response.error_for_status().map_err(|e| {
        if status.is_server_error() {
            DownloadError::Transient(e.into())
        } else {
            DownloadError::Permanent(e.into())
        }
    })?;

    let mut file =
        std::fs::File::create(part_path).map_err(|e| DownloadError::Transient(e.into()))?;

    match response.content_length() {
        Some(total) if total > 0 => {
            let bar = pcb_ui::ProgressBar::builder(total)
                .message(format!("Downloading FreeRouting ({FREEROUTING_VERSION})"))
                .template("{msg}  |{bar:40.green/gray}| {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})")
                .start();
            let mut buf = [0u8; 65536];
            loop {
                let n = std::io::Read::read(&mut response, &mut buf)
                    .map_err(|e| DownloadError::Transient(e.into()))?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut file, &buf[..n])
                    .map_err(|e| DownloadError::Transient(e.into()))?;
                bar.inc(n as u64);
            }
            bar.finish();
        }
        _ => {
            std::io::copy(&mut response, &mut file)
                .map_err(|e| DownloadError::Transient(e.into()))?;
        }
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
}
