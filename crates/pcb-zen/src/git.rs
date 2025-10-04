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
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git fetch failed in bare repo"))
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

/// Push a git tag to remote
pub fn push_tag(repo_root: &Path, tag_name: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("push")
        .arg("origin")
        .arg(tag_name)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git push failed for tag {tag_name}"))
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
