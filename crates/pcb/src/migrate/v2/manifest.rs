use anyhow::{Context, Result};
use pcb_zen_core::config::{PcbToml, WorkspaceConfig};
use pcb_zen_core::DefaultFileProvider;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

/// Detect git repository URL from the current branch's tracking remote
fn detect_repository(workspace_root: &Path) -> Result<String> {
    // Try to get the current branch's upstream
    let upstream_output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("--symbolic-full-name")
        .arg("@{u}")
        .output()
        .context("Failed to get git upstream branch")?;

    let remote = if upstream_output.status.success() {
        // Extract remote name from upstream (e.g., "origin/main" -> "origin")
        let upstream = String::from_utf8(upstream_output.stdout)?
            .trim()
            .to_string();
        upstream
            .split('/')
            .next()
            .context("Invalid upstream format")?
            .to_string()
    } else {
        // Fall back to "origin" if no upstream configured
        "origin".to_string()
    };

    // Get remote URL
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("remote")
        .arg("get-url")
        .arg(&remote)
        .output()
        .context("Failed to get remote URL")?;

    if !output.status.success() {
        anyhow::bail!(
            "No git remote '{}' configured. Run: git remote add {} <url>",
            remote,
            remote
        );
    }

    let url = String::from_utf8(output.stdout)?.trim().to_string();

    // Parse URL to standard format: github.com/user/repo
    parse_git_url(&url)
}

/// Parse various git URL formats to standard form
fn parse_git_url(url: &str) -> Result<String> {
    // Handle HTTPS: https://github.com/user/repo.git -> github.com/user/repo
    if let Some(rest) = url.strip_prefix("https://") {
        let normalized = rest.strip_suffix(".git").unwrap_or(rest);
        return Ok(normalized.to_string());
    }

    // Handle SSH: git@github.com:user/repo.git -> github.com/user/repo
    if let Some(rest) = url.strip_prefix("git@") {
        let normalized = rest
            .replace(':', "/")
            .strip_suffix(".git")
            .unwrap_or(&rest.replace(':', "/"))
            .to_string();
        return Ok(normalized);
    }

    anyhow::bail!("Unsupported git URL format: {}", url)
}

/// Calculate workspace path relative to git repository root
fn detect_workspace_path(workspace_root: &Path) -> Result<Option<String>> {
    // Get git repository root
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .context("Failed to get git repository root")?;

    if !output.status.success() {
        anyhow::bail!("Not in a git repository");
    }

    let git_root = PathBuf::from(String::from_utf8(output.stdout)?.trim());

    // Calculate relative path
    let rel = workspace_root
        .strip_prefix(&git_root)
        .context("Workspace not within git repository")?;

    if rel == Path::new("") {
        Ok(None) // Workspace at repo root
    } else {
        Ok(Some(rel.to_string_lossy().replace('\\', "/")))
    }
}

/// Convert all pcb.toml files in workspace to V2
pub fn convert_workspace_to_v2(workspace_root: &Path) -> Result<()> {
    let repository = detect_repository(workspace_root)?;
    let path = detect_workspace_path(workspace_root)?;

    eprintln!("  Repository: {}", repository);
    if let Some(ref p) = path {
        eprintln!("  Path: {}", p);
    }

    // Convert root pcb.toml
    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        convert_pcb_toml_to_v2(&root_pcb_toml, Some(&repository), path.as_deref())?;
        eprintln!("  ✓ Converted {}", root_pcb_toml.display());
    }

    // Find and convert all member pcb.toml files
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.file_name() == Some(std::ffi::OsStr::new("pcb.toml"))
            && path != root_pcb_toml
        {
            convert_pcb_toml_to_v2(path, None, None)?;
            eprintln!("  ✓ Converted {}", path.display());
        }
    }

    Ok(())
}

/// Convert a single pcb.toml file to V2 format
fn convert_pcb_toml_to_v2(
    path: &Path,
    repository: Option<&str>,
    workspace_path: Option<&str>,
) -> Result<()> {
    let file_provider = DefaultFileProvider::new();

    // Read existing config
    let config = PcbToml::from_file(&file_provider, path)?;

    // Convert to V2
    let v2 = match config {
        PcbToml::V2(_) => {
            eprintln!("  ⊙ Already V2: {}", path.display());
            return Ok(());
        }
        PcbToml::V1(ref v1) => {
            // Clone members and default_board before conversion
            let members = v1
                .workspace
                .as_ref()
                .map(|w| w.members.clone())
                .unwrap_or_default();
            let default_board = v1.workspace.as_ref().and_then(|w| w.default_board.clone());

            // Use existing conversion logic
            let mut v2_config = config.to_v2()?;

            // Update workspace section if this is the root
            if let Some(repo) = repository {
                v2_config.workspace = Some(WorkspaceConfig {
                    name: None,
                    repository: Some(repo.to_string()),
                    path: workspace_path.map(|s| s.to_string()),
                    resolver: Some("2".to_string()),
                    pcb_version: Some("0.2".to_string()),
                    members,
                    default_board,
                });
            }

            PcbToml::V2(v2_config)
        }
    };

    // Serialize and write back
    let PcbToml::V2(v2_config) = v2 else {
        unreachable!()
    };

    // Remove [packages] section by not including it in serialization
    let content = toml::to_string_pretty(&v2_config)
        .context("Failed to serialize V2 config")?;

    std::fs::write(path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_url_https() {
        assert_eq!(
            parse_git_url("https://github.com/diodeinc/stdlib.git").unwrap(),
            "github.com/diodeinc/stdlib"
        );
        assert_eq!(
            parse_git_url("https://github.com/diodeinc/stdlib").unwrap(),
            "github.com/diodeinc/stdlib"
        );
    }

    #[test]
    fn test_parse_git_url_ssh() {
        assert_eq!(
            parse_git_url("git@github.com:diodeinc/stdlib.git").unwrap(),
            "github.com/diodeinc/stdlib"
        );
        assert_eq!(
            parse_git_url("git@github.com:diodeinc/stdlib").unwrap(),
            "github.com/diodeinc/stdlib"
        );
    }
}
