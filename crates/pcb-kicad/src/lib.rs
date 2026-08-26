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
use std::process::{Child, Command, Stdio};
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

struct PlatformDefaults {
    python_interpreter: &'static [&'static str],
    kicad_cli: &'static [&'static str],
    kicad_cli_command: &'static str,
    pcbnew: &'static [&'static str],
}

struct KiCadInstallation {
    python_interpreter: String,
    python_site_packages: Option<String>,
    kicad_cli: String,
    pcbnew: String,
}

impl KiCadInstallation {
    fn discover() -> Self {
        let defaults = platform_defaults();
        Self {
            python_interpreter: discover_path(
                "KICAD_PYTHON_INTERPRETER",
                None,
                defaults.python_interpreter,
            ),
            // Native KiCad interpreters already include pcbnew in their import path.
            python_site_packages: std::env::var("KICAD_PYTHON_SITE_PACKAGES")
                .ok()
                .map(|path| expand_home(&path)),
            kicad_cli: discover_path(
                "KICAD_CLI",
                Some(defaults.kicad_cli_command),
                defaults.kicad_cli,
            ),
            pcbnew: discover_path("KICAD_PCBNEW", None, defaults.pcbnew),
        }
    }

    fn python_path(&self, extra_paths: Vec<String>) -> Result<String> {
        let mut paths = extra_paths;
        paths.extend(self.python_site_packages.iter().cloned());
        Ok(std::env::join_paths(paths)
            .context("Failed to construct PYTHONPATH")?
            .to_string_lossy()
            .into_owned())
    }
}

fn discover_path(env_var: &str, command: Option<&str>, candidates: &[&str]) -> String {
    if let Ok(path) = std::env::var(env_var) {
        return expand_home(&path);
    }

    if let Some(command) = command
        && std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|path| path.join(command).exists())
        })
    {
        return command.to_string();
    }

    candidates
        .iter()
        .map(|path| expand_home(path))
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| expand_home(candidates[0]))
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
fn platform_defaults() -> PlatformDefaults {
    PlatformDefaults {
        python_interpreter: &[
            "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3",
        ],
        kicad_cli: &["/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"],
        kicad_cli_command: "kicad-cli",
        pcbnew: &[
            "/Applications/KiCad/KiCad.app/Contents/Applications/pcbnew.app/Contents/MacOS/pcbnew",
        ],
    }
}

#[cfg(target_os = "windows")]
fn platform_defaults() -> PlatformDefaults {
    PlatformDefaults {
        python_interpreter: &[
            r"C:\Program Files\KiCad\10.0\bin\python.exe",
            r"C:\Program Files\KiCad\9.0\bin\python.exe",
        ],
        kicad_cli: &[
            r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe",
            r"C:\Program Files\KiCad\9.0\bin\kicad-cli.exe",
        ],
        kicad_cli_command: "kicad-cli.exe",
        pcbnew: &[
            r"C:\Program Files\KiCad\10.0\bin\pcbnew.exe",
            r"C:\Program Files\KiCad\9.0\bin\pcbnew.exe",
        ],
    }
}

#[cfg(target_os = "linux")]
fn platform_defaults() -> PlatformDefaults {
    PlatformDefaults {
        python_interpreter: &["/usr/bin/python3"],
        kicad_cli: &["/usr/bin/kicad-cli"],
        kicad_cli_command: "kicad-cli",
        pcbnew: &["/usr/bin/pcbnew"],
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

/// Build the per-platform command that opens a board in the KiCad GUI. On
/// macOS `waitable` launches a dedicated app instance through `open -n -W`
/// so the returned child tracks the editor's lifetime.
fn pcbnew_launch_command(pcbnew_path: &str, pcb_path: &Path, waitable: bool) -> Result<Command> {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("open");
        if waitable {
            cmd.arg("-n").arg("-W");
        }
        cmd.arg("-a")
            .arg(pcbnew_app_bundle_path(pcbnew_path)?)
            .arg(pcb_path);
        Ok(cmd)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = waitable;
        let mut cmd = Command::new(pcbnew_path);
        cmd.arg(pcb_path);
        Ok(cmd)
    }
}

/// Open a KiCad board in the GUI that matches this toolchain's discovered install.
pub fn open_pcbnew(pcb_path: impl AsRef<Path>) -> Result<()> {
    let pcb_path = pcb_path.as_ref();
    let pcbnew_path = require_pcbnew_launch(pcb_path)?;
    let cmd = pcbnew_launch_command(&pcbnew_path, pcb_path, false)?;
    spawn_pcbnew_command(cmd, &pcbnew_path, pcb_path)?;
    Ok(())
}

pub struct PcbnewSession {
    child: Child,
}

impl PcbnewSession {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child
            .try_wait()
            .context("Failed while checking KiCad PCB Editor status")
    }
}

/// Open a KiCad board in a process that can be waited on.
pub fn open_pcbnew_session(pcb_path: impl AsRef<Path>) -> Result<PcbnewSession> {
    let pcb_path = pcb_path.as_ref();
    let pcbnew_path = require_pcbnew_launch(pcb_path)?;
    let cmd = pcbnew_launch_command(&pcbnew_path, pcb_path, true)?;
    spawn_pcbnew_command(cmd, &pcbnew_path, pcb_path).map(|child| PcbnewSession { child })
}

fn require_pcbnew_launch(pcb_path: &Path) -> Result<String> {
    if !pcb_path.exists() {
        anyhow::bail!("PCB file not found: {}", pcb_path.display());
    }

    let pcbnew = KiCadInstallation::discover().pcbnew;
    if !Path::new(&pcbnew).exists() {
        anyhow::bail!(
            "KiCad PCB Editor not found at expected location: {pcbnew}\n\
             Please ensure KiCad is installed.\n\
             If KiCad PCB Editor is in a non-standard location, set the KICAD_PCBNEW environment variable."
        );
    }
    Ok(pcbnew)
}

fn spawn_pcbnew_command(mut cmd: Command, pcbnew_path: &str, pcb_path: &Path) -> Result<Child> {
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
        })
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
        let kicad_cli = KiCadInstallation::discover().kicad_cli;
        let mut cmd = CommandRunner::new(&kicad_cli);

        for arg in &self.args {
            cmd = cmd.arg(arg);
        }

        if let Some(dir) = &self.current_dir {
            cmd = cmd.current_dir(dir);
        }

        for (key, value) in self.env_vars {
            cmd = cmd.env(key, value);
        }

        if let Some(log_file) = self.log_file {
            cmd = cmd.log_file(log_file);
        }

        let output = cmd
            .run()
            .with_context(|| format!("Failed to execute KiCad CLI at {kicad_cli}"))?;

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
        let kicad_cli = KiCadInstallation::discover().kicad_cli;
        let mut cmd = std::process::Command::new(&kicad_cli);

        for arg in &self.args {
            cmd.arg(arg);
        }

        if let Some(dir) = &self.current_dir {
            cmd.current_dir(dir);
        }

        for (key, value) in self.env_vars {
            cmd.env(key, value);
        }

        cmd.output()
            .with_context(|| format!("Failed to execute KiCad CLI at {kicad_cli}"))
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

        let installation = KiCadInstallation::discover();
        let python_path = installation.python_path(self.extra_python_paths)?;
        let python_interpreter = installation.python_interpreter;
        let mut cmd = CommandRunner::new(&python_interpreter).arg(temp_file_path);

        for arg in &self.args {
            cmd = cmd.arg(arg);
        }

        cmd = cmd.env("PYTHONPATH", python_path);

        for (key, value) in self.env_vars {
            cmd = cmd.env(key, value);
        }

        if let Some(log_file) = self.log_file {
            cmd = cmd.log_file(log_file);
        }

        let output = cmd
            .run()
            .with_context(|| format!("Failed to execute KiCad Python at {python_interpreter}"))?;

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
