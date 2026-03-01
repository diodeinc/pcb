use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ariadne::{Label, Report, ReportKind, Source};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::FileProvider;

/// Top-level pcb.toml configuration.
///
/// The toolchain only supports V2 manifests. Some legacy V1 fields still exist here so `pcb migrate`
/// can parse and upgrade older projects, but V1 dependency resolution is not supported at runtime.
///
/// `is_v2()` is used to detect whether a manifest is V2-compatible (and to detect legacy V1 inputs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PcbToml {
    /// Workspace configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceConfig>,

    /// Module configuration section (legacy V1; used by `pcb migrate` only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleConfig>,

    /// Board configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,

    /// Package aliases configuration section (legacy V1; used by `pcb migrate` only)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub packages: HashMap<String, String>,

    /// Dependencies (V2 only - code packages with pcb.toml)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    /// Legacy assets section (V2).
    ///
    /// Parsed for backwards compatibility with old manifests.
    /// Runtime behavior is limited to using matching KiCad asset paths as version hints.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, AssetDependencySpec>,

    /// Patches for local development (V2 only)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patch: BTreeMap<String, PatchSpec>,

    /// Access control configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessConfig>,
}

impl PcbToml {
    /// Check if this uses legacy V1-only constructs.
    fn requires_v1(&self) -> bool {
        !self.packages.is_empty() || self.module.is_some()
    }

    /// Check if this manifest is V2-compatible.
    ///
    /// Returns `false` for legacy V1 manifests (used by `pcb migrate`).
    pub fn is_v2(&self) -> bool {
        if let Some(w) = &self.workspace {
            // Workspace present: V2 if pcb-version >= 0.3.0
            if let Some(version_str) = &w.pcb_version {
                return Self::parse_pcb_version(version_str)
                    .map(|(major, minor, _)| major > 0 || minor >= 3)
                    .unwrap_or(false);
            }
            // No pcb-version means legacy V1.
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
        toml::from_str(content).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Parse from file, rendering errors with ariadne-style diagnostics
    pub fn from_file(file_provider: &dyn FileProvider, path: &Path) -> Result<Self> {
        let content = file_provider
            .read_file(path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        Self::parse_with_path(&content, path)
    }

    /// Parse from a local filesystem path.
    pub fn from_path(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse_with_path(&content, path)
    }

    /// Parse TOML content with path context for error reporting
    pub fn parse_with_path(content: &str, path: &Path) -> Result<Self> {
        toml::from_str(content).map_err(|e| {
            if let Some(span) = e.span() {
                let path_str = path.display().to_string();
                let mut buf = Vec::new();
                let _ = Report::<(&str, std::ops::Range<usize>)>::build(
                    ReportKind::Error,
                    (path_str.as_str(), span.clone()),
                )
                .with_message(format!("failed to parse {}", path.display()))
                .with_label(
                    Label::new((path_str.as_str(), span))
                        .with_message(e.message())
                        .with_color(ariadne::Color::Red),
                )
                .finish()
                .write((path_str.as_str(), Source::from(content)), &mut buf);
                anyhow::anyhow!("{}", String::from_utf8_lossy(&buf).trim())
            } else {
                anyhow::anyhow!("failed to parse {}: {e}", path.display())
            }
        })
    }

    /// Extract and parse inline pcb.toml from .zen file content
    ///
    /// Looks for a block in leading comments like:
    /// ```text
    /// # ```pcb
    /// # [workspace]
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

    /// Check if this configuration represents a module (legacy V1; used by `pcb migrate` only)
    pub fn is_module(&self) -> bool {
        self.module.is_some()
    }

    /// Check if this configuration represents a board
    pub fn is_board(&self) -> bool {
        self.board.is_some()
    }

    /// Get package aliases (legacy V1; V2 does not support aliases)
    pub fn packages(&self) -> HashMap<String, String> {
        self.packages.clone()
    }

    /// Auto-generate aliases from dependencies (V2 only)
    ///
    /// Takes the last path segment as the alias key. Only creates alias if unique (no collisions).
    /// Examples:
    /// - "github.com/diodeinc/stdlib" → "@stdlib"
    /// - "github.com/diodeinc/registry/reference/XAL7070-562MEx" → "@XAL7070-562MEx"
    /// - "gitlab.com/kicad/libraries/kicad-symbols" → "@kicad-symbols"
    pub fn auto_generated_aliases(&self) -> HashMap<String, String> {
        let mut aliases = HashMap::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();

        // Collect all URLs from dependencies
        let all_urls: Vec<String> = self.dependencies.keys().cloned().collect();

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// Optional workspace name (legacy; ignored)
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

    /// Dependency resolver version (legacy; ignored)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<String>,

    /// Minimum compatible toolchain release series (e.g., "0.3")
    /// V2 only. Indicates breaking changes requiring newer compiler.
    #[serde(skip_serializing_if = "Option::is_none", rename = "pcb-version")]
    pub pcb_version: Option<String>,

    /// Kicad-style library linkage configuration.
    #[serde(
        default = "default_kicad_library",
        skip_serializing_if = "is_default_kicad_library"
    )]
    pub kicad_library: Vec<KicadLibraryConfig>,

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
            kicad_library: default_kicad_library(),
            default_board: None,
            members: default_members(),
            vendor: Vec::new(),
            exclude: Vec::new(),
        }
    }
}

/// Kicad-style library relationship hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KicadLibraryConfig {
    /// Concrete semver version, e.g. "9.0.3".
    pub version: Version,
    /// Symbols repo URL (dependency base path).
    pub symbols: String,
    /// Footprints repo URL (dependency base path).
    pub footprints: String,
    /// Mapping from KiCad text variable name to model repo URL.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    /// Optional HTTP archive URL template for materialization.
    ///
    /// Supports `{repo}`, `{repo_name}`, `{version}`, `{major}` placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_mirror: Option<String>,
}

pub const DEFAULT_KICAD_HTTP_MIRROR_TEMPLATE: &str =
    "https://kicad-mirror.api.diode.computer/{repo_name}-{version}.tar.zst";

pub const DEFAULT_KICAD_LIBRARY_VERSION: Version = Version::new(9, 0, 3);

fn default_kicad_library() -> Vec<KicadLibraryConfig> {
    vec![KicadLibraryConfig {
        version: DEFAULT_KICAD_LIBRARY_VERSION,
        symbols: "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
        footprints: "gitlab.com/kicad/libraries/kicad-footprints".to_string(),
        models: BTreeMap::from([(
            "KICAD9_3DMODEL_DIR".to_string(),
            "gitlab.com/kicad/libraries/kicad-packages3D".to_string(),
        )]),
        http_mirror: Some(DEFAULT_KICAD_HTTP_MIRROR_TEMPLATE.to_string()),
    }]
}

fn is_default_kicad_library(value: &[KicadLibraryConfig]) -> bool {
    value == default_kicad_library().as_slice()
}

/// Access control configuration (shared by V1 and V2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessConfig {
    /// Access control list (email patterns)
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Module configuration (V1 only)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleConfig {
    /// Module name
    pub name: String,
}

/// Board configuration (used in both V1 and V2)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// V2 Patch specification for local development or branch overrides
///
/// Patches can override dependencies with:
/// - A local path: `{ path = "../stdlib" }`
/// - A git branch: `{ branch = "feature-branch" }`
/// - A git revision: `{ rev = "abc123" }`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSpec {
    /// Local path to use as replacement
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Git branch to use instead of the declared version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Git revision (commit hash) to use instead of the declared version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
}

/// Legacy V2 asset dependency specification.
///
/// Parsed only for backwards compatibility with older manifests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssetDependencySpec {
    /// Simple ref string - used literally as git tag/branch (no v-prefix logic)
    /// Examples: "v7.0.0", "2024-09-release", "kicad-7.0.0"
    Ref(String),

    /// Detailed specification with branch/rev support
    Detailed(AssetDependencyDetail),
}

/// Legacy detailed asset dependency specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    // Kept for old manifests only.
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
    /// None for non-package repos without pcb.toml (for example KiCad repos)
    pub manifest_hash: Option<String>,
}

/// V2 Lockfile (pcb.sum)
///
/// Stores resolved versions and cryptographic hashes for reproducible builds.
/// Automatically updated when dependencies change.
#[derive(Debug, Clone, Default)]
pub struct Lockfile {
    /// Map from (module_path, version) to lock entry.
    /// Uses BTreeMap for deterministic iteration order.
    pub entries: BTreeMap<(String, String), LockEntry>,
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
        let mut entries = BTreeMap::new();

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
/// # [workspace]
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

/// Find the workspace root by walking up from `start`.
///
/// Resolution order:
/// 1. First pcb.toml with explicit `[workspace]` section wins
/// 2. If no explicit workspace found, first pcb.toml encountered
/// 3. If no pcb.toml found, the start directory (or parent if start is a file)
///
/// Returns an error if a pcb.toml file exists but fails to parse.
/// Always returns a canonicalized absolute path on success.
pub fn find_workspace_root(file_provider: &dyn FileProvider, start: &Path) -> Result<PathBuf> {
    let abs_start = file_provider
        .canonicalize(start)
        .unwrap_or_else(|_| start.to_path_buf());

    let start_dir = if file_provider.is_directory(&abs_start) {
        abs_start
    } else {
        abs_start.parent().unwrap_or(&abs_start).to_path_buf()
    };

    // Collect all pcb.toml locations walking up, failing on parse errors
    let mut candidates = Vec::new();
    for dir in std::iter::successors(Some(start_dir.as_path()), |dir| dir.parent()) {
        let pcb_toml = dir.join("pcb.toml");
        if !file_provider.exists(&pcb_toml) {
            continue;
        }

        // Fail early if pcb.toml exists but can't be parsed
        let config = PcbToml::from_file(file_provider, &pcb_toml)?;
        candidates.push((dir.to_path_buf(), config.is_workspace()));
    }

    // Prefer explicit [workspace], otherwise first pcb.toml
    Ok(candidates
        .iter()
        .find(|(_, is_explicit)| *is_explicit)
        .or_else(|| candidates.first())
        .map(|(path, _)| path.clone())
        .unwrap_or(start_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kicad_library::validate_kicad_library_config;

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
    fn test_workspace_kicad_library_defaults_to_kicad9() {
        let content = r#"
[workspace]
pcb-version = "0.3"
"#;

        let config = PcbToml::parse(content).unwrap();
        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.kicad_library.len(), 1);
        assert_eq!(
            workspace.kicad_library[0].version,
            DEFAULT_KICAD_LIBRARY_VERSION
        );
        assert_eq!(
            workspace.kicad_library[0].symbols,
            "gitlab.com/kicad/libraries/kicad-symbols"
        );
        assert_eq!(
            workspace.kicad_library[0].footprints,
            "gitlab.com/kicad/libraries/kicad-footprints"
        );
        assert_eq!(
            workspace.kicad_library[0].models.get("KICAD9_3DMODEL_DIR"),
            Some(&"gitlab.com/kicad/libraries/kicad-packages3D".to_string())
        );
        assert_eq!(
            workspace.kicad_library[0].http_mirror.as_deref(),
            Some(DEFAULT_KICAD_HTTP_MIRROR_TEMPLATE)
        );
    }

    #[test]
    fn test_parse_v2_workspace() {
        let content = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]

[[workspace.kicad_library]]
version = "9.0.3"
symbols = "gitlab.com/kicad/libraries/kicad-symbols"
footprints = "gitlab.com/kicad/libraries/kicad-footprints"
models = { KICAD9_3DMODEL_DIR = "gitlab.com/kicad/libraries/kicad-packages3D" }

[access]
allow = ["*@weaverobots.com"]
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_v2());
        assert!(config.is_workspace());

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.pcb_version.as_deref(), Some("0.3"));
        assert_eq!(workspace.kicad_library.len(), 1);
        assert_eq!(workspace.kicad_library[0].version, Version::new(9, 0, 3));
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
        assert_eq!(patch.path.as_deref(), Some("../stdlib"));
    }

    #[test]
    fn test_parse_v2_patch_branch() {
        let content = r#"
[workspace]
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[patch]
"github.com/diodeinc/registry/components/FOO" = { branch = "feature-branch" }
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.patch.len(), 1);

        let patch = config
            .patch
            .get("github.com/diodeinc/registry/components/FOO")
            .unwrap();
        assert_eq!(patch.branch.as_deref(), Some("feature-branch"));
        assert_eq!(patch.path, None);
        assert_eq!(patch.rev, None);
    }

    #[test]
    fn test_parse_v2_patch_rev() {
        let content = r#"
[workspace]
pcb-version = "0.3"

[board]
name = "Test"
path = "test.zen"

[patch]
"github.com/diodeinc/registry/components/BAR" = { rev = "abc123def456" }
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.patch.len(), 1);

        let patch = config
            .patch
            .get("github.com/diodeinc/registry/components/BAR")
            .unwrap();
        assert_eq!(patch.rev.as_deref(), Some("abc123def456"));
        assert_eq!(patch.path, None);
        assert_eq!(patch.branch, None);
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
    fn test_validate_kicad_library_config() {
        let mut entry = KicadLibraryConfig {
            version: Version::new(9, 0, 3),
            symbols: "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
            footprints: "gitlab.com/kicad/libraries/kicad-footprints".to_string(),
            models: BTreeMap::from([(
                "KICAD9_3DMODEL_DIR".to_string(),
                "gitlab.com/kicad/libraries/kicad-packages3D".to_string(),
            )]),
            http_mirror: None,
        };
        assert!(validate_kicad_library_config(&entry).is_ok());

        entry.symbols = "".to_string();
        assert!(validate_kicad_library_config(&entry).is_err());
    }
}
