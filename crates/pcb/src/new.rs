use anyhow::{bail, Context, Result};
use clap::Args;
use colored::Colorize;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::migrate::codemods::manifest_v2::pcb_version_from_cargo;

#[derive(Args, Debug)]
#[command(about = "Create a new PCB workspace")]
pub struct NewArgs {
    /// Name for the new workspace directory
    #[arg(long)]
    pub workspace: String,

    /// Git repository URL (e.g., https://github.com/user/repo)
    #[arg(long)]
    pub repo: String,
}

/// Validate workspace name for use as directory/git repo name
fn validate_workspace_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Workspace name cannot be empty");
    }

    if name.len() > 100 {
        bail!("Workspace name cannot exceed 100 characters");
    }

    if name.starts_with('.') || name.starts_with('-') {
        bail!("Workspace name cannot start with '.' or '-'");
    }

    for c in name.chars() {
        if !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.' {
            bail!(
                "Workspace name contains invalid character '{}'. Only alphanumeric, hyphens, underscores, and dots are allowed",
                c
            );
        }
    }

    Ok(())
}

/// Clean a git repository URL to the canonical format (e.g., "github.com/user/repo")
fn clean_repo_url(url: &str) -> Result<String> {
    let url = url.trim();

    // Handle SSH format: git@github.com:user/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        // Replace first ':' with '/'
        if let Some(idx) = rest.find(':') {
            let (host, path) = rest.split_at(idx);
            let path = &path[1..]; // skip the ':'
            return Ok(format!("{}/{}", host, path));
        }
        bail!("Invalid SSH git URL format: {}", url);
    }

    // Handle HTTPS format: https://github.com/user/repo.git
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let url = url.strip_suffix(".git").unwrap_or(url);
    let url = url.strip_suffix('/').unwrap_or(url);

    // Validate it looks like a valid repo path
    let parts: Vec<&str> = url.split('/').collect();
    if parts.len() < 3 {
        bail!(
            "Repository URL must include host and path (e.g., github.com/user/repo): {}",
            url
        );
    }

    Ok(url.to_string())
}

pub fn execute(args: NewArgs) -> Result<()> {
    // Validate workspace name
    validate_workspace_name(&args.workspace)?;

    // Clean the repository URL
    let repository = clean_repo_url(&args.repo)?;

    let workspace_path = Path::new(&args.workspace);

    // Check if directory already exists
    if workspace_path.exists() {
        bail!("Directory '{}' already exists", args.workspace);
    }

    // Create the directory
    std::fs::create_dir_all(workspace_path)
        .with_context(|| format!("Failed to create directory '{}'", args.workspace))?;

    // Run git init (suppress output)
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(workspace_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to run 'git init'")?;

    if !status.success() {
        bail!("'git init' failed with exit code: {:?}", status.code());
    }

    // Generate pcb.toml
    let pcb_toml_content = format!(
        r#"[workspace]
repository = "{repository}"
pcb-version = "{version}"
members = [
    "components/*",
    "modules/*",
    "boards/*",
]
vendor = ["github.com/diodeinc/registry/**"]
"#,
        repository = repository,
        version = pcb_version_from_cargo(),
    );

    std::fs::write(workspace_path.join("pcb.toml"), pcb_toml_content)
        .context("Failed to write pcb.toml")?;

    // Generate README.md
    let readme_content = r#"# PCB boards

This repository contains schematics and boards built using the `pcb` command line utility by Diode, using the Zener language and open source compiler
"#;

    std::fs::write(workspace_path.join("README.md"), readme_content)
        .context("Failed to write README.md")?;

    eprintln!(
        "{} {} ({})",
        "Created".green(),
        args.workspace.bold(),
        repository.cyan()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_workspace_name() {
        // Valid names
        assert!(validate_workspace_name("my-project").is_ok());
        assert!(validate_workspace_name("my_project").is_ok());
        assert!(validate_workspace_name("myProject123").is_ok());
        assert!(validate_workspace_name("project.v2").is_ok());

        // Invalid names
        assert!(validate_workspace_name("").is_err());
        assert!(validate_workspace_name(".hidden").is_err());
        assert!(validate_workspace_name("-invalid").is_err());
        assert!(validate_workspace_name("has spaces").is_err());
        assert!(validate_workspace_name("has/slash").is_err());
        assert!(validate_workspace_name(&"a".repeat(101)).is_err());
    }

    #[test]
    fn test_clean_repo_url() {
        // HTTPS URLs
        assert_eq!(
            clean_repo_url("https://github.com/user/repo").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("https://github.com/user/repo.git").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("https://github.com/user/repo/").unwrap(),
            "github.com/user/repo"
        );

        // SSH URLs
        assert_eq!(
            clean_repo_url("git@github.com:user/repo.git").unwrap(),
            "github.com/user/repo"
        );
        assert_eq!(
            clean_repo_url("git@gitlab.com:user/repo").unwrap(),
            "gitlab.com/user/repo"
        );

        // Already clean
        assert_eq!(
            clean_repo_url("github.com/user/repo").unwrap(),
            "github.com/user/repo"
        );

        // Invalid
        assert!(clean_repo_url("invalid").is_err());
        assert!(clean_repo_url("github.com/user").is_err());
    }
}
