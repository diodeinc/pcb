//! git_sandbox.rs
//!
//! Hermetic, offline Git test sandbox for Rust tests.
//! - Rewrites GitHub/GitLab URLs to local `file://` bare repos via a private `.gitconfig`
//! - Disables all network protocols (whitelists `file` only)
//! - Isolates cache directory via `DIODE_STAR_CACHE_DIR`
//! - Lets you create fixture repos (files/commits/tags) and mirror-push them
//! - Run arbitrary commands or use `duct` directly via `cmd()`
//!
//! Everything lives under an `assert_fs::TempDir` and is cleaned up on drop.
//!
//! ## Quick example
//! ```no_run
//! use std::fs;
//! use std::path::Path;
//! use pcb_test_utils::sandbox::Sandbox;
//!
//! let sb = Sandbox::new();
//!
//! // Create a fake GitHub remote and seed it
//! let fx = sb.git_fixture("https://github.com/foo/bar.git");
//! fx.write("README.md", "hello")
//!   .commit("init")
//!   .tag("v1", true)
//!   .push_mirror();
//!
//! // Use sandbox's cmd() for system binaries  
//! sb.cmd("git", &["clone", "https://github.com/foo/bar.git", "clone"])
//!     .dir(sb.root_path())
//!     .run()
//!     .expect("git clone failed");
//!
//! assert_eq!(fs::read_to_string(sb.root_path().join("clone/README.md")).unwrap(), "hello");
//!
//! // Run a cargo binary (cwd is relative to sandbox root)
//! let output = sb.run("my-binary", ["--help"], Some(Path::new("clone"))).unwrap();
//! println!("Binary output: {}", output);
//! ```

use assert_fs::fixture::PathChild;
use assert_fs::TempDir;
use duct::Expression;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Sandbox {
    root: TempDir,
    pub home: PathBuf,
    pub gitconfig: PathBuf,
    pub mock_github: PathBuf,
    pub mock_gitlab: PathBuf,
    pub cache_dir: PathBuf,
    default_cwd: PathBuf,
    trace: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Sandbox {
    /// Create a new sandbox; all state is under an auto-cleaned TempDir.
    pub fn new() -> Self {
        let root = TempDir::new().expect("create sandbox TempDir");
        let home = root.child("home").to_path_buf();
        let gitconfig = home.join(".gitconfig");
        let mock_github = root.child("mock/github").to_path_buf();
        let mock_gitlab = root.child("mock/gitlab").to_path_buf();
        let cache_dir = root.child("cache").to_path_buf();

        fs::create_dir_all(&home).expect("create home dir");
        fs::create_dir_all(&mock_github).expect("create mock github dir");
        fs::create_dir_all(&mock_gitlab).expect("create mock gitlab dir");
        fs::create_dir_all(&cache_dir).expect("create cache dir");

        let default_cwd = root.path().to_path_buf();

        let s = Self {
            root,
            home,
            gitconfig,
            mock_github,
            mock_gitlab,
            cache_dir,
            default_cwd,
            trace: false,
        };
        s.write_gitconfig();
        s
    }

    /// Enable `GIT_TRACE=1` for commands run with `run` / `run_ok` / `cmd`.
    pub fn with_trace(mut self, yes: bool) -> Self {
        self.trace = yes;
        self
    }

    /// Get the current default working directory for commands.
    pub fn default_cwd(&self) -> &Path {
        &self.default_cwd
    }

    /// Set the default working directory for commands. Path is relative to sandbox root if not absolute.
    pub fn set_default_cwd<P: AsRef<Path>>(&mut self, cwd: P) -> &mut Self {
        let cwd = cwd.as_ref();
        self.default_cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            self.root_path().join(cwd)
        };
        self
    }

    /// Absolute path to the sandbox root (useful for placing clones or artifacts).
    pub fn root_path(&self) -> &Path {
        self.root.path()
    }

    /// Write/overwrite a file relative to the sandbox root.
    pub fn write<P: AsRef<Path>, S: AsRef<[u8]>>(&mut self, rel: P, contents: S) -> &mut Self {
        let p = self.root_path().join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(p, contents).expect("write file");
        self
    }

    /// Create and initialize a git fixture for a given GitHub/GitLab URL.
    /// Returns a builder you can use to write files, commit, tag, and finally `push_mirror`.
    pub fn git_fixture<S: AsRef<str>>(&self, url: S) -> FixtureRepo {
        let url = url.as_ref();
        let (host, rel) = parse_supported_url(url);
        let base = match host {
            "github.com" => &self.mock_github,
            "gitlab.com" => &self.mock_gitlab,
            _ => panic!("unsupported host: {host}"),
        };

        let rel = ensure_dot_git(rel);
        let bare = base.join(&rel);
        if let Some(parent) = bare.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }

        // Init bare remote
        run_git(&["init", "--bare", bare.to_str().unwrap()]);

        // Prepare a work repo to compose commits, independent of rewrite rules
        let work = self
            .root_path()
            .join(format!("work_{}", sanitize_name(&rel)));
        if work.exists() {
            fs::remove_dir_all(&work).expect("remove work dir");
        }
        run_git(&["init", work.to_str().unwrap()]);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "config",
            "user.email",
            "test@example.com",
        ]);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "config",
            "user.name",
            "Sandbox",
        ]);
        run_git(&["-C", work.to_str().unwrap(), "branch", "-M", "main"]);

        // Add file:// remote (fixture creation doesn’t depend on URL rewrite)
        let bare_url = file_url(&bare);
        run_git(&[
            "-C",
            work.to_str().unwrap(),
            "remote",
            "add",
            "origin",
            &bare_url,
        ]);

        FixtureRepo {
            work,
            bare,
            default_branch: "main".into(),
        }
    }

    /// Build a `duct::Expression` pre-wired with the sandbox env and default cwd.
    /// Useful for system binaries. You can chain `.dir()`, etc. and then `.run()` or `.read()`.
    pub fn cmd<S: AsRef<OsStr>, I: IntoIterator>(&self, program: S, args: I) -> Expression
    where
        I::Item: AsRef<OsStr>,
    {
        let program_str = program.as_ref().to_string_lossy();
        let args: Vec<_> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string_lossy().to_string())
            .collect();
        let expr = duct::cmd(program_str.as_ref(), args).dir(&self.default_cwd);
        self.inject_env(expr)
    }

    /// Run a cargo binary inside this sandbox and return stdout as String.
    /// Uses `cargo_bin!()` to locate the binary. Errors if the process exits with non-zero status.
    /// For system binaries, use `cmd()` method instead.
    pub fn run<I>(&self, program: &str, args: I, cwd: Option<&Path>) -> Result<String, String>
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        let cargo_bin_path = assert_cmd::cargo::cargo_bin(program)
            .to_string_lossy()
            .to_string();
        let args: Vec<_> = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string_lossy().to_string())
            .collect();

        let mut expr = duct::cmd(&cargo_bin_path, args);

        let working_dir = if let Some(dir) = cwd {
            if dir.is_absolute() {
                dir.to_path_buf()
            } else {
                self.root_path().join(dir)
            }
        } else {
            self.default_cwd.clone()
        };
        expr = expr.dir(working_dir);

        expr = self.inject_env(expr);

        expr.read().map_err(|e| format!("command failed: {e}"))
    }

    fn write_gitconfig(&self) {
        let mut f = File::create(&self.gitconfig).expect("create gitconfig file");
        let gh = file_url(&self.mock_github) + "/";
        let gl = file_url(&self.mock_gitlab) + "/";

        writeln!(
            f,
            r#"[protocol]
    allow = never
[protocol "file"]
    allow = always

[url "{gh}"]
    insteadOf = https://github.com/
    insteadOf = ssh://git@github.com/
    insteadOf = git@github.com:

[url "{gl}"]
    insteadOf = https://gitlab.com/
    insteadOf = ssh://git@gitlab.com/
    insteadOf = git@gitlab.com:
"#
        )
        .expect("write gitconfig");
    }

    pub fn inject_env(&self, mut expr: Expression) -> Expression {
        let mut env_map: HashMap<String, String> = HashMap::new();
        if let Ok(path) = std::env::var("PATH") {
            env_map.insert("PATH".into(), path);
        }
        env_map.insert("HOME".into(), self.home.to_string_lossy().into_owned());
        env_map.insert(
            "XDG_CONFIG_HOME".into(),
            self.home.to_string_lossy().into_owned(),
        );
        env_map.insert(
            "GIT_CONFIG_GLOBAL".into(),
            self.gitconfig.to_string_lossy().into_owned(),
        );
        env_map.insert(
            "GIT_CONFIG_SYSTEM".into(),
            if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        );
        env_map.insert("GIT_ALLOW_PROTOCOL".into(), "file".into());
        env_map.insert(
            "DIODE_STAR_CACHE_DIR".into(),
            self.cache_dir.to_string_lossy().into_owned(),
        );
        if self.trace {
            env_map.insert("GIT_TRACE".into(), "1".into());
            env_map.insert("GIT_CURL_VERBOSE".into(), "1".into());
        }

        expr = expr.full_env(&env_map);

        expr
    }
}

pub struct FixtureRepo {
    work: PathBuf,
    bare: PathBuf,
    default_branch: String,
}

impl FixtureRepo {
    /// Write/overwrite a file relative to the work tree.
    pub fn write<P: AsRef<Path>, S: AsRef<[u8]>>(&self, rel: P, contents: S) -> &Self {
        let p = self.work.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(p, contents).expect("write file");
        self
    }

    /// Stage all changes and commit with the given message.
    pub fn commit<S: AsRef<str>>(&self, msg: S) -> &Self {
        run_git(&["-C", self.work_str(), "add", "-A"]);
        run_git(&["-C", self.work_str(), "commit", "-m", msg.as_ref()]);
        self
    }

    /// Set/rename the default branch.
    pub fn set_default_branch<S: AsRef<str>>(&mut self, name: S) -> &mut Self {
        let name = name.as_ref();
        run_git(&["-C", self.work_str(), "branch", "-M", name]);
        self.default_branch = name.to_string();
        self
    }

    /// Create or move a tag. If `annotated`, creates/updates an annotated tag.
    pub fn tag<S: AsRef<str>>(&self, name: S, annotated: bool) -> &Self {
        let name = name.as_ref();
        if annotated {
            run_git(&["-C", self.work_str(), "tag", "-fa", name, "-m", name]);
        } else {
            run_git(&["-C", self.work_str(), "tag", "-f", name]);
        }
        self
    }

    /// Mirror-push all refs to the bare “remote”.
    pub fn push_mirror(&self) {
        run_git(&["-C", self.work_str(), "push", "--mirror", "origin"]);
    }

    pub fn work_dir(&self) -> &Path {
        &self.work
    }
    pub fn bare_dir(&self) -> &Path {
        &self.bare
    }

    fn work_str(&self) -> &str {
        self.work.to_str().expect("utf-8 path")
    }
}

/* ------------- helpers ------------- */

fn run_git(args: &[&str]) {
    duct::cmd("git", args)
        .stdout_null()
        .stderr_null()
        .run()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

fn ensure_dot_git(mut rel: String) -> String {
    if !rel.ends_with(".git") {
        rel.push_str(".git");
    }
    rel
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Build a `file://` URL for an absolute path. Best-effort normalization.
fn file_url(p: &Path) -> String {
    let abs = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut s = abs.to_string_lossy().replace('\\', "/");
    if !s.starts_with('/') && !s.starts_with(":/") {
        s = format!("/{s}");
    }
    format!("file://{s}")
}

/// Parse minimal forms for GitHub/GitLab: https, ssh, scp.
/// Returns (host, "org/repo[.git]").
fn parse_supported_url(url: &str) -> (&'static str, String) {
    for host in ["github.com", "gitlab.com"] {
        let https = format!("https://{host}/");
        if let Some(rest) = url.strip_prefix(&https) {
            return (host, rest.trim_start_matches('/').to_string());
        }
        let ssh = format!("ssh://git@{host}/");
        if let Some(rest) = url.strip_prefix(&ssh) {
            return (host, rest.trim_start_matches('/').to_string());
        }
        let scp = format!("git@{host}:");
        if let Some(rest) = url.strip_prefix(&scp) {
            return (host, rest.to_string());
        }
    }
    panic!("unsupported URL format: {url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_sandbox_basic_functionality() {
        let sb = Sandbox::new();

        // Create a fixture repository with some test files
        sb.git_fixture("https://github.com/test/repo.git")
            .write("README.md", "# Test Repository\n\nThis is a test.")
            .write(
                "src/main.rs",
                "fn main() {\n    println!(\"Hello, world!\");\n}",
            )
            .write(
                "Cargo.toml",
                "[package]\nname = \"test\"\nversion = \"0.1.0\"",
            )
            .commit("Initial commit")
            .push_mirror();

        // Clone the repository using the sandbox's cmd() method (uses default cwd = root)
        sb.cmd(
            "git",
            &["clone", "https://github.com/test/repo.git", "cloned"],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("git clone should succeed");

        // Run ls to check the contents (uses default cwd = root)
        let ls_output = sb
            .cmd("ls", &["-la", "cloned"])
            .read()
            .expect("ls should succeed");

        println!("Directory contents:\n{}", ls_output);

        // Verify the files exist
        let clone_dir = sb.root_path().join("cloned");
        assert!(clone_dir.is_dir());
        assert!(clone_dir.join("README.md").is_file());
        assert!(clone_dir.join("src/main.rs").is_file());
        assert!(clone_dir.join("Cargo.toml").is_file());

        // Verify file contents
        assert_eq!(
            std::fs::read_to_string(clone_dir.join("README.md")).unwrap(),
            "# Test Repository\n\nThis is a test."
        );
        assert_eq!(
            std::fs::read_to_string(clone_dir.join("src/main.rs")).unwrap(),
            "fn main() {\n    println!(\"Hello, world!\");\n}"
        );
    }

    #[test]
    fn test_cwd_relative_to_sandbox() {
        let mut sb = Sandbox::new();

        // Create a test directory structure using the fluent API
        sb.write("test_dir/file.txt", "test content")
            .write("test_dir/another.txt", "more content");

        // Change default cwd to the test directory
        sb.set_default_cwd("test_dir");
        assert_eq!(sb.default_cwd(), sb.root_path().join("test_dir"));

        // Run ls without specifying directory - should use default cwd
        let output = sb.cmd("ls", &["-la"]).read().expect("ls should succeed");

        println!("Test directory contents:\n{}", output);

        assert!(output.contains("file.txt"));
        assert!(output.contains("another.txt"));

        // Verify cache dir env var is set correctly
        let cache_output = sb
            .cmd("sh", &["-c", "echo $DIODE_STAR_CACHE_DIR"])
            .read()
            .expect("echo should succeed");
        println!("Cache dir env var: {}", cache_output.trim());
        assert!(cache_output.trim().contains("cache"));
    }
}
