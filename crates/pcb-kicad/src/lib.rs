pub mod drc;
pub mod erc;
pub mod footprint;

use anyhow::{Context, Result, anyhow};
use pcb_command_runner::CommandRunner;
use pcb_sexpr::Sexpr;
use pcb_zen_core::Diagnostics;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use tempfile::NamedTempFile;

fn expand_home(path: &str) -> String {
    path.replace(
        "~",
        dirs::home_dir()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default(),
    )
}

fn env_or_path(env_var: &str, default: &str) -> String {
    expand_home(&std::env::var(env_var).unwrap_or_else(|_| default.to_string()))
}

#[cfg(target_os = "windows")]
fn first_existing_path(candidates: &[&str]) -> String {
    candidates
        .iter()
        .map(|path| expand_home(path))
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| expand_home(candidates[0]))
}

#[cfg(target_os = "windows")]
fn env_or_first_existing_path(env_var: &str, candidates: &[&str]) -> String {
    std::env::var(env_var)
        .map(|path| expand_home(&path))
        .unwrap_or_else(|_| first_existing_path(candidates))
}

fn require_tool_path(
    path: String,
    tool_name: &str,
    env_var: &str,
    install_hint: &str,
) -> Result<String> {
    if Path::new(&path).exists() {
        Ok(path)
    } else {
        Err(anyhow!(
            "{tool_name} not found at expected location: {path}\n\
             {install_hint}\n\
             If {tool_name} is in a non-standard location, set the {env_var} environment variable."
        ))
    }
}

#[cfg(target_os = "macos")]
fn pcbnew_app_bundle_path(pcbnew_path: &str) -> Result<String> {
    let path = Path::new(pcbnew_path);

    if path.extension().is_some_and(|ext| ext == "app") {
        return Ok(pcbnew_path.to_string());
    }

    path.ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|ext| ext == "app"))
        .map(|ancestor| ancestor.to_string_lossy().to_string())
        .ok_or_else(|| {
            anyhow!(
                "Failed to derive pcbnew.app bundle path from {}.\n\
                 Set KICAD_PCBNEW to either the pcbnew.app bundle or the pcbnew binary inside it.",
                pcbnew_path
            )
        })
}

#[cfg(target_os = "macos")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        super::env_or_path(
            "KICAD_PYTHON_INTERPRETER",
            "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3",
        )
    }

    pub(crate) fn python_site_packages() -> String {
        super::env_or_path(
            "KICAD_PYTHON_SITE_PACKAGES",
            "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/lib/python3.9/site-packages",
        )
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
        super::env_or_path(
            "KICAD_CLI",
            "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli",
        )
    }

    pub(crate) fn pcbnew() -> String {
        super::env_or_path(
            "KICAD_PCBNEW",
            "/Applications/KiCad/KiCad.app/Contents/Applications/pcbnew.app/Contents/MacOS/pcbnew",
        )
    }
}

#[cfg(target_os = "windows")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        super::env_or_first_existing_path(
            "KICAD_PYTHON_INTERPRETER",
            &[
                r"C:\Program Files\KiCad\10.0\bin\python.exe",
                r"C:\Program Files\KiCad\9.0\bin\python.exe",
            ],
        )
    }

    pub(crate) fn python_site_packages() -> String {
        super::env_or_first_existing_path(
            "KICAD_PYTHON_SITE_PACKAGES",
            &[
                r"~\Documents\KiCad\10.0\3rdparty\Python311\site-packages",
                r"~\Documents\KiCad\9.0\3rdparty\Python311\site-packages",
            ],
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
        super::env_or_first_existing_path(
            "KICAD_CLI",
            &[
                r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe",
                r"C:\Program Files\KiCad\9.0\bin\kicad-cli.exe",
            ],
        )
    }

    pub(crate) fn pcbnew() -> String {
        super::env_or_first_existing_path(
            "KICAD_PCBNEW",
            &[
                r"C:\Program Files\KiCad\10.0\bin\pcbnew.exe",
                r"C:\Program Files\KiCad\9.0\bin\pcbnew.exe",
            ],
        )
    }
}

#[cfg(target_os = "linux")]
mod paths {
    pub(crate) fn python_interpreter() -> String {
        super::env_or_path("KICAD_PYTHON_INTERPRETER", "/usr/bin/python3")
    }

    pub(crate) fn python_site_packages() -> String {
        super::env_or_path(
            "KICAD_PYTHON_SITE_PACKAGES",
            "/usr/lib/python3/dist-packages",
        )
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
        super::env_or_path("KICAD_CLI", "/usr/bin/kicad-cli")
    }

    pub(crate) fn pcbnew() -> String {
        super::env_or_path("KICAD_PCBNEW", "/usr/bin/pcbnew")
    }
}

/// Check if KiCad is installed and return a helpful error if not
fn check_kicad_installed() -> Result<()> {
    let kicad_path = require_tool_path(
        paths::kicad_cli(),
        "KiCad CLI",
        "KICAD_CLI",
        "Please ensure KiCad is installed. You can download it from https://www.kicad.org/",
    )?;

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
    let python_path = require_tool_path(
        paths::python_interpreter(),
        "KiCad Python interpreter",
        "KICAD_PYTHON_INTERPRETER",
        "Please ensure KiCad is installed with Python support.",
    )?;

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

fn version_major(version_text: &str) -> Option<u32> {
    version_text
        .split(|c: char| !c.is_ascii_digit())
        .find(|part| !part.is_empty())?
        .parse()
        .ok()
}

fn read_board_kicad_major_version(pcb_path: &Path) -> Result<Option<u32>> {
    if !pcb_path.exists() {
        return Ok(None);
    }

    let file = File::open(pcb_path)
        .with_context(|| format!("Failed to read PCB file: {}", pcb_path.display()))?;
    let mut version = None;
    pcb_sexpr::walk_stream(BufReader::new(file), |node| {
        let Some(items) = node.as_list() else {
            return true;
        };
        if items.first().and_then(Sexpr::as_sym) == Some("generator_version") {
            version = items.get(1).and_then(Sexpr::as_str).and_then(version_major);
            return false;
        }
        true
    })
    .with_context(|| format!("Failed to parse PCB file: {}", pcb_path.display()))?;

    Ok(version)
}

fn installed_kicad_major_version() -> Result<Option<u32>> {
    Ok(version_major(&get_kicad_version()?))
}

pub fn get_kicad_version() -> Result<String> {
    let output = KiCadCliBuilder::new()
        .command("version")
        .output()
        .context("Failed to detect KiCad version")?;

    if !output.status.success() {
        anyhow::bail!("Failed to detect KiCad version");
    }

    String::from_utf8(output.stdout)
        .map(|version| version.trim().to_string())
        .context("Failed to parse KiCad version output")
}

pub fn ensure_board_compatible_with_installed_kicad(pcb_path: &Path) -> Result<()> {
    let Some(board_major) = read_board_kicad_major_version(pcb_path)? else {
        return Ok(());
    };

    let Some(installed_major) = installed_kicad_major_version()? else {
        return Ok(());
    };

    if board_major > installed_major {
        anyhow::bail!(
            "{} requires KiCad {}; found {} locally. Upgrade KiCad.",
            pcb_path.display(),
            board_major,
            installed_major
        );
    }

    Ok(())
}

/// Open a KiCad board in the GUI that matches this toolchain's discovered install.
pub fn open_pcbnew(pcb_path: impl AsRef<Path>) -> Result<()> {
    let pcb_path = pcb_path.as_ref();
    if !pcb_path.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_path.display());
    }

    let pcbnew_path = require_tool_path(
        paths::pcbnew(),
        "KiCad PCB Editor",
        "KICAD_PCBNEW",
        "Please ensure KiCad is installed.",
    )?;

    #[cfg(target_os = "macos")]
    let mut cmd = {
        let pcbnew_app = pcbnew_app_bundle_path(&pcbnew_path)?;
        let mut cmd = Command::new("open");
        cmd.arg("-a").arg(pcbnew_app).arg(pcb_path);
        cmd
    };

    #[cfg(not(target_os = "macos"))]
    let mut cmd = {
        let mut cmd = Command::new(&pcbnew_path);
        cmd.arg(pcb_path);
        cmd
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to launch KiCad PCB Editor at {} for {}",
                pcbnew_path,
                pcb_path.display()
            )
        })?;

    Ok(())
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

/// Run KiCad DRC checks, write the raw KiCad JSON report to `output_path`, and return the parsed report.
///
/// Set `schematic_parity=true` to have KiCad include schematic-vs-layout parity diagnostics
/// (useful for validating the PCB is in sync with the schematic).
pub fn run_drc(
    pcb_path: impl AsRef<Path>,
    schematic_parity: bool,
    working_dir: Option<&Path>,
    output_path: &Path,
) -> Result<drc::DrcReport> {
    check_kicad_installed()?;

    let pcb_path = pcb_path.as_ref();
    if !pcb_path.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_path.display());
    }

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
        .arg(output_path.to_string_lossy())
        .arg(pcb_path.to_string_lossy());

    if let Some(dir) = working_dir {
        builder = builder.current_dir(dir.to_string_lossy().to_string());
    }

    builder.run().context("Failed to run KiCad DRC")?;

    drc::DrcReport::from_file(output_path).context("Failed to parse DRC report")
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

#[cfg(test)]
mod tests {
    use super::{read_board_kicad_major_version, version_major};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn version_major_extracts_first_number() {
        assert_eq!(version_major("10.0"), Some(10));
        assert_eq!(version_major("9.0.8"), Some(9));
        assert_eq!(version_major("KiCad CLI 10.0.1"), Some(10));
        assert_eq!(version_major("unknown"), None);
    }

    #[test]
    fn read_board_kicad_major_version_from_pcb_file() {
        let temp = tempdir().expect("tempdir");
        let pcb_path = temp.path().join("layout.kicad_pcb");
        fs::write(
            &pcb_path,
            "(kicad_pcb\n\t(generator \"pcbnew\")\n\t(generator_version \"10.0\")\n)\n",
        )
        .expect("write pcb");

        assert_eq!(
            read_board_kicad_major_version(&pcb_path).expect("read board version"),
            Some(10)
        );
    }

    #[test]
    fn read_board_kicad_major_version_stops_after_header() {
        let temp = tempdir().expect("tempdir");
        let pcb_path = temp.path().join("layout.kicad_pcb");
        let mut pcb =
            b"(kicad_pcb\n\t(generator \"pcbnew\")\n\t(generator_version \"10.0\")\n".to_vec();
        pcb.extend_from_slice(&[0xff, 0xfe, 0xfd]);
        fs::write(&pcb_path, pcb).expect("write pcb");

        assert_eq!(
            read_board_kicad_major_version(&pcb_path).expect("read board version"),
            Some(10)
        );
    }
}
