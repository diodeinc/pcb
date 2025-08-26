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
//! let mut sb = Sandbox::new();
//!
//! // Create a fake GitHub remote and seed it
//! sb.git_fixture("https://github.com/foo/bar.git")
//!   .write("README.md", "hello")
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
//! // Run a cargo binary and snapshot the output
//! let output = sb.cwd("clone").snapshot_run("my-binary", ["--help"]);
//! pcb_test_utils::assert_snapshot!("help", output);
//! ```

use assert_fs::fixture::PathChild;
use assert_fs::TempDir;
use duct::Expression;
use std::collections::HashMap;
use std::ffi::OsStr;

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub use assert_cmd::cargo_bin;

/// Macro to create a snapshot assertion with a clean interface
#[macro_export]
macro_rules! assert_snapshot {
    ($name:expr, $content:expr) => {
        insta::assert_snapshot!($name, $content)
    };
}

const STDLIB_GIT_URL: &str = "https://github.com/diodeinc/stdlib";
const KICAD_SYMBOLS_GIT_URL: &str = "https://gitlab.com/kicad/libraries/kicad-symbols";
const KICAD_FOOTPRINTS_GIT_URL: &str = "https://gitlab.com/kicad/libraries/kicad-footprints";

pub struct Sandbox {
    root: TempDir,
    pub home: PathBuf,
    pub gitconfig: PathBuf,
    pub mock_github: PathBuf,
    pub mock_gitlab: PathBuf,
    pub cache_dir: PathBuf,
    default_cwd: PathBuf,
    trace: bool,
    hash_globs: Vec<String>,
    ignore_globs: Vec<String>,
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
            hash_globs: Vec::new(),
            ignore_globs: Vec::new(),
        };
        s.write_gitconfig();
        s
    }

    /// Enable `GIT_TRACE=1` for commands run with `run` / `run_ok` / `cmd`.
    pub fn with_trace(mut self, yes: bool) -> Self {
        self.trace = yes;
        self
    }

    /// Set glob patterns for files that should always be hashed in snapshots.
    pub fn hash_globs<I, S>(&mut self, globs: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.hash_globs = globs.into_iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    /// Set glob patterns for files that should be ignored in snapshots.
    pub fn ignore_globs<I, S>(&mut self, globs: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.ignore_globs = globs.into_iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    /// Get the current default working directory for commands.
    pub fn default_cwd(&self) -> &Path {
        &self.default_cwd
    }

    /// Set the working directory for commands. Path is relative to sandbox root if not absolute.
    /// Creates the directory if it doesn't exist.
    pub fn cwd<P: AsRef<Path>>(&mut self, cwd: P) -> &mut Self {
        let cwd = cwd.as_ref();
        self.default_cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            self.root_path().join(cwd)
        };
        fs::create_dir_all(self.default_cwd()).expect("create default cwd directory");
        self
    }

    /// Absolute path to the sandbox root (useful for placing clones or artifacts).
    pub fn root_path(&self) -> &Path {
        self.root.path()
    }

    pub fn leak_root(&mut self) {
        let temp_dir = std::mem::replace(&mut self.root, TempDir::new().unwrap());
        self.root = temp_dir.into_persistent();
    }

    /// Run a git command in the sandbox's default cwd with injected env.
    fn git<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args_vec: Vec<String> = args
            .into_iter()
            .map(|a| a.as_ref().to_string_lossy().to_string())
            .collect();
        let display = format!("git {}", args_vec.join(" "));
        self.cmd("git", &args_vec)
            .stdout_null()
            .stderr_null()
            .run()
            .unwrap_or_else(|e| panic!("{display} failed: {e}"));
        self
    }

    /// Initialize a git repository in the current default cwd and set default user.
    pub fn init_git(&mut self) -> &mut Self {
        self.git(["init"]);
        // default user
        self.git(["config", "user.email", "test@example.com"]);
        self.git(["config", "user.name", "Sandbox"]);
        self
    }

    /// Stage all changes and commit with the given message.
    pub fn commit<S: AsRef<str>>(&mut self, msg: S) -> &mut Self {
        self.git(["add", "."]);
        self.git(["commit", "-m", msg.as_ref()]);
        self
    }

    /// Create a lightweight tag at HEAD.
    pub fn tag<S: AsRef<str>>(&mut self, name: S) -> &mut Self {
        self.git(["tag", name.as_ref()]);
        self
    }

    /// Write/overwrite a file relative to the current working directory.
    pub fn write<P: AsRef<Path>, S: AsRef<[u8]>>(&mut self, rel: P, contents: S) -> &mut Self {
        let rel_path = rel.as_ref();
        let p = if rel_path.is_absolute() {
            rel_path.to_path_buf()
        } else {
            self.default_cwd.join(rel_path)
        };
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(p, contents).expect("write file");
        self
    }

    /// Take a directory snapshot and return the manifest content.
    /// Path is relative to current working directory if not absolute.
    pub fn snapshot_dir<P: AsRef<Path>>(&self, path: P) -> String {
        let path = path.as_ref();
        let dir_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.default_cwd.join(path)
        };

        let hash_refs: Vec<&str> = self.hash_globs.iter().map(|s| s.as_str()).collect();
        let ignore_refs: Vec<&str> = self.ignore_globs.iter().map(|s| s.as_str()).collect();
        let manifest = crate::snapdir::build_manifest(&dir_path, &hash_refs, &ignore_refs);

        // Sanitize temp paths and timestamps in the manifest
        self.sanitize_output(&manifest)
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
        run_git(&[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ]);

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

    /// Run a cargo binary inside this sandbox and return the formatted output for snapshotting.
    /// Uses `cargo_bin!()` to locate the binary.
    /// For system binaries, use `cmd()` method instead.
    pub fn snapshot_run<I>(&mut self, program: &str, args: I) -> String
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

        let mut expr = duct::cmd(&cargo_bin_path, args.clone());
        expr = expr.dir(&self.default_cwd);
        expr = self.inject_env(expr);

        // Capture both stdout and stderr to prevent terminal output during tests
        let output = expr
            .stderr_capture()
            .stdout_capture()
            .unchecked()
            .run()
            .unwrap();

        if !output.status.success() {
            println!("Leaking root directory: {}", self.root.display());
            self.leak_root();
        }

        let exit_code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let manifest = format!(
            "Command: {} {}\nExit Code: {}\n\n--- STDOUT ---\n{}\n--- STDERR ---\n{}",
            program,
            args.join(" "),
            exit_code,
            stdout.trim_end(),
            stderr.trim_end()
        );
        // Sanitize temp paths and timestamps to make snapshots deterministic
        self.sanitize_output(&manifest)
    }

    pub fn run<I>(&mut self, program: &str, args: I) -> Expression
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

        let mut expr = duct::cmd(&cargo_bin_path, args.clone());
        expr = expr.dir(&self.default_cwd);
        expr = self.inject_env(expr);
        expr
    }

    /// Sanitize temporary paths and timestamps in output to make snapshots deterministic
    pub fn sanitize_output(&self, content: &str) -> String {
        use regex::Regex;

        let mut result = content.to_string();

        // Replace temp directory paths with a placeholder
        // macOS: /private/var/folders/XX/YY/T/.tmpZZZ or /var/folders/XX/YY/T/.tmpZZZ
        let macos_pattern =
            Regex::new(r"(?:/private)?/var/folders/[^/]+/[^/]+/T/\.tmp[a-zA-Z0-9]+").unwrap();
        result = macos_pattern.replace_all(&result, "<TEMP_DIR>").to_string();

        // Linux: /tmp/.tmpXXX
        let linux_pattern = Regex::new(r"/tmp/\.tmp[a-zA-Z0-9]+").unwrap();
        result = linux_pattern.replace_all(&result, "<TEMP_DIR>").to_string();

        // Sanitize ISO 8601 timestamps
        let timestamp_pattern =
            Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+\+\d{2}:\d{2}").unwrap();
        result = timestamp_pattern
            .replace_all(&result, "<TIMESTAMP>")
            .to_string();

        // Sanitize git hashes in JSON "hash" field only
        let git_hash_json_pattern = Regex::new(r#""hash":\s*"[a-f0-9]{7}""#).unwrap();
        result = git_hash_json_pattern
            .replace_all(&result, r#""hash": "<GIT_HASH>""#)
            .to_string();

        // Sanitize system information that varies across platforms
        let arch_pattern = Regex::new(r#""arch":\s*"[^"]+""#).unwrap();
        result = arch_pattern
            .replace_all(&result, r#""arch": "<ARCH>""#)
            .to_string();

        let platform_pattern = Regex::new(r#""platform":\s*"[^"]+""#).unwrap();
        result = platform_pattern
            .replace_all(&result, r#""platform": "<PLATFORM>""#)
            .to_string();

        let user_pattern = Regex::new(r#""user":\s*"[^"]+""#).unwrap();
        result = user_pattern
            .replace_all(&result, r#""user": "<USER>""#)
            .to_string();

        // Sanitize CLI version fields in JSON
        let cli_version_pattern = Regex::new(r#""cli_version"\s*:\s*"[^"]+""#).unwrap();
        result = cli_version_pattern
            .replace_all(&result, r#""cli_version": "<CLI_VERSION>""#)
            .to_string();

        result
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
        // Block HTTP requests by setting invalid proxy - prevents Strategy 3 fallback
        env_map.insert("HTTP_PROXY".into(), "http://127.0.0.1:0".into());
        env_map.insert("HTTPS_PROXY".into(), "http://127.0.0.1:0".into());
        env_map.insert("NO_PROXY".into(), "".into()); // Don't bypass proxy for any domains
        if self.trace {
            env_map.insert("GIT_TRACE".into(), "1".into());
            env_map.insert("GIT_CURL_VERBOSE".into(), "1".into());
        }

        expr = expr.full_env(&env_map);

        expr
    }

    /// Seed the sandbox with real git repositories from remote URLs.
    /// Uses pcb-zen's download functionality for GitHub and GitLab repos.
    pub fn seed_from_git(&mut self, url: &str, revs: &[&str]) {
        let parsed = url::Url::parse(url.trim_end_matches(".git")).expect("valid git URL expected");

        // Translate the URL to the LoadSpec stem we need (@github/... or @gitlab/...)
        let stem = match parsed.host_str() {
            Some("github.com") => {
                let mut segs = parsed.path_segments().expect("URL must have path");
                let user = segs.next().expect("GitHub URL must have user");
                let repo = segs.next().expect("GitHub URL must have repo");
                format!("@github/{user}/{repo}")
            }
            Some("gitlab.com") => {
                let path = parsed.path().trim_start_matches('/');
                format!("@gitlab/{path}")
            }
            _ => panic!("seed_from_git supports github.com / gitlab.com only"),
        };

        let real_cache = pcb_zen::load::cache_dir().unwrap();

        for rev in revs {
            // Let LoadSpec do the heavy lifting for us
            let spec_str = format!("{stem}:{rev}");
            let spec = pcb_zen_core::LoadSpec::parse(&spec_str).expect("generated spec must parse");

            // 1. Ensure it is cached (download if necessary)
            let checked_out = pcb_zen::load::ensure_remote_cached(&spec).unwrap();

            // 2. Mirror it inside the sandbox's private cache directory
            let suffix = checked_out.strip_prefix(&real_cache).unwrap();
            let sandbox_path = self.cache_dir.join(suffix);

            if sandbox_path.exists() {
                fs::remove_dir_all(&sandbox_path).unwrap();
            }
            fs::create_dir_all(sandbox_path.parent().unwrap()).unwrap();

            #[cfg(unix)]
            std::os::unix::fs::symlink(&checked_out, &sandbox_path).unwrap();
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&checked_out, &sandbox_path).unwrap();
        }
    }

    pub fn seed_stdlib(&mut self, versions: &[&str]) -> &mut Self {
        self.seed_from_git(STDLIB_GIT_URL, versions);
        self
    }

    pub fn seed_kicad(&mut self, versions: &[&str]) -> &mut Self {
        self.seed_from_git(KICAD_SYMBOLS_GIT_URL, versions);
        self.seed_from_git(KICAD_FOOTPRINTS_GIT_URL, versions);
        self
    }
}

pub struct FixtureRepo {
    work: PathBuf,
    bare: PathBuf,
    default_branch: String,
}

impl FixtureRepo {
    /// Write/overwrite a file relative to the work tree.
    pub fn write<P: AsRef<Path>, S: AsRef<[u8]>>(&mut self, rel: P, contents: S) -> &mut Self {
        let p = self.work.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(p, contents).expect("write file");
        self
    }

    /// Stage all changes and commit with the given message.
    pub fn commit<S: AsRef<str>>(&mut self, msg: S) -> &mut Self {
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

    /// Create and check out a new branch (or reset existing branch to HEAD).
    pub fn branch<S: AsRef<str>>(&mut self, name: S) -> &mut Self {
        let name = name.as_ref();
        run_git(&["-C", self.work_str(), "checkout", "-B", name]);
        self
    }

    /// Check out an existing branch or commit.
    pub fn checkout<S: AsRef<str>>(&mut self, refname: S) -> &mut Self {
        let refname = refname.as_ref();
        run_git(&["-C", self.work_str(), "checkout", refname]);
        self
    }

    /// Scoped branch helper: create/switch to branch, run closure, then return to original HEAD.
    pub fn with_branch<S: AsRef<str>, F>(&mut self, name: S, f: F) -> &mut Self
    where
        F: FnOnce(&mut Self),
    {
        let original_head = current_head(&self.work);
        self.branch(name);
        f(self);
        run_git(&["-C", self.work_str(), "checkout", &original_head]);
        self
    }

    /// Create or move a tag. If `annotated`, creates/updates an annotated tag.
    pub fn tag<S: AsRef<str>>(&mut self, name: S, annotated: bool) -> &mut Self {
        let name = name.as_ref();
        if annotated {
            run_git(&["-C", self.work_str(), "tag", "-fa", name, "-m", name]);
        } else {
            run_git(&["-C", self.work_str(), "tag", "-f", name]);
        }
        self
    }

    /// Mirror-push all refs to the bare “remote”.
    pub fn push_mirror(&mut self) -> &mut Self {
        run_git(&["-C", self.work_str(), "push", "--mirror", "origin"]);
        self
    }

    pub fn work_dir(&self) -> &Path {
        &self.work
    }
    pub fn bare_dir(&self) -> &Path {
        &self.bare
    }

    pub fn rev_parse_head(&self) -> String {
        let output = duct::cmd("git", ["-C", self.work_str(), "rev-parse", "HEAD"])
            .read()
            .expect("get HEAD");
        output.trim().to_string()
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

fn current_head(work: &Path) -> String {
    let output = duct::cmd(
        "git",
        [
            "-C",
            work.to_str().unwrap(),
            "symbolic-ref",
            "--short",
            "HEAD",
        ],
    )
    .read()
    .unwrap_or_else(|_| {
        // Fallback to rev-parse if we're in detached HEAD state
        duct::cmd("git", ["-C", work.to_str().unwrap(), "rev-parse", "HEAD"])
            .read()
            .expect("get HEAD")
    });
    output.trim().to_string()
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
    #[cfg(not(target_os = "windows"))]
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
        let _ls_output = sb
            .cmd("ls", &["-la", "cloned"])
            .read()
            .expect("ls should succeed");

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
        sb.cwd("test_dir");
        assert_eq!(sb.default_cwd(), sb.root_path().join("test_dir"));

        // Run ls without specifying directory - should use default cwd
        let output = sb.cmd("ls", &["-la"]).read().expect("ls should succeed");

        assert!(output.contains("file.txt"));
        assert!(output.contains("another.txt"));

        // Verify cache dir env var is set correctly
        let cache_output = sb
            .cmd("sh", &["-c", "echo $DIODE_STAR_CACHE_DIR"])
            .read()
            .expect("echo should succeed");
        assert!(cache_output.trim().contains("cache"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_assert_dir_snapshot_integration() {
        let mut sb = Sandbox::new();

        // Create a realistic project structure
        sb.write(
            "project/Cargo.toml",
            r#"[package]
name = "test-project"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = "1.0"
"#,
        )
        .write(
            "project/src/main.rs",
            r#"use std::collections::HashMap;

fn main() {
    let mut map = HashMap::new();
    map.insert("key", "value");
    println!("Hello from {:?}!", map);
}
"#,
        )
        .write(
            "project/src/lib.rs",
            r#"//! A test library

pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }
}
"#,
        )
        .write(
            "project/README.md",
            "# Test Project\n\nThis is a test project for snapshots.\n",
        )
        .write("project/.gitignore", "target/\n*.tmp\n.DS_Store\n");

        // Create a git fixture and clone it to test git integration with snapshots
        sb.git_fixture("https://github.com/example/snapshot-test.git")
            .write("hello.txt", "Hello, snapshot!")
            .write(
                "docs/guide.md",
                "# Getting Started\n\nWelcome to the guide.",
            )
            .commit("Initial commit")
            .tag("v1.0.0", true)
            .push_mirror();

        // Clone the fixture
        sb.cmd(
            "git",
            &[
                "clone",
                "https://github.com/example/snapshot-test.git",
                "cloned-repo",
            ],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("git clone should succeed");

        // Snapshot the entire project directory
        crate::assert_snapshot!("project", sb.snapshot_dir("project"));

        // Also snapshot the cloned repository
        crate::assert_snapshot!("cloned_repo", sb.snapshot_dir("cloned-repo"));
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn test_multi_branch_tags_with_snapshots() {
        let mut sb = Sandbox::new();
        sb.cwd("src");

        // Create a multi-branch fixture with simple single-file content per version
        let mut fixture = sb.git_fixture("https://github.com/example/multi-version.git");

        // Set up main branch
        // Create v1.5 branch with different content
        // Create v1.0 branch with minimal content
        // Back on main, add latest tag
        fixture
            .write("version.txt", "ref=main\nversion=2.0.0-dev")
            .commit("Main branch")
            .with_branch("v1.5", |f| {
                f.write("version.txt", "ref=v1.5\nversion=1.5.0")
                    .commit("Version 1.5.0 release")
                    .tag("v1.5.0", true);
            })
            .with_branch("v1.0", |f| {
                f.write("version.txt", "ref=v1.0\nversion=1.0.0")
                    .commit("Version 1.0.0 release")
                    .tag("v1.0.0", true);
            })
            .checkout("main")
            .tag("latest", false)
            .push_mirror();

        // Clone each branch/tag to separate directories
        sb.cmd(
            "git",
            &[
                "clone",
                "https://github.com/example/multi-version.git",
                "cloned-main",
            ],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("clone main should succeed");

        sb.cmd(
            "git",
            &[
                "clone",
                "-b",
                "v1.5",
                "https://github.com/example/multi-version.git",
                "cloned-v1.5",
            ],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("clone v1.5 should succeed");

        sb.cmd(
            "git",
            &[
                "clone",
                "-b",
                "v1.0",
                "https://github.com/example/multi-version.git",
                "cloned-v1.0",
            ],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("clone v1.0 should succeed");

        // Clone by tag
        sb.cmd(
            "git",
            &[
                "clone",
                "--branch",
                "v1.0.0",
                "https://github.com/example/multi-version.git",
                "cloned-tag-v1.0.0",
            ],
        )
        .stdout_null()
        .stderr_null()
        .run()
        .expect("clone tag v1.0.0 should succeed");

        // Snapshot the current directory (src) to see all cloned directories
        crate::assert_snapshot!("multi_branches", sb.snapshot_dir("."));
    }
}
