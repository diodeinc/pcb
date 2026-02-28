use anyhow::{Context, Result, anyhow};
use pcb_command_runner::CommandRunner;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "macos")]
fn ngspice_path() -> String {
    let path = std::env::var("NGSPICE").unwrap_or_default();
    if !path.is_empty() {
        return path.replace(
            "~",
            dirs::home_dir()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default(),
        );
    }
    for candidate in ["/opt/homebrew/bin/ngspice", "/usr/local/bin/ngspice"] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    // Fall back to first candidate so the error message is helpful
    "/opt/homebrew/bin/ngspice".to_string()
}

#[cfg(target_os = "windows")]
fn ngspice_path() -> String {
    std::env::var("NGSPICE")
        .unwrap_or_else(|_| r"C:\Program Files\ngspice\bin\ngspice.exe".to_string())
        .replace(
            "~",
            dirs::home_dir()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default(),
        )
}

#[cfg(target_os = "linux")]
fn ngspice_path() -> String {
    std::env::var("NGSPICE")
        .unwrap_or_else(|_| "/usr/bin/ngspice".to_string())
        .replace(
            "~",
            dirs::home_dir()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default(),
        )
}

fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "You can install it with: brew install ngspice"
    } else if cfg!(target_os = "windows") {
        "You can download it from: https://ngspice.sourceforge.io/download.html"
    } else {
        "You can install it with: sudo apt install ngspice"
    }
}

/// Result of running ngspice in captured mode (for LSP/programmatic use).
pub struct SimulationResult {
    pub success: bool,
    /// Plain text output (ANSI stripped).
    pub output: String,
}

/// Check if ngspice is installed and return a helpful error if not.
pub fn check_ngspice_installed() -> Result<String> {
    let path = ngspice_path();

    if !Path::new(&path).exists() {
        return Err(anyhow!(
            "ngspice not found at expected location: {}\n\
             {}\n\
             If ngspice is installed in a non-standard location, set the NGSPICE environment variable.",
            path,
            install_hint()
        ));
    }

    match Command::new(&path).arg("--version").output() {
        Ok(output) if output.status.success() => Ok(path),
        Ok(_) => Err(anyhow!(
            "ngspice found at {} but failed to execute. Please check your installation.",
            path
        )),
        Err(e) => Err(anyhow!(
            "Failed to execute ngspice at {}: {}\n\
             {}",
            path,
            e,
            install_hint()
        )),
    }
}

/// Default timeout for ngspice simulations (5 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Run ngspice in batch mode, capturing output.
///
/// `work_dir` sets the working directory for ngspice (e.g. the directory of the
/// `.zen` source file so that relative includes resolve correctly).
///
/// The process is killed after 30 seconds to prevent hanging the caller (e.g. LSP).
pub fn run_ngspice_captured(cir_path: &Path, work_dir: &Path) -> Result<SimulationResult> {
    let ngspice = check_ngspice_installed()?;

    let output = CommandRunner::new(&ngspice)
        .arg("-b")
        .arg(cir_path.to_string_lossy())
        .current_dir(work_dir.to_string_lossy())
        .timeout(DEFAULT_TIMEOUT)
        .run()
        .context("Failed to execute ngspice")?;

    Ok(SimulationResult {
        success: output.success,
        output: output.plain_as_string(),
    })
}
