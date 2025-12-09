use std::collections::HashMap;

// Re-export path splitting functions from core
pub use pcb_zen_core::config::{split_asset_repo_and_subpath, split_repo_and_subpath};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(repo_root: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root);
    cmd
}

fn git_global() -> Command {
    Command::new("git")
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
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
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

pub fn get_all_tag_annotations(repo_root: &Path) -> HashMap<String, String> {
    const RECORD_SEP: &str = "\x1E";
    const FIELD_SEP: &str = "\x1F";
    let format = format!("%(refname:short){FIELD_SEP}%(contents){RECORD_SEP}");

    let mut cmd = git(repo_root);
    cmd.args(["for-each-ref", &format!("--format={}", format), "refs/tags"]);

    let Some(stdout) = run_stdout_opt(cmd) else {
        return HashMap::new();
    };

    stdout
        .split(RECORD_SEP)
        .filter_map(|record| {
            let record = record.trim();
            record
                .split_once(FIELD_SEP)
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

pub fn clone_bare_with_filter(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let mut cmd = git_global();
    cmd.args([
        "clone",
        "--bare",
        "--filter=blob:none",
        "--quiet",
        remote_url,
    ])
    .arg(dest_dir);
    run_silent(cmd)
}

pub fn clone_bare(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let mut cmd = git_global();
    cmd.args(["clone", "--bare", "--quiet", remote_url])
        .arg(dest_dir);
    run_silent(cmd)
}

pub fn clone_bare_with_fallback(repo_url: &str, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest.parent().unwrap_or(dest))?;
    let https_url = format!("https://{}.git", repo_url);
    if clone_bare_with_filter(&https_url, dest).is_ok() {
        return Ok(());
    }
    clone_bare_with_filter(&format_ssh_url(repo_url), dest)
}

pub fn fetch_in_bare_repo(bare_repo: &Path) -> anyhow::Result<()> {
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
            "+refs/heads/*:refs/heads/*",
        ],
    )
}

pub fn fetch_branch(repo_root: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    run_in(repo_root, &["fetch", remote, branch, "--quiet"])
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

pub fn cat_file_fetch_head(repo_root: &Path) -> Option<String> {
    run_output_opt(repo_root, &["cat-file", "-p", "FETCH_HEAD"])
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

    for url in [&https_url, &ssh_url] {
        let out = git_global().args(["ls-remote", url, refspec]).output()?;
        if out.status.success() {
            if let Some(commit) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().next())
            {
                return Ok((commit.to_string(), url.clone()));
            }
        }
    }
    anyhow::bail!(
        "Failed to ls-remote {} for {} (tried HTTPS and SSH)",
        refspec,
        module_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_split_repo_and_subpath() {
        assert_eq!(
            split_repo_and_subpath("github.com/user/repo"),
            ("github.com/user/repo", "")
        );
        assert_eq!(
            split_repo_and_subpath("github.com/user/repo/pkg"),
            ("github.com/user/repo", "pkg")
        );
        assert_eq!(
            split_repo_and_subpath("github.com/user/repo/a/b/c"),
            ("github.com/user/repo", "a/b/c")
        );
        assert_eq!(
            split_repo_and_subpath("gitlab.com/group/project/pkg"),
            ("gitlab.com/group/project/pkg", "")
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
    fn test_split_asset_repo_and_subpath() {
        // Known KiCad asset repos
        assert_eq!(
            split_asset_repo_and_subpath("gitlab.com/kicad/libraries/kicad-footprints"),
            ("gitlab.com/kicad/libraries/kicad-footprints", "")
        );
        assert_eq!(
            split_asset_repo_and_subpath(
                "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty"
            ),
            (
                "gitlab.com/kicad/libraries/kicad-footprints",
                "Resistor_SMD.pretty"
            )
        );
        assert_eq!(
            split_asset_repo_and_subpath("gitlab.com/kicad/libraries/kicad-symbols"),
            ("gitlab.com/kicad/libraries/kicad-symbols", "")
        );
        assert_eq!(
            split_asset_repo_and_subpath(
                "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym"
            ),
            (
                "gitlab.com/kicad/libraries/kicad-symbols",
                "Device.kicad_sym"
            )
        );

        // Unknown repos fall back to standard split
        assert_eq!(
            split_asset_repo_and_subpath("github.com/user/assets"),
            ("github.com/user/assets", "")
        );
        assert_eq!(
            split_asset_repo_and_subpath("github.com/user/assets/subdir"),
            ("github.com/user/assets", "subdir")
        );
    }
}
