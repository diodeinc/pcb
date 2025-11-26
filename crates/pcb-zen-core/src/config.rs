use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::FileProvider;

/// Top-level pcb.toml configuration
///
/// Version detection:
/// - If [workspace].resolver = "2" → V2 mode (new packaging system)
/// - Else → V1 mode (legacy)
///
/// Both V1 and V2 fields coexist in the same struct. The resolver value determines behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PcbToml {
    /// Workspace configuration section
    pub workspace: Option<WorkspaceConfig>,

    /// Module configuration section (V1 only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleConfig>,

    /// Board configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,

    /// Package aliases configuration section (V1 only)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub packages: HashMap<String, String>,

    /// Dependencies (V2 only - code packages with pcb.toml)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    /// Assets (V2 only - repositories without pcb.toml, e.g., KiCad libraries)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, AssetDependencySpec>,

    /// Patches for local development (V2 only)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patch: BTreeMap<String, PatchSpec>,

    /// Vendor configuration (V2 only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<VendorConfig>,

    /// Access control configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessConfig>,
}

impl PcbToml {
    /// Check if this is V2 mode
    /// - Workspace with resolver="2" → V2
    /// - Workspace with no resolver or resolver="1" → V1
    /// - Has V1 [packages] section → V1
    /// - Has V2 fields (dependencies, assets, patch, vendor) → V2
    /// - Empty or ambiguous → V1 (backwards compat)
    pub fn is_v2(&self) -> bool {
        // Explicit resolver in workspace takes precedence
        if let Some(w) = &self.workspace {
            return w.resolver.as_deref() == Some("2");
        }

        // V1 [packages] section means V1
        if !self.packages.is_empty() {
            return false;
        }

        // V2-specific fields indicate V2
        !self.dependencies.is_empty()
            || !self.assets.is_empty()
            || !self.patch.is_empty()
            || self.vendor.is_some()
    }

    /// Parse from TOML string
    pub fn parse(content: &str) -> Result<Self> {
        toml::from_str(content).map_err(|e| anyhow::anyhow!("Failed to parse pcb.toml: {e}"))
    }

    /// Parse from file content
    pub fn from_file(file_provider: &dyn FileProvider, path: &Path) -> Result<Self> {
        let content = file_provider.read_file(path)?;
        Self::parse(&content)
    }

    /// Check if this configuration represents a workspace
    pub fn is_workspace(&self) -> bool {
        self.workspace.is_some()
    }

    /// Check if this configuration represents a module (V1 only)
    pub fn is_module(&self) -> bool {
        self.module.is_some()
    }

    /// Check if this configuration represents a board
    pub fn is_board(&self) -> bool {
        self.board.is_some()
    }

    /// Get the version as a string
    pub fn version(&self) -> &str {
        if self.is_v2() {
            "2"
        } else {
            "1"
        }
    }

    /// Get package aliases (V1 only - V2 does not support aliases)
    pub fn packages(&self) -> HashMap<String, String> {
        if self.is_v2() {
            HashMap::new()
        } else {
            self.packages.clone()
        }
    }

    /// Auto-generate aliases from dependencies and assets (V2 only)
    ///
    /// Takes the last path segment as the alias key. Only creates alias if unique (no collisions).
    /// Examples:
    /// - "github.com/diodeinc/stdlib" → "@stdlib"
    /// - "github.com/diodeinc/registry/reference/XAL7070-562MEx" → "@XAL7070-562MEx"
    /// - "gitlab.com/kicad/libraries/kicad-symbols" → "@kicad-symbols"
    pub fn auto_generated_aliases(&self) -> HashMap<String, String> {
        let mut aliases = HashMap::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();

        // Collect all URLs from dependencies and assets
        let all_urls: Vec<String> = self
            .dependencies
            .keys()
            .chain(self.assets.keys())
            .cloned()
            .collect();

        // First pass: count occurrences of each last segment
        for url in &all_urls {
            if let Some(last_segment) = url.split('/').next_back() {
                *seen_names.entry(last_segment.to_string()).or_insert(0) += 1;
            }
        }

        // Second pass: only add non-duplicate aliases
        for url in &all_urls {
            if let Some(last_segment) = url.split('/').next_back() {
                let segment_string = last_segment.to_string();
                if seen_names.get(&segment_string) == Some(&1) {
                    aliases.insert(segment_string, url.clone());
                }
            }
        }

        aliases
    }
}

/// Workspace configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Optional workspace name (V1 only, ignored in V2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Repository URL for workspace (V2 only, required for V2 multi-package workspaces)
    /// Example: "github.com/diodeinc/registry"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,

    /// Optional subpath within repository (V2 only)
    /// Only needed if workspace root is not at repository root
    /// Example: "hardware/boards" for nested workspaces in monorepos
    /// Member package paths are inferred as: repository + "/" + path + "/" + relative_dir
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Dependency resolver version (V2: "2", V1: "1" or absent)
    /// Determines packaging system version. Required for V2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,

    /// Minimum compatible toolchain release series (e.g., "0.3")
    /// V2 only. Indicates breaking changes requiring newer compiler.
    #[serde(skip_serializing_if = "Option::is_none", rename = "pcb-version")]
    pub pcb_version: Option<String>,

    /// List of board directories/patterns (supports globs)
    #[serde(default = "default_members")]
    pub members: Vec<String>,

    /// Default board name to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_board: Option<String>,
}

/// Access control configuration (shared by V1 and V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessConfig {
    /// Access control list (email patterns)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
}

/// Module configuration (V1 only)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleConfig {
    /// Module name
    pub name: String,
}

/// Board configuration (used in both V1 and V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    /// Board name
    pub name: String,

    /// Path to the .zen file for this board (relative to pcb.toml)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Optional description of the board
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Board configuration (used for compatibility with external crates expecting BoardConfig name)
pub type BoardConfig = Board;

/// V2 Dependency specification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencySpec {
    /// Simple version string (e.g., "0.3.2", "^0.3.2", "0")
    Version(String),

    /// Detailed specification with additional options
    Detailed(DependencyDetail),
}

/// V2 Detailed dependency specification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyDetail {
    /// Specific version requirement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Git branch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Git revision (commit hash)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    /// Local path dependency
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// V2 Patch specification for local development
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchSpec {
    /// Local path to use as replacement
    pub path: String,
}

/// V2 Asset dependency specification
///
/// Asset dependencies are Git repositories without pcb.toml manifests (e.g., KiCad libraries).
/// They are leaf nodes - no transitive dependencies, no semver coalescing.
/// Each ref/tag is treated as isolated (no MVS participation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssetDependencySpec {
    /// Simple ref string - used literally as git tag/branch (no v-prefix logic)
    /// Examples: "v7.0.0", "2024-09-release", "kicad-7.0.0"
    Ref(String),

    /// Detailed specification with branch/rev support
    Detailed(AssetDependencyDetail),
}

/// V2 Detailed asset dependency specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDependencyDetail {
    /// Git ref (tag/branch) - used literally, no semver parsing or v-prefix fallback
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Git branch - resolved to commit hash in lockfile
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Git revision (commit hash)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    // Note: No `path` field - local asset development uses [patch], not inline path
}

/// V2 Lockfile entry
///
/// Stores resolved version and cryptographic hashes for a dependency.
/// Format mirrors Go's go.sum with separate content and manifest hashes.
///
/// # Example
/// ```
/// github.com/diodeinc/stdlib v0.3.2 h1:abc123...
/// github.com/diodeinc/stdlib v0.3.2/pcb.toml h1:def456...
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// Module path (e.g., "github.com/diodeinc/stdlib")
    pub module_path: String,

    /// Resolved version (may be pseudo-version for branches)
    pub version: String,

    /// Content hash (h1: prefix + base64-encoded SHA-256)
    pub content_hash: String,

    /// Manifest hash (h1: prefix + base64-encoded SHA-256)
    /// None for asset packages without pcb.toml
    pub manifest_hash: Option<String>,
}

/// V2 Lockfile (pcb.sum)
///
/// Stores resolved versions and cryptographic hashes for reproducible builds.
/// Automatically updated when dependencies change.
#[derive(Debug, Clone, Default)]
pub struct Lockfile {
    /// Map from (module_path, version) to lock entry
    pub entries: HashMap<(String, String), LockEntry>,
}

impl Lockfile {
    /// Parse pcb.sum file
    ///
    /// Format:
    /// ```
    /// module_path version h1:hash
    /// module_path version/pcb.toml h1:hash
    /// ```
    pub fn parse(content: &str) -> Result<Self> {
        let mut entries = HashMap::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() != 3 {
                anyhow::bail!("Invalid lockfile line: {}", line);
            }

            let module_path = parts[0];
            let version_part = parts[1];
            let hash = parts[2];

            if !hash.starts_with("h1:") {
                anyhow::bail!("Invalid hash format (expected h1:): {}", hash);
            }

            // Check if this is a manifest hash line (ends with /pcb.toml)
            if let Some(version) = version_part.strip_suffix("/pcb.toml") {
                // Update existing entry with manifest hash
                let key = (module_path.to_string(), version.to_string());
                entries
                    .entry(key.clone())
                    .or_insert_with(|| LockEntry {
                        module_path: module_path.to_string(),
                        version: version.to_string(),
                        content_hash: String::new(),
                        manifest_hash: None,
                    })
                    .manifest_hash = Some(hash.to_string());
            } else {
                // Content hash line
                let key = (module_path.to_string(), version_part.to_string());
                entries
                    .entry(key.clone())
                    .or_insert_with(|| LockEntry {
                        module_path: module_path.to_string(),
                        version: version_part.to_string(),
                        content_hash: String::new(),
                        manifest_hash: None,
                    })
                    .content_hash = hash.to_string();
            }
        }

        Ok(Lockfile { entries })
    }

    /// Get lock entry for a module
    pub fn get(&self, module_path: &str, version: &str) -> Option<&LockEntry> {
        self.entries
            .get(&(module_path.to_string(), version.to_string()))
    }

    /// Insert or update lock entry
    pub fn insert(&mut self, entry: LockEntry) {
        let key = (entry.module_path.clone(), entry.version.clone());
        self.entries.insert(key, entry);
    }

    /// Iterate over all lock entries
    pub fn iter(&self) -> impl Iterator<Item = &LockEntry> {
        self.entries.values()
    }

    /// Find any locked version for a module path
    ///
    /// Returns the first entry found for the given module path (useful for branch/rev lookups).
    pub fn find_by_path(&self, module_path: &str) -> Option<&LockEntry> {
        self.entries.values().find(|e| e.module_path == module_path)
    }
}

impl std::fmt::Display for Lockfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut lines = Vec::new();

        // Sort entries for deterministic output
        let mut sorted: Vec<_> = self.entries.values().collect();
        sorted.sort_by(|a, b| {
            a.module_path
                .cmp(&b.module_path)
                .then(a.version.cmp(&b.version))
        });

        for entry in sorted {
            // Content hash line
            lines.push(format!(
                "{} {} {}",
                entry.module_path, entry.version, entry.content_hash
            ));

            // Manifest hash line (if present)
            if let Some(manifest_hash) = &entry.manifest_hash {
                lines.push(format!(
                    "{} {}/pcb.toml {}",
                    entry.module_path, entry.version, manifest_hash
                ));
            }
        }

        writeln!(f, "{}", lines.join("\n"))
    }
}

/// V2 Vendor configuration
///
/// Controls which dependencies are vendored. Dependencies are always resolved
/// from the vendor directory first if present, falling back to network fetch.
///
/// # Example (Vendor Everything)
/// ```toml
/// [vendor]
/// directory = "vendor"
/// match = ["*"]
/// ```
///
/// # Example (Selective Vendoring)
/// ```toml
/// [vendor]
/// directory = "vendor"
/// match = [
///     "github.com/diodeinc/registry/reference/ti",
///     "github.com/diodeinc/stdlib"
/// ]
/// ```
///
/// # Example (Vendor All Registry Components)
/// ```toml
/// [vendor]
/// directory = "vendor"
/// match = ["github.com/diodeinc/registry/reference"]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VendorConfig {
    /// Directory where vendored dependencies are stored
    /// Defaults to "vendor" if not specified
    #[serde(default = "default_vendor_directory")]
    pub directory: String,

    /// List of package prefixes to vendor
    ///
    /// **Default: vendor everything** (empty list = all packages)
    ///
    /// Examples:
    /// - `[]` (empty/default) → vendors all dependencies
    /// - `["*"]` → vendors all dependencies (explicit)
    /// - `["github.com/org/repo"]` → vendors only packages matching this prefix
    /// - `["prefix1", "prefix2"]` → vendors packages matching any prefix
    #[serde(default, rename = "match")]
    pub match_patterns: Vec<String>,
}

impl VendorConfig {
    /// Check if a package should be vendored based on match patterns
    ///
    /// # Arguments
    /// * `package_url` - The package URL (e.g., "github.com/diodeinc/stdlib")
    ///
    /// # Returns
    /// `true` if the package matches any vendor pattern
    pub fn should_vendor(&self, package_url: &str) -> bool {
        // Empty patterns means vendor everything (default behavior)
        if self.match_patterns.is_empty() {
            return true;
        }

        // Check for wildcard
        if self.match_patterns.contains(&"*".to_string()) {
            return true;
        }

        // Check if package URL matches any prefix pattern
        self.match_patterns
            .iter()
            .any(|pattern| package_url.starts_with(pattern))
    }
}

/// Default vendor directory
fn default_vendor_directory() -> String {
    "vendor".to_string()
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
    // Strategy: Prefer explicit [workspace], fall back to first V2 pcb.toml (implicit workspace)
    let candidates: Vec<_> = std::iter::successors(Some(start_dir.as_path()), |dir| dir.parent())
        .filter_map(|dir| {
            let pcb_toml = dir.join("pcb.toml");
            if !file_provider.exists(&pcb_toml) {
                return None;
            }

            match PcbToml::from_file(file_provider, &pcb_toml) {
                Ok(config) => {
                    let is_explicit_workspace = config.is_workspace();
                    let is_v2 = config.is_v2();
                    // V1 requires explicit workspace; V2 can be implicit
                    if is_explicit_workspace || is_v2 {
                        Some((dir.to_path_buf(), is_explicit_workspace))
                    } else {
                        None
                    }
                }
                Err(_) => None,
            }
        })
        .collect();

    // Prefer explicit workspaces, fall back to first V2 package (implicit workspace)
    candidates
        .iter()
        .find(|(_, is_explicit)| *is_explicit)
        .or_else(|| candidates.first())
        .map(|(path, _)| path.clone())
        .unwrap_or(start_dir)
}

/// Helper function to find single .zen file in a directory
fn find_single_zen_file(dir: &Path) -> Option<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let zen_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file() && e.path().extension().is_some_and(|ext| ext == "zen"))
        .collect();

    if zen_files.len() == 1 {
        zen_files[0]
            .file_name()
            .to_string_lossy()
            .to_string()
            .into()
    } else {
        None
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
                            // Handle V1 board configs
                            if !config.is_v2() {
                                if let Some(board_config) = &config.board {
                                    visited_directories.insert(path.to_path_buf());

                                    // Determine the zen file path
                                    let zen_path = if let Some(configured_path) = &board_config.path
                                    {
                                        configured_path.clone()
                                    } else {
                                        // Look for single .zen file in directory
                                        match find_single_zen_file(path) {
                                            Some(zen_file) => zen_file,
                                            None => {
                                                errors.push(DiscoveryError {
                                                    path: pcb_toml_path.clone(),
                                                    error: "No path specified and no single .zen file found in directory".to_string(),
                                                });
                                                continue;
                                            }
                                        }
                                    };

                                    let workspace_relative_zen_path = relative_path.join(&zen_path);
                                    let board = BoardInfo {
                                        name: board_config.name.clone(),
                                        zen_path: workspace_relative_zen_path
                                            .to_string_lossy()
                                            .to_string(),
                                        description: board_config.description.clone(),
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

            // Look for single .zen file in this directory
            if let Some(zen_filename) = find_single_zen_file(&path) {
                // Board name is the filename without extension
                let board_name = zen_filename
                    .strip_suffix(".zen")
                    .unwrap_or(&zen_filename)
                    .to_string();

                // Calculate workspace-relative path
                let board_dir_relative = path.strip_prefix(workspace_root).unwrap_or(&path);
                let workspace_relative_zen_path = board_dir_relative.join(&zen_filename);
                let board = BoardInfo {
                    name: board_name,
                    zen_path: workspace_relative_zen_path.to_string_lossy().to_string(),
                    description: String::new(),
                };
                insert_board(
                    &mut boards_by_name,
                    &mut errors,
                    board,
                    path.join(&zen_filename),
                    true,
                );
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
                Ok(config) => {
                    if config.is_v2() {
                        // TODO: Handle V2 workspace config
                        None
                    } else {
                        config.workspace
                    }
                }
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
            repository: None,
            path: None,
            resolver: None,
            pcb_version: None,
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

    /// Given an absolute .zen path, return the full BoardInfo
    /// (or None if the file is not one of the workspace boards).
    pub fn board_info_for_zen(&self, zen_path: &Path) -> Option<&BoardInfo> {
        let canon = zen_path.canonicalize().ok()?;
        self.boards
            .iter()
            .find(|b| b.absolute_zen_path(&self.root) == canon)
    }

    /// Find a board by name, returning an error with available boards if not found
    pub fn find_board_by_name(&self, board_name: &str) -> Result<&BoardInfo> {
        self.boards
            .iter()
            .find(|b| b.name == board_name)
            .ok_or_else(|| {
                let available: Vec<_> = self.boards.iter().map(|b| b.name.as_str()).collect();
                anyhow::anyhow!(
                    "Board '{board_name}' not found. Available: [{}]",
                    available.join(", ")
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_v1_workspace() {
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
        assert!(!config.is_v2());
        assert!(config.is_workspace());
        assert!(!config.is_module());
        assert!(!config.is_board());
        assert_eq!(config.version(), "1");

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
    fn test_parse_v1_board() {
        let content = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "A test board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_v2());
        assert!(config.is_board());
        assert_eq!(config.version(), "1");

        let board = config.board.unwrap();
        assert_eq!(board.name, "TestBoard");
        assert_eq!(board.path, Some("test_board.zen".to_string()));
        assert_eq!(board.description, "A test board");
    }

    #[test]
    fn test_parse_v2_package() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2());
        assert_eq!(config.version(), "2");

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.resolver.as_deref(), Some("2"));
        assert_eq!(workspace.pcb_version.as_deref(), Some("0.3"));

        let board = config.board.as_ref().unwrap();
        assert_eq!(board.name, "WV0002");
        assert_eq!(board.path, Some("WV0002.zen".to_string()));
        assert_eq!(board.description, "Power Regulator Board");
    }

    #[test]
    fn test_parse_v2_workspace() {
        let content = r#"
[workspace]
resolver = "2"
members = ["boards/*"]

[access]
allow = ["*@weaverobots.com"]
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2());
        assert!(config.is_workspace());
        assert_eq!(config.version(), "2");

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.resolver.as_deref(), Some("2"));
        assert_eq!(workspace.members, vec!["boards/*"]);

        let access = config.access.as_ref().unwrap();
        assert_eq!(access.allow, vec!["*@weaverobots.com"]);
    }

    #[test]
    fn test_parse_v2_dependencies() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[dependencies]
"github.com/diodeinc/stdlib" = "0.3.2"
"github.com/diodeinc/registry/reference/ti/tps54331" = { version = "^1.0.0" }
"github.com/user/custom" = { branch = "main" }
"github.com/user/local" = { path = "../local" }
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.dependencies.len(), 4);

        // Test simple version string
        match config
            .dependencies
            .get("github.com/diodeinc/stdlib")
            .unwrap()
        {
            DependencySpec::Version(v) => assert_eq!(v, "0.3.2"),
            _ => panic!("Expected Version variant"),
        }

        // Test detailed spec with version
        match config
            .dependencies
            .get("github.com/diodeinc/registry/reference/ti/tps54331")
            .unwrap()
        {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.version, Some("^1.0.0".to_string()));
            }
            _ => panic!("Expected Detailed variant"),
        }

        // Test branch spec
        match config.dependencies.get("github.com/user/custom").unwrap() {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.branch, Some("main".to_string()));
            }
            _ => panic!("Expected Detailed variant"),
        }
    }

    #[test]
    fn test_parse_v2_patch() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[patch]
"github.com/diodeinc/stdlib" = { path = "../stdlib" }
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.patch.len(), 1);

        let patch = config.patch.get("github.com/diodeinc/stdlib").unwrap();
        assert_eq!(patch.path, "../stdlib");
    }

    #[test]
    fn test_v2_workspace_and_board() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"
members = ["boards/*"]

[board]
name = "RootBoard"
"#;

        let result = PcbToml::parse(content);
        assert!(result.is_ok());
        let config = result.unwrap();
        assert!(config.workspace.is_some());
        assert!(config.board.is_some());
    }

    #[test]
    fn test_unsupported_version() {
        let content = r#"
[workspace]
resolver = "3"
"#;

        // Currently we don't validate resolver version during parse,
        // so this will succeed. The version check happens elsewhere.
        let result = PcbToml::parse(content);
        assert!(result.is_ok());
    }

    #[test]
    fn test_explicit_v1() {
        let content = r#"
[workspace]
resolver = "1"

[board]
name = "TestBoard"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_v2());
        assert_eq!(config.version(), "1");
    }

    #[test]
    fn test_v2_vendor_config() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[vendor]
directory = "my-vendor"
match = ["github.com/diodeinc/registry/reference/ti"]
"#;

        let config = PcbToml::parse(content).unwrap();
        let vendor = config.vendor.as_ref().unwrap();
        assert_eq!(vendor.directory, "my-vendor");
        assert_eq!(
            vendor.match_patterns,
            vec!["github.com/diodeinc/registry/reference/ti"]
        );

        // Test should_vendor
        assert!(vendor.should_vendor("github.com/diodeinc/registry/reference/ti/tps54331"));
        assert!(!vendor.should_vendor("github.com/diodeinc/stdlib"));
    }

    #[test]
    fn test_v2_vendor_config_defaults() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[vendor]
"#;

        let config = PcbToml::parse(content).unwrap();
        let vendor = config.vendor.as_ref().unwrap();
        assert_eq!(vendor.directory, "vendor"); // default
        assert!(vendor.match_patterns.is_empty()); // default empty = vendor all

        // Empty patterns should vendor everything
        assert!(vendor.should_vendor("github.com/diodeinc/stdlib"));
        assert!(vendor.should_vendor("github.com/any/package"));
    }

    #[test]
    fn test_v2_workspace_vendor_config() {
        let content = r#"
[workspace]
resolver = "2"
members = ["boards/*"]

[vendor]
directory = "workspace-vendor"
match = ["github.com/diodeinc/registry/reference"]
"#;

        let config = PcbToml::parse(content).unwrap();
        let vendor = config.vendor.as_ref().unwrap();
        assert_eq!(vendor.directory, "workspace-vendor");
        assert_eq!(
            vendor.match_patterns,
            vec!["github.com/diodeinc/registry/reference"]
        );

        // Test should_vendor with workspace pattern
        assert!(vendor.should_vendor("github.com/diodeinc/registry/reference/ti/tps54331"));
        assert!(!vendor.should_vendor("github.com/diodeinc/stdlib"));
    }

    #[test]
    fn test_v2_vendor_wildcard() {
        let content = r#"
[workspace]
resolver = "2"
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[vendor]
match = ["*"]
"#;

        let config = PcbToml::parse(content).unwrap();
        let vendor = config.vendor.as_ref().unwrap();
        assert_eq!(vendor.match_patterns, vec!["*"]);

        // Wildcard should vendor everything
        assert!(vendor.should_vendor("github.com/diodeinc/stdlib"));
        assert!(vendor.should_vendor("github.com/any/package"));
        assert!(vendor.should_vendor("gitlab.com/kicad/symbols"));
    }
}
