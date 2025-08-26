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

/// Clone a Git repository as branch or tag (fast, shallow)
pub fn clone_as_branch_or_tag(remote_url: &str, rev: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--branch")
        .arg(rev)
        .arg("--single-branch")
        .arg("--quiet")
        .arg(remote_url)
        .arg(dest_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git clone failed for {remote_url}@{rev}"))
    }
}

/// Clone default branch of a Git repository (shallow)
pub fn clone_default_branch(remote_url: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg("--depth")
        .arg("1")
        .arg("--quiet")
        .arg(remote_url)
        .arg(dest_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git clone failed for {remote_url}"))
    }
}

/// Fetch a specific commit from origin (shallow)
pub fn fetch_commit(repo_root: &Path, rev: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("fetch")
        .arg("--depth")
        .arg("1")
        .arg("--quiet")
        .arg("origin")
        .arg(rev)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git fetch failed for commit {rev}"))
    }
}

/// Checkout a specific revision
pub fn checkout_revision(repo_root: &Path, rev: &str) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("checkout")
        .arg("--quiet")
        .arg(rev)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Git checkout failed for {rev}"))
    }
}
