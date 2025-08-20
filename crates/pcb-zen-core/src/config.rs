use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::FileProvider;

/// Complete representation of a pcb.toml configuration file
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PcbToml {
    /// Workspace configuration section
    #[serde(default)]
    pub workspace: Option<WorkspaceConfig>,

    /// Module configuration section  
    #[serde(default)]
    pub module: Option<ModuleConfig>,

    /// Board configuration section
    #[serde(default)]
    pub board: Option<BoardConfig>,

    /// Package aliases configuration section
    #[serde(default)]
    pub packages: HashMap<String, String>,
}

/// Configuration for [workspace] section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Optional workspace name
    pub name: Option<String>,

    /// List of board directories/patterns (supports globs)
    /// Defaults to ["boards/*"] if not specified
    #[serde(default = "default_members")]
    pub members: Vec<String>,

    /// Default board name to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_board: Option<String>,
}

/// Configuration for [module] section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleConfig {
    /// Module name
    pub name: String,
}

/// Configuration for [board] section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Board name
    pub name: String,

    /// Path to the .zen file for this board (relative to pcb.toml)
    pub path: String,

    /// Optional description of the board
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Board discovery information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardInfo {
    /// Board name
    pub name: String,

    /// Path to the .zen file (relative to board directory)
    pub zen_path: String,

    /// Board description
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// Directory containing the board
    pub directory: PathBuf,
}

/// Discovery errors that can occur during board discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryError {
    pub path: PathBuf,
    pub error: String,
}

/// Result of board discovery with any errors encountered
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub boards: Vec<BoardInfo>,
    pub errors: Vec<DiscoveryError>,
}

/// Workspace information with discovered boards
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    /// Workspace root directory
    pub root: PathBuf,

    /// Workspace configuration if present
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<WorkspaceConfig>,

    /// All discovered boards
    pub boards: Vec<BoardInfo>,

    /// Discovery errors
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<DiscoveryError>,
}

/// Default members pattern
fn default_members() -> Vec<String> {
    vec!["boards/*".to_string()]
}

impl PcbToml {
    /// Parse a pcb.toml file from string content
    pub fn parse(content: &str) -> Result<Self> {
        toml::from_str(content).map_err(|e| anyhow::anyhow!("Failed to parse pcb.toml: {e}"))
    }

    /// Read and parse a pcb.toml file from the filesystem
    pub fn from_file(file_provider: &dyn FileProvider, path: &Path) -> Result<Self> {
        let content = file_provider.read_file(path)?;
        Self::parse(&content)
    }

    /// Check if this configuration represents a workspace
    pub fn is_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    /// Check if this configuration represents a module
    pub fn is_module(&self) -> bool {
        self.module.is_some()
    }

    /// Check if this configuration represents a board
    pub fn is_board(&self) -> bool {
        self.board.is_some()
    }
}

impl BoardInfo {
    /// Get the absolute path to the board's .zen file
    pub fn absolute_zen_path(&self) -> PathBuf {
        self.directory.join(&self.zen_path)
    }
}

/// Walk up the directory tree starting at `start` until a directory containing
/// `pcb.toml` with a `[workspace]` section is found. If we reach the filesystem root
/// without finding one, return the parent directory of `start`.
pub fn find_workspace_root(file_provider: &dyn FileProvider, start: &Path) -> PathBuf {
    let mut current = if !file_provider.is_directory(start) {
        // For files we search from their parent directory.
        start.parent().map(|p| p.to_path_buf())
    } else {
        Some(start.to_path_buf())
    };

    while let Some(dir) = current {
        let pcb_toml = dir.join("pcb.toml");
        if file_provider.exists(&pcb_toml) {
            // Check if the TOML file contains a [workspace] section
            if let Ok(config) = PcbToml::from_file(file_provider, &pcb_toml) {
                if config.is_workspace() {
                    return dir;
                }
            }
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    // If start is a file, use its parent; otherwise use start itself
    if !file_provider.is_directory(start) {
        start.parent().unwrap_or(start).to_path_buf()
    } else {
        start.to_path_buf()
    }
}

/// Discover all boards in a workspace using glob patterns
pub fn discover_boards(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    workspace_config: &Option<WorkspaceConfig>,
) -> Result<DiscoveryResult> {
    let member_patterns = workspace_config
        .as_ref()
        .map(|c| c.members.clone())
        .unwrap_or_else(default_members);

    // Build glob matchers
    let mut builder = GlobSetBuilder::new();
    for pattern in &member_patterns {
        builder.add(Glob::new(pattern)?);
        // Also match the pattern without the /* suffix to catch exact directory matches
        if pattern.ends_with("/*") {
            let exact_pattern = &pattern[..pattern.len() - 2];
            builder.add(Glob::new(exact_pattern)?);
        }
    }

    let glob_set = builder.build()?;
    let mut boards = Vec::new();
    let mut errors = Vec::new();

    // Walk the workspace directory
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Skip if not a directory
        if !path.is_dir() {
            continue;
        }

        // Check if directory matches any glob pattern
        if let Ok(relative_path) = path.strip_prefix(workspace_root) {
            if glob_set.is_match(relative_path) {
                // Look for pcb.toml in this directory
                let pcb_toml_path = path.join("pcb.toml");
                if file_provider.exists(&pcb_toml_path) {
                    match PcbToml::from_file(file_provider, &pcb_toml_path) {
                        Ok(config) => {
                            if let Some(board_config) = config.board {
                                // Check for duplicate board names
                                if boards
                                    .iter()
                                    .any(|b: &BoardInfo| b.name == board_config.name)
                                {
                                    errors.push(DiscoveryError {
                                        path: pcb_toml_path.clone(),
                                        error: format!(
                                            "Duplicate board name: '{}'",
                                            board_config.name
                                        ),
                                    });
                                } else {
                                    boards.push(BoardInfo {
                                        name: board_config.name,
                                        zen_path: board_config.path,
                                        description: board_config.description,
                                        directory: path.to_path_buf(),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            errors.push(DiscoveryError {
                                path: pcb_toml_path,
                                error: format!("Failed to parse pcb.toml: {e}"),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(DiscoveryResult { boards, errors })
}

/// Get complete workspace information including discovered boards
pub fn get_workspace_info(
    file_provider: &dyn FileProvider,
    start_path: &Path,
) -> Result<WorkspaceInfo> {
    let workspace_root = find_workspace_root(file_provider, start_path);

    // Try to read workspace config
    let workspace_config = {
        let pcb_toml_path = workspace_root.join("pcb.toml");
        if file_provider.exists(&pcb_toml_path) {
            match PcbToml::from_file(file_provider, &pcb_toml_path) {
                Ok(config) => config.workspace,
                Err(_) => None,
            }
        } else {
            None
        }
    };

    // Discover boards
    let discovery = discover_boards(file_provider, &workspace_root, &workspace_config)?;

    Ok(WorkspaceInfo {
        root: workspace_root,
        config: workspace_config,
        boards: discovery.boards,
        errors: discovery.errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_workspace_config() {
        let content = r#"
[workspace]
name = "test_workspace"
members = ["boards/*", "custom/board"]
default_board = "MainBoard"

[packages]
stdlib = "@github/diodeinc/stdlib:v1.0.0"
kicad = "@github/diodeinc/kicad"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_workspace());
        assert!(!config.is_module());
        assert!(!config.is_board());

        let workspace = config.workspace.unwrap();
        assert_eq!(workspace.name, Some("test_workspace".to_string()));
        assert_eq!(workspace.members, vec!["boards/*", "custom/board"]);
        assert_eq!(workspace.default_board, Some("MainBoard".to_string()));

        assert_eq!(config.packages.len(), 2);
        assert_eq!(
            config.packages.get("stdlib"),
            Some(&"@github/diodeinc/stdlib:v1.0.0".to_string())
        );
    }

    #[test]
    fn test_parse_module_config() {
        let content = r#"
[module]
name = "led_module"

[packages]
kicad = "@github/custom/kicad"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_workspace());
        assert!(config.is_module());
        assert!(!config.is_board());

        let module = config.module.unwrap();
        assert_eq!(module.name, "led_module");
    }

    #[test]
    fn test_parse_board_config() {
        let content = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "A test board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_workspace());
        assert!(!config.is_module());
        assert!(config.is_board());

        let board = config.board.unwrap();
        assert_eq!(board.name, "TestBoard");
        assert_eq!(board.path, "test_board.zen");
        assert_eq!(board.description, "A test board");
    }

    #[test]
    fn test_parse_board_config_no_description() {
        let content = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
"#;

        let config = PcbToml::parse(content).unwrap();
        let board = config.board.unwrap();
        assert_eq!(board.description, "");
    }

    #[test]
    fn test_parse_empty_config() {
        let content = "";
        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_workspace());
        assert!(!config.is_module());
        assert!(!config.is_board());
        assert!(config.packages.is_empty());
    }

    #[test]
    fn test_packages_only() {
        let content = r#"
[packages]
stdlib = "@github/diodeinc/stdlib:v1.0.0"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.packages.len(), 1);
        assert_eq!(
            config.packages.get("stdlib"),
            Some(&"@github/diodeinc/stdlib:v1.0.0".to_string())
        );
    }

    #[test]
    fn test_default_members() {
        let content = r#"
[workspace]
name = "test"
"#;

        let config = PcbToml::parse(content).unwrap();
        let workspace = config.workspace.unwrap();
        assert_eq!(workspace.members, vec!["boards/*"]);
    }
}
