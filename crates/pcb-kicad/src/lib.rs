pub mod drc;
pub mod erc;

use anyhow::{anyhow, Context, Result};
use pcb_command_runner::CommandRunner;
use pcb_zen_core::Diagnostics;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

#[cfg(target_os = "macos")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        std::env::var("KICAD_PYTHON_INTERPRETER").unwrap_or_else(|_|
            "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3".to_string()).replace("~", dirs::home_dir().unwrap_or_default().to_str().unwrap_or_default())
    }

    pub(crate) fn python_site_packages() -> String {
        std::env::var("KICAD_PYTHON_SITE_PACKAGES").unwrap_or_else(|_|
            "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/lib/python3.9/site-packages".to_string()).replace("~", dirs::home_dir().unwrap_or_default().to_str().unwrap_or_default())
    }

    pub(crate) fn venv_site_packages() -> String {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".diode")
            .join("venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages")
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn kicad_cli() -> String {
        std::env::var("KICAD_CLI")
            .unwrap_or_else(|_| {
                "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli".to_string()
            })
            .replace(
                "~",
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default(),
            )
    }
}

#[cfg(target_os = "windows")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        std::env::var("KICAD_PYTHON_INTERPRETER")
            .unwrap_or_else(|_| r"C:\Program Files\KiCad\9.0\bin\python.exe".to_string())
    }

    pub(crate) fn python_site_packages() -> String {
        std::env::var("KICAD_PYTHON_SITE_PACKAGES")
            .unwrap_or_else(|_| {
                r"~\Documents\KiCad\9.0\3rdparty\Python311\site-packages".to_string()
            })
            .replace(
                "~",
                dirs::home_dir()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default(),
            )
    }

    pub(crate) fn venv_site_packages() -> String {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".diode")
            .join("venv")
            .join("Lib")
            .join("site-packages")
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn kicad_cli() -> String {
        std::env::var("KICAD_CLI")
            .unwrap_or_else(|_| r"C:\Program Files\KiCad\9.0\bin\kicad-cli.exe".to_string())
    }
}

#[cfg(target_os = "linux")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        std::env::var("KICAD_PYTHON_INTERPRETER").unwrap_or_else(|_| "/usr/bin/python3".to_string())
    }

    pub(crate) fn python_site_packages() -> String {
        std::env::var("KICAD_PYTHON_SITE_PACKAGES")
            .unwrap_or_else(|_| "/usr/lib/python3/dist-packages".to_string())
    }

    pub(crate) fn venv_site_packages() -> String {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".diode")
            .join("venv")
            .join("lib")
            .join("python3.12")
            .join("site-packages")
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn kicad_cli() -> String {
        std::env::var("KICAD_CLI").unwrap_or_else(|_| "/usr/bin/kicad-cli".to_string())
    }
}

/// Check if KiCad is installed and return a helpful error if not
fn check_kicad_installed() -> Result<()> {
    let kicad_path = paths::kicad_cli();

    // First check if the file exists
    if !Path::new(&kicad_path).exists() {
        return Err(anyhow!(
            "KiCad CLI not found at expected location: {}\n\
             Please ensure KiCad is installed. You can download it from https://www.kicad.org/\n\
             If KiCad is installed in a non-standard location, set the KICAD_CLI environment variable.",
            kicad_path
        ));
    }

    // Try to run kicad-cli --version to verify it's executable
    match Command::new(&kicad_path).arg("--version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => Err(anyhow!(
            "KiCad CLI found but failed to execute. Please check your KiCad installation."
        )),
        Err(e) => Err(anyhow!(
            "Failed to execute KiCad CLI at {}: {}\n\
             Please ensure KiCad is properly installed and accessible.",
            kicad_path,
            e
        )),
    }
}

/// Check if KiCad Python is available and return a helpful error if not
fn check_kicad_python() -> Result<()> {
    let python_path = paths::python_interpreter();

    // First check if the file exists
    if !Path::new(&python_path).exists() {
        return Err(anyhow!(
            "KiCad Python interpreter not found at expected location: {}\n\
             Please ensure KiCad is installed with Python support.\n\
             If KiCad Python is in a non-standard location, set the KICAD_PYTHON_INTERPRETER environment variable.",
            python_path
        ));
    }

    // Try to run python --version to verify it's executable
    match Command::new(&python_path).arg("--version").output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(_) => Err(anyhow!(
            "KiCad Python found but failed to execute. Please check your KiCad installation."
        )),
        Err(e) => Err(anyhow!(
            "Failed to execute KiCad Python at {}: {}\n\
             Please ensure KiCad is properly installed with Python support.",
            python_path,
            e
        )),
    }
}

/// Builder for KiCad CLI commands
#[derive(Debug, Default)]
pub struct KiCadCliBuilder {
    args: Vec<String>,
    log_file: Option<File>,
    env_vars: HashMap<String, String>,
    suppress_error_output: bool,
    current_dir: Option<String>,
}

impl KiCadCliBuilder {
    /// Create a new KiCad CLI command builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a command (e.g., "pcb", "sch", etc.)
    pub fn command(mut self, cmd: &str) -> Self {
        self.args.push(cmd.to_string());
        self
    }

    /// Add a subcommand (e.g., "export", "import", etc.)
    pub fn subcommand(mut self, subcmd: &str) -> Self {
        self.args.push(subcmd.to_string());
        self
    }

    /// Add an argument
    pub fn arg<S: Into<String>>(mut self, arg: S) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(|s| s.into()));
        self
    }

    /// Set a log file for capturing output
    pub fn log_file(mut self, file: File) -> Self {
        self.log_file = Some(file);
        self
    }

    /// Suppress error output to stderr (useful for commands with verbose non-critical output)
    pub fn suppress_error_output(mut self, suppress: bool) -> Self {
        self.suppress_error_output = suppress;
        self
    }

    /// Set the current directory for the command
    pub fn current_dir(mut self, dir: impl Into<String>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Add an environment variable
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Execute the KiCad CLI command
    pub fn run(self) -> Result<()> {
        // Check if KiCad is installed before trying to run
        check_kicad_installed()?;

        let args_refs: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();

        // Build command with environment variables
        let mut cmd = CommandRunner::new(paths::kicad_cli());

        // Add all arguments
        for arg in &args_refs {
            cmd = cmd.arg(*arg);
        }

        if let Some(dir) = &self.current_dir {
            cmd = cmd.current_dir(dir);
        }

        // Add environment variables
        for (key, value) in self.env_vars {
            cmd = cmd.env(key, value);
        }

        // Add log file if provided
        if let Some(log_file) = self.log_file {
            cmd = cmd.log_file(log_file);
        }

        // Run the command
        let output = cmd.run().context("Failed to execute kicad-cli")?;

        if !output.success {
            if !self.suppress_error_output {
                std::io::stderr().write_all(&output.raw_output)?;
            }
            anyhow::bail!("kicad-cli execution failed");
        }

        Ok(())
    }

    /// Execute the KiCad CLI command and return the output
    pub fn output(self) -> Result<std::process::Output> {
        // Check if KiCad is installed before trying to run
        check_kicad_installed()?;

        let args_refs: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();

        // Build command with environment variables
        let mut cmd = std::process::Command::new(paths::kicad_cli());

        // Add all arguments
        for arg in &args_refs {
            cmd.arg(*arg);
        }

        if let Some(dir) = &self.current_dir {
            cmd.current_dir(dir);
        }

        // Add environment variables
        for (key, value) in self.env_vars {
            cmd.env(key, value);
        }

        // Execute and return output
        cmd.output().context("Failed to execute kicad-cli")
    }
}

/// Direct function for simple KiCad CLI calls
pub fn kicad_cli<I, S>(args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut builder = KiCadCliBuilder::new();
    for arg in args {
        builder = builder.arg(arg.as_ref());
    }
    builder.run()
}

/// Run KiCad DRC checks on a PCB file and add violations to diagnostics
///
/// # Arguments
/// * `pcb_path` - Path to the .kicad_pcb file to check
/// * `diagnostics` - Diagnostics collection to add violations to
///
/// # Example
/// ```no_run
/// use pcb_kicad::run_drc;
/// use pcb_zen_core::Diagnostics;
/// let mut diagnostics = Diagnostics::default();
/// run_drc("layout/layout.kicad_pcb", &mut diagnostics).unwrap();
/// if diagnostics.error_count() > 0 {
///     eprintln!("DRC errors found!");
/// }
/// ```
pub fn run_drc(pcb_path: impl AsRef<Path>, diagnostics: &mut Diagnostics) -> Result<()> {
    let pcb_path = pcb_path.as_ref();
    let report = run_drc_report(pcb_path, false, None).context("Failed to run KiCad DRC")?;

    // Parse and add to diagnostics
    report.add_to_diagnostics(diagnostics, &pcb_path.to_string_lossy());
    Ok(())
}

/// Run KiCad DRC checks and return the parsed JSON report.
///
/// Set `schematic_parity=true` to have KiCad include schematic-vs-layout parity diagnostics
/// (useful for validating the PCB is in sync with the schematic).
pub fn run_drc_report(
    pcb_path: impl AsRef<Path>,
    schematic_parity: bool,
    working_dir: Option<&Path>,
) -> Result<drc::DrcReport> {
    check_kicad_installed()?;

    let pcb_path = pcb_path.as_ref();
    if !pcb_path.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_path.display());
    }

    // Create a temporary file for the JSON output
    let temp_file =
        NamedTempFile::new().context("Failed to create temporary file for DRC output")?;
    let temp_path = temp_file.path();

    // Run kicad-cli pcb drc with JSON output
    let mut builder = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("drc")
        .arg("--format")
        .arg("json")
        .arg("--severity-all") // Report all severities (errors and warnings)
        .arg("--severity-exclusions"); // Include violations excluded by user in KiCad
    if schematic_parity {
        builder = builder.arg("--schematic-parity");
    }

    builder = builder
        .arg("--output")
        .arg(temp_path.to_string_lossy())
        .arg(pcb_path.to_string_lossy());

    if let Some(dir) = working_dir {
        builder = builder.current_dir(dir.to_string_lossy().to_string());
    }

    builder.run().context("Failed to run KiCad DRC")?;

    drc::DrcReport::from_file(temp_path).context("Failed to parse DRC report")
}

/// Run KiCad ERC checks and add violations to diagnostics
pub fn run_erc(schematic_path: impl AsRef<Path>, diagnostics: &mut Diagnostics) -> Result<()> {
    let schematic_path = schematic_path.as_ref();
    let report = run_erc_report(schematic_path, None).context("Failed to run KiCad ERC")?;
    report.add_to_diagnostics(diagnostics, &schematic_path.to_string_lossy());
    Ok(())
}

/// Run KiCad ERC checks and return the parsed JSON report.
pub fn run_erc_report(
    schematic_path: impl AsRef<Path>,
    working_dir: Option<&Path>,
) -> Result<erc::ErcReport> {
    check_kicad_installed()?;

    let schematic_path = schematic_path.as_ref();
    if !schematic_path.exists() {
        anyhow::bail!("Schematic file not found: {}", schematic_path.display());
    }

    // Create a temporary file for the JSON output
    let temp_file =
        NamedTempFile::new().context("Failed to create temporary file for ERC output")?;
    let temp_path = temp_file.path();

    // Run kicad-cli sch erc with JSON output
    let mut builder = KiCadCliBuilder::new()
        .command("sch")
        .subcommand("erc")
        .arg("--format")
        .arg("json")
        .arg("--severity-all") // Report all severities (errors and warnings)
        .arg("--severity-exclusions") // Include violations excluded by user in KiCad
        .arg("--output")
        .arg(temp_path.to_string_lossy())
        .arg(schematic_path.to_string_lossy());

    if let Some(dir) = working_dir {
        builder = builder.current_dir(dir.to_string_lossy().to_string());
    }

    builder.run().context("Failed to run KiCad ERC")?;

    erc::ErcReport::from_file(temp_path).context("Failed to parse ERC report")
}

/// Builder pattern for Python script execution in the KiCad Python environment
#[derive(Debug, Default)]
pub struct PythonScriptBuilder {
    script: String,
    args: Vec<String>,
    log_file: Option<File>,
    env_vars: HashMap<String, String>,
    extra_python_paths: Vec<String>,
}

impl PythonScriptBuilder {
    /// Create a new Python script builder with the given script content
    pub fn new(script: impl Into<String>) -> Self {
        Self {
            script: script.into(),
            ..Default::default()
        }
    }

    /// Create a builder from a script file
    pub fn from_file(path: &Path) -> Result<Self> {
        let script = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Python script from {path:?}"))?;
        Ok(Self::new(script))
    }

    /// Add an extra directory to PYTHONPATH
    ///
    /// This allows the script to import modules from the specified directory.
    pub fn python_path<S: Into<String>>(mut self, path: S) -> Self {
        self.extra_python_paths.push(path.into());
        self
    }

    /// Add a command-line argument for the script
    pub fn arg<S: Into<String>>(mut self, arg: S) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add multiple arguments
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(|s| s.into()));
        self
    }

    /// Set a log file for capturing output
    pub fn log_file(mut self, file: File) -> Self {
        self.log_file = Some(file);
        self
    }

    /// Add an environment variable
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.env_vars.insert(key.into(), value.into());
        self
    }

    /// Execute the script in the KiCad Python environment
    pub fn run(self) -> Result<()> {
        check_kicad_python()?;

        // Create a temporary file for the script
        let mut temp_file =
            NamedTempFile::new().context("Failed to create temporary file for Python script")?;

        temp_file
            .write_all(self.script.as_bytes())
            .context("Failed to write Python script to temporary file")?;

        let temp_file_path = temp_file
            .path()
            .to_str()
            .ok_or_else(|| anyhow!("Failed to convert temporary file path to string"))?;

        // Set up PYTHONPATH
        #[cfg(target_os = "windows")]
        let path_separator = ";";
        #[cfg(not(target_os = "windows"))]
        let path_separator = ":";

        // Build PYTHONPATH: extra paths first, then system paths
        let mut python_path_parts = self.extra_python_paths;
        python_path_parts.push(paths::python_site_packages());
        python_path_parts.push(paths::venv_site_packages());
        let python_path = python_path_parts.join(path_separator);

        // Build the command
        let mut cmd = CommandRunner::new(paths::python_interpreter()).arg(temp_file_path);

        // Add script arguments
        for arg in &self.args {
            cmd = cmd.arg(arg);
        }

        // Set PYTHONPATH
        cmd = cmd.env("PYTHONPATH", python_path);

        // Add custom environment variables
        for (key, value) in self.env_vars {
            cmd = cmd.env(key, value);
        }

        // Add log file if provided
        if let Some(log_file) = self.log_file {
            cmd = cmd.log_file(log_file);
        }

        // Run the command
        let output = cmd.run().context("Failed to execute Python script")?;

        if !output.success {
            std::io::stderr().write_all(&output.raw_output)?;
            anyhow::bail!("Python script execution failed");
        }

        Ok(())
    }
}
