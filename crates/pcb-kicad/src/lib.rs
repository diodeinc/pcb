use anyhow::{anyhow, Context, Result};
use pcb_command_runner::CommandRunner;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

/// Detect if we're running in WSL
fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        // Check for WSL-specific indicators
        if let Ok(version) = std::fs::read_to_string("/proc/version") {
            if version.to_lowercase().contains("microsoft")
                || version.to_lowercase().contains("wsl")
            {
                return true;
            }
        }
        // Also check for WSL environment variable
        if std::env::var("WSL_DISTRO_NAME").is_ok() {
            return true;
        }
    }
    false
}

/// Convert a WSL Linux path to a Windows path that Windows executables can understand
fn wsl_path_to_windows(linux_path: &str) -> Result<String> {
    let output = Command::new("wslpath")
        .arg("-w")
        .arg(linux_path)
        .output()
        .context("Failed to run wslpath command")?;

    if !output.status.success() {
        anyhow::bail!("wslpath command failed");
    }

    let windows_path = String::from_utf8(output.stdout)
        .context("wslpath output is not valid UTF-8")?
        .trim()
        .to_string();

    Ok(windows_path)
}

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
    use super::is_wsl;

    pub(crate) fn python_interpreter() -> String {
        if let Ok(path) = std::env::var("KICAD_PYTHON_INTERPRETER") {
            return path;
        }

        if is_wsl() {
            // In WSL, try to use Windows KiCad Python
            // First check common Windows installation paths
            let windows_paths = vec!["/mnt/c/Program Files/KiCad/9.0/bin/python.exe"];

            for path in windows_paths {
                if std::path::Path::new(path).exists() {
                    return path.to_string();
                }
            }

            // Fallback to trying to execute python.exe from PATH
            "python.exe".to_string()
        } else {
            // Native Linux
            "/usr/bin/python3".to_string()
        }
    }

    pub(crate) fn python_site_packages() -> String {
        if let Ok(path) = std::env::var("KICAD_PYTHON_SITE_PACKAGES") {
            return path;
        }

        if is_wsl() {
            // In WSL, use Windows KiCad Python site-packages
            // Try to find it in common locations
            let windows_paths = vec!["/mnt/c/Program Files/KiCad/9.0/lib/python3.11/site-packages"];

            for path in windows_paths {
                if std::path::Path::new(path).exists() {
                    return path.to_string();
                }
            }

            // Fallback
            "/mnt/c/Program Files/KiCad/9.0/lib/python3.11/site-packages".to_string()
        } else {
            // Native Linux
            "/usr/lib/python3/dist-packages".to_string()
        }
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
        if let Ok(path) = std::env::var("KICAD_CLI") {
            return path;
        }

        if is_wsl() {
            // In WSL, try to use Windows KiCad CLI
            let windows_paths = vec!["/mnt/c/Program Files/KiCad/9.0/bin/kicad-cli.exe"];

            for path in windows_paths {
                if std::path::Path::new(path).exists() {
                    return path.to_string();
                }
            }

            // Fallback
            "kicad-cli.exe".to_string()
        } else {
            // Native Linux
            "/usr/bin/kicad-cli".to_string()
        }
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

/// Options for running Python scripts in the KiCad Python environment
#[derive(Debug, Default)]
pub struct PythonScriptOptions {
    /// Arguments to pass to the script
    pub args: Vec<String>,
    /// Optional log file for capturing output
    pub log_file: Option<File>,
    /// Additional environment variables
    pub env_vars: HashMap<String, String>,
}

/// Run a Python script string in the KiCad Python environment
pub fn run_python_script(script: &str, options: PythonScriptOptions) -> Result<()> {
    // Check if KiCad Python is available
    check_kicad_python()?;

    let args_refs: Vec<&str> = options.args.iter().map(|s| s.as_str()).collect();

    // Create a temporary file for the script
    let mut temp_file =
        NamedTempFile::new().context("Failed to create temporary file for Python script")?;

    temp_file
        .write_all(script.as_bytes())
        .context("Failed to write Python script to temporary file")?;

    let temp_file_path = temp_file
        .path()
        .to_str()
        .ok_or_else(|| anyhow!("Failed to convert temporary file path to string"))?;

    // In WSL, if we're calling a Windows executable, we need to convert the path
    let temp_file_path_for_cmd = if is_wsl() && paths::python_interpreter().ends_with(".exe") {
        wsl_path_to_windows(temp_file_path)?
    } else {
        temp_file_path.to_string()
    };

    // Set up PYTHONPATH
    #[cfg(target_os = "windows")]
    let path_separator = ";";
    #[cfg(not(target_os = "windows"))]
    let path_separator = ":";

    // In WSL calling Windows executables, use Windows path separator
    let path_separator = if is_wsl() && paths::python_interpreter().ends_with(".exe") {
        ";"
    } else {
        path_separator
    };

    // Convert PYTHONPATH to Windows format if needed
    let python_site_packages = paths::python_site_packages();
    let venv_site_packages = paths::venv_site_packages();

    let (python_site_packages, venv_site_packages) =
        if is_wsl() && paths::python_interpreter().ends_with(".exe") {
            // Convert both paths to Windows format
            let psp = if python_site_packages.starts_with("/mnt/") {
                // Already a mount path, convert it
                wsl_path_to_windows(&python_site_packages)
                    .unwrap_or_else(|_| python_site_packages.clone())
            } else {
                python_site_packages.clone()
            };

            let vsp = wsl_path_to_windows(&venv_site_packages)
                .unwrap_or_else(|_| venv_site_packages.clone());

            (psp, vsp)
        } else {
            (python_site_packages, venv_site_packages)
        };

    let python_path = format!("{python_site_packages}{path_separator}{venv_site_packages}");

    // Build the command
    let mut cmd = CommandRunner::new(paths::python_interpreter()).arg(temp_file_path_for_cmd);

    // Add script arguments
    for arg in args_refs {
        cmd = cmd.arg(arg);
    }

    // Set PYTHONPATH
    cmd = cmd.env("PYTHONPATH", python_path);

    // Add custom environment variables
    for (key, value) in options.env_vars {
        cmd = cmd.env(key, value);
    }

    // Add log file if provided
    if let Some(log_file) = options.log_file {
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

/// Run a Python script from a file in the KiCad Python environment
pub fn run_python_file(script_path: &Path, options: PythonScriptOptions) -> Result<()> {
    let script = std::fs::read_to_string(script_path)
        .with_context(|| format!("Failed to read Python script from {script_path:?}"))?;
    run_python_script(&script, options)
}

/// Builder pattern for Python script execution
#[derive(Debug)]
pub struct PythonScriptBuilder {
    script: String,
    options: PythonScriptOptions,
}

impl PythonScriptBuilder {
    /// Create a new Python script builder with the given script content
    pub fn new(script: impl Into<String>) -> Self {
        Self {
            script: script.into(),
            options: PythonScriptOptions::default(),
        }
    }

    /// Create a builder from a script file
    pub fn from_file(path: &Path) -> Result<Self> {
        let script = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read Python script from {path:?}"))?;
        Ok(Self::new(script))
    }

    /// Add a command-line argument for the script
    pub fn arg<S: Into<String>>(mut self, arg: S) -> Self {
        self.options.args.push(arg.into());
        self
    }

    /// Add multiple arguments
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.options.args.extend(args.into_iter().map(|s| s.into()));
        self
    }

    /// Set a log file for capturing output
    pub fn log_file(mut self, file: File) -> Self {
        self.options.log_file = Some(file);
        self
    }

    /// Add an environment variable
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.options.env_vars.insert(key.into(), value.into());
        self
    }

    /// Execute the script
    pub fn run(self) -> Result<()> {
        run_python_script(&self.script, self.options)
    }
}
