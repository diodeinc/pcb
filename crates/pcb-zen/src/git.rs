use std::collections::HashMap;

use jiff::{
    Timestamp,
    tz::{Offset, TimeZone},
};
use pcb_zen_core::config::split_repo_and_subpath;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tar::Archive;

#[derive(Debug, Clone)]
pub struct TagMetadata {
    pub target: String,
    pub timestamp: String,
}

fn git(repo_root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root);
    cmd
}

fn git_global() -> Command {
    Command::new("git")
}

fn git_global_noninteractive() -> Command {
    let mut cmd = git_global();
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GCM_INTERACTIVE", "never");
    cmd
}

fn git_global_with_prompt(interactive: bool) -> Command {
    if interactive {
        git_global()
    } else {
        git_global_noninteractive()
    }
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
    cmd.args(["status", "--porcelain", "-z"]);

    let Some(stdout) = run_stdout_opt(cmd) else {
        return Vec::new();
    };

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
        let oid = header_fields.next().unwrap_or_default();
        let object_type = header_fields.next();
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
        let mut target = None;
        let mut timestamp = None;
        for line in object_text.lines() {
            if object_type == Some("tag") {
                if let Some(object) = line.strip_prefix("object ") {
                    target = Some(object.to_string());
                } else if let Some(tag) = line.strip_prefix("tag ") {
                    tag_name = Some(tag.to_string());
                } else if let Some(tagger) = line.strip_prefix("tagger ") {
                    timestamp = parse_git_person_timestamp(tagger);
                }
            } else {
                target = Some(oid.to_string());
                if let Some(committer) = line.strip_prefix("committer ") {
                    timestamp = parse_git_person_timestamp(committer);
                }
            }

            if tag_name.is_some() && target.is_some() && timestamp.is_some() {
                break;
            }
        }

        if let (Some(tag), Some(target), Some(timestamp)) = (tag_name, target, timestamp) {
            metadata.insert(tag, TagMetadata { target, timestamp });
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

fn clone(remote_url: &str, dest_dir: &Path, bare: bool, prompt: bool) -> anyhow::Result<()> {
    let mut cmd = git_global_with_prompt(prompt);
    cmd.arg("clone");
    if bare {
        cmd.arg("--bare");
    }
    cmd.args(["--quiet", remote_url]).arg(dest_dir);
    run_silent(cmd)
}

pub fn clone_bare(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    clone(remote_url, dest_dir, true, true)
}

pub fn clone_bare_with_fallback(repo_url: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    let https_url = format!("https://{}.git", repo_url);
    if clone(&https_url, dest, true, false).is_ok() {
        return Ok(());
    }
    clone_bare(&format_ssh_url(repo_url), dest)
}

fn repo_uses_partial_clone(repo_root: &Path) -> bool {
    run_output_opt(repo_root, &["config", "--get", "remote.origin.promisor"]).is_some()
        || run_output_opt(
            repo_root,
            &["config", "--get", "remote.origin.partialclonefilter"],
        )
        .is_some()
        || run_output_opt(repo_root, &["config", "--get", "extensions.partialclone"]).is_some()
}

fn unset_config_all_if_present(repo_root: &Path, key: &str) -> anyhow::Result<()> {
    let mut cmd = git(repo_root);
    cmd.args(["config", "--unset-all", key]);
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(());
    }

    // `git config --unset-all` exits 5 when the key is missing.
    if out.status.code() == Some(5) {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("does not exist") {
        return Ok(());
    }

    anyhow::bail!("git command failed: {}", stderr.trim())
}

/// Backcompat migration for older `~/.pcb/bare/...` repos that were created as
/// partial/promisor clones.
///
/// Sandbox builds now rely on the shared bare repo being able to serve arbitrary
/// local commits, including unpushed sandbox refs. That does not work reliably
/// when the bare repo is itself a partial clone: `git-upload-pack` disables lazy
/// object fetching when serving another local repo, so the bare repo may have the
/// commit object but still be unable to serve required trees/blobs.
///
/// To keep the migration transparent, we hydrate the existing bare repo in place
/// with `git fetch --refetch` while temporarily overriding the partial-clone
/// config for that one fetch. Only after the refetch succeeds do we clear the
/// promisor settings permanently, committing to fully hydrated bare repos going
/// forward.
fn hydrate_bare_repo_to_full(bare_repo: &Path) -> anyhow::Result<()> {
    if !repo_uses_partial_clone(bare_repo) {
        return Ok(());
    }

    let mut cmd = git(bare_repo);
    cmd.args([
        "-c",
        "remote.origin.promisor=false",
        "-c",
        "remote.origin.partialclonefilter=",
        "-c",
        "extensions.partialclone=",
        "fetch",
        "--refetch",
        "origin",
        "--tags",
        "--force",
        "--prune",
        "--prune-tags",
        "--quiet",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    run_silent(cmd)?;

    unset_config_all_if_present(bare_repo, "remote.origin.promisor")?;
    unset_config_all_if_present(bare_repo, "remote.origin.partialclonefilter")?;
    unset_config_all_if_present(bare_repo, "extensions.partialclone")?;

    Ok(())
}

pub fn fetch_in_bare_repo(bare_repo: &Path) -> anyhow::Result<()> {
    hydrate_bare_repo_to_full(bare_repo)?;
    run_in(
        bare_repo,
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

pub fn ensure_rev_in_bare_repo(bare_repo: &Path, rev: &str) -> anyhow::Result<()> {
    if rev_parse(bare_repo, rev).is_some() {
        return Ok(());
    }

    run_in(bare_repo, &["fetch", "origin", "--quiet", rev])
}

pub fn archive_to_dir(repo_root: &Path, treeish: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let mut cmd = git_global();
    cmd.arg("--git-dir")
        .arg(repo_root)
        .args(["archive", "--format=tar", treeish])
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
    run_in(repo_root, &["fetch", remote, branch, "--quiet"])
}

/// Fetch and sync tags from remote, pruning deleted tags and force-updating moved ones
pub fn fetch_tags(repo_root: &Path, remote: &str) -> anyhow::Result<()> {
    run_in(
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

pub fn push_tag(repo_root: &Path, tag_name: &str, remote: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["push", remote, tag_name])
}

pub fn push_tags(repo_root: &Path, tag_names: &[&str], remote: &str) -> anyhow::Result<()> {
    let mut args = vec!["push", remote];
    args.extend(tag_names);
    run_in(repo_root, &args)
}

pub fn push_branch(repo_root: &Path, branch: &str, remote: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["push", remote, branch])
}

pub fn push_branch_force(repo_root: &Path, branch: &str, remote: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["push", "--force", remote, branch])
}

/// Clone a repository to a destination directory (regular clone, not bare)
pub fn clone_repo(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    clone(remote_url, dest_dir, false, true)
}

/// Clone a repository with HTTPS, falling back to SSH
pub fn clone_with_fallback(repo_url: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    let https_url = format!("https://{}.git", repo_url);
    if clone(&https_url, dest, false, false).is_ok() {
        return Ok(());
    }
    clone_repo(&format_ssh_url(repo_url), dest)
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
    run_in(repo_root, &["fetch", remote, "--quiet"])
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
    let git_root = get_repo_root(workspace_root)?;
    let rel = workspace_root
        .strip_prefix(&git_root)
        .map_err(|_| anyhow::anyhow!("Workspace not within git repository"))?;
    if rel == Path::new("") {
        Ok(None)
    } else {
        Ok(Some(rel.to_path_buf()))
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

pub fn parse_remote_url(url: &str) -> anyhow::Result<String> {
    if let Some(rest) = url.strip_prefix("https://") {
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
    let (repo_url, _) = split_repo_and_subpath(module_path);
    let https_url = format!("https://{}.git", repo_url);
    let ssh_url = format_ssh_url(repo_url);

    for (url, interactive) in [(&https_url, false), (&ssh_url, true)] {
        let mut cmd = git_global_with_prompt(interactive);
        let out = cmd.args(["ls-remote", url, refspec]).output()?;
        if out.status.success()
            && let Some(commit) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().next())
        {
            return Ok((commit.to_string(), url.clone()));
        }
    }
    anyhow::bail!(
        "Failed to ls-remote {} for {} (tried HTTPS and SSH)",
        refspec,
        module_path
    )
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
}
