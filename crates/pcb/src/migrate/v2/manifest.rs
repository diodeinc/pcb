use anyhow::{Context, Result};
use ignore::WalkBuilder;
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

    // Detect and filter member patterns
    let members = detect_member_patterns(workspace_root)?;
    if !members.is_empty() {
        eprintln!("  Members: {:?}", members);
    }

    // Generate member package pcb.toml files
    generate_member_packages(workspace_root, &members)?;

    // Convert root pcb.toml
    let root_pcb_toml = workspace_root.join("pcb.toml");
    if root_pcb_toml.exists() {
        convert_pcb_toml_to_v2(&root_pcb_toml, Some(&repository), path.as_deref(), &members)?;
        eprintln!("  ✓ Converted {}", root_pcb_toml.display());
    }

    // Find and convert all member pcb.toml files (including newly created ones)
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if path.file_name() == Some(std::ffi::OsStr::new("pcb.toml")) && path != root_pcb_toml {
            convert_pcb_toml_to_v2(path, None, None, &[])?;
            eprintln!("  ✓ Converted {}", path.display());
        }
    }

    Ok(())
}

/// Detect member patterns based on existing directories
fn detect_member_patterns(workspace_root: &Path) -> Result<Vec<String>> {
    let base_patterns = [
        "components/*",
        "reference/*",
        "common/*",
        "modules/*",
        "boards/*",
        "graphics/*",
        "harness/*",
    ];
    let mut filtered = Vec::new();

    for pattern in &base_patterns {
        // Extract directory name (e.g., "components/*" -> "components")
        let dir_name = pattern.trim_end_matches("/*");
        let dir_path = workspace_root.join(dir_name);

        if dir_path.exists() && dir_path.is_dir() {
            filtered.push(pattern.to_string());
        }
    }

    Ok(filtered)
}

/// Generate empty pcb.toml files for member packages
fn generate_member_packages(workspace_root: &Path, members: &[String]) -> Result<()> {
    use std::collections::HashSet;

    let mut created_packages: HashSet<PathBuf> = HashSet::new();

    for pattern in members {
        // Extract directory name (e.g., "components/*" -> "components")
        let dir_name = pattern.trim_end_matches("/*");
        let base_dir = workspace_root.join(dir_name);

        if !base_dir.exists() {
            continue;
        }

        // Walk the directory looking for .zen files, respecting .gitignore
        let walker = WalkBuilder::new(&base_dir)
            .max_depth(Some(3)) // Limit to 3 levels: components/a/b/foo.zen
            .hidden(true) // Ignore hidden files and directories
            .git_ignore(true) // Respect .gitignore
            .git_exclude(true) // Respect .git/info/exclude
            .build();

        for entry in walker.filter_map(|e| e.ok()) {
            let path = entry.path();

            // Skip directories and non-.zen files
            if !path.is_file() || path.extension() != Some(std::ffi::OsStr::new("zen")) {
                continue;
            }

            // Get the directory containing this .zen file
            let zen_dir = match path.parent() {
                Some(dir) => dir,
                None => continue,
            };

            // Skip if we already created a pcb.toml at this level or a parent level
            if created_packages.iter().any(|pkg| zen_dir.starts_with(pkg)) {
                continue;
            }

            // Create empty pcb.toml in this directory
            let pcb_toml = zen_dir.join("pcb.toml");
            if !pcb_toml.exists() {
                std::fs::write(&pcb_toml, "")?;
                eprintln!("  ✓ Created {}", pcb_toml.display());
                created_packages.insert(zen_dir.to_path_buf());
            }
        }
    }

    Ok(())
}

/// Convert a single pcb.toml file to V2 format
fn convert_pcb_toml_to_v2(
    path: &Path,
    repository: Option<&str>,
    workspace_path: Option<&str>,
    members: &[String],
) -> Result<()> {
    let file_provider = DefaultFileProvider::new();

    // Read existing config
    let mut config = PcbToml::from_file(&file_provider, path)?;

    // Check if already V2
    if config.is_v2() {
        eprintln!("  ⊙ Already V2: {}", path.display());
        return Ok(());
    }

    // Clone default_board before conversion
    let default_board = config
        .workspace
        .as_ref()
        .and_then(|w| w.default_board.clone());

    // Clear V1 fields
    config.packages.clear();
    config.module = None;

    // Update workspace section if this is the root
    if let Some(repo) = repository {
        config.workspace = Some(WorkspaceConfig {
            name: None,
            repository: Some(repo.to_string()),
            path: workspace_path.map(|s| s.to_string()),
            resolver: Some("2".to_string()),
            pcb_version: Some("0.2".to_string()),
            members: members.to_vec(),
            default_board,
        });
    } else if let Some(mut ws) = config.workspace.take() {
        // Member package - add resolver
        ws.resolver = Some("2".to_string());
        config.workspace = Some(ws);
    }

    // Serialize and write back
    let content = toml::to_string_pretty(&config).context("Failed to serialize V2 config")?;

    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))?;

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
