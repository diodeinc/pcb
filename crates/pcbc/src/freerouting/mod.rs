//! Local auto-routing engine backed by FreeRouting.
//!
//! Invoked as `pcb route --engine freerouting`. Routes boards locally using a
//! FreeRouting Java JAR.
//!
//! The local path uses **Specctra DSN** as an intermediate format: the KiCad
//! board is exported to DSN via `pcbnew`, FreeRouting reads the DSN and
//! writes a **SES** session file, which is then imported back into the
//! `.kicad_pcb`.
//!
//! All work happens on a copy of the board in a private temp directory. The
//! original board on disk is only overwritten once a validated result is
//! ready, so a failed or interrupted run never leaves the workspace board
//! mutated or littered with `.bak`/`.dsn`/`.ses` files.
//!
//! Routing itself goes through FreeRouting's local REST API server mode
//! (`self::api`) rather than its one-shot CLI mode (`-de`/`-do`).
//! CLI mode only writes `.ses` output once, gated on an internal `COMPLETED`
//! state that a confirmed upstream bug (`TIMED_OUT` never promotes to
//! `COMPLETED`) prevents an interrupted or timed-out job from ever reaching —
//! so Ctrl+C and timeouts can never recover a partial route in CLI mode. The
//! API's `GET /jobs/{id}/output` explicitly supports returning partial output
//! for a still-running or just-cancelled job.

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
use api::{FreeroutingApiClient, JobOutput, JobState};

/// Pinned FreeRouting release version. The URL, cache filename, and download
/// messaging are all derived from this so a version bump only touches this
/// constant and the hash below.
///
/// Upstream issues freerouting/freerouting#721 and #759 report a
/// `StackOverflowError` in v2.2.4 from recursion in
/// `PolylineTrace.combine()`. Local reproduction shows v2.0.1, v2.1.0, and
/// v2.2.4 all crash identically under a constrained stack, so the bug isn't
/// specific to 2.2.4 and pinning older buys no verified safety — hence
/// latest. Fix PRs #723 and #764 are open but unmerged; revisit once one
/// ships in a release.
const FREEROUTING_VERSION: &str = "2.2.4";

/// SHA-256 digest of the `freerouting-{FREEROUTING_VERSION}.jar` release
/// artifact. Verifying this hash catches truncated downloads and tampered
/// releases.
const FREEROUTING_JAR_SHA256: &str =
    "f5ed374182900ccc78e473518bbb9f6b869f4a07159495f663a76f52bb10523b";

/// Max routing passes we tell FreeRouting to run (via `update_settings`).
/// Shared with `poll_job` so the spinner can show `pass N/{MAX}` as a rough
/// completion signal — FreeRouting can still finish earlier via its own
/// `improvement_threshold`, so this is approximate, not exact.
const FREEROUTING_MAX_PASSES: u32 = 200;

fn freerouting_jar_filename() -> String {
    format!("freerouting-{FREEROUTING_VERSION}.jar")
}

fn freerouting_jar_url() -> String {
    format!(
        "https://github.com/freerouting/freerouting/releases/download/v{FREEROUTING_VERSION}/freerouting-{FREEROUTING_VERSION}.jar"
    )
}

/// Cancellation flag shared between the Ctrl+C handler and the routing loop.
/// The handler only *requests* cancellation; the code that owns the
/// FreeRouting `Child` (in `run_freerouting`) is responsible for actually
/// killing it and cleaning up through normal control flow.
static CANCEL: AtomicBool = AtomicBool::new(false);

pub fn execute(
    args: &RouteArgs,
    board_path: &Path,
    project_path: &Path,
    board_name: &str,
) -> Result<()> {
    // JAR first so --fr-jar errors surface immediately
    let fr_jar = find_freerouting_jar(args.fr_jar.as_deref())?;
    let java_path = resolve_java()?;

    println!(
        "Routing {} via FreeRouting",
        board_path.file_name().unwrap().to_string_lossy().green()
    );
    println!("  JAR: {}", fr_jar.display());

    // All work happens in a private temp directory; the workspace board is
    // never touched until we have a validated result to publish.
    let work_dir = tempfile::tempdir().context("Failed to create temp working directory")?;
    let work_board = work_dir.path().join(format!("{board_name}.kicad_pcb"));
    std::fs::copy(board_path, &work_board).context("Failed to stage board copy")?;
    // Since KiCad 6, design rules and net classes live in the project file,
    // not the board file — a project-less pcbnew.LoadBoard falls back to
    // default rules, so DSN export and zone fill need this staged too.
    // Never published back: the original project is left untouched.
    if project_path.exists() {
        std::fs::copy(project_path, work_dir.path().join(format!("{board_name}.kicad_pro")))
            .context("Failed to stage project file copy")?;
    }
    let dsn_path = work_dir.path().join(format!("{board_name}.dsn"));
    let ses_path = work_dir.path().join(format!("{board_name}.ses"));

    // The handler only requests cancellation; `run_freerouting`'s poll loop
    // owns the job and actually cancels it, so process exit always happens
    // through normal control flow.
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

    // CANCEL is otherwise only consulted inside run_freerouting's poll loop;
    // without this, Ctrl+C during export would print "Stopping..." but then
    // still start FreeRouting anyway.
    if CANCEL.load(Ordering::SeqCst) {
        println!("  No routing progress to save. Board left untouched.");
        return Ok(());
    }

    let start_time = Instant::now();
    let outcome = run_freerouting(&java_path, &fr_jar, &dsn_path, &ses_path, args.fr_timeout)?;

    if !ses_path.exists() {
        println!("  No routing progress to save. Board left untouched.");
        return Ok(());
    }

    let spinner = Spinner::builder("Importing SES...").start();
    import_ses(&work_board, &ses_path)?;
    spinner.finish();

    // Same as above, for Ctrl+C during import_ses's zone fill.
    if CANCEL.load(Ordering::SeqCst) {
        publish_board(&work_board, board_path)?;
        println!(
            "Partial result saved to {}",
            board_path.display().to_string().cyan()
        );
        return Ok(());
    }

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

/// Atomically replace `dst` with the contents of `src` (rename when on the
/// same filesystem, falling back to copy).
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

/// Minimum Java major version FreeRouting's pinned jar requires. FreeRouting
/// v2.2.4's `build.gradle` targets `JavaVersion.VERSION_25`, so a jar built
/// against that toolchain refuses to load (`UnsupportedClassVersionError`)
/// on anything older, regardless of what FreeRouting's own docs suggest.
/// Keep in sync with `FREEROUTING_VERSION`'s pinned release.
const REQUIRED_JAVA_VERSION: u32 = 25;

/// Resolve a Java binary on `$PATH` meeting `REQUIRED_JAVA_VERSION`.
///
/// We deliberately do not auto-download a JRE/JDK — that's too heavy and
/// opinionated for a CLI tool. Ask the user to install one instead.
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

/// Check whether `java_path` is a Java binary meeting `REQUIRED_JAVA_VERSION`
/// by running `java -version`.
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

/// Locate the FreeRouting JAR via (in priority order):
///   1. `--fr-jar` flag
///   2. `FREEROUTING_JAR` env var
///   3. `freerouting.jar` on `$PATH`
///   4. Auto-download to `~/.cache/pcb/freerouting/freerouting-{FREEROUTING_VERSION}.jar`
fn find_freerouting_jar(provided: Option<&Path>) -> Result<PathBuf> {
    let expected_hash = hex_decode32(FREEROUTING_JAR_SHA256);

    // 1. Explicit --fr-jar flag
    if let Some(path) = provided {
        if path.exists() {
            warn_on_hash_mismatch(path, &expected_hash);
            return Ok(path.to_path_buf());
        }
        anyhow::bail!(
            "FreeRouting JAR not found at --fr-jar path: {}",
            path.display()
        );
    }

    // 2. FREEROUTING_JAR environment variable
    if let Ok(path) = std::env::var("FREEROUTING_JAR") {
        let p = PathBuf::from(&path);
        if p.exists() {
            warn_on_hash_mismatch(&p, &expected_hash);
            return Ok(p);
        }
        anyhow::bail!("FreeRouting JAR not found at FREEROUTING_JAR={}", path);
    }

    // 3. Search $PATH for freerouting.jar. Unlike --fr-jar/FREEROUTING_JAR
    //    (explicit user overrides, which may intentionally point at a
    //    different build), this candidate is discovered implicitly, so we
    //    hold it to the same integrity bar as the auto-download path below
    //    rather than handing an unverified JAR straight to `java -jar`.
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("freerouting.jar");
            if !candidate.exists() {
                continue;
            }
            match sha256_file(&candidate) {
                Ok(hash) if hash == expected_hash => return Ok(candidate),
                _ => eprintln!(
                    "  {} {} does not match the pinned FreeRouting v{FREEROUTING_VERSION} SHA-256, ignoring",
                    "!".yellow(),
                    candidate.display()
                ),
            }
        }
    }

    // 4. Auto-download to cache dir as a last resort
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pcb")
        .join("freerouting");
    let jar_filename = freerouting_jar_filename();
    let cached = cache_dir.join(&jar_filename);

    if cached.exists() {
        match sha256_file(&cached) {
            Ok(hash) if hash == expected_hash => return Ok(cached),
            _ => {
                eprintln!("  Cached JAR is corrupted, re-downloading...");
                let _ = std::fs::remove_file(&cached);
            }
        }
    }

    std::fs::create_dir_all(&cache_dir).context("Failed to create FreeRouting cache dir")?;

    let tmp_path = cache_dir.join(format!("{jar_filename}.tmp"));
    download_to_file(&freerouting_jar_url(), &tmp_path, 3)?;

    let actual_hash =
        sha256_file(&tmp_path).context("Failed to hash downloaded FreeRouting JAR")?;
    if actual_hash != expected_hash {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::bail!(
            "Downloaded FreeRouting JAR has unexpected SHA-256\n\
             Expected: {}\n\
             Actual:   {}\n\
             The download may have been tampered with or the release has changed.\n\
             Download manually from: https://github.com/freerouting/freerouting/releases/tag/v{FREEROUTING_VERSION}",
            FREEROUTING_JAR_SHA256,
            hex::encode(actual_hash),
        );
    }

    std::fs::rename(&tmp_path, &cached).context("Failed to move FreeRouting JAR to cache")?;
    println!("  Downloaded to {}", cached.display());

    Ok(cached)
}

// ---------------------------------------------------------------------------
// DSN/SES pipeline
// ---------------------------------------------------------------------------

/// Export a Specctra DSN file from the KiCad board via `pcbnew`.
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

/// A running FreeRouting API-server process, bound to `127.0.0.1` on an
/// OS-assigned free port. Owns the `Child` for its whole lifetime: `Drop`
/// guarantees the process is killed on every exit path — success, failure,
/// our own timeout, or Ctrl+C — without needing to remember to call `kill()`
/// at each return site.
struct FreeroutingServer {
    child: Child,
    base_url: String,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
}

impl FreeroutingServer {
    /// Spawn FreeRouting in headless, local-only API-server mode.
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

    /// Captured server stdout+stderr, for diagnostics when startup fails.
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

/// Bind an OS-assigned free port on loopback and immediately release it.
/// Small TOCTOU race between release and FreeRouting binding it is accepted
/// (same tradeoff already used for the OAuth callback server elsewhere in
/// this codebase); this only needs to avoid colliding with concurrent `pcb
/// route` invocations on the default FreeRouting port, not be airtight.
fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind ephemeral port for FreeRouting API server")?;
    Ok(listener.local_addr()?.port())
}

/// Spawn a background thread that reads `stream` to EOF into a shared
/// buffer, for capturing a child process's stdout/stderr without blocking.
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

/// Route `dsn_path` via FreeRouting's local REST API, writing the resulting
/// SES data to `ses_path`. We use the API instead of FreeRouting's one-shot
/// CLI mode (`-de`/`-do`) specifically so Ctrl+C and timeouts can retrieve a
/// genuine partial result: CLI mode only writes its output once, gated on
/// reaching an internal `COMPLETED` state that a confirmed upstream bug
/// prevents an interrupted/timed-out job from ever reaching.
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

    // Full HH:MM:SS precision so FreeRouting's own job_timeout matches our
    // poll deadline exactly. Rounding down to whole minutes (as before) let
    // FreeRouting time itself out under our own deadline for any non-minute-
    // aligned --fr-timeout (e.g. 90s truncated to "00:01:00" = 60s), stopping
    // the job early with TIMED_OUT well before the user-requested window
    // ends.
    api.update_settings(&job_id, FREEROUTING_MAX_PASSES, &format_hms(timeout_secs.max(1)))?;

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
        (RunOutcome::Completed, _) => {
            spinner.success(format!("FreeRouting finished in {elapsed:.1}s"));
        }
        (RunOutcome::Cancelled, true) => {
            spinner.warning(format!(
                "FreeRouting stopped after {elapsed:.1}s (partial result)"
            ));
        }
        (RunOutcome::Cancelled, false) => {
            // Distinct from the "finished" wording above: nothing was
            // produced, so there's no result to save (execute() checks
            // ses_path.exists() next and reports "No routing progress to
            // save" — this message is what explains *why* to the user
            // instead of just "finished", which read as success).
            spinner.warning(format!(
                "FreeRouting stopped after {elapsed:.1}s with no routing progress"
            ));
        }
    }

    if let Some(bytes) = poll_result.output {
        std::fs::write(ses_path, bytes).context("Failed to write FreeRouting SES output")?;
    }
    // Else: no output was ever produced — cancelled before any progress, or
    // the board had nothing left to route. Leaving `ses_path` absent is
    // correct either way: `execute()` already treats a missing SES as "no
    // progress to save".

    // `server` drops here, killing the FreeRouting process.
    Ok(poll_result.outcome)
}

struct PollResult {
    outcome: RunOutcome,
    output: Option<Vec<u8>>,
}

/// Poll the job's status until it completes, our own timeout elapses, or
/// cancellation is requested via Ctrl+C — checked every 100ms regardless of
/// the ~1s HTTP poll cadence, so Ctrl+C stays responsive without hammering
/// the local server.
///
/// While the job is still genuinely in progress, we opportunistically fetch
/// and cache its latest output alongside its status. This is deliberate:
/// FreeRouting's `GET /output` only returns partial data while the job is
/// `RUNNING`/`PAUSED`/`STOPPING` — the instant it settles into the terminal
/// `CANCELLED` state, the same endpoint unconditionally reports "no valid
/// output", even if valid partial data existed moments earlier. Since
/// `cancel_job` only *requests* a stop and returns well before the job
/// actually reaches that terminal state, a single `get_output()` call made
/// right after cancelling would race the state transition and could
/// permanently lose real progress. Falling back to output captured during
/// the last known-safe `RUNNING` poll sidesteps that race entirely, rather
/// than trying to win it.
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

        // Refresh the elapsed time every ~1s tick (this loop's cadence)
        // regardless of whether the pass count changed, so the spinner
        // shows visible progress even on boards where FreeRouting sits on
        // one pass for a long time.
        //
        // Only show the pass counter once it's > 0: FreeRouting's initial
        // routing stage runs before any numbered optimization pass starts,
        // so `pass 0` can sit static for a long time on some boards. Since
        // that's indistinguishable from a stuck/broken counter, it's better
        // to just show elapsed time until a real pass increment proves the
        // counter is actually moving.
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
                        return Ok(PollResult {
                            outcome: RunOutcome::Completed,
                            output: best_effort_output(api, job_id).or(last_known_output),
                        });
                    }
                    JobState::Cancelled | JobState::TimedOut => {
                        // FreeRouting stopped on its own (e.g. its internal
                        // job_timeout fired) without us calling cancel_job,
                        // so we never got a chance to cache output right
                        // before this. Best-effort output here can still
                        // land after the CANCELLED transition and come back
                        // empty — that's the same structural limitation
                        // CLI mode has for this specific case, just not one
                        // we can poll our way around after the fact.
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
                        // Still safely in progress (Stopping is a job we or
                        // FreeRouting itself asked to cancel, still settling
                        // toward Cancelled): refresh our cached output now,
                        // while the API is guaranteed to serve it, rather
                        // than waiting until we're racing a cancellation.
                        if let Ok(JobOutput::Data(bytes)) = api.get_output(job_id) {
                            last_known_output = Some(bytes);
                        }
                    }
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= 20 {
                    // Server's unreachable, but whatever we cached during
                    // the last known-good RUNNING poll is still real
                    // progress worth returning, same as a cancel/timeout.
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

/// A single, non-retried attempt at fetching current output — used only as
/// a bonus on top of `last_known_output`, for the case where the job hasn't
/// actually settled into `CANCELLED` yet and a fresher result is available.
fn best_effort_output(api: &FreeroutingApiClient, job_id: &str) -> Option<Vec<u8>> {
    match api.get_output(job_id) {
        Ok(JobOutput::Data(bytes)) => Some(bytes),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

/// Format a duration in seconds as `HH:MM:SS` for FreeRouting's
/// `job_timeout` setting, at full second precision (no rounding to whole
/// minutes, which would let FreeRouting time itself out before our own
/// deadline).
fn format_hms(total_secs: u64) -> String {
    let hh = total_secs / 3600;
    let mm = (total_secs % 3600) / 60;
    let ss = total_secs % 60;
    format!("{hh:02}:{mm:02}:{ss:02}")
}

/// Warn (non-fatally) if `path` doesn't match `expected_hash`. Used for
/// --fr-jar/FREEROUTING_JAR: both are explicit user overrides, so a mismatch
/// isn't rejected outright (it may be an intentional dev/patched build) —
/// but it's surfaced rather than silently running an unverified JAR with no
/// visibility at all.
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

/// Hash `path` with SHA-256, returning the raw 32-byte digest. Byte
/// comparison (rather than comparing hex-encoded strings) is the natural way
/// to check a hash for equality.
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

/// Decode a lowercase hex string (as used for our pinned SHA-256 constants)
/// into a raw 32-byte digest.
fn hex_decode32(hex: &str) -> [u8; 32] {
    <[u8; 32]>::try_from(
        hex::decode(hex).expect("FREEROUTING_JAR_SHA256 must be a valid 64-char hex constant"),
    )
    .expect("FREEROUTING_JAR_SHA256 must decode to exactly 32 bytes")
}

/// Download `url` to `dest` (via a `.part` sibling, renamed on success once
/// fully written), retrying only on transient failures (network errors and
/// 5xx responses). Client errors (4xx) are treated as permanent and are not
/// retried.
fn download_to_file(url: &str, dest: &Path, max_attempts: u32) -> Result<()> {
    let part_path = dest.with_extension(format!(
        "{}.part",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));

    for attempt in 1..=max_attempts {
        match try_download_to_file(url, &part_path) {
            Ok(()) => {
                std::fs::rename(&part_path, dest)
                    .context("Failed to finalize downloaded file")?;
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

    // Report percentage progress when the server tells us the total size;
    // fall back to a plain unstyled copy (no total to show a bar against)
    // when it doesn't, e.g. a chunked-encoding response with no
    // Content-Length.
    match response.content_length() {
        Some(total) if total > 0 => {
            let bar = pcb_ui::ProgressBar::builder(total)
                .message(format!("Downloading FreeRouting ({FREEROUTING_VERSION})"))
                // indicatif's {bytes}/{total_bytes} auto-format human-
                // readable sizes (e.g. "2.3 MiB") instead of the default
                // template's raw {pos}/{len} byte counts.
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
        // Regression test: a prior version rounded down to whole minutes
        // (90s -> "00:01:00" = 60s), letting FreeRouting's own job_timeout
        // expire before our poll deadline.
        assert_eq!(format_hms(90), "00:01:30");
        assert_eq!(format_hms(59), "00:00:59");
        assert_eq!(format_hms(3661), "01:01:01");
        assert_eq!(format_hms(0), "00:00:00");
    }
}
