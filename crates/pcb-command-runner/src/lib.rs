use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
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

impl Default for CommandOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandOutput {
    /// Create a new empty CommandOutput
    pub fn new() -> Self {
        Self {
            raw_output: Vec::new(),
            plain_output: Vec::new(),
            success: false,
        }
    }

    /// Get the raw output as a UTF-8 string
    pub fn raw_as_string(&self) -> String {
        String::from_utf8_lossy(&self.raw_output).to_string()
    }

    /// Get the plain output as a UTF-8 string
    pub fn plain_as_string(&self) -> String {
        String::from_utf8_lossy(&self.plain_output).to_string()
    }

    /// Write the plain output to a file
    pub fn write_to_file<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let mut file = File::create(path)?;
        file.write_all(&self.plain_output)?;
        Ok(())
    }

    /// Append the plain output to an existing file
    pub fn append_to_file(&self, file: &mut File) -> Result<()> {
        file.write_all(&self.plain_output)?;
        Ok(())
    }
}

/// Options for running a command
pub struct CommandRunnerOptions {
    /// Capture the command output (both stdout and stderr)
    pub capture_output: bool,
    /// Optional log file to write the output to
    pub log_file: Option<File>,
    /// Environment variables to set for the command
    pub env_vars: Vec<(String, String)>,
    /// Current directory for the command
    pub current_dir: Option<String>,
    /// Optional string to pipe into stdin
    pub stdin_input: Option<String>,
}

impl Default for CommandRunnerOptions {
    fn default() -> Self {
        Self {
            capture_output: true,
            log_file: None,
            env_vars: Vec::new(),
            current_dir: None,
            stdin_input: None,
        }
    }
}

/// Run a command and return its output
///
/// # Arguments
///
/// * `program` - The program to run
/// * `args` - Arguments to pass to the program
/// * `options` - Options for running the command
///
/// # Returns
///
/// Returns the command output if capture_output is true, otherwise returns an empty CommandOutput
pub fn run_command<S, I, T>(
    program: S,
    args: I,
    options: CommandRunnerOptions,
) -> Result<CommandOutput>
where
    S: AsRef<str>,
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let mut command = Command::new(program.as_ref());
    command.args(args.into_iter().map(|s| s.as_ref().to_owned()));

    // Set environment variables
    for (key, value) in options.env_vars {
        command.env(key, value);
    }

    // Set current directory if specified
    if let Some(dir) = options.current_dir {
        command.current_dir(dir);
    }

    // Configure stdin if input is provided
    if options.stdin_input.is_some() {
        command.stdin(Stdio::piped());
    }

    let mut output = CommandOutput::new();

    if options.capture_output {
        // Create pipes for stdout and stderr
        let (mut reader, writer) = os_pipe::pipe().context("Failed to create pipe")?;

        command.stdout(Stdio::from(
            writer.try_clone().context("Failed to clone pipe writer")?,
        ));
        command.stderr(Stdio::from(writer));

        // Start the command
        let mut child = command.spawn().context("Failed to spawn command")?;

        // Write stdin input if provided
        if let Some(input) = options.stdin_input {
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(input.as_bytes())
                    .context("Failed to write to stdin")?;
            }
        }

        // Read the output in a separate thread to avoid deadlocks
        let reader_thread = thread::spawn(move || {
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).map(|_| buffer)
        });

        // Wait for the command to complete
        let status = child.wait().context("Failed to wait for command")?;
        output.success = status.success();

        drop(command);

        // Get the captured output
        output.raw_output = reader_thread
            .join()
            .expect("Failed to join reader thread")
            .context("Failed to read command output")?;

        // Strip ANSI escape sequences for the plain output
        output.plain_output = strip_ansi_escapes::strip(&output.raw_output);

        // Write to log file if provided
        if let Some(mut log_file) = options.log_file {
            log_file
                .write_all(&output.plain_output)
                .context("Failed to write to log file")?;
        }
    } else {
        // If not capturing output, just run the command and wait for it to finish
        let (out, err) = if let Some(log_file) = options.log_file {
            (
                Stdio::from(log_file.try_clone().unwrap()),
                Stdio::from(log_file.try_clone().unwrap()),
            )
        } else {
            (Stdio::inherit(), Stdio::inherit())
        };

        let mut child = command
            .stdout(out)
            .stderr(err)
            .spawn()
            .context("Failed to spawn command")?;

        // Write stdin input if provided
        if let Some(input) = options.stdin_input {
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(input.as_bytes())
                    .context("Failed to write to stdin")?;
            }
        }

        let status = child.wait().context("Failed to wait for command")?;
        output.success = status.success();
    }

    Ok(output)
}

/// Builder for constructing command execution
pub struct CommandRunner {
    program: String,
    args: Vec<String>,
    options: CommandRunnerOptions,
}

impl CommandRunner {
    /// Create a new CommandRunner for the specified program
    pub fn new<S: AsRef<str>>(program: S) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            options: CommandRunnerOptions::default(),
        }
    }

    /// Add an argument to the command
    pub fn arg<S: AsRef<str>>(mut self, arg: S) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Add multiple arguments to the command
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(args.into_iter().map(|s| s.as_ref().to_owned()));
        self
    }

    /// Set whether to capture the command output
    pub fn capture_output(mut self, capture: bool) -> Self {
        self.options.capture_output = capture;
        self
    }

    /// Set the log file to write the output to
    pub fn log_file(mut self, file: File) -> Self {
        self.options.log_file = Some(file);
        self
    }

    /// Add an environment variable to the command
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.options
            .env_vars
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Set the current directory for the command
    pub fn current_dir<P: AsRef<str>>(mut self, dir: P) -> Self {
        self.options.current_dir = Some(dir.as_ref().to_owned());
        self
    }

    /// Set the input to pipe into stdin
    pub fn stdin_input<S: AsRef<str>>(mut self, input: S) -> Self {
        self.options.stdin_input = Some(input.as_ref().to_owned());
        self
    }

    /// Execute the command and return its output
    pub fn run(self) -> Result<CommandOutput> {
        run_command(self.program, self.args, self.options)
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
    fn test_run_echo_command() {
        let output = CommandRunner::new("echo")
            .arg("Hello, world!")
            .run()
            .unwrap();

        assert!(output.success);
        assert_eq!(output.plain_as_string().trim(), "Hello, world!");
    }

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

    #[test]
    fn test_with_stdin_input() {
        let output = CommandRunner::new("cat")
            .stdin_input("Hello from stdin!")
            .run()
            .unwrap();

        assert!(output.success);
        assert_eq!(output.plain_as_string().trim(), "Hello from stdin!");
    }
}
