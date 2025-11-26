use std::path::Path;
use std::process::Command;

pub fn rev_parse(repo_root: &Path, ref_name: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg(ref_name)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(s)
    } else {
        None
    }
}

pub fn rev_parse_head(repo_root: &Path) -> Option<String> {
    rev_parse(repo_root, "HEAD")
}

pub fn symbolic_ref_short_head(repo_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("symbolic-ref")
        .arg("-q")
        .arg("--short")
        .arg("HEAD")
        .output()
        .ok()?;
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

pub fn tag_exists(repo_root: &Path, tag_name: &str) -> bool {
    // return true if the tag exists in the repo
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("tag")
        .arg("-l")
        .arg(tag_name)
        .output()
        .unwrap();
    if !out.status.success() {
        return false;
    }
    let out = String::from_utf8_lossy(&out.stdout).trim().to_string();
    out == tag_name
}

/// Check if git is available on the system
pub fn is_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Clone a bare repository with blob filtering for use as a shared object store
pub fn clone_bare_with_filter(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg("--filter=blob:none")
        .arg("--quiet")
        .arg(remote_url)
        .arg(dest_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git bare clone failed for {remote_url}"))
    }
}

/// Clone a bare repository without filtering (fallback for file:// URLs)
pub fn clone_bare(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg("--quiet")
        .arg(remote_url)
        .arg(dest_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git bare clone failed for {remote_url}"))
    }
}

/// Fetch updates in a bare repository
pub fn fetch_in_bare_repo(bare_repo: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(bare_repo)
        .arg("fetch")
        .arg("origin")
        .arg("--tags")
        .arg("--force")
        .arg("--prune")
        .arg("--prune-tags")
        .arg("--quiet")
        .arg("+refs/heads/*:refs/heads/*")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git fetch failed in bare repo"))
    }
}

/// Prune stale worktree administrative data
pub fn prune_worktrees(bare_repo: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(bare_repo)
        .arg("worktree")
        .arg("prune")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git worktree prune failed"))
    }
}

/// Create a worktree from a bare repository for a specific ref
pub fn create_worktree(bare_repo: &Path, worktree_dir: &Path, rev: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(bare_repo)
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg("--quiet")
        .arg(worktree_dir)
        .arg(rev)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git worktree creation failed for {rev}"))
    }
}

/// Create a git tag
pub fn create_tag(repo_root: &Path, tag_name: &str, message: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("tag")
        .arg("-a")
        .arg(tag_name)
        .arg("-m")
        .arg(message)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git tag creation failed for {tag_name}"))
    }
}

/// Push a git tag to a specific remote
pub fn push_tag(repo_root: &Path, tag_name: &str, remote: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("push")
        .arg(remote)
        .arg(tag_name)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git push failed for tag {tag_name}"))
    }
}

/// Push multiple git tags to a specific remote in one command
pub fn push_tags(repo_root: &Path, tag_names: &[&str], remote: &str) -> anyhow::Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).arg("push").arg(remote);
    for tag in tag_names {
        cmd.arg(tag);
    }

    let status = cmd.status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git push failed"))
    }
}

/// Delete a local git tag
pub fn delete_tag(repo_root: &Path, tag_name: &str) -> anyhow::Result<()> {
    delete_tags(repo_root, &[tag_name])
}

/// Delete multiple local git tags in one command
pub fn delete_tags(repo_root: &Path, tag_names: &[&str]) -> anyhow::Result<()> {
    if tag_names.is_empty() {
        return Ok(());
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_root)
        .arg("tag")
        .arg("-d")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    for tag in tag_names {
        cmd.arg(tag);
    }

    let status = cmd.status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git tag delete failed"))
    }
}

/// List git tags matching a pattern
pub fn list_tags(repo_root: &Path, pattern: &str) -> anyhow::Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("tag")
        .arg("-l")
        .arg(pattern)
        .output()?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("Git tag list failed"));
    }

    let tags_output = String::from_utf8_lossy(&out.stdout);
    let tags: Vec<String> = tags_output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect();

    Ok(tags)
}

/// Get the remote URL for origin
pub fn get_remote_url(repo_root: &Path) -> anyhow::Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("remote")
        .arg("get-url")
        .arg("origin")
        .output()?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("Failed to get remote URL"));
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Check if working directory has uncommitted changes
pub fn has_uncommitted_changes(repo_root: &Path) -> anyhow::Result<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("status")
        .arg("--porcelain")
        .output()?;

    if !out.status.success() {
        return Err(anyhow::anyhow!("Failed to check git status"));
    }

    Ok(!out.stdout.is_empty())
}

/// Get the remote that a branch is tracking
pub fn get_branch_remote(repo_root: &Path, branch: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("config")
        .arg("--get")
        .arg(format!("branch.{}.remote", branch))
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let remote = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if remote.is_empty() {
        None
    } else {
        Some(remote)
    }
}

/// Fetch a specific branch from a remote
pub fn fetch_branch(repo_root: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg(remote)
        .arg(branch)
        .arg("--quiet")
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Failed to fetch {} from {}",
            branch,
            remote
        ))
    }
}
