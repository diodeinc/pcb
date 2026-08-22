//! Local auto-routing engine backed by FreeRouting (`pcb route --engine
//! freerouting`). KiCad board -> Specctra DSN -> FreeRouting -> SES -> board,
//! driven through FreeRouting's REST API server mode (`self::api`).

use std::io::{Read, Write};
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
use api::{FreeroutingApiClient, GET_OUTPUT_TIMEOUT, JobOutput, JobState};

const FREEROUTING_VERSION: &str = "2.3.0";

const FREEROUTING_REPO: &str = "freerouting/freerouting";

const FREEROUTING_JAR_SHA256: &str =
    "3cf18d608437740bc497db6b8ef5888e2e60a08de0def20691d1bad0c0e0ee24";

const FREEROUTING_MAX_PASSES: u32 = 200;

/// Must exceed `GET_OUTPUT_TIMEOUT`, or FreeRouting's own job_timeout can
/// fire mid-fetch and refuse output.
const JOB_TIMEOUT_SAFETY_MARGIN_SECS: u64 = 150;

fn freerouting_jar_filename() -> String {
    format!("freerouting-{FREEROUTING_VERSION}.jar")
}

fn freerouting_jar_url() -> String {
    format!(
        "https://github.com/{FREEROUTING_REPO}/releases/download/v{FREEROUTING_VERSION}/freerouting-{FREEROUTING_VERSION}.jar"
    )
}

static CANCEL: AtomicBool = AtomicBool::new(false);
static FORCE_KILL: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<Arc<Mutex<Child>>>> = Mutex::new(None);

pub fn execute(
    args: &RouteArgs,
    board_path: &Path,
    project_path: &Path,
    board_name: &str,
) -> Result<()> {
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
    if project_path.exists() {
        std::fs::copy(
            project_path,
            work_dir.path().join(format!("{board_name}.kicad_pro")),
        )
        .context("Failed to stage project file copy")?;
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
    *CHILD.lock().unwrap() = None;
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

    if CANCEL.load(Ordering::SeqCst) {
        println!("  No routing progress to save. Board left untouched.");
        return Ok(());
    }

    let start_time = Instant::now();
    let run_result = run_freerouting(
        &java_path,
        &fr_jar,
        &dsn_path,
        &ses_path,
        args.timeout as u64 * 60,
    )?;
    let outcome = run_result.outcome;

    if !ses_path.exists() {
        println!("  No routing progress to save. Board left untouched.");
        bail_if_terminated(outcome)?;
        return Ok(());
    }

    let spinner = Spinner::builder("Importing SES...").start();
    import_ses(&work_board, &ses_path)?;
    spinner.finish();

    publish_board(&work_board, board_path)?;

    let elapsed = start_time.elapsed();
    println!("  Time:       {}", format_duration(elapsed));
    println!();
    match outcome {
        RunOutcome::Cancelled => println!(
            "Partial result saved to {}",
            board_path.display().to_string().cyan()
        ),
        RunOutcome::Completed => match run_result.unrouted {
            Some(n) if n > 0 => println!(
                "Result saved to {} ({n} unrouted connection{} remaining)",
                board_path.display().to_string().cyan(),
                if n == 1 { "" } else { "s" }
            ),
            Some(_) => println!(
                "Result saved to {}",
                board_path.display().to_string().cyan()
            ),
            None => println!(
                "Result saved to {} (could not verify remaining unrouted connections)",
                board_path.display().to_string().cyan()
            ),
        },
        RunOutcome::Terminated => println!(
            "Partial result saved to {} (FreeRouting terminated unexpectedly)",
            board_path.display().to_string().cyan()
        ),
    }

    if !args.no_open {
        let _ = pcb_kicad::open_pcbnew(board_path);
    }

    bail_if_terminated(outcome)?;

    Ok(())
}

fn bail_if_terminated(outcome: RunOutcome) -> Result<()> {
    if outcome == RunOutcome::Terminated {
        anyhow::bail!("FreeRouting terminated unexpectedly; see the log path printed above");
    }
    Ok(())
}

fn publish_board(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
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

const REQUIRED_JAVA_VERSION: u32 = 25;

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

    let tmp = tempfile::Builder::new()
        .prefix(&format!("{jar_filename}."))
        .suffix(".part")
        .tempfile_in(&cache_dir)
        .context("Failed to create temp file for FreeRouting JAR download")?;
    download_to_file(&freerouting_jar_url(), tmp.as_file())?;

    let actual_hash =
        sha256_file(tmp.path()).context("Failed to hash downloaded FreeRouting JAR")?;
    if actual_hash != *expected_hash {
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

    tmp.persist(&cached)
        .map_err(|e| anyhow::anyhow!(e.error))
        .context("Failed to move FreeRouting JAR to cache")?;
    println!("  Downloaded to {}", cached.display());

    Ok(cached)
}

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

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RunOutcome {
    Completed,
    Cancelled,
    Terminated,
}

struct FreeroutingServer {
    child: Arc<Mutex<Child>>,
    base_url: String,
    log_file: Option<tempfile::NamedTempFile>,
}

impl FreeroutingServer {
    fn spawn(java_path: &Path, jar_path: &Path) -> Result<Self> {
        let port = pick_free_port()?;

        let log_file = tempfile::Builder::new()
            .prefix("pcb-freerouting-")
            .suffix(".log")
            .tempfile()
            .context("Failed to create FreeRouting log file")?;
        let (log_stdout, log_stderr) = pcb_command_runner::log_file_stdio(log_file.as_file())?;

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
        detach_process_group(&mut command);

        let child = command
            .spawn()
            .context("Failed to launch FreeRouting API server (java -jar)")?;
        let child = Arc::new(Mutex::new(child));
        *CHILD.lock().unwrap() = Some(child.clone());

        Ok(Self {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            log_file: Some(log_file),
        })
    }

    fn still_alive(&mut self) -> Result<bool> {
        Ok(self.child.lock().unwrap().try_wait()?.is_none())
    }

    fn keep_log(&mut self) -> PathBuf {
        match self.log_file.take() {
            Some(f) => match f.keep() {
                Ok((_, path)) => path,
                Err(mut e) => {
                    e.file.disable_cleanup(true);
                    e.file.path().to_path_buf()
                }
            },
            None => PathBuf::new(),
        }
    }

    fn log_summary(&mut self) -> String {
        let log_path = self.keep_log();
        match self.child.lock().unwrap().try_wait().ok().flatten() {
            Some(status) => format!("log: {} ({status})", log_path.display()),
            None => format!("log: {}", log_path.display()),
        }
    }
}

impl Drop for FreeroutingServer {
    fn drop(&mut self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
        *CHILD.lock().unwrap() = None;
    }
}

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

fn kill_child_now() {
    let child = CHILD.lock().unwrap().clone();
    if let Some(child) = child {
        let _ = child.lock().unwrap().kill();
    }
}

fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("Failed to bind ephemeral port for FreeRouting API server")?;
    Ok(listener.local_addr()?.port())
}

struct RunResult {
    outcome: RunOutcome,
    unrouted: Option<usize>,
}

fn run_freerouting(
    java_path: &Path,
    jar_path: &Path,
    dsn_path: &Path,
    ses_path: &Path,
    timeout_secs: u64,
) -> Result<RunResult> {
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
        (RunOutcome::Completed, true) => match poll_result.unrouted {
            Some(n) if n > 0 => spinner.warning(format!(
                "FreeRouting finished in {elapsed:.1}s with {n} unrouted connection{}",
                if n == 1 { "" } else { "s" }
            )),
            Some(_) => spinner.success(format!("FreeRouting finished in {elapsed:.1}s")),
            None => spinner.warning(format!(
                "FreeRouting finished in {elapsed:.1}s (could not verify unrouted connections)"
            )),
        },
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

    if poll_result.outcome == RunOutcome::Terminated {
        eprintln!(
            "  {} FreeRouting terminated unexpectedly; see log for details: {}",
            "!".yellow(),
            server.keep_log().display()
        );
    }

    if let Some(bytes) = poll_result.output {
        std::fs::write(ses_path, bytes).context("Failed to write FreeRouting SES output")?;
    }

    Ok(RunResult {
        outcome: poll_result.outcome,
        unrouted: poll_result.unrouted,
    })
}

struct PollResult {
    outcome: RunOutcome,
    output: Option<Vec<u8>>,
    unrouted: Option<usize>,
}

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

    loop {
        for _ in 0..10 {
            if FORCE_KILL.load(Ordering::SeqCst) {
                return Ok(PollResult {
                    outcome: RunOutcome::Cancelled,
                    output: None,
                    unrouted: None,
                });
            }
            if CANCEL.load(Ordering::SeqCst) {
                return Ok(stop_and_capture(api, job_id));
            }
            thread::sleep(Duration::from_millis(100));
        }

        if Instant::now() > deadline {
            return Ok(stop_and_capture(api, job_id));
        }

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
                        return Ok(match api.get_output(job_id, GET_OUTPUT_TIMEOUT)? {
                            JobOutput::Data(bytes) => PollResult {
                                outcome: RunOutcome::Completed,
                                output: Some(bytes),
                                unrouted: api.get_unrouted_count(job_id).ok(),
                            },
                            JobOutput::NothingToRoute => PollResult {
                                outcome: RunOutcome::Completed,
                                output: None,
                                unrouted: None,
                            },
                        });
                    }
                    JobState::Cancelled | JobState::TimedOut => {
                        return Ok(PollResult {
                            outcome: RunOutcome::Cancelled,
                            output: best_effort_output(api, job_id),
                            unrouted: None,
                        });
                    }
                    JobState::Terminated => {
                        return Ok(PollResult {
                            outcome: RunOutcome::Terminated,
                            output: best_effort_output(api, job_id),
                            unrouted: None,
                        });
                    }
                    JobState::Invalid => {
                        anyhow::bail!(
                            "FreeRouting rejected the job input (state: INVALID) — the exported DSN may be malformed\n{}",
                            server.log_summary()
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

fn stop_and_capture(api: &FreeroutingApiClient, job_id: &str) -> PollResult {
    let output = best_effort_output(api, job_id);
    let _ = api.cancel_job(job_id);
    PollResult {
        outcome: RunOutcome::Cancelled,
        output,
        unrouted: None,
    }
}

fn best_effort_output(api: &FreeroutingApiClient, job_id: &str) -> Option<Vec<u8>> {
    match api.get_output(job_id, GET_OUTPUT_TIMEOUT) {
        Ok(JobOutput::Data(bytes)) => Some(bytes),
        _ => None,
    }
}

fn format_hms(total_secs: u64) -> String {
    let hh = total_secs / 3600;
    let mm = (total_secs % 3600) / 60;
    let ss = total_secs % 60;
    format!("{hh:02}:{mm:02}:{ss:02}")
}

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

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

fn download_to_file(url: &str, mut file: &std::fs::File) -> Result<()> {
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
        file.write_all(&buf[..n])
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
    fn sha256_file_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"hello world").unwrap();
        let hash = sha256_file(&path).unwrap();
        assert_eq!(
            hex::encode(hash),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn format_hms_preserves_seconds_not_just_minutes() {
        assert_eq!(format_hms(90), "00:01:30");
        assert_eq!(format_hms(59), "00:00:59");
        assert_eq!(format_hms(3661), "01:01:01");
        assert_eq!(format_hms(0), "00:00:00");
    }
}
