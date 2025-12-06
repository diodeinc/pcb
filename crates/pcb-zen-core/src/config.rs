use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::FileProvider;

/// Known KiCad asset repositories: (alias, base_url, default_version)
pub const KICAD_ASSETS: &[(&str, &str, &str)] = &[
    (
        "kicad-footprints",
        "gitlab.com/kicad/libraries/kicad-footprints",
        "9.0.3",
    ),
    (
        "kicad-symbols",
        "gitlab.com/kicad/libraries/kicad-symbols",
        "9.0.3",
    ),
];

/// Top-level pcb.toml configuration
///
/// Version detection uses the `is_v2()` method:
/// - V1 requires explicit V1-only constructs: `[packages]` or `[module]`
/// - V2 is the default - empty pcb.toml, board-only, or any V2 fields are all valid V2
/// - Workspaces require `pcb-version = "0.3"` or higher for V2 mode
///
/// Both V1 and V2 fields coexist in the same struct.
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
    /// Check if this requires V1 mode
    ///
    /// V1 is required when using V1-only constructs that have no V2 equivalent:
    /// - `[packages]` section (V2 uses `[dependencies]`)
    /// - `[module]` section (V2 uses different module system)
    fn requires_v1(&self) -> bool {
        !self.packages.is_empty() || self.module.is_some()
    }

    /// Check if this is V2 mode
    ///
    /// - Workspace with pcb-version >= 0.3.0 → V2
    /// - Workspace with pcb-version < 0.3.0 or no pcb-version → V1 (backwards compat)
    /// - No workspace + V1 constructs (packages, module) → V1
    /// - No workspace + no V1 constructs → V2 (e.g., empty pcb.toml, board-only)
    pub fn is_v2(&self) -> bool {
        if let Some(w) = &self.workspace {
            // Workspace present: V2 if pcb-version >= 0.3.0
            if let Some(version_str) = &w.pcb_version {
                return Self::parse_pcb_version(version_str)
                    .map(|(major, minor, _)| major > 0 || minor >= 3)
                    .unwrap_or(false);
            }
            // No pcb-version means V1 (backwards compat)
            return false;
        }

        // No workspace: V2 unless it has V1-only constructs
        !self.requires_v1()
    }

    /// Parse pcb-version string into (major, minor, patch) tuple
    /// Supports formats: "0.3", "0.3.0", "0.3.2"
    fn parse_pcb_version(s: &str) -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = s.split('.').collect();
        match parts.len() {
            2 => {
                let major = parts[0].parse().ok()?;
                let minor = parts[1].parse().ok()?;
                Some((major, minor, 0))
            }
            3 => {
                let major = parts[0].parse().ok()?;
                let minor = parts[1].parse().ok()?;
                let patch = parts[2].parse().ok()?;
                Some((major, minor, patch))
            }
            _ => None,
        }
    }

    /// Parse from TOML string
    pub fn parse(content: &str) -> Result<Self> {
        toml::from_str(content).map_err(|e| anyhow::anyhow!("Failed to parse pcb.toml: {e}"))
    }

    /// Parse from file content
    pub fn from_file(file_provider: &dyn FileProvider, path: &Path) -> Result<Self> {
        let content = file_provider
            .read_file(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content).with_context(|| format!("failed to parse {}", path.display()))
    }

    /// Extract and parse inline pcb.toml from .zen file content
    ///
    /// Looks for a block in leading comments like:
    /// ```text
    /// # ```pcb
    /// # [package]
    /// # pcb-version = "0.3"
    /// # ```
    /// ```
    ///
    /// Returns `Some(Ok(config))` if inline manifest found and parsed successfully,
    /// `Some(Err(...))` if found but failed to parse,
    /// `None` if no inline manifest block found.
    pub fn from_zen_content(zen_content: &str) -> Option<Result<Self>> {
        extract_inline_manifest(zen_content).map(|toml| Self::parse(&toml))
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

    /// Get package aliases (V1 only - V2 does not support aliases)
    pub fn packages(&self) -> HashMap<String, String> {
        self.packages.clone()
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

        // Always include KiCad asset aliases
        for (alias, base_url, _) in KICAD_ASSETS {
            aliases.insert(alias.to_string(), base_url.to_string());
        }

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

    /// Patterns for dependencies to auto-vendor during build (supports globs)
    /// Example: ["github.com/diodeinc/registry/*"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendor: Vec<String>,

    /// Patterns to exclude from member discovery (supports globs, applied after members)
    /// Example: ["modules/deprecated/*", "boards/test-*"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            name: None,
            repository: None,
            path: None,
            resolver: None,
            pcb_version: None,
            default_board: None,
            members: default_members(),
            vendor: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

/// Access control configuration (shared by V1 and V2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessConfig {
    /// Access control list (email patterns)
    #[serde(default)]
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
/// ```text
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
    /// ```text
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

/// Default members pattern
pub fn default_members() -> Vec<String> {
    vec![
        "components/*".to_string(),
        "reference/*".to_string(),
        "modules/*".to_string(),
        "boards/*".to_string(),
        "graphics/*".to_string(),
    ]
}

/// Extract inline pcb.toml manifest from .zen file content
///
/// Looks for a comment block in the leading comments like:
/// ```text
/// # ```pcb
/// # [package]
/// # pcb-version = "0.3"
/// # ```
/// ```
///
/// Returns the TOML content (with comment prefixes stripped) if found.
pub fn extract_inline_manifest(zen_content: &str) -> Option<String> {
    let mut in_block = false;
    let mut toml_lines = Vec::new();

    for line in zen_content.lines() {
        let trimmed = line.trim();

        // Stop scanning once we hit non-comment, non-empty, non-shebang content
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            break;
        }

        // Check for opening marker: # ```pcb
        if !in_block && trimmed.starts_with('#') {
            let after_hash = trimmed.strip_prefix('#').unwrap().trim();
            if after_hash == "```pcb" {
                in_block = true;
                continue;
            }
        }

        // Check for closing marker: # ```
        if in_block && trimmed.starts_with('#') {
            let after_hash = trimmed.strip_prefix('#').unwrap().trim();
            if after_hash == "```" {
                // Found complete block
                return Some(toml_lines.join("\n"));
            }

            // Strip "# " prefix and collect TOML content
            let content = trimmed
                .strip_prefix('#')
                .unwrap()
                .strip_prefix(' ')
                .unwrap_or(trimmed.strip_prefix('#').unwrap());
            toml_lines.push(content.to_string());
        }
    }

    None
}

/// Split a module path into (repo_url, subpath) for github.com repos
///
/// Examples:
/// - "github.com/user/repo" -> ("github.com/user/repo", "")
/// - "github.com/user/repo/pkg" -> ("github.com/user/repo", "pkg")
/// - "github.com/user/repo/a/b/c" -> ("github.com/user/repo", "a/b/c")
pub fn split_repo_and_subpath(module_path: &str) -> (&str, &str) {
    let parts: Vec<&str> = module_path.split('/').collect();
    if parts.is_empty() {
        return (module_path, "");
    }
    if parts[0] == "github.com" && parts.len() > 3 {
        let boundary = parts[..3].join("/").len();
        return (&module_path[..boundary], &module_path[boundary + 1..]);
    }
    (module_path, "")
}

/// Split asset dependency key into (repo_url, subpath)
///
/// Handles known asset repositories with nested group paths (e.g., gitlab.com/kicad/libraries/...).
/// For unknown repos, falls back to standard split_repo_and_subpath logic.
///
/// Examples:
/// - "gitlab.com/kicad/libraries/kicad-footprints" -> ("gitlab.com/kicad/libraries/kicad-footprints", "")
/// - "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty" -> ("gitlab.com/kicad/libraries/kicad-footprints", "Resistor_SMD.pretty")
/// - "github.com/user/assets/foo" -> ("github.com/user/assets", "foo")
pub fn split_asset_repo_and_subpath(asset_key: &str) -> (&str, &str) {
    for (_, base_url, _) in KICAD_ASSETS {
        if asset_key.starts_with(base_url) {
            if asset_key.len() > base_url.len() && asset_key.as_bytes()[base_url.len()] == b'/' {
                return (base_url, &asset_key[base_url.len() + 1..]);
            } else if asset_key.len() == base_url.len() {
                return (base_url, "");
            }
        }
    }

    // Fallback to standard split logic for github repos
    split_repo_and_subpath(asset_key)
}

/// Extract ref string from AssetDependencySpec.
///
/// Returns `Some(ref_str)` for version, branch, or rev (excluding HEAD).
/// Returns `None` if no ref is specified or if it's HEAD.
///
/// For error handling (rejecting HEAD explicitly), use `extract_asset_ref_strict`.
pub fn extract_asset_ref(spec: &AssetDependencySpec) -> Option<String> {
    match spec {
        AssetDependencySpec::Ref(r) if r != "HEAD" => Some(r.clone()),
        AssetDependencySpec::Detailed(d) => d
            .version
            .clone()
            .or_else(|| d.branch.clone())
            .or_else(|| d.rev.clone())
            .filter(|r| r != "HEAD"),
        _ => None,
    }
}

/// Extract ref string from AssetDependencySpec with strict validation.
///
/// Returns an error if:
/// - No version, branch, or rev is specified
/// - The ref is "HEAD" (not allowed for reproducible builds)
pub fn extract_asset_ref_strict(spec: &AssetDependencySpec) -> Result<String> {
    match spec {
        AssetDependencySpec::Ref(r) => {
            if r == "HEAD" {
                anyhow::bail!(
                    "Asset ref 'HEAD' is not allowed; use an explicit version, branch, or rev"
                );
            }
            Ok(r.clone())
        }
        AssetDependencySpec::Detailed(detail) => {
            let ref_str = detail
                .version
                .clone()
                .or_else(|| detail.branch.clone())
                .or_else(|| detail.rev.clone())
                .ok_or_else(|| anyhow::anyhow!("Asset must specify version, branch, or rev"))?;

            if ref_str == "HEAD" {
                anyhow::bail!(
                    "Asset ref 'HEAD' is not allowed; use an explicit version, branch, or rev"
                );
            }
            Ok(ref_str)
        }
    }
}

/// Find the workspace root by walking up from `start`.
///
/// Resolution order:
/// 1. First pcb.toml with explicit `[workspace]` section wins
/// 2. If no explicit workspace found, first pcb.toml encountered
/// 3. If no pcb.toml found, the start directory (or parent if start is a file)
///
/// Always returns a canonicalized absolute path.
pub fn find_workspace_root(file_provider: &dyn FileProvider, start: &Path) -> PathBuf {
    let abs_start = file_provider
        .canonicalize(start)
        .unwrap_or_else(|_| start.to_path_buf());

    let start_dir = if file_provider.is_directory(&abs_start) {
        abs_start
    } else {
        abs_start.parent().unwrap_or(&abs_start).to_path_buf()
    };

    // Collect all pcb.toml locations walking up
    let candidates: Vec<_> = std::iter::successors(Some(start_dir.as_path()), |dir| dir.parent())
        .filter_map(|dir| {
            let pcb_toml = dir.join("pcb.toml");
            if !file_provider.exists(&pcb_toml) {
                return None;
            }

            match PcbToml::from_file(file_provider, &pcb_toml) {
                Ok(config) => Some((dir.to_path_buf(), config.is_workspace())),
                Err(_) => None,
            }
        })
        .collect();

    // Prefer explicit [workspace], otherwise first pcb.toml
    candidates
        .iter()
        .find(|(_, is_explicit)| *is_explicit)
        .or_else(|| candidates.first())
        .map(|(path, _)| path.clone())
        .unwrap_or(start_dir)
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
    fn test_parse_board_only() {
        // Board-only configs are V2 (no V1-specific constructs)
        let content = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "A test board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2()); // No V1 constructs, so it's V2
        assert!(config.is_board());

        let board = config.board.unwrap();
        assert_eq!(board.name, "TestBoard");
        assert_eq!(board.path, Some("test_board.zen".to_string()));
        assert_eq!(board.description, "A test board");
    }

    #[test]
    fn test_parse_v1_module() {
        // [module] section requires V1
        let content = r#"
[module]
name = "stdlib"
module_path = "github.com/diodeinc/stdlib"
version = "0.3.0"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_v2()); // Has [module], requires V1
        assert!(config.is_module());
    }

    #[test]
    fn test_parse_v2_package() {
        let content = r#"
[workspace]
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2());

        let workspace = config.workspace.as_ref().unwrap();
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
pcb-version = "0.3"
members = ["boards/*"]

[access]
allow = ["*@weaverobots.com"]
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2());
        assert!(config.is_workspace());

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.pcb_version.as_deref(), Some("0.3"));
        assert_eq!(workspace.members, vec!["boards/*"]);

        let access = config.access.as_ref().unwrap();
        assert_eq!(access.allow, vec!["*@weaverobots.com"]);
    }

    #[test]
    fn test_parse_v2_dependencies() {
        let content = r#"
[workspace]
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
    fn test_workspace_no_pcb_version_is_v1() {
        let content = r#"
[workspace]
members = ["boards/*"]

[board]
name = "TestBoard"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_v2());
    }

    #[test]
    fn test_workspace_old_pcb_version_is_v1() {
        let content = r#"
[workspace]
pcb-version = "0.2"

[board]
name = "TestBoard"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(!config.is_v2());
    }

    #[test]
    fn test_empty_is_v2() {
        // Empty pcb.toml is valid V2 (no V1 constructs)
        let config = PcbToml::parse("").unwrap();
        assert!(config.is_v2());
    }

    #[test]
    fn test_v2_vendor_config() {
        let content = r#"
[workspace]
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
pcb-version = "0.3"
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

    #[test]
    fn test_extract_inline_manifest_basic() {
        let zen_content = r#"#!/usr/bin/env pcb build
#
# ```pcb
# [workspace]
# pcb-version = "0.3"
#
# [dependencies]
# "github.com/diodeinc/stdlib" = "0.3"
# ```

load("github.com/diodeinc/stdlib/units.zen", "Voltage")
"#;

        let result = extract_inline_manifest(zen_content);
        assert!(result.is_some());
        let toml = result.unwrap();
        assert!(toml.contains("[workspace]"));
        assert!(toml.contains("pcb-version = \"0.3\""));
        assert!(toml.contains("[dependencies]"));
    }

    #[test]
    fn test_extract_inline_manifest_no_shebang() {
        let zen_content = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("foo.zen", "Bar")
"#;

        let result = extract_inline_manifest(zen_content);
        assert!(result.is_some());
    }

    #[test]
    fn test_extract_inline_manifest_missing() {
        let zen_content = r#"# Just a regular comment
load("foo.zen", "Bar")
"#;

        let result = extract_inline_manifest(zen_content);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_inline_manifest_unclosed() {
        let zen_content = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# Missing closing marker

load("foo.zen", "Bar")
"#;

        let result = extract_inline_manifest(zen_content);
        // Unclosed block should return None
        assert!(result.is_none());
    }

    #[test]
    fn test_from_zen_content() {
        let zen_content = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

load("foo.zen", "Bar")
"#;

        let result = PcbToml::from_zen_content(zen_content);
        assert!(result.is_some());
        let config = result.unwrap().unwrap();
        assert!(config.is_v2());
    }

    #[test]
    fn test_from_zen_content_v1() {
        // V1 style inline manifest (no pcb-version)
        let zen_content = r#"# ```pcb
# [packages]
# stdlib = "@github/diodeinc/stdlib:v0.3.2"
# ```

load("@stdlib/foo.zen", "Bar")
"#;

        let result = PcbToml::from_zen_content(zen_content);
        assert!(result.is_some());
        let config = result.unwrap().unwrap();
        assert!(!config.is_v2()); // Has [packages] which requires V1
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
        // Non-github repos return full path as repo_url
        assert_eq!(
            split_repo_and_subpath("gitlab.com/group/project/pkg"),
            ("gitlab.com/group/project/pkg", "")
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
