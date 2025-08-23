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

    /// Path to the .zen file (relative to workspace root)
    pub zen_path: String,

    /// Board description
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
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
    pub fn absolute_zen_path(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(&self.zen_path)
    }
}

/// Walk up the directory tree starting at `start` until a directory containing
/// `pcb.toml` with a `[workspace]` section is found. If we reach the filesystem root
/// without finding one, return the start directory (or its parent if start is a file).
/// Always returns a canonicalized absolute path.
pub fn find_workspace_root(file_provider: &dyn FileProvider, start: &Path) -> PathBuf {
    // Convert to absolute path using combinators
    let abs_start = start
        .canonicalize()
        .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(start)))
        .unwrap_or_else(|_| start.to_path_buf());

    // Start directory (parent if file, self if directory)
    let start_dir = if file_provider.is_directory(&abs_start) {
        abs_start
    } else {
        abs_start.parent().unwrap_or(&abs_start).to_path_buf()
    };

    // Walk up looking for workspace
    std::iter::successors(Some(start_dir.as_path()), |dir| dir.parent())
        .find(|dir| {
            let pcb_toml = dir.join("pcb.toml");
            file_provider.exists(&pcb_toml)
                && PcbToml::from_file(file_provider, &pcb_toml)
                    .is_ok_and(|config| config.is_workspace())
        })
        .map(|p| p.to_path_buf())
        .unwrap_or(start_dir)
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
    let mut boards_by_name = std::collections::HashMap::new();
    let mut errors = Vec::new();
    let mut visited_directories = std::collections::HashSet::new();

    // Helper function to insert boards and handle duplicates (case-insensitive)
    fn insert_board(
        boards_by_name: &mut std::collections::HashMap<String, BoardInfo>,
        errors: &mut Vec<DiscoveryError>,
        board: BoardInfo,
        culprit_path: PathBuf,
        legacy: bool,
    ) {
        // Detect conflicts ignoring case, but preserve original casing for storage/display
        let has_conflict = boards_by_name
            .keys()
            .any(|k| k.eq_ignore_ascii_case(&board.name));

        if has_conflict {
            errors.push(DiscoveryError {
                path: culprit_path,
                error: format!(
                    "Duplicate board name: '{}'{}",
                    board.name,
                    if legacy { " (legacy discovery)" } else { "" }
                ),
            });
        } else {
            boards_by_name.insert(board.name.clone(), board);
        }
    }

    // Primary pass: Walk the workspace directory for pcb.toml files
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
                                visited_directories.insert(path.to_path_buf());
                                let workspace_relative_zen_path =
                                    relative_path.join(&board_config.path);
                                let board = BoardInfo {
                                    name: board_config.name,
                                    zen_path: workspace_relative_zen_path
                                        .to_string_lossy()
                                        .to_string(),
                                    description: board_config.description,
                                };
                                insert_board(
                                    &mut boards_by_name,
                                    &mut errors,
                                    board,
                                    pcb_toml_path,
                                    false,
                                );
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

    // Secondary pass: Look for legacy boards directly under boards/
    let boards_dir = workspace_root.join("boards");
    if file_provider.exists(&boards_dir) {
        // Use FileProvider for consistency
        let entries = match std::fs::read_dir(&boards_dir) {
            Ok(entries) => entries,
            Err(_) => {
                return Ok(DiscoveryResult {
                    boards: Vec::new(),
                    errors,
                })
            }
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();

            // Skip if not a directory or already visited
            if !path.is_dir() || visited_directories.contains(&path) {
                continue;
            }

            // Find .zen files in this directory
            if let Ok(zen_entries) = std::fs::read_dir(&path) {
                let zen_files: Vec<_> = zen_entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "zen")
                    })
                    .collect();

                // Only consider directories with exactly one .zen file
                if zen_files.len() == 1 {
                    let zen_file = &zen_files[0];
                    let zen_filename = zen_file.file_name();
                    let zen_path_str = zen_filename.to_string_lossy();

                    // Board name is the filename without extension
                    let board_name = zen_path_str
                        .strip_suffix(".zen")
                        .unwrap_or(&zen_path_str)
                        .to_string();

                    // Calculate workspace-relative path
                    let board_dir_relative = path.strip_prefix(workspace_root).unwrap_or(&path);
                    let workspace_relative_zen_path = board_dir_relative.join(&*zen_path_str);

                    let board = BoardInfo {
                        name: board_name,
                        zen_path: workspace_relative_zen_path.to_string_lossy().to_string(),
                        description: String::new(),
                    };
                    insert_board(
                        &mut boards_by_name,
                        &mut errors,
                        board,
                        zen_file.path(),
                        true,
                    );
                }
            }
        }
    }

    // Convert to sorted Vec
    let mut boards: Vec<_> = boards_by_name.into_values().collect();
    boards.sort_by(|a, b| a.name.cmp(&b.name));

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

    // If no default_board is configured and we have boards, set the last one as default
    let mut final_config = workspace_config;
    if let Some(config) = &mut final_config {
        if config.default_board.is_none() && !discovery.boards.is_empty() {
            config.default_board = Some(discovery.boards.last().unwrap().name.clone());
        }
    } else if !discovery.boards.is_empty() {
        // Create a minimal workspace config with the last board as default
        final_config = Some(WorkspaceConfig {
            name: None,
            members: default_members(),
            default_board: Some(discovery.boards.last().unwrap().name.clone()),
        });
    }

    Ok(WorkspaceInfo {
        root: workspace_root,
        config: final_config,
        boards: discovery.boards,
        errors: discovery.errors,
    })
}

impl WorkspaceInfo {
    /// Given an absolute .zen path, return the board name
    /// (or None if the file is not one of the workspace boards).
    pub fn board_name_for_zen(&self, zen_path: &Path) -> Option<String> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards
            .iter()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
            .map(|b| b.name.clone())
    }
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
