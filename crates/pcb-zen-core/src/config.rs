use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ariadne::{Label, Report, ReportKind, Source};
use serde::{Deserialize, Serialize};

use crate::FileProvider;

/// Top-level pcb.toml configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PcbToml {
    /// Workspace configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceConfig>,

    /// Board configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,

    /// Code package dependencies.
    #[serde(default, skip_serializing_if = "DependencyTable::is_empty")]
    pub dependencies: DependencyTable,

    /// Patches for local development.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patch: BTreeMap<String, PatchSpec>,

    /// Parts associated with symbols in this package.
    ///
    /// Each entry maps a symbol (relative `.kicad_sym` path within the package)
    /// to a manufacturer part (MPN + manufacturer).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<ManifestPart>,

    /// Access control configuration section
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessConfig>,
}

/// Dependency tables stored under `[dependencies]` and `[dependencies.indirect]`.
///
/// Existing runtime behavior only consumes the direct dependency map. The nested
/// indirect table is hydrated by `pcb sync`. Direct keys stay bare module paths;
/// indirect keys may be lane-qualified as `<module>@<lane>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyTable {
    /// Direct dependencies under `[dependencies]`.
    #[serde(flatten)]
    pub direct: BTreeMap<String, DependencySpec>,

    /// Tool-managed transitive dependency closure under `[dependencies.indirect]`.
    ///
    /// `pcb sync` writes exact version entries here, but we reuse
    /// `DependencySpec` so both dependency tables share the same manifest
    /// encoding.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub indirect: BTreeMap<String, DependencySpec>,
}

impl DependencyTable {
    pub fn is_empty(&self) -> bool {
        self.direct.is_empty() && self.indirect.is_empty()
    }

    fn remove_kicad_library_dependencies(&mut self) {
        self.direct
            .retain(|path, _| !crate::is_kicad_library_package(path));
        self.indirect
            .retain(|key, _| !crate::is_kicad_library_dependency_key(key));
    }
}

/// Parse a `pcb-version` string into its `(major, minor)` pair.
///
/// `pcb-version` is always written and compared as `major.minor`.
pub fn parse_pcb_version(s: &str) -> Option<(u32, u32)> {
    let (major, minor) = s.split_once('.')?;
    if minor.contains('.') {
        return None;
    }
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn current_pcb_version() -> (u32, u32) {
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse().ok())
        .expect("CARGO_PKG_VERSION must be valid semver");
    let minor = parts
        .next()
        .and_then(|part| part.parse().ok())
        .expect("CARGO_PKG_VERSION must be valid semver");
    (major, minor)
}

/// Format the running pcb binary version as a `major.minor` string.
pub fn pcb_version_from_cargo() -> String {
    let (major, minor) = current_pcb_version();
    format!("{major}.{minor}")
}

/// Whether `current` is older than `required`, comparing only `major.minor`.
pub fn pcb_version_is_older(current: &str, required: &str) -> Option<bool> {
    let current = parse_pcb_version(current)?;
    let required = parse_pcb_version(required)?;
    Some(current < required)
}

impl PcbToml {
    fn finish_parse(mut self) -> Result<Self> {
        self.dependencies.remove_kicad_library_dependencies();
        self.validate_pcb_version()?;
        Ok(self)
    }

    fn validate_pcb_version(&self) -> Result<()> {
        if let Some(version) = self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.pcb_version.as_deref())
            && parse_pcb_version(version).is_none()
        {
            anyhow::bail!(
                "invalid `pcb-version`: expected \"major.minor\" like \"0.4\", got \"{}\"",
                version
            );
        }

        Ok(())
    }

    /// Parse from TOML string
    pub fn parse(content: &str) -> Result<Self> {
        let parsed: Self = toml::from_str(content).map_err(|e| anyhow::anyhow!("{e}"))?;
        parsed.finish_parse()
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
        let parsed: Self = toml::from_str(content).map_err(|e| {
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
        })?;
        parsed
            .finish_parse()
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
    }

    /// Extract and parse inline pcb.toml from .zen file content
    ///
    /// Looks for a block in leading comments like:
    /// ```text
    /// # ```pcb
    /// # [workspace]
    /// # pcb-version = "0.4"
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

    /// Check if this configuration represents a board
    pub fn is_board(&self) -> bool {
        self.board.is_some()
    }

    /// Auto-generate aliases from dependencies (V2 only)
    ///
    /// Takes the last path segment as the alias key. Only creates alias if unique (no collisions).
    /// Examples:
    /// - "github.com/diodeinc/registry/reference/XAL7070-562MEx" → "@XAL7070-562MEx"
    pub fn auto_generated_aliases(&self) -> HashMap<String, String> {
        let mut aliases = HashMap::new();
        let mut seen_names: HashMap<String, usize> = HashMap::new();

        // Collect all URLs from dependencies
        let all_urls: Vec<String> = self.dependencies.direct.keys().cloned().collect();

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Optional Diode workspace name override.
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

    /// Minimum compatible toolchain release series (e.g., "0.4")
    /// V2 only. Indicates breaking changes requiring newer compiler.
    #[serde(skip_serializing_if = "Option::is_none", rename = "pcb-version")]
    pub pcb_version: Option<String>,

    /// Base host used for Diode app/API URLs in this workspace.
    /// Example: "diode.computer" -> app/api hosts resolve under this domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,

    /// BOM command and sourcing configuration.
    #[serde(default, skip_serializing_if = "BomConfig::is_default")]
    pub bom: BomConfig,

    /// Default board name to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_board: Option<String>,

    /// Patterns for dependencies to auto-vendor during build (supports globs)
    /// Example: ["github.com/diodeinc/registry/*"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vendor: Vec<String>,

    /// Workspace-relative package paths that should be highlighted as preferred.
    /// Example: ["components/RP2350A", "reference/RP2350A"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferred: Vec<String>,

    /// Patterns to exclude from workspace package discovery (supports globs)
    /// Example: ["modules/deprecated/*", "boards/test-*"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BomConfig {
    /// Require exact MPN matching when fetching availability from the BOM service.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

impl BomConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// Access control configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessConfig {
    /// Access control list (email patterns)
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Board configuration.
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

/// A manufacturer part associated with a symbol in a package manifest.
///
/// Declared in `pcb.toml` as:
/// ```toml
/// parts = [
///   { mpn = "PESD3V3L1ULSYL", symbol = "C7472904.kicad_sym", symbol_name = "PESD3V3L1ULSYL", manufacturer = "Nexperia USA Inc.", qualifications = ["AEC-Q101"] },
/// ]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPart {
    /// Manufacturer part number.
    pub mpn: String,
    /// Relative path to the `.kicad_sym` file within the package.
    pub symbol: String,
    /// Optional symbol name inside the `.kicad_sym` file for multi-symbol libraries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
    /// Manufacturer name.
    pub manufacturer: String,
    /// Optional qualification tags for this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qualifications: Vec<String>,
    /// Optional datasheet URL or path for this part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasheet: Option<String>,
}

/// Extract inline pcb.toml manifest from .zen file content
///
/// Looks for a comment block in the leading comments like:
/// ```text
/// # ```pcb
/// # [workspace]
/// # pcb-version = "0.4"
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

    #[test]
    fn test_parse_board_only() {
        let content = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "A test board"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_board());

        let board = config.board.unwrap();
        assert_eq!(board.name, "TestBoard");
        assert_eq!(board.path, Some("test_board.zen".to_string()));
        assert_eq!(board.description, "A test board");
    }

    #[test]
    fn test_parse_rejects_legacy_module_section() {
        let err = PcbToml::parse(
            r#"
[module]
name = "legacy"
"#,
        )
        .expect_err("legacy [module] should not parse");

        assert!(err.to_string().contains("unknown field `module`"));
    }

    #[test]
    fn test_parse_rejects_legacy_packages_section() {
        let err = PcbToml::parse(
            r#"
[packages]
registry = "github.com/diodeinc/registry"
"#,
        )
        .expect_err("legacy [packages] should not parse");

        assert!(err.to_string().contains("unknown field `packages`"));
    }

    #[test]
    fn test_parse_rejects_legacy_assets_section() {
        let err = PcbToml::parse(
            r#"
[assets]
"github.com/example/assets" = "1.0.0"
"#,
        )
        .expect_err("legacy [assets] should not parse");

        assert!(err.to_string().contains("unknown field `assets`"));
    }

    #[test]
    fn test_parse_rejects_legacy_workspace_resolver() {
        let err = PcbToml::parse(
            r#"
[workspace]
resolver = "2"
"#,
        )
        .expect_err("legacy workspace resolver should not parse");

        assert!(err.to_string().contains("unknown field `resolver`"));
    }

    #[test]
    fn test_parse_rejects_workspace_members() {
        let err = PcbToml::parse(
            r#"
[workspace]
members = ["boards/*"]
"#,
        )
        .expect_err("workspace.members should not parse");

        assert!(err.to_string().contains("unknown field `members`"));
    }

    #[test]
    fn test_parse_v2_package() {
        let content = r#"
[workspace]
pcb-version = "0.4"

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"
"#;

        let config = PcbToml::parse(content).unwrap();

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.pcb_version.as_deref(), Some("0.4"));

        let board = config.board.as_ref().unwrap();
        assert_eq!(board.name, "WV0002");
        assert_eq!(board.path, Some("WV0002.zen".to_string()));
        assert_eq!(board.description, "Power Regulator Board");
    }

    #[test]
    fn test_parse_v2_workspace() {
        let content = r#"
[workspace]
pcb-version = "0.4"

[access]
allow = ["*@weaverobots.com"]
"#;

        let config = PcbToml::parse(content).unwrap();
        assert!(config.is_workspace());

        let workspace = config.workspace.as_ref().unwrap();
        assert_eq!(workspace.pcb_version.as_deref(), Some("0.4"));

        let access = config.access.as_ref().unwrap();
        assert_eq!(access.allow, vec!["*@weaverobots.com"]);
    }

    #[test]
    fn test_parse_v2_dependencies() {
        let content = r#"
[workspace]
pcb-version = "0.4"

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
        assert_eq!(config.dependencies.direct.len(), 4);

        // Test simple version string
        match config
            .dependencies
            .direct
            .get("github.com/diodeinc/stdlib")
            .unwrap()
        {
            DependencySpec::Version(v) => assert_eq!(v, "0.3.2"),
            _ => panic!("Expected Version variant"),
        }

        // Test detailed spec with version
        match config
            .dependencies
            .direct
            .get("github.com/diodeinc/registry/reference/ti/tps54331")
            .unwrap()
        {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.version, Some("^1.0.0".to_string()));
            }
            _ => panic!("Expected Detailed variant"),
        }

        // Test branch spec
        match config
            .dependencies
            .direct
            .get("github.com/user/custom")
            .unwrap()
        {
            DependencySpec::Detailed(d) => {
                assert_eq!(d.branch, Some("main".to_string()));
            }
            _ => panic!("Expected Detailed variant"),
        }
    }

    #[test]
    fn test_parse_indirect_dependencies() {
        let content = r#"
[dependencies]
"github.com/example/direct" = "1.2.3"

[dependencies.indirect]
"github.com/example/indirect@0.8" = "4.5.6"
"github.com/example/other@1" = "7.8.9"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(config.dependencies.direct.len(), 1);
        assert_eq!(config.dependencies.indirect.len(), 2);
        match config
            .dependencies
            .indirect
            .get("github.com/example/indirect@0.8")
        {
            Some(DependencySpec::Version(v)) => assert_eq!(v, "4.5.6"),
            other => panic!("expected Version variant, got {other:?}"),
        }
    }

    #[test]
    fn test_serialize_indirect_dependencies_as_nested_table() {
        let config = PcbToml {
            dependencies: DependencyTable {
                direct: BTreeMap::from([(
                    "github.com/example/direct".to_string(),
                    DependencySpec::Version("1.2.3".to_string()),
                )]),
                indirect: BTreeMap::from([(
                    "github.com/example/indirect@0.8".to_string(),
                    DependencySpec::Version("4.5.6".to_string()),
                )]),
            },
            ..PcbToml::default()
        };

        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(toml.contains("[dependencies]"));
        assert!(toml.contains("[dependencies.indirect]"));
        assert!(toml.contains("\"github.com/example/indirect@0.8\" = \"4.5.6\""));
    }

    #[test]
    fn test_parse_v2_patch() {
        let content = r#"
[workspace]
pcb-version = "0.4"

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
    fn test_parse_workspace_bom_config() {
        let content = r#"
[workspace]
pcb-version = "0.4"

[workspace.bom]
strict = true
"#;

        let config = PcbToml::parse(content).unwrap();
        let workspace = config.workspace.unwrap();

        assert!(workspace.bom.strict);
    }

    #[test]
    fn test_parse_v2_patch_branch() {
        let content = r#"
[workspace]
pcb-version = "0.4"

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
pcb-version = "0.4"

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
pcb-version = "0.4"

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
    fn test_workspace_no_pcb_version_parses() {
        let content = r#"
[workspace]

[board]
name = "TestBoard"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(
            config
                .workspace
                .as_ref()
                .and_then(|w| w.pcb_version.as_deref()),
            None
        );
    }

    #[test]
    fn test_workspace_old_pcb_version_parses() {
        let content = r#"
[workspace]
pcb-version = "0.2"

[board]
name = "TestBoard"
"#;

        let config = PcbToml::parse(content).unwrap();
        assert_eq!(
            config
                .workspace
                .as_ref()
                .and_then(|w| w.pcb_version.as_deref()),
            Some("0.2")
        );
    }

    #[test]
    fn test_parse_rejects_patch_pcb_version() {
        let err = PcbToml::parse(
            r#"
[workspace]
pcb-version = "0.4.1"
"#,
        )
        .expect_err("expected patch pcb-version to be rejected at parse time");

        assert!(err.to_string().contains("expected \"major.minor\""));
    }

    #[test]
    fn test_empty_manifest_parses() {
        let config = PcbToml::parse("").unwrap();
        assert!(config.workspace.is_none());
        assert!(config.board.is_none());
    }

    #[test]
    fn test_extract_inline_manifest_basic() {
        let zen_content = r#"#!/usr/bin/env pcb build
#
# ```pcb
# [workspace]
# pcb-version = "0.4"
#
# [dependencies]
# ```

load("@stdlib/units.zen", "Voltage")
"#;

        let result = extract_inline_manifest(zen_content);
        assert!(result.is_some());
        let toml = result.unwrap();
        assert!(toml.contains("[workspace]"));
        assert!(toml.contains("pcb-version = \"0.4\""));
        assert!(toml.contains("[dependencies]"));
    }

    #[test]
    fn test_extract_inline_manifest_no_shebang() {
        let zen_content = r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
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
# pcb-version = "0.4"
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
# pcb-version = "0.4"
# ```

load("foo.zen", "Bar")
"#;

        let result = PcbToml::from_zen_content(zen_content);
        assert!(result.is_some());
        let config = result.unwrap().unwrap();
        assert!(config.workspace.is_some());
    }

    #[test]
    fn test_from_zen_content_rejects_legacy_packages() {
        let zen_content = r#"# ```pcb
# [packages]
# stdlib = "@github/diodeinc/stdlib:v0.3.2"
# ```

load("@stdlib/foo.zen", "Bar")
"#;

        let result = PcbToml::from_zen_content(zen_content);
        assert!(result.is_some());
        let err = result
            .unwrap()
            .expect_err("legacy [packages] should not parse");
        assert!(err.to_string().contains("unknown field `packages`"));
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
}
