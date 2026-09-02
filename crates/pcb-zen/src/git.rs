use std::collections::HashMap;
use std::ffi::OsString;

use anyhow::{Context, bail};
use jiff::{
    Timestamp,
    tz::{Offset, TimeZone},
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use tar::Archive;
use tempfile::Builder;
use url::Url;

const DIODEHUB_CREDENTIAL_CACHE_TIMEOUT_SECONDS: u64 = 55 * 60;
const DEFAULT_DIODEHUB_HOST: &str = "code.diode.computer";
const LEGACY_DIODEHUB_CREDENTIAL_HELPER: &str = "!pcb auth git";
const DIODEHUB_CREDENTIAL_HELPER_CONFIG: &str = "credential.https://code.diode.computer.helper";
const DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG: &str =
    "credential.https://code.diode.computer.useHttpPath";
const PCB_GIT_CONFIG_FILE: &str = "gitconfig";
const PCB_GIT_CONFIG_INCLUDE: &str = "include.path";
const GIT_CONFIG_NOT_FOUND: i32 = 5;

static RESOLVED_REPOS: LazyLock<Mutex<HashMap<String, (String, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone)]
pub struct TagMetadata {
    pub timestamp: String,
}

fn git(repo_root: &Path) -> Command {
    let mut cmd = git_global();
    cmd.arg("-C").arg(repo_root);
    cmd
}

fn git_global() -> Command {
    Command::new("git")
}

fn make_noninteractive(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GCM_INTERACTIVE", "never");
}

fn git_network(repo_root: &Path) -> anyhow::Result<Command> {
    let mut cmd = git_global_network()?;
    make_noninteractive(&mut cmd);
    cmd.arg("-C").arg(repo_root);
    Ok(cmd)
}

fn git_global_network() -> anyhow::Result<Command> {
    let mut cmd = git_global();
    add_default_diodehub_https_auth_config(&mut cmd, &credential_cache_socket()?)?;
    Ok(cmd)
}

fn git_global_network_with_prompt(interactive: bool) -> anyhow::Result<Command> {
    let mut cmd = git_global_network()?;
    if !interactive {
        make_noninteractive(&mut cmd);
    }
    Ok(cmd)
}

fn pcb_config_dir() -> anyhow::Result<PathBuf> {
    let config_dir = if let Ok(config_dir) = std::env::var("PCB_CONFIG_DIR") {
        PathBuf::from(config_dir)
    } else {
        dirs::home_dir()
            .context("Failed to get home directory")?
            .join(".pcb")
    };
    let config_dir = if config_dir.is_absolute() {
        config_dir
    } else {
        std::env::current_dir()
            .context("Failed to resolve PCB config directory")?
            .join(config_dir)
    };
    Ok(config_dir)
}

fn credential_cache_socket() -> anyhow::Result<PathBuf> {
    Ok(pcb_config_dir()?
        .join("git-credential-cache")
        .join("socket"))
}

fn pcb_git_config_path() -> anyhow::Result<PathBuf> {
    Ok(pcb_config_dir()?.join(PCB_GIT_CONFIG_FILE))
}

fn credential_cache_helper(socket: &Path) -> anyhow::Result<String> {
    let socket = socket
        .to_str()
        .context("PCB config directory is not valid UTF-8")?;
    Ok(format!(
        "cache --timeout={DIODEHUB_CREDENTIAL_CACHE_TIMEOUT_SECONDS} --socket={}",
        shell_quote(socket)
    ))
}

fn diodehub_credential_helper(host: &str) -> String {
    format!("!pcb auth git --host={}", shell_quote(host))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn add_git_config(cmd: &mut Command, key: &str, value: &str) {
    cmd.arg("-c").arg(format!("{key}={value}"));
}

// Preserve automatic authentication for existing Commercial users. Other
// deployments use the managed Git configuration installed by `configure`.
fn add_default_diodehub_https_auth_config(
    cmd: &mut Command,
    cache_socket: &Path,
) -> anyhow::Result<()> {
    add_git_config(cmd, DIODEHUB_CREDENTIAL_HELPER_CONFIG, "");
    add_git_config(
        cmd,
        DIODEHUB_CREDENTIAL_HELPER_CONFIG,
        &credential_cache_helper(cache_socket)?,
    );
    add_git_config(
        cmd,
        DIODEHUB_CREDENTIAL_HELPER_CONFIG,
        &diodehub_credential_helper(DEFAULT_DIODEHUB_HOST),
    );
    add_git_config(cmd, DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG, "true");
    Ok(())
}

fn run_silent(mut cmd: Command) -> anyhow::Result<()> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git command failed: {}", stderr.trim())
    }
}

fn run_stdout(mut cmd: Command) -> anyhow::Result<String> {
    let out = cmd.output()?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git command failed: {}", stderr.trim())
    }
}

fn run_stdout_opt(mut cmd: Command) -> Option<String> {
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn run_lines(cmd: Command) -> Vec<String> {
    run_stdout_opt(cmd)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn run_check_output(mut cmd: Command, expected: &str) -> bool {
    cmd.output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == expected)
        .unwrap_or(false)
}

pub fn run_in(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = git(repo_root);
    cmd.args(args);
    run_silent(cmd)
}

fn run_network_in(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = git_network(repo_root)?;
    cmd.args(args);
    run_silent(cmd)
}

pub fn run_output(repo_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = git(repo_root);
    cmd.args(args);
    run_stdout(cmd)
}

pub fn run_output_opt(repo_root: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = git(repo_root);
    cmd.args(args);
    run_stdout_opt(cmd)
}

pub fn init(repo_root: &Path) -> anyhow::Result<()> {
    if repo_root.join(".git").exists() {
        return Ok(());
    }

    let status = git(repo_root)
        .args(["init", "-b", "main"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run `git init`")?;
    if !status.success() {
        bail!("`git init` failed with {status}");
    }
    Ok(())
}

pub fn configure_diodehub_credentials_globally(repository_url: &str) -> anyhow::Result<()> {
    let url = credential_url(repository_url)?;
    let credential_origin = url.origin().ascii_serialization();
    let credential_host = url.host_str().expect("credential URL has a host");
    let config_path = pcb_git_config_path()?;
    let cache_helper = credential_cache_helper(&credential_cache_socket()?)?;
    write_pcb_git_config(
        &config_path,
        &credential_origin,
        credential_host,
        &cache_helper,
    )?;
    ensure_git_config_include(&config_path)?;
    remove_legacy_diodehub_config()
}

pub fn unconfigure_diodehub_credentials_globally() -> anyhow::Result<()> {
    clear_diodehub_credential_cache();
    let config_path = pcb_git_config_path()?;
    let config_path = config_path
        .to_str()
        .context("PCB config directory is not valid UTF-8")?;
    unset_git_config_value(PCB_GIT_CONFIG_INCLUDE, config_path)?;
    match std::fs::remove_file(config_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Failed to remove PCB Git configuration"),
    }
    remove_legacy_diodehub_config()
}

fn credential_url(repository_url: &str) -> anyhow::Result<Url> {
    let url = Url::parse(repository_url).context("Invalid DiodeHub repository URL")?;
    if url.scheme() != "https" {
        bail!("DiodeHub repository URL must use HTTPS");
    }
    if url.host_str().is_none() {
        bail!("DiodeHub repository URL must include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("DiodeHub repository URL must not include credentials");
    }
    Ok(url)
}

fn write_pcb_git_config(
    config_path: &Path,
    credential_origin: &str,
    credential_host: &str,
    cache_helper: &str,
) -> anyhow::Result<()> {
    let config_dir = config_path
        .parent()
        .context("PCB Git configuration must have a parent directory")?;
    std::fs::create_dir_all(config_dir).context("Failed to create PCB config directory")?;
    let temp = Builder::new()
        .prefix(".gitconfig.")
        .tempfile_in(config_dir)
        .context("Failed to create temporary PCB Git configuration")?;
    let temp_path = temp.into_temp_path();
    let helper_config = format!("credential.{credential_origin}.helper");
    let use_http_path_config = format!("credential.{credential_origin}.useHttpPath");
    let credential_helper = diodehub_credential_helper(credential_host);

    for (key, value) in [
        (helper_config.as_str(), ""),
        (helper_config.as_str(), cache_helper),
        (helper_config.as_str(), credential_helper.as_str()),
        (use_http_path_config.as_str(), "true"),
    ] {
        run_git_config_file(&temp_path, &["--add", key, value])?;
    }
    temp_path
        .persist(config_path)
        .map_err(|error| anyhow::anyhow!(error))
        .context("Failed to persist PCB Git configuration")?;
    Ok(())
}

fn ensure_git_config_include(config_path: &Path) -> anyhow::Result<()> {
    let config_path = config_path
        .to_str()
        .context("PCB config directory is not valid UTF-8")?;
    unset_git_config_value(PCB_GIT_CONFIG_INCLUDE, config_path)?;
    run_git_config(&["--add", PCB_GIT_CONFIG_INCLUDE, config_path])
}

fn remove_legacy_diodehub_config() -> anyhow::Result<()> {
    let cache_helper = credential_cache_helper(&credential_cache_socket()?)?;
    for value in ["", cache_helper.as_str(), LEGACY_DIODEHUB_CREDENTIAL_HELPER] {
        unset_git_config_value(DIODEHUB_CREDENTIAL_HELPER_CONFIG, value)?;
    }
    unset_git_config_value(DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG, "true")
}

pub fn clear_diodehub_credential_cache() {
    let _ = stop_credential_cache();
}

fn stop_credential_cache() -> anyhow::Result<()> {
    let mut socket_argument = OsString::from("--socket=");
    socket_argument.push(credential_cache_socket()?);
    let output = git_global()
        .arg("credential-cache")
        .arg(socket_argument)
        .arg("exit")
        .output()
        .context("Failed to stop Git credential cache")?;
    if !output.status.success() {
        bail!("`git credential-cache exit` failed with {}", output.status);
    }
    Ok(())
}

fn run_git_config(args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = git_global();
    cmd.args(["config", "--global"]).args(args);
    run_silent(cmd)
}

fn run_git_config_file(path: &Path, args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = git_global();
    cmd.args(["config", "--file"]).arg(path).args(args);
    run_silent(cmd)
}

fn unset_git_config_value(key: &str, value: &str) -> anyhow::Result<()> {
    let status = git_global()
        .args([
            "config",
            "--global",
            "--fixed-value",
            "--unset-all",
            key,
            value,
        ])
        .status()
        .context("Failed to run `git config`")?;
    if !status.success() && status.code() != Some(GIT_CONFIG_NOT_FOUND) {
        bail!("`git config --global --unset-all {key}` failed with {status}");
    }
    Ok(())
}

pub fn rev_parse(repo_root: &Path, ref_name: &str) -> Option<String> {
    let s = run_output_opt(repo_root, &["rev-parse", ref_name])?;
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s)
    } else {
        None
    }
}

pub fn rev_parse_head(repo_root: &Path) -> Option<String> {
    rev_parse(repo_root, "HEAD")
}

pub fn rev_parse_short_head(repo_root: &Path) -> Option<String> {
    run_output_opt(repo_root, &["rev-parse", "--short", "HEAD"])
}

pub fn get_repo_root(path: &Path) -> anyhow::Result<PathBuf> {
    run_output(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

pub fn symbolic_ref_short_head(repo_root: &Path) -> Option<String> {
    run_output_opt(repo_root, &["symbolic-ref", "-q", "--short", "HEAD"])
}

pub fn rev_parse_abbrev_ref_head(repo_root: &Path) -> Option<String> {
    run_output_opt(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD")
}

pub fn tag_exists(repo_root: &Path, tag_name: &str) -> bool {
    let mut cmd = git(repo_root);
    cmd.args(["tag", "-l", tag_name]);
    run_check_output(cmd, tag_name)
}

pub fn list_tags(repo_root: &Path, pattern: &str) -> anyhow::Result<Vec<String>> {
    run_output(repo_root, &["tag", "-l", pattern]).map(|s| {
        s.lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
}

pub fn list_all_tags(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    list_tags(repo_root, "*")
}

pub fn list_all_tags_vec(repo_root: &Path) -> Vec<String> {
    run_lines({
        let mut cmd = git(repo_root);
        cmd.args(["tag", "-l"]);
        cmd
    })
}

pub fn list_tags_merged_into(repo_root: &Path, commit: &str) -> Vec<String> {
    run_lines({
        let mut cmd = git(repo_root);
        cmd.args(["tag", "--merged", commit]);
        cmd
    })
}

pub fn log_subjects(repo_root: &Path, range: Option<&str>, pathspec: Option<&Path>) -> Vec<String> {
    run_lines({
        let mut cmd = git(repo_root);
        cmd.args(["log", "--format=%s"]);
        if let Some(range) = range {
            cmd.arg(range);
        }
        if let Some(pathspec) = pathspec.filter(|path| !path.as_os_str().is_empty()) {
            cmd.arg("--").arg(pathspec);
        }
        cmd
    })
}

pub fn decorated_commits(repo_root: &Path) -> Vec<String> {
    run_lines({
        let mut cmd = git(repo_root);
        cmd.args([
            "log",
            "--simplify-by-decoration",
            "--format=%H%x00%D",
            "HEAD",
        ]);
        cmd
    })
}

pub fn changed_paths_since_in_repo(repo_root: &Path, base: &str) -> Vec<PathBuf> {
    let range = format!("{base}..HEAD");
    let mut cmd = git(repo_root);
    cmd.args(["diff", "--name-only", "--no-renames", &range]);

    run_lines(cmd).into_iter().map(PathBuf::from).collect()
}

pub fn status_paths_in_repo(repo_root: &Path) -> Vec<PathBuf> {
    let mut cmd = git(repo_root);
    cmd.args(["status", "--porcelain", "-z", "--no-renames"]);

    let Ok(output) = cmd.output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .split('\0')
        .filter_map(|record| {
            if record.len() < 4 {
                return None;
            }
            Some(PathBuf::from(&record[3..]))
        })
        .collect()
}

pub fn tags_pointing_at_head(repo_root: &Path) -> Vec<String> {
    run_lines({
        let mut cmd = git(repo_root);
        cmd.args(["tag", "--points-at", "HEAD"]);
        cmd
    })
}

pub fn create_tag(repo_root: &Path, tag_name: &str, message: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["tag", "-a", tag_name, "-m", message])
}

pub fn delete_tag(repo_root: &Path, tag_name: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["tag", "-d", tag_name])
}

pub fn delete_tags(repo_root: &Path, tag_names: &[&str]) -> anyhow::Result<()> {
    if tag_names.is_empty() {
        return Ok(());
    }
    let mut args = vec!["tag", "-d"];
    args.extend(tag_names);
    run_in(repo_root, &args)
}

pub fn describe_tags(repo_root: &Path, commit: &str, tag_prefix: Option<&str>) -> Option<String> {
    let mut args = vec!["describe", "--tags", "--abbrev=0"];
    let match_pattern;
    if let Some(prefix) = tag_prefix {
        match_pattern = format!("{}/*", prefix);
        args.push("--match");
        args.push(&match_pattern);
    }
    args.push(commit);
    run_output_opt(repo_root, &args)
}

pub fn get_tag_metadata(repo_root: &Path, tags: &[String]) -> HashMap<String, TagMetadata> {
    if tags.is_empty() {
        return HashMap::new();
    }

    let mut cmd = git(repo_root);
    cmd.arg("cat-file")
        .arg("--batch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());

    let Ok(mut child) = cmd.spawn() else {
        return HashMap::new();
    };

    if let Some(mut stdin) = child.stdin.take() {
        for tag in tags {
            if writeln!(stdin, "refs/tags/{tag}").is_err() {
                return HashMap::new();
            }
        }
    }

    let Ok(output) = child.wait_with_output() else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }

    parse_cat_file_tag_metadata(&output.stdout, tags)
}

fn parse_cat_file_tag_metadata(mut bytes: &[u8], tags: &[String]) -> HashMap<String, TagMetadata> {
    let mut metadata = HashMap::new();
    let mut input_tags = tags.iter();

    while !bytes.is_empty() {
        let input_tag = input_tags.next();
        let Some(header_end) = bytes.iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&bytes[..header_end]);
        let mut header_fields = header.split_whitespace();
        let object_type = header_fields.nth(1);
        let size = header_fields
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);

        bytes = &bytes[header_end + 1..];
        if bytes.len() < size {
            break;
        }
        let object = &bytes[..size];
        bytes = &bytes[size..];
        if bytes.first() == Some(&b'\n') {
            bytes = &bytes[1..];
        }

        let object_text = String::from_utf8_lossy(object);
        let mut tag_name = input_tag.cloned();
        let mut timestamp = None;
        for line in object_text.lines() {
            if object_type == Some("tag") {
                if let Some(tag) = line.strip_prefix("tag ") {
                    tag_name = Some(tag.to_string());
                } else if let Some(tagger) = line.strip_prefix("tagger ") {
                    timestamp = parse_git_person_timestamp(tagger);
                }
            } else if let Some(committer) = line.strip_prefix("committer ") {
                timestamp = parse_git_person_timestamp(committer);
            }

            if tag_name.is_some() && timestamp.is_some() {
                break;
            }
        }

        if let (Some(tag), Some(timestamp)) = (tag_name, timestamp) {
            metadata.insert(tag, TagMetadata { timestamp });
        }
    }

    metadata
}

fn parse_git_person_timestamp(line: &str) -> Option<String> {
    let mut fields = line.rsplitn(3, ' ');
    let offset = fields.next()?;
    let seconds = fields.next()?.parse::<i64>().ok()?;
    let timestamp = Timestamp::from_second(seconds).ok()?;
    let offset_seconds = parse_git_timezone_offset(offset)?;

    if offset_seconds == 0 {
        return Some(timestamp.strftime("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    let offset = Offset::from_seconds(offset_seconds).ok()?;
    let zoned = timestamp.to_zoned(TimeZone::fixed(offset));
    Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

fn parse_git_timezone_offset(offset: &str) -> Option<i32> {
    let bytes = offset.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hour = offset[1..3].parse::<i32>().ok()?;
    let minute = offset[3..5].parse::<i32>().ok()?;
    Some(sign * (hour * 3_600 + minute * 60))
}

fn clone(remote_url: &str, dest_dir: &Path, prompt: bool) -> anyhow::Result<()> {
    let mut cmd = git_global_network_with_prompt(prompt)?;
    cmd.arg("clone");
    cmd.args(["--quiet", "--no-checkout", remote_url])
        .arg(dest_dir);
    run_silent(cmd)
}

pub fn fetch_in_source_repo(source_repo: &Path) -> anyhow::Result<()> {
    run_network_in(
        source_repo,
        &[
            "fetch",
            "origin",
            "--tags",
            "--force",
            "--prune",
            "--prune-tags",
            "--quiet",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )
}

pub fn ensure_rev_in_source_repo(source_repo: &Path, rev: &str) -> anyhow::Result<()> {
    if rev_parse(source_repo, rev).is_some() {
        return Ok(());
    }

    run_network_in(source_repo, &["fetch", "origin", "--quiet", rev])
}

pub fn archive_to_dir(repo_root: &Path, treeish: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let mut cmd = git(repo_root);
    cmd.args(["archive", "--format=tar", treeish])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture git archive stdout"))?;

    let unpack_result = Archive::new(stdout).unpack(dest_dir);
    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git command failed: {}", stderr.trim())
    }

    unpack_result?;
    Ok(())
}

pub fn fetch_branch(repo_root: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    run_network_in(repo_root, &["fetch", remote, branch, "--quiet"])
}

/// Fetch and sync tags from remote, pruning deleted tags and force-updating moved ones
pub fn fetch_tags(repo_root: &Path, remote: &str) -> anyhow::Result<()> {
    run_network_in(
        repo_root,
        &[
            "fetch",
            remote,
            "--prune-tags",
            "--tags",
            "--force",
            "--quiet",
        ],
    )
}

/// Fetch tags from remote without deleting local-only tags.
pub fn fetch_tags_without_pruning(repo_root: &Path, remote: &str) -> anyhow::Result<()> {
    run_network_in(
        repo_root,
        &["fetch", remote, "--tags", "--force", "--quiet"],
    )
}

pub fn push_tag(repo_root: &Path, tag_name: &str, remote: &str) -> anyhow::Result<()> {
    run_network_in(repo_root, &["push", remote, tag_name])
}

pub fn push_tags(repo_root: &Path, tag_names: &[&str], remote: &str) -> anyhow::Result<()> {
    let mut args = vec!["push", remote];
    args.extend(tag_names);
    run_network_in(repo_root, &args)
}

pub fn push_branch(repo_root: &Path, branch: &str, remote: &str) -> anyhow::Result<()> {
    run_network_in(repo_root, &["push", remote, branch])
}

pub fn push_branch_force(repo_root: &Path, branch: &str, remote: &str) -> anyhow::Result<()> {
    run_network_in(repo_root, &["push", "--force", remote, branch])
}

/// Clone a repository with HTTPS, falling back to SSH
pub fn clone_with_fallback(repo_url: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    with_remote_fallback(repo_url, |url, prompt| clone(url, dest, prompt)).map(|_| ())
}

/// Create or reset a branch to point at a specific ref
pub fn checkout_branch_reset(
    repo_root: &Path,
    branch: &str,
    start_point: &str,
) -> anyhow::Result<()> {
    run_in(repo_root, &["checkout", "-B", branch, start_point])
}

/// Fetch from remote
pub fn fetch(repo_root: &Path, remote: &str) -> anyhow::Result<()> {
    run_network_in(repo_root, &["fetch", remote, "--quiet"])
}

pub fn prune_worktrees(bare_repo: &Path) -> anyhow::Result<()> {
    run_in(bare_repo, &["worktree", "prune"])
}

pub fn create_worktree(bare_repo: &Path, worktree_dir: &Path, rev: &str) -> anyhow::Result<()> {
    let mut cmd = git(bare_repo);
    cmd.args(["worktree", "add", "--detach", "--quiet"])
        .arg(worktree_dir)
        .arg(rev);
    run_silent(cmd)
}

pub fn get_remote_url(repo_root: &Path) -> anyhow::Result<String> {
    run_output(repo_root, &["remote", "get-url", "origin"])
}

pub fn get_remote_url_for(repo_root: &Path, remote: &str) -> anyhow::Result<String> {
    run_output(repo_root, &["remote", "get-url", remote])
}

pub fn get_branch_remote(repo_root: &Path, branch: &str) -> Option<String> {
    run_output_opt(
        repo_root,
        &["config", "--get", &format!("branch.{}.remote", branch)],
    )
}

pub fn detect_repository_url(repo_root: &Path) -> anyhow::Result<String> {
    let remote = run_output_opt(
        repo_root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .and_then(|s| s.split('/').next().map(str::to_string))
    .unwrap_or_else(|| "origin".to_string());
    let url = get_remote_url_for(repo_root, &remote)?;
    parse_remote_url(&url)
}

pub fn get_repo_subpath(workspace_root: &Path) -> anyhow::Result<Option<PathBuf>> {
    let prefix = run_output(workspace_root, &["rev-parse", "--show-prefix"])?;
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        Ok(None)
    } else {
        Ok(Some(PathBuf::from(prefix)))
    }
}

pub fn has_uncommitted_changes(repo_root: &Path) -> anyhow::Result<bool> {
    let out = git(repo_root).args(["status", "--porcelain"]).output()?;
    if !out.status.success() {
        anyhow::bail!("Failed to check git status");
    }
    Ok(!out.stdout.is_empty())
}

pub fn has_uncommitted_changes_in_path(repo_root: &Path, path: &Path) -> bool {
    let path_arg = if path == Path::new("") || path == Path::new(".") {
        "."
    } else {
        return git(repo_root)
            .args(["status", "--porcelain", "--"])
            .arg(path)
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(true);
    };
    git(repo_root)
        .args(["status", "--porcelain", "--", path_arg])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(true)
}

pub fn commit(repo_root: &Path, message: &str) -> anyhow::Result<String> {
    run_in(repo_root, &["add", "-A"])?;
    run_in(repo_root, &["commit", "-m", message])?;
    rev_parse(repo_root, "HEAD").ok_or_else(|| anyhow::anyhow!("Failed to get commit SHA"))
}

pub fn commit_with_trailers(repo_root: &Path, message: &str) -> anyhow::Result<String> {
    run_in(repo_root, &["add", "-A"])?;
    run_in(
        repo_root,
        &[
            "commit",
            "-m",
            message,
            "--trailer",
            "Generated-by: pcb publish",
        ],
    )?;
    rev_parse(repo_root, "HEAD").ok_or_else(|| anyhow::anyhow!("Failed to get commit SHA"))
}

pub fn reset_hard(repo_root: &Path, commit: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["reset", "--hard", commit])
}

pub fn is_available() -> bool {
    git_global()
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn cat_file(repo_root: &Path, object: &str) -> Option<String> {
    run_output_opt(repo_root, &["cat-file", "-p", object])
}

pub fn show_commit_timestamp(repo_root: &Path, commit: &str) -> Option<i64> {
    run_output_opt(repo_root, &["show", "-s", "--format=%ct", commit]).and_then(|s| s.parse().ok())
}

pub fn format_ssh_url(module_path: &str) -> String {
    match module_path.split_once('/') {
        Some((host, path)) => format!("git@{}:{}.git", host, path),
        None => format!("https://{}.git", module_path),
    }
}

/// Split a module path into `(repo_url, subpath)`.
pub fn split_repo_and_subpath(module_path: &str) -> anyhow::Result<(String, String)> {
    if let Some(resolved) = RESOLVED_REPOS
        .lock()
        .expect("repo resolution cache mutex poisoned")
        .get(module_path)
        .cloned()
    {
        return Ok(resolved);
    }

    let resolved = resolve_repo_and_subpath(module_path)?;
    RESOLVED_REPOS
        .lock()
        .expect("repo resolution cache mutex poisoned")
        .insert(module_path.to_string(), resolved.clone());
    Ok(resolved)
}

fn resolve_repo_and_subpath(module_path: &str) -> anyhow::Result<(String, String)> {
    resolve_repo_and_subpath_with(module_path, repo_is_accessible)
}

fn resolve_repo_and_subpath_with(
    module_path: &str,
    mut is_accessible: impl FnMut(&str) -> anyhow::Result<bool>,
) -> anyhow::Result<(String, String)> {
    for (repo_url, subpath) in repo_prefixes(module_path) {
        if is_accessible(&repo_url)? {
            return Ok((repo_url, subpath));
        }
    }
    Ok((module_path.to_string(), String::new()))
}

pub(crate) fn repo_prefixes(module_path: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = module_path.split('/').collect();
    let mut prefixes = Vec::new();
    for n in [4usize, 3] {
        if parts.len() >= n {
            prefixes.push((parts[..n].join("/"), parts[n..].join("/")));
        }
    }
    prefixes
}

fn repo_is_accessible(repo_url: &str) -> anyhow::Result<bool> {
    if crate::cache_index::source_repo_dir(repo_url)?
        .join(".git")
        .exists()
    {
        return Ok(true);
    }

    Ok(remote_is_accessible(repo_url, |url, interactive| {
        ls_remote(url, "HEAD", interactive).map(drop)
    }))
}

fn remote_is_accessible(
    repo_url: &str,
    probe: impl FnMut(&str, bool) -> anyhow::Result<()>,
) -> bool {
    with_remote_fallback(repo_url, probe).is_ok()
}

fn with_remote_fallback<T>(
    repo_url: &str,
    mut operation: impl FnMut(&str, bool) -> anyhow::Result<T>,
) -> anyhow::Result<(T, String)> {
    let https_url = format!("https://{}.git", repo_url);
    match operation(&https_url, false) {
        Ok(value) => Ok((value, https_url)),
        Err(_) => {
            let ssh_url = format_ssh_url(repo_url);
            operation(&ssh_url, true).map(|value| (value, ssh_url))
        }
    }
}

pub fn parse_remote_url(url: &str) -> anyhow::Result<String> {
    if let Some(rest) = url.strip_prefix("https://") {
        return Ok(rest.strip_suffix(".git").unwrap_or(rest).to_string());
    }
    if let Some(rest) = url.strip_prefix("ssh://git@") {
        return Ok(rest.strip_suffix(".git").unwrap_or(rest).to_string());
    }
    if let Some(rest) = url.strip_prefix("git@") {
        let normalized = rest.replace(':', "/");
        return Ok(normalized
            .strip_suffix(".git")
            .unwrap_or(&normalized)
            .to_string());
    }
    anyhow::bail!("Unsupported git URL format: {}", url)
}

pub fn ls_remote_with_fallback(
    module_path: &str,
    refspec: &str,
) -> anyhow::Result<(String, String)> {
    let (repo_url, _) = split_repo_and_subpath(module_path)?;
    let (commit, url) = with_remote_fallback(&repo_url, |url, interactive| {
        ls_remote(url, refspec, interactive)
    })
    .with_context(|| format!("Failed to ls-remote {} for {}", refspec, module_path))?;
    let commit = commit.with_context(|| format!("No matching ref {refspec} for {module_path}"))?;
    Ok((commit, url))
}

fn ls_remote(url: &str, refspec: &str, interactive: bool) -> anyhow::Result<Option<String>> {
    let mut cmd = git_global_network_with_prompt(interactive)?;
    cmd.args(["ls-remote", url, refspec]);
    let out = run_stdout(cmd)?;
    Ok(out
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string))
}

pub fn resolve_branch_head(module_path: &str, branch: &str) -> anyhow::Result<String> {
    let refspec = format!("refs/heads/{}", branch);
    let (commit, _) = ls_remote_with_fallback(module_path, &refspec)?;
    Ok(commit)
}

pub fn lock_manifest(manifest_path: &Path) -> anyhow::Result<fslock::LockFile> {
    let lock_path = manifest_lock_path(manifest_path);
    let lock_dir = lock_path.parent().expect("lock path must have parent");
    std::fs::create_dir_all(lock_dir)?;
    let mut lock = fslock::LockFile::open(&lock_path)?;
    lock.lock()?;
    Ok(lock)
}

fn manifest_lock_path(manifest_path: &Path) -> PathBuf {
    let parent = manifest_path
        .parent()
        .expect("manifest path must have parent");
    let file_name = manifest_path
        .file_name()
        .expect("manifest path must have file name");
    parent
        .join(".pcb")
        .join("locks")
        .join(format!("{}.lock", file_name.to_string_lossy()))
}

/// Acquire a file lock for a directory to prevent concurrent access.
/// Returns a guard that releases the lock when dropped.
pub fn lock_dir(dir: &Path) -> anyhow::Result<fslock::LockFile> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Use OsString to properly append .lock suffix without replacing extension
    // (Path::with_extension would turn "0.4.10" into "0.4.lock")
    let mut lock_path = dir.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = std::path::PathBuf::from(lock_path);
    let mut lock = fslock::LockFile::open(&lock_path)?;
    lock.lock()?;
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_path_appends_suffix() {
        // Verify that lock_dir uses ".lock" suffix appending, not with_extension
        // which would incorrectly turn "0.4.10" into "0.4.lock"
        let check = |dir: &str, expected: &str| {
            let dir = Path::new(dir);
            let mut lock_path = dir.as_os_str().to_os_string();
            lock_path.push(".lock");
            assert_eq!(lock_path.to_string_lossy(), expected);
        };

        check("/cache/pkg/0.4.10", "/cache/pkg/0.4.10.lock");
        check("/cache/pkg/1.0.0", "/cache/pkg/1.0.0.lock");
        check("/cache/pkg/foo", "/cache/pkg/foo.lock");
        check("/cache/pkg/foo.bar", "/cache/pkg/foo.bar.lock");
    }

    #[test]
    fn test_manifest_lock_path() {
        let manifest = Path::new("/repo/boards/IP0003/pcb.toml");
        let lock_path = manifest_lock_path(manifest);
        let parent = manifest.parent().unwrap();
        let expected = parent.join(".pcb").join("locks").join("pcb.toml.lock");
        assert_eq!(lock_path, expected);
    }

    #[test]
    fn test_parse_remote_url_https() {
        assert_eq!(
            parse_remote_url("https://github.com/diodeinc/stdlib.git").unwrap(),
            "github.com/diodeinc/stdlib"
        );
        assert_eq!(
            parse_remote_url("https://github.com/diodeinc/stdlib").unwrap(),
            "github.com/diodeinc/stdlib"
        );
    }

    #[test]
    fn test_parse_remote_url_ssh() {
        assert_eq!(
            parse_remote_url("git@github.com:diodeinc/stdlib.git").unwrap(),
            "github.com/diodeinc/stdlib"
        );
        assert_eq!(
            parse_remote_url("git@github.com:diodeinc/stdlib").unwrap(),
            "github.com/diodeinc/stdlib"
        );
        assert_eq!(
            parse_remote_url("ssh://git@code.diode.computer/demo/b/DM0001").unwrap(),
            "code.diode.computer/demo/b/DM0001"
        );
    }

    #[test]
    fn test_format_ssh_url() {
        assert_eq!(
            format_ssh_url("github.com/user/repo"),
            "git@github.com:user/repo.git"
        );
        assert_eq!(
            format_ssh_url("gitlab.com/group/project"),
            "git@gitlab.com:group/project.git"
        );
    }

    #[test]
    fn repo_resolution_continues_to_shorter_accessible_prefix() {
        let module_path =
            "code.diode.computer/diode/registry/modules/UsbCSource16P/UsbCSource16P.zen";
        let mut probed = Vec::new();

        let resolved = resolve_repo_and_subpath_with(module_path, |repo_url| {
            probed.push(repo_url.to_string());
            Ok(repo_url == "code.diode.computer/diode/registry")
        })
        .unwrap();

        assert_eq!(
            resolved,
            (
                "code.diode.computer/diode/registry".to_string(),
                "modules/UsbCSource16P/UsbCSource16P.zen".to_string(),
            )
        );
        assert_eq!(
            probed,
            [
                "code.diode.computer/diode/registry/modules",
                "code.diode.computer/diode/registry",
            ]
        );
    }

    #[test]
    fn inaccessible_remote_probe_does_not_depend_on_error_text() {
        let mut attempts = Vec::new();

        let accessible = remote_is_accessible(
            "code.diode.computer/diode/registry/modules",
            |url, interactive| {
                attempts.push((url.to_string(), interactive));
                if url.starts_with("https://") {
                    anyhow::bail!("404 Not Found");
                }
                anyhow::bail!("Error: invalid repo");
            },
        );

        assert!(!accessible);
        assert_eq!(
            attempts,
            [
                (
                    "https://code.diode.computer/diode/registry/modules.git".to_string(),
                    false,
                ),
                (
                    "git@code.diode.computer:diode/registry/modules.git".to_string(),
                    true,
                ),
            ]
        );
    }

    #[test]
    fn remote_fallback_uses_ssh_after_https_failure() {
        let mut attempts = Vec::new();
        let (value, url) =
            with_remote_fallback("code.diode.computer/diode/registry", |url, interactive| {
                attempts.push((url.to_string(), interactive));
                if url.starts_with("https://") {
                    anyhow::bail!("HTTPS unavailable");
                }
                Ok("fetched")
            })
            .unwrap();

        assert_eq!(value, "fetched");
        assert_eq!(url, "git@code.diode.computer:diode/registry.git");
        assert_eq!(
            attempts,
            [
                (
                    "https://code.diode.computer/diode/registry.git".to_string(),
                    false
                ),
                (
                    "git@code.diode.computer:diode/registry.git".to_string(),
                    true
                ),
            ]
        );
    }

    #[test]
    fn remote_fallback_stops_after_https_success() {
        let mut attempts = Vec::new();
        let (value, _) = with_remote_fallback("code.diode.computer/diode/registry", |_, prompt| {
            attempts.push(prompt);
            Ok(None::<String>)
        })
        .unwrap();

        assert_eq!(value, None);
        assert_eq!(attempts, [false]);
    }

    #[test]
    fn quotes_credential_cache_socket_for_the_shell() {
        assert_eq!(
            shell_quote("/tmp/PCB's cache/socket"),
            "'/tmp/PCB'\\''s cache/socket'"
        );
    }

    #[test]
    fn repository_network_commands_are_noninteractive() {
        let command = git_network(Path::new(".")).unwrap();
        let env = |key| {
            command
                .get_envs()
                .find(|(name, _)| name == &key)
                .and_then(|(_, value)| value)
        };

        assert_eq!(
            env(std::ffi::OsStr::new("GIT_TERMINAL_PROMPT")),
            Some(std::ffi::OsStr::new("0"))
        );
        assert_eq!(
            env(std::ffi::OsStr::new("GCM_INTERACTIVE")),
            Some(std::ffi::OsStr::new("never"))
        );
    }

    #[test]
    fn process_local_auth_preserves_the_commercial_default() {
        let mut command = git_global();
        add_default_diodehub_https_auth_config(&mut command, Path::new("/tmp/pcb-cache")).unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.iter().any(|arg| {
            arg == "credential.https://code.diode.computer.helper=!pcb auth git --host='code.diode.computer'"
        }));
    }
}
