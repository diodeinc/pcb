use std::{
    fs::File,
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

/// Output from a command execution, capturing both stdout and stderr
#[derive(Clone, Debug)]
pub struct CommandOutput {
    /// The raw output bytes including ANSI escape sequences
    pub raw_output: Vec<u8>,
    /// The output with ANSI escape sequences removed
    pub plain_output: Vec<u8>,
    /// Whether the command execution was successful
    pub success: bool,
}

impl CommandOutput {
    /// Get the plain output as a UTF-8 string
    pub fn plain_as_string(&self) -> String {
        String::from_utf8_lossy(&self.plain_output).to_string()
    }
}

/// Two independent handles to `file` for redirecting a child's stdout and
/// stderr to the same on-disk log without either handle's `Drop` closing
/// the fd the other is using.
pub fn log_file_stdio(file: &File) -> Result<(Stdio, Stdio)> {
    let stdout = Stdio::from(
        file.try_clone()
            .context("Failed to duplicate log file handle")?,
    );
    let stderr = Stdio::from(
        file.try_clone()
            .context("Failed to duplicate log file handle")?,
    );
    Ok((stdout, stderr))
}

/// Builder for running a command with stdout and stderr captured together
pub struct CommandRunner {
    program: String,
    args: Vec<String>,
    /// Optional log file to write the plain output to
    log_file: Option<File>,
    /// Environment variables to set for the command
    env_vars: Vec<(String, String)>,
    /// Current directory for the command
    current_dir: Option<String>,
    /// Optional timeout — the child process is killed if it exceeds this duration
    timeout: Option<Duration>,
}

impl CommandRunner {
    /// Create a new CommandRunner for the specified program
    pub fn new<S: AsRef<str>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            log_file: None,
            env_vars: Vec::new(),
            current_dir: None,
            timeout: None,
        }
    }

    /// Add an argument to the command
    pub fn arg<S: AsRef<str>>(mut self, arg: S) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Set the log file to write the output to
    pub fn log_file(mut self, file: File) -> Self {
        self.log_file = Some(file);
        self
    }

    /// Add an environment variable to the command
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.env_vars
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Set the current directory for the command
    pub fn current_dir<P: AsRef<str>>(mut self, dir: P) -> Self {
        self.current_dir = Some(dir.as_ref().to_owned());
        self
    }

    /// Set a timeout — the child process is killed if it exceeds this duration
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Execute the command and return its output
    pub fn run(self) -> Result<CommandOutput> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.envs(self.env_vars);
        if let Some(dir) = self.current_dir {
            command.current_dir(dir);
        }
        command.stdin(Stdio::null());

        // Create pipes for stdout and stderr
        let (mut reader, writer) = os_pipe::pipe().context("Failed to create pipe")?;

        command.stdout(Stdio::from(
            writer.try_clone().context("Failed to clone pipe writer")?,
        ));
        command.stderr(Stdio::from(writer));

        // Start the command
        let mut child = command.spawn().context("Failed to spawn command")?;

        // Read the output in a separate thread to avoid deadlocks
        let reader_thread = thread::spawn(move || {
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).map(|_| buffer)
        });

        // Wait for the command to complete (with optional timeout)
        let success = if let Some(timeout) = self.timeout {
            let start = Instant::now();
            loop {
                match child.try_wait().context("Failed to check command status")? {
                    Some(status) => break status.success(),
                    None if start.elapsed() > timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        anyhow::bail!("Command timed out after {}s", timeout.as_secs());
                    }
                    None => thread::sleep(Duration::from_millis(100)),
                }
            }
        } else {
            child
                .wait()
                .context("Failed to wait for command")?
                .success()
        };

        drop(command);

        // Get the captured output
        let raw_output = reader_thread
            .join()
            .expect("Failed to join reader thread")
            .context("Failed to read command output")?;

        // Strip ANSI escape sequences for the plain output
        let plain_output = strip_ansi_escapes::strip(&raw_output);

        // Write to log file if provided
        if let Some(mut log_file) = self.log_file {
            log_file
                .write_all(&plain_output)
                .context("Failed to write to log file")?;
        }

        Ok(CommandOutput {
            raw_output,
            plain_output,
            success,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::io::Seek;
    use std::io::SeekFrom;
    use tempfile::tempfile;

    #[test]
    fn test_run_with_env_var() {
        let output = CommandRunner::new("sh")
            .arg("-c")
            .arg("echo $TEST_VAR")
            .env("TEST_VAR", "test_value")
            .run()
            .unwrap();

        assert!(output.success);
        assert_eq!(output.plain_as_string().trim(), "test_value");
    }

    #[test]
    fn test_write_to_log_file() {
        let mut temp_file = tempfile().unwrap();

        let output = CommandRunner::new("echo")
            .arg("Hello, log file!")
            .log_file(temp_file.try_clone().unwrap())
            .run()
            .unwrap();

        assert!(output.success);

        // Read the content of the log file
        temp_file.seek(SeekFrom::Start(0)).unwrap();
        let mut log_content = String::new();
        temp_file.read_to_string(&mut log_content).unwrap();

        assert_eq!(log_content.trim(), "Hello, log file!");
    }

    #[test]
    fn test_with_ansi_escape_sequences() {
        // Create a string with ANSI color codes
        let colored_output = CommandRunner::new("sh")
            .arg("-c")
            .arg("printf '\\033[31mRed\\033[0m \\033[32mGreen\\033[0m'")
            .run()
            .unwrap();

        assert!(colored_output.success);

        // The raw output should contain the ANSI escape sequences
        assert!(colored_output.raw_output.len() > colored_output.plain_output.len());

        // The plain output should just be "Red Green"
        assert_eq!(colored_output.plain_as_string().trim(), "Red Green");
    }
}
