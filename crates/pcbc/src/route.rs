//! PCB auto-routing command
//!
//! Routes boards locally using FreeRouting Java JAR.
//! Cloud routing via DeepPCB API is not yet exposed.
//!
//! The local path uses **Specctra DSN** as an intermediate format: the KiCad
//! board is exported to DSN via `pcbnew`, FreeRouting reads the DSN and writes
//! a **SES** session file, which is then imported back into the `.kicad_pcb`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_diode_api::routing::{self, RoutingJob, RoutingStatus, StartRoutingRequest};
use pcb_kicad::PythonScriptBuilder;
use pcb_layout::utils;
use pcb_ui::prelude::*;
use std::sync::LazyLock;

use crate::file_walker;

/// PID of the currently running FreeRouting child process (if any).
/// Set by `run_freerouting` after spawning; cleared after the child exits.
/// Used by the Ctrl+C handler to kill FreeRouting before `process::exit`.
static FREEROUTING_PID: LazyLock<Mutex<Option<u32>>> = LazyLock::new(|| Mutex::new(None));

/// URL for the pinned FreeRouting v2.0.1 release JAR.
const FREEROUTING_JAR_URL: &str =
    "https://github.com/freerouting/freerouting/releases/download/v2.0.1/freerouting-2.0.1.jar";

/// SHA-256 digest of the `freerouting-2.0.1.jar` release artifact.
/// Verifying this hash catches truncated downloads and tampered releases.
const FREEROUTING_JAR_SHA256: &str =
    "d7fd0f63f52e6d74b0fad6715f87ca9f0ffd7109d66b2a584638000270592ecf";

#[derive(Args, Debug, Clone)]
#[command(about = "Auto-route PCB locally via FreeRouting")]
pub struct RouteArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Routing timeout in minutes (default: 20, max: 60)
    #[arg(long, short = 't', default_value = "20")]
    pub timeout: u32,

    /// Don't open KiCad after routing
    #[arg(long)]
    pub no_open: bool,

    /// Override project ID (default: derived from .zen file name)
    #[arg(long)]
    pub project_id: Option<String>,

    // /// Use cloud DeepPCB service instead of local FreeRouting
    // #[arg(long)]
    // pub remote: bool,

    /// Path to freerouting.jar (default: search FREEROUTING_JAR env or $PATH)
    #[arg(long)]
    pub fr_jar: Option<PathBuf>,

    /// FreeRouting timeout in seconds (default: 300)
    #[arg(long, default_value = "300")]
    pub fr_timeout: u64,

    /// Keep temporary DSN/SES files after routing
    #[arg(long)]
    pub keep: bool,

    /// Clear existing traces before re-routing
    #[arg(long)]
    pub force: bool,
}

pub fn execute(args: RouteArgs) -> Result<()> {
    file_walker::require_zen_file(&args.file)?;

    let board_path = resolve_board(&args.file)?;
    let board_name = board_path.file_stem().unwrap().to_string_lossy();

    // if args.remote {
    //     route_via_cloud(&args, &board_path, &board_name)
    // } else {
    route_via_local(&args, &board_path, &board_name)
    // }
}

/// Shared board discovery: evaluate .zen, find .kicad_pcb, validate.
fn resolve_board(zen_path: &Path) -> Result<PathBuf> {
    let resolution_result = crate::resolve::resolve(Some(zen_path), false)?;

    let (output, diagnostics) =
        pcb_zen::run(zen_path, resolution_result, Default::default()).unpack();

    if diagnostics.has_errors() {
        anyhow::bail!("Failed to evaluate {}: build errors", zen_path.display());
    }

    let schematic = output.context("No schematic output from evaluation")?;

    let layout_dir = utils::resolve_layout_dir(&schematic)?
        .context("No layout path defined in schematic. Add layout=\"path\" to your module.")?;

    let kicad_files = utils::require_kicad_files(&layout_dir)?;
    let board_path = kicad_files.kicad_pcb();

    if !board_path.exists() {
        anyhow::bail!(
            "No layout found at {}\n\nRun {} first to generate the board.",
            board_path.display(),
            "pcb layout".yellow()
        );
    }

    Ok(board_path)
}

// ---------------------------------------------------------------------------
// Cloud routing (existing DeepPCB backend)
// ---------------------------------------------------------------------------

fn route_via_cloud(args: &RouteArgs, board_path: &Path, _board_name: &str) -> Result<()> {
    if args.timeout > 60 {
        anyhow::bail!("Timeout cannot exceed 60 minutes");
    }

    // Validate project file exists
    let project_path = board_path.with_extension("kicad_pro");
    if !project_path.exists() {
        anyhow::bail!(
            "Missing project file: {}\n\nEnsure the layout was generated with KiCad 6+",
            project_path.display()
        );
    }

    let zen_stem = args
        .file
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let project_id = args.project_id.clone().unwrap_or(zen_stem);

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl+C handler")?;

    println!(
        "Starting routing for {}",
        board_path.file_name().unwrap().to_string_lossy().green()
    );

    let spinner = Spinner::builder("Uploading board...").start();

    let request = StartRoutingRequest {
        project_id: project_id.clone(),
        timeout: Some(args.timeout),
    };
    let ctx = pcb_diode_api::WorkspaceContext::from_path(&args.file);

    let job_id = match routing::start_routing(&ctx, board_path, &project_path, &request) {
        Ok(id) => {
            spinner.finish();
            id
        }
        Err(e) => {
            spinner.error("Failed to start routing");
            anyhow::bail!("{}", e);
        }
    };

    let short_job_id = job_id.split('-').next().unwrap_or(&job_id);
    println!(
        "Job {} started ({} min timeout, $0.50/min)",
        short_job_id.yellow(),
        args.timeout
    );
    println!();

    let mut last_revision: u32 = 0;
    let start_time = Instant::now();
    let mut last_status: Option<RoutingJob> = None;
    let mut consecutive_errors = 0;
    let mut board_opened = false;

    while running.load(Ordering::SeqCst) {
        match routing::get_routing_status(&ctx, &job_id) {
            Ok(status) => {
                consecutive_errors = 0;

                if let Some(ref stats) = status.stats
                    && stats.revision_number > last_revision
                    && status.status != RoutingStatus::Queued
                {
                    let ses_bytes = match download_ses_bytes(&ctx, &job_id) {
                        Ok(b) => b,
                        Err(e) => {
                            println!("{} Failed to download: {}", "!".yellow(), e);
                            continue;
                        }
                    };
                    match import_ses(board_path, &ses_bytes) {
                        Ok(()) => {
                            println!("{}", format_progress(&status, stats.revision_number));
                            last_revision = stats.revision_number;
                            if !args.no_open
                                && !board_opened
                                && pcb_kicad::open_pcbnew(board_path).is_ok()
                            {
                                board_opened = true;
                            }
                        }
                        Err(e) => {
                            println!("{} Failed to apply: {}", "!".yellow(), e);
                        }
                    }
                }

                if matches!(
                    status.status,
                    RoutingStatus::Complete | RoutingStatus::Error
                ) {
                    last_status = Some(status);
                    break;
                }

                if status.converged {
                    println!("{} Converged! Stopping...", "✓".green());
                    let _ = routing::stop_routing(&ctx, &job_id);
                    last_status = Some(status);
                    break;
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    println!("{} Error: {}", "✗".red(), e);
                }
            }
        }

        for _ in 0..30 {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    if !running.load(Ordering::SeqCst) {
        println!();
        println!("Stopping routing job...");
        let _ = routing::stop_routing(&ctx, &job_id);
        println!("{} Stopped. Best result applied to board.", "✓".green());
    }

    println!();
    if let Some(status) = last_status {
        display_summary(&status, start_time.elapsed(), board_path);
    }

    Ok(())
}

fn download_ses_bytes(ctx: &pcb_diode_api::WorkspaceContext, job_id: &str) -> Result<Vec<u8>> {
    routing::download_routing_result(ctx, job_id)
}

// ---------------------------------------------------------------------------
// Local routing via FreeRouting
// ---------------------------------------------------------------------------

fn route_via_local(args: &RouteArgs, board_path: &Path, board_name: &str) -> Result<()> {
    // 1. Check prerequisites
    let java_path = resolve_java()?;
    let fr_jar = find_freerouting_jar(args.fr_jar.as_deref())?;

    // 2. Session files live alongside the board; cleaned up unless --keep
    let board_dir = board_path.parent().unwrap();
    let dsn_path = board_dir.join(format!("{}.dsn", board_name));
    let ses_path = board_dir.join(format!("{}.ses", board_name));

    println!(
        "Local routing {} via FreeRouting",
        board_path.file_name().unwrap().to_string_lossy().green()
    );
    println!("  JAR: {}", fr_jar.display());

    // 3. Check for existing routes
    let (traces, vias) = count_existing_routes(board_path);
    if (traces > 0 || vias > 0) && !args.force {
        anyhow::bail!(
            "Board already has {} traces and {} vias.\n\
             Use --force to clear existing routes and re-route from scratch.",
            traces,
            vias,
        );
    }

    // 4. Back up the board before clearing traces, so we can restore on failure.
    //    The backup is removed on success.
    let backup_path = board_dir.join(format!("{}.bak", board_name));
    let did_clear = args.force;

    if did_clear {
        let spinner = Spinner::builder("Backing up board...").start();
        std::fs::copy(board_path, &backup_path)
            .context("Failed to create board backup before clearing traces")?;
        spinner.finish();

        let spinner = Spinner::builder("Clearing existing traces...").start();
        if let Err(e) = clear_traces(board_path) {
            let _ = std::fs::copy(&backup_path, board_path);
            let _ = std::fs::remove_file(&backup_path);
            anyhow::bail!("Failed to clear traces: {}", e);
        }
        spinner.finish();
    }

    // Install a Ctrl+C handler so interrupting FreeRouting kills the child
    // process. When --force was used, also restore the board from backup.
    let completed = Arc::new(AtomicBool::new(false));
    {
        let restore_board = board_path.to_path_buf();
        let restore_backup = backup_path.clone();
        let done = completed.clone();
        let should_restore = did_clear;
        if let Err(e) = ctrlc::set_handler(move || {
            // Kill FreeRouting first so it doesn't orphan
            if let Ok(guard) = FREEROUTING_PID.lock() {
                if let Some(pid) = *guard {
                    #[cfg(unix)]
                    let _ = std::process::Command::new("kill")
                        .arg(pid.to_string())
                        .status();
                    #[cfg(windows)]
                    let _ = std::process::Command::new("taskkill")
                        .arg("/F")
                        .arg("/PID")
                        .arg(pid.to_string())
                        .status();
                }
            }
            // Only restore backup if --force cleared traces and routing hasn't completed
            if should_restore && !done.load(Ordering::SeqCst) && restore_backup.exists() {
                let _ = std::fs::copy(&restore_backup, &restore_board);
                let _ = std::fs::remove_file(&restore_backup);
            }
            std::process::exit(130);
        }) {
            eprintln!("{} Could not set Ctrl+C handler: {}", "!".yellow(), e);
        }
    }

    // 5. DSN → FreeRouting → SES import
    let result = run_local_route_chain(
        &java_path,
        board_path,
        &dsn_path,
        &ses_path,
        &fr_jar,
        args.fr_timeout,
    );

    // Mark as completed immediately on success so Ctrl+C won't restore backup
    if result.is_ok() {
        completed.store(true, Ordering::SeqCst);
    }

    // If routing failed, restore the backup so the user's board isn't left empty,
    // and clean up session files unless the user asked to keep them.
    if result.is_err() {
        if did_clear && backup_path.exists() {
            let _ = std::fs::copy(&backup_path, board_path);
            let _ = std::fs::remove_file(&backup_path);
        }
        if !args.keep {
            let _ = std::fs::remove_file(&dsn_path);
            let _ = std::fs::remove_file(&ses_path);
        }
        return result;
    }

    // 6. Remove backup and session files on success
    if did_clear {
        let _ = std::fs::remove_file(&backup_path);
    }
    if !args.keep {
        let _ = std::fs::remove_file(&dsn_path);
        let _ = std::fs::remove_file(&ses_path);
    } else {
        println!(
            "  Session files kept at:\n    {}\n    {}",
            dsn_path.display(),
            ses_path.display()
        );
    }

    // 7. Open KiCad
    if !args.no_open {
        let _ = pcb_kicad::open_pcbnew(board_path);
    }

    result
}

/// Verify that a Java 21+ runtime is available on `$PATH`.
/// Resolve a Java 21+ binary, auto-downloading if not found on `$PATH`.
///
/// Checks (in order):
///   1. `java` on `$PATH`
///   2. Cached JDK at `~/.cache/pcb/jdk/21/`
///   3. Auto-download Eclipse Temurin JDK 21 to the cache
fn resolve_java() -> Result<PathBuf> {
    // 1. Check $PATH
    if is_java_21_plus("java") {
        return Ok(PathBuf::from("java"));
    }

    // 2. Check cached JDK (both the binary and the integrity sidecar)
    let cache_dir = java_cache_dir();
    let cache_home = cache_dir.join("home");
    let cached_java = cache_home.join("bin").join("java");
    let cache_valid = cached_java.exists()
        && is_java_21_plus(&cached_java)
        && cache_dir.join("home.sha256").exists();
    if cache_valid {
        return Ok(cached_java);
    }
    // Stale or partial cache — clear so download_jdk_21 starts fresh
    if cache_home.exists() {
        eprintln!("  JDK cache invalid, re-downloading...");
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    // 3. Auto-download Temurin JDK 21
    println!("  Java 21+ not found on $PATH. Downloading JDK 21...");
    download_jdk_21()?;

    if cached_java.exists() && is_java_21_plus(&cached_java) {
        return Ok(cached_java);
    }

    anyhow::bail!(
        "Failed to set up Java 21. Try installing manually:\n\
         macOS:   brew install openjdk@21\n\
         Linux:   apt install openjdk-21-jdk  (or your distro'\''s equivalent)\n\
         Windows: https://adoptium.net/temurin/releases/?version=21\n\
         Then ensure '\''java -version'\'' shows version 21 or later."
    );
}

/// Check whether `java_path` is a Java 21+ binary by running `java -version`.
fn is_java_21_plus(java_path: impl AsRef<Path>) -> bool {
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
    major >= 21
}

/// Directory for the cached JDK: `~/.cache/pcb/jdk/21/`.
fn java_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pcb")
        .join("jdk")
        .join("21")
}

/// Download Eclipse Temurin JDK 21 for the current platform, extract it, and
/// place the JDK home at `java_cache_dir()/home/`.
fn download_jdk_21() -> Result<()> {
    let os = match std::env::consts::OS {
        "macos" => "mac",
        "linux" => "linux",
        "windows" => "windows",
        other => anyhow::bail!("Unsupported OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => anyhow::bail!("Unsupported architecture: {other}"),
    };

    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/21/ga/{os}/{arch}/hotspot/normal/eclipse"
    );

    let cache_dir = java_cache_dir();
    std::fs::create_dir_all(&cache_dir).context("Failed to create JDK cache directory")?;

    // Download to a temp file (JDK tarball is ~50-70 MB, keep in memory is fine
    // given the FreeRouting JAR is already pulled into memory at 80 MB).
    let bytes = download_with_retry(&url, 3)?;

    // Determine archive format from URL (after redirect) — .zip or .tar.gz.
    // Always extract to a temp dir, then rename the JDK home to cache_dir/home/.
    let extract_root = cache_dir.join(".extract");
    if extract_root.exists() {
        let _ = std::fs::remove_dir_all(&extract_root);
    }
    std::fs::create_dir_all(&extract_root)?;

    // Write the downloaded bytes to a temp file so we can inspect it
    let archive_path = cache_dir.join(".jdk-archive");
    std::fs::write(&archive_path, &bytes).context("Failed to write JDK archive to temp file")?;

    let is_zip = bytes
        .first()
        .map(|&b| b == 0x50) // PK\x03\x04 ZIP magic
        .unwrap_or(false);

    if is_zip {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .context("Failed to open JDK zip archive")?;
        zip.extract(&extract_root)
            .context("Failed to extract JDK zip archive")?;
    } else {
        let status = Command::new("tar")
            .arg("xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&extract_root)
            .status()
            .context("Failed to run tar to extract JDK archive")?;
        anyhow::ensure!(status.success(), "tar extraction failed");
    }

    let _ = std::fs::remove_file(&archive_path);

    // Find bin/java inside the extracted tree (the top-level dir is
    // versioned, e.g. jdk-21.0.2+13/).
    let java = find_bin_java(&extract_root)
        .ok_or_else(|| anyhow::anyhow!("JDK archive does not contain bin/java"))?;

    // The JDK home is the parent of bin/.
    let home = java
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| anyhow::anyhow!("Unexpected JDK directory structure"))?;

    // Rename home -> cache_dir/home/ (atomic-ish on same filesystem).
    let dst = cache_dir.join("home");
    if dst.exists() {
        let _ = std::fs::remove_dir_all(&dst);
    }
    std::fs::rename(home, &dst).context("Failed to move JDK home into cache")?;

    // Save sidecar hash for integrity checks on future cache hits
    {
        use sha2::Digest;
        let hash = hex_str(&sha2::Sha256::digest(&bytes));
        std::fs::write(cache_dir.join("home.sha256"), &hash)
            .context("Failed to write JDK cache hash")?;
    }

    // Clean up extraction root
    let _ = std::fs::remove_dir_all(&extract_root);

    println!("  JDK 21 cached at {}", dst.display());
    Ok(())
}

/// Recursively search for `bin/java` inside `dir`.
fn find_bin_java(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_bin_java(&path) {
                return Some(found);
            }
        } else if path.is_file()
            && path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                == Some("bin")
        {
            let fname = path.file_name().and_then(|n| n.to_str());
            let is_java = fname == Some("java") || fname == Some("java.exe");
            if is_java {
                return Some(path);
            }
        }
    }
    None
}

/// Locate the FreeRouting JAR via (in priority order):
///   1. `--fr-jar` flag
///   2. `FREEROUTING_JAR` env var
///   3. `freerouting.jar` on `$PATH`
///   4. Auto-download to `~/.cache/pcb/freerouting/freerouting-2.0.1.jar`
///
/// The cached JAR is verified by SHA-256 before use; corrupted copies trigger a
/// re-download. The download itself retries up to three times with back-off.
fn find_freerouting_jar(provided: Option<&Path>) -> Result<PathBuf> {
    // 1. Explicit --fr-jar flag
    if let Some(path) = provided {
        if path.exists() {
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
            return Ok(p);
        }
        anyhow::bail!("FreeRouting JAR not found at FREEROUTING_JAR={}", path);
    }

    // 3. Search $PATH for freerouting.jar
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("freerouting.jar");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    // 4. Auto-download to cache dir as a last resort
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pcb")
        .join("freerouting");
    let cached = cache_dir.join("freerouting-2.0.1.jar");

    // Verify cached file integrity; discard if corrupted
    if cached.exists() {
        if sha256_hex(&cached) == Some(FREEROUTING_JAR_SHA256.to_string()) {
            return Ok(cached);
        }
        eprintln!("  Cached JAR is corrupted, re-downloading...");
        let _ = std::fs::remove_file(&cached);
    }

    // Download to a temp file, verify hash, then atomically rename
    // Retry up to 3 times with backoff in case of network flakiness.
    println!("  Downloading FreeRouting v2.0.1 (80 MB)...");
    std::fs::create_dir_all(&cache_dir).context("Failed to create FreeRouting cache dir")?;

    let tmp_path = cache_dir.join("freerouting-2.0.1.jar.tmp");
    let bytes = download_with_retry(FREEROUTING_JAR_URL, 3)?;

    // Compute hash before writing to cache
    let actual_hash = {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&bytes);
        hex_str(&hasher.finalize())
    };

    if actual_hash != FREEROUTING_JAR_SHA256 {
        anyhow::bail!(
            "Downloaded FreeRouting JAR has unexpected SHA-256\n\
             Expected: {}\n\
             Actual:   {}\n\
             The download may have been tampered with or the release has changed.\n\
             Download manually from: https://github.com/freerouting/freerouting/releases/tag/v2.0.1",
            FREEROUTING_JAR_SHA256,
            actual_hash,
        );
    }

    std::fs::write(&tmp_path, &bytes).context("Failed to write FreeRouting JAR to temp file")?;
    std::fs::rename(&tmp_path, &cached).context("Failed to move FreeRouting JAR to cache")?;
    println!("  Downloaded to {}", cached.display());

    Ok(cached)
}

/// Compute the hex-encoded SHA-256 digest of a file. Returns `None` if the file
/// cannot be opened or read (e.g., it was truncated mid-write by another process).
fn sha256_hex(path: &Path) -> Option<String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut file, &mut buf).ok()? {
            0 => break,
            n => hasher.update(&buf[..n]),
        }
    }
    Some(hex_str(&hasher.finalize()))
}

/// Format raw digest bytes as a lowercase hex string.
fn hex_str(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download a URL into memory with retry and a simple progress indicator.
/// Prints a dot per ~5 MB downloaded so the user sees activity during the 80 MB transfer.
fn download_with_retry(url: &str, max_attempts: u32) -> Result<Vec<u8>> {
    use std::io::{Read, Write};

    for attempt in 1..=max_attempts {
        let response = match reqwest::blocking::get(url) {
            Ok(r) => match r.error_for_status() {
                Ok(res) => res,
                Err(e) => {
                    if attempt < max_attempts {
                        let delay = Duration::from_secs(2u64.pow(attempt));
                        eprintln!(
                            "\n  Download failed (attempt {attempt}/{max_attempts}), \
                             retrying in {}s: {e}",
                            delay.as_secs()
                        );
                        thread::sleep(delay);
                        continue;
                    }
                    anyhow::bail!("Download failed after {max_attempts} attempts: {e}");
                }
            },
            Err(e) => {
                if attempt < max_attempts {
                    let delay = Duration::from_secs(2u64.pow(attempt));
                    eprintln!(
                        "\n  Download failed (attempt {attempt}/{max_attempts}), \
                         retrying in {}s: {e}",
                        delay.as_secs()
                    );
                    thread::sleep(delay);
                    continue;
                }
                anyhow::bail!("Download failed after {max_attempts} attempts: {e}");
            }
        };

        let total = response.content_length().unwrap_or(0);
        let mut data = Vec::with_capacity(total as usize);
        let mut reader = response.take(200_000_000); // 200 MB safety limit
        let mut buf = [0u8; 65536];
        let mut dot_at: u64 = 0;
        let mut ok = true;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    data.extend_from_slice(&buf[..n]);
                    let mb = data.len() as u64 / 1_000_000;
                    if mb >= dot_at + 5 {
                        print!(".");
                        std::io::stdout().flush().ok();
                        dot_at = mb;
                    }
                }
                Err(e) => {
                    ok = false;
                    if attempt < max_attempts {
                        let delay = Duration::from_secs(2u64.pow(attempt));
                        eprintln!(
                            "\n  Download interrupted (attempt {attempt}/{max_attempts}), \
                             retrying in {}s: {e}",
                            delay.as_secs()
                        );
                        thread::sleep(delay);
                    }
                    break;
                }
            }
        }

        if ok && !data.is_empty() {
            if dot_at > 0 {
                println!();
            }
            return Ok(data);
        }
    }

    anyhow::bail!(
        "Failed to download after {max_attempts} attempts.\n\
         Check your internet connection."
    );
}

/// Export a Specctra DSN session file from the KiCad board via `pcbnew`.
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

/// Count existing traces and vias in the `.kicad_pcb` S-expression file.
/// Used to advise the user when `--force` is missing on a pre-routed board.
fn count_existing_routes(board_path: &Path) -> (usize, usize) {
    let content = match std::fs::read_to_string(board_path) {
        Ok(c) => c,
        Err(_) => return (0, 0),
    };

    let traces = content
        .lines()
        .filter(|l| {
            let s = l.trim_start();
            s.starts_with("(segment") || s.starts_with("(arc")
        })
        .count();

    let vias = content
        .lines()
        .filter(|l| {
            let s = l.trim_start();
            s.starts_with("(via") && !s.starts_with("(via_")
        })
        .count();

    (traces, vias)
}

/// Delete all tracks and vias from the board via `pcbnew` so FreeRouting can
/// re-route from a clean slate.
fn clear_traces(board_path: &Path) -> Result<()> {
    let script = r#"
import pcbnew
import sys
board = pcbnew.LoadBoard(sys.argv[1])
for t in list(board.GetTracks()):
    board.Delete(t)
pcbnew.SaveBoard(sys.argv[1], board)
"#;

    PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .run()
        .context("Failed to clear existing traces from board")?;

    Ok(())
}

/// Run the full DSN → FreeRouting → SES import chain. On success the board has
/// been re-routed and saved; on failure it has not been modified.
fn run_local_route_chain(
    java_path: &Path,
    board_path: &Path,
    dsn_path: &Path,
    ses_path: &Path,
    fr_jar: &Path,
    fr_timeout: u64,
) -> Result<()> {
    let spinner = Spinner::builder("Exporting DSN...").start();
    export_dsn(board_path, dsn_path)?;
    spinner.finish();

    let start_time = Instant::now();
    run_freerouting(java_path, fr_jar, dsn_path, ses_path, fr_timeout)?;

    let spinner = Spinner::builder("Importing SES...").start();
    let ses_bytes =
        std::fs::read(ses_path).context("Failed to read SES file produced by FreeRouting")?;
    import_ses(board_path, &ses_bytes)?;
    spinner.finish();

    let elapsed = start_time.elapsed();
    println!("  Time:       {}", format_duration(elapsed));
    println!();
    println!(
        "Result saved to {}",
        board_path.display().to_string().cyan()
    );

    Ok(())
}

/// Launch headless FreeRouting with the given DSN input and SES output paths.
/// stdout and stderr are drained in background threads so the OS pipe buffer
/// never fills up (which would deadlock FreeRouting). A heartbeat dot is
/// printed every 10 s while the router runs.
fn run_freerouting(
    java_path: &Path,
    jar_path: &Path,
    dsn_path: &Path,
    ses_path: &Path,
    timeout_secs: u64,
) -> Result<()> {
    println!("  Running FreeRouting (timeout: {}s)...", timeout_secs);

    // FreeRouting's internal timeout in MM:SS format. Always at least 1 minute
    // so a value of 00:00:00 doesn't disable the timeout entirely, and at most
    // 59 minutes so the minute field never overflows into the hour position.
    let fr_timeout_mins = (timeout_secs / 60).clamp(1, 59);

    let mut child = Command::new(java_path)
        .arg("-jar")
        .arg(jar_path)
        .arg("-de")
        .arg(dsn_path)
        .arg("-do")
        .arg(ses_path)
        .arg("--gui.enabled=false")
        .arg("--api_server.enabled=false")
        .arg("--router.max_passes=200")
        .arg(format!("--router.job_timeout=00:{:02}:00", fr_timeout_mins))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to launch FreeRouting (java -jar)")?;

    *FREEROUTING_PID.lock().unwrap() = Some(child.id());
    let result = run_freerouting_inner(&mut child, ses_path, timeout_secs);
    *FREEROUTING_PID.lock().unwrap() = None;
    result
}

/// Inner FreeRouting loop (after spawn), extracted so PID cleanup always runs.
fn run_freerouting_inner(
    child: &mut std::process::Child,
    ses_path: &Path,
    timeout_secs: u64,
) -> Result<()> {
    use std::io::Write;

    let timeout_dur = Duration::from_secs(timeout_secs);

    // Drain stdout/stderr in background threads so the OS pipe buffers never fill
    // up — otherwise FreeRouting blocks on write() and hangs.
    let mut child_stdout = child.stdout.take().unwrap();
    let mut child_stderr = child.stderr.take().unwrap();

    let captured_stdout: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let captured_stderr: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let out = captured_stdout.clone();
    let out_thread = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut child_stdout, &mut buf) {
                Ok(0) => break,
                Ok(n) => out
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
    });

    let err = captured_stderr.clone();
    let err_thread = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut child_stderr, &mut buf) {
                Ok(0) => break,
                Ok(n) => err
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
    });

    let start = Instant::now();
    let mut last_output = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Reader threads finish once the child closes its pipe ends
                let _ = out_thread.join();
                let _ = err_thread.join();
                let stdout = captured_stdout.lock().unwrap().clone();
                let stderr = captured_stderr.lock().unwrap().clone();

                if !status.success() {
                    anyhow::bail!(
                        "FreeRouting exited with status {}{}{}",
                        status,
                        if stdout.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", stdout.trim())
                        },
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\n{}", stderr.trim())
                        },
                    );
                }
                if !stdout.is_empty() {
                    println!("  FreeRouting:\n{}", stdout.trim());
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() > timeout_dur {
                    let _ = child.kill();
                    let _ = child.wait();
                    // Reader threads see EOF from killed child and finish
                    let _ = out_thread.join();
                    let _ = err_thread.join();
                    anyhow::bail!("FreeRouting timed out after {}s", timeout_secs);
                }
                // Print a heartbeat every 10s so the user knows it's alive
                if last_output.elapsed() > Duration::from_secs(10) {
                    print!("  .");
                    std::io::stdout().flush().ok();
                    last_output = Instant::now();
                }
                thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out_thread.join();
                let _ = err_thread.join();
                anyhow::bail!("Error polling FreeRouting process: {}", e);
            }
        }
    }
    println!();

    if !ses_path.exists() {
        anyhow::bail!(
            "FreeRouting completed but no SES output found at {}",
            ses_path.display()
        );
    }

    println!(
        "  FreeRouting finished in {:.1}s",
        start.elapsed().as_secs_f64()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// SES import (shared between cloud and local paths)
// ---------------------------------------------------------------------------

fn import_ses(board_path: &Path, ses_bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let mut temp_file = tempfile::NamedTempFile::new()?;
    temp_file.write_all(ses_bytes)?;
    let ses_path = temp_file.path().to_path_buf();

    let script = r#"
import pcbnew
import sys

brd_filename = sys.argv[1]
ses_filename = sys.argv[2]
brd = pcbnew.LoadBoard(brd_filename)
pcbnew.ImportSpecctraSES(brd, ses_filename)

filler = pcbnew.ZONE_FILLER(brd)
filler.Fill(brd.Zones())

pcbnew.SaveBoard(brd_filename, brd)
"#;

    PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .arg(ses_path.to_string_lossy())
        .run()
        .context("Failed to import SES file")?;

    Ok(())
}

fn format_progress(status: &RoutingJob, revision: u32) -> String {
    if let Some(ref stats) = status.stats {
        let sep = "·".dimmed();
        format!(
            "{:>3}  {:>2}/{:<2} nets {} {:>3}/{:<3} air {} {:>2} vias {} {:>6.1} mm",
            format!("#{}", revision).cyan().bold(),
            stats.nets_completed,
            stats.total_nets,
            sep,
            stats.air_wires_connected,
            stats.air_wires_total,
            sep,
            stats.vias,
            sep,
            stats.wire_length / 1000.0
        )
    } else {
        format!("{}", format!("#{}", revision).cyan().bold())
    }
}

fn display_summary(status: &RoutingJob, elapsed: Duration, board_path: &Path) {
    let cost = elapsed.as_secs_f64() / 60.0 * 0.5;

    if let Some(ref stats) = status.stats {
        println!("{}", "Routing complete".green().bold());
        println!(
            "  Nets:       {}/{}",
            stats.nets_completed, stats.total_nets
        );
        println!(
            "  Air wires:  {}/{}",
            stats.air_wires_connected, stats.air_wires_total
        );
        println!("  Vias:       {}", stats.vias);
        println!("  Wire:       {:.1} mm", stats.wire_length / 1000.0);
        println!("  Time:       {}", format_duration(elapsed));
        println!("  Cost:       ${:.2}", cost);
    }

    println!();
    println!(
        "Result saved to {}",
        board_path.display().to_string().cyan()
    );
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_bin_java_finds_java() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("java"), "").unwrap();
        assert!(find_bin_java(dir.path()).is_some());
    }

    #[test]
    fn find_bin_java_finds_java_exe() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("java.exe"), "").unwrap();
        assert!(find_bin_java(dir.path()).is_some());
    }

    #[test]
    fn find_bin_java_ignores_non_bin() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("java"), "").unwrap();
        assert!(find_bin_java(dir.path()).is_none());
    }

    #[test]
    fn find_bin_java_returns_none_for_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_bin_java(dir.path()).is_none());
    }

    #[test]
    fn is_java_21_plus_returns_false_for_nonexistent_path() {
        assert!(!is_java_21_plus("/nonexistent/java"));
    }

    #[test]
    fn count_existing_routes_inline_via() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("test.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb (version 20230121)
  (setup
    (via_drill 0.3)
    (via_size 0.6)
  )
  (net 0)
  (via (at 10 20) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net 0))
  (segment (start 0 0) (end 10 10) (width 0.25) (layer \"F.Cu\") (net 0))
)
",
        )
        .unwrap();
        let (traces, vias) = count_existing_routes(&board);
        assert_eq!(traces, 1, "should count one segment");
        assert_eq!(
            vias, 1,
            "should count one inline via, not via_drill/via_size"
        );
    }

    #[test]
    fn count_existing_routes_multi_line_via() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("test.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb (version 20230121)
  (setup
    (via_drill 0.3)
  )
  (net 0)
  (via
    (at 10 20)
    (size 0.6)
    (drill 0.3)
    (layers \"F.Cu\" \"B.Cu\")
    (net 0)
  )
  (segment (start 0 0) (end 10 10) (width 0.25) (layer \"F.Cu\") (net 0))
)
",
        )
        .unwrap();
        let (traces, vias) = count_existing_routes(&board);
        assert_eq!(traces, 1, "should count one segment");
        assert_eq!(vias, 1, "should count one multi-line via");
    }

    #[test]
    fn count_existing_routes_arc() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("test.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb (version 20230121)
  (net 0)
  (arc (start 0 0) (mid 5 10) (end 10 0) (width 0.25) (layer \"F.Cu\") (net 0))
)
",
        )
        .unwrap();
        let (traces, vias) = count_existing_routes(&board);
        assert_eq!(traces, 1, "should count one arc as a trace");
        assert_eq!(vias, 0, "should have no vias");
    }

    #[test]
    fn count_existing_routes_empty_board() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("test.kicad_pcb");
        std::fs::write(
            &board,
            "(kicad_pcb (version 20230121)
  (setup
    (via_drill 0.3)
    (via_size 0.6)
  )
)
",
        )
        .unwrap();
        let (traces, vias) = count_existing_routes(&board);
        assert_eq!(traces, 0, "no segments on empty board");
        assert_eq!(vias, 0, "via_drill/via_size should not count as vias");
    }

    #[test]
    fn count_existing_routes_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let board = dir.path().join("nonexistent.kicad_pcb");
        let (traces, vias) = count_existing_routes(&board);
        assert_eq!(traces, 0);
        assert_eq!(vias, 0);
    }
}
