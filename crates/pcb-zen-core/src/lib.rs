use std::{
    any::Any,
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

pub mod config;
pub mod convert;
pub mod diagnostics;
mod file_provider;
pub mod graph;
pub mod lang;
pub mod load_spec;
mod moved;
pub mod passes;
pub mod resolution;
pub mod workspace;

/// Pinned stdlib version bundled with this toolchain.
///
/// This version is used as an implicit minimum dependency for all packages.
/// Users can upgrade past this version by explicitly declaring a newer version
/// in their pcb.toml, but cannot use an older version.
pub const STDLIB_VERSION: &str = "0.5.6";

/// The module path for stdlib
pub const STDLIB_MODULE_PATH: &str = "github.com/diodeinc/stdlib";

/// Attribute, net, and record field constants used across the core
pub mod attrs {
    pub const MODEL_DEF: &str = "__model_def";
    pub const MODEL_NAME: &str = "__model_name";
    pub const MODEL_NETS: &str = "__model_nets";
    pub const MODEL_ARGS: &str = "__model_args";
    pub const SIGNATURE: &str = "__signature";
    pub const LAYOUT_PATH: &str = "layout_path";
    pub const FOOTPRINT: &str = "footprint";
    pub const PREFIX: &str = "prefix";
    pub const MPN: &str = "mpn";
    pub const MANUFACTURER: &str = "manufacturer";
    pub const TYPE: &str = "type";
    pub const SYMBOL_NAME: &str = "symbol_name";
    pub const SYMBOL_PATH: &str = "symbol_path";
    pub const SYMBOL_VALUE: &str = "__symbol_value";
    pub const PADS: &str = "pads";
    pub const DNP: &str = "dnp";
    pub const SKIP_BOM: &str = "skip_bom";
    pub const SKIP_POS: &str = "skip_pos";
    pub const DATASHEET: &str = "datasheet";
    pub const DESCRIPTION: &str = "description";
}

// Re-export commonly used types
pub use config::{
    extract_asset_ref, extract_asset_ref_strict, AssetDependencyDetail, AssetDependencySpec,
    BoardConfig, LockEntry, Lockfile, ModuleConfig, PcbToml, WorkspaceConfig,
};
pub use diagnostics::{
    Diagnostic, DiagnosticError, DiagnosticFrame, DiagnosticReport, Diagnostics, DiagnosticsPass,
    DiagnosticsReport, LoadError, WithDiagnostics,
};
pub use lang::error::SuppressedDiagnostics;
pub use lang::eval::{EvalContext, EvalOutput};
pub use load_spec::LoadSpec;
pub use passes::{
    AggregatePass, CommentSuppressPass, FilterHiddenPass, JsonExportPass, LspFilterPass,
    PromotePass, SortPass, StylePromotePass, SuppressPass,
};

// Re-export file provider types
pub use file_provider::InMemoryFileProvider;

// Re-export types needed by pcb-zen
pub use lang::component::FrozenComponentValue;
pub use lang::interface::FrozenInterfaceValue;
pub use lang::module::{FrozenModuleValue, ModulePath};
pub use lang::net::{FrozenNetValue, NetId};
pub use lang::spice_model::FrozenSpiceModelValue;

/// Abstraction for file system access to make the core WASM-compatible
pub trait FileProvider: Send + Sync {
    /// Read the contents of a file at the given path
    fn read_file(&self, path: &std::path::Path) -> Result<String, FileProviderError>;

    /// Check if a file exists
    fn exists(&self, path: &std::path::Path) -> bool;

    /// Check if a path is a directory
    fn is_directory(&self, path: &std::path::Path) -> bool;

    /// Check if a path is a symlink
    fn is_symlink(&self, path: &std::path::Path) -> bool;

    /// List files in a directory (for directory imports)
    fn list_directory(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, FileProviderError>;

    /// Canonicalize a path (make it absolute)
    fn canonicalize(&self, path: &std::path::Path)
        -> Result<std::path::PathBuf, FileProviderError>;

    /// Global package cache directory (e.g. `~/.pcb/cache`).
    /// Returns empty path by default (WASM / in-memory providers).
    fn cache_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::new()
    }
}

/// Blanket implementation of FileProvider for Arc<T> where T: FileProvider
impl<T: FileProvider + ?Sized> FileProvider for Arc<T> {
    fn read_file(&self, path: &std::path::Path) -> Result<String, FileProviderError> {
        (**self).read_file(path)
    }

    fn exists(&self, path: &std::path::Path) -> bool {
        (**self).exists(path)
    }

    fn is_directory(&self, path: &std::path::Path) -> bool {
        (**self).is_directory(path)
    }

    fn is_symlink(&self, path: &std::path::Path) -> bool {
        (**self).is_symlink(path)
    }

    fn list_directory(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, FileProviderError> {
        (**self).list_directory(path)
    }

    fn canonicalize(
        &self,
        path: &std::path::Path,
    ) -> Result<std::path::PathBuf, FileProviderError> {
        (**self).canonicalize(path)
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        (**self).cache_dir()
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum FileProviderError {
    #[error("File not found: {0}")]
    NotFound(std::path::PathBuf),

    #[error("Permission denied: {0}")]
    PermissionDenied(std::path::PathBuf),

    #[error("IO error: {0}")]
    IoError(String),
}

/// Information about a symbol in a module
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub kind: SymbolKind,
    pub parameters: Option<Vec<String>>,
    pub source_path: Option<std::path::PathBuf>,
    pub type_name: String,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Module,
    Class,
    Variable,
    Interface,
    Component,
}

/// Default implementation of FileProvider that uses the actual file system with caching
#[cfg(feature = "native")]
#[derive(Clone)]
pub struct DefaultFileProvider {
    canonicalize_cache: Arc<RwLock<HashMap<PathBuf, Result<PathBuf, FileProviderError>>>>,
}

#[cfg(feature = "native")]
impl DefaultFileProvider {
    pub fn new() -> Self {
        Self {
            canonicalize_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[cfg(feature = "native")]
impl Default for DefaultFileProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "native")]
impl std::fmt::Debug for DefaultFileProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cache_size = self.canonicalize_cache.read().unwrap().len();
        f.debug_struct("DefaultFileProvider")
            .field("cache_size", &cache_size)
            .finish()
    }
}

#[cfg(feature = "native")]
impl FileProvider for DefaultFileProvider {
    fn read_file(&self, path: &std::path::Path) -> Result<String, FileProviderError> {
        std::fs::read_to_string(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileProviderError::NotFound(path.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => {
                FileProviderError::PermissionDenied(path.to_path_buf())
            }
            _ => FileProviderError::IoError(e.to_string()),
        })
    }

    fn exists(&self, path: &std::path::Path) -> bool {
        path.exists()
    }

    fn is_directory(&self, path: &std::path::Path) -> bool {
        path.is_dir()
    }

    fn is_symlink(&self, path: &std::path::Path) -> bool {
        path.is_symlink()
    }

    fn list_directory(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, FileProviderError> {
        let entries = std::fs::read_dir(path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileProviderError::NotFound(path.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => {
                FileProviderError::PermissionDenied(path.to_path_buf())
            }
            _ => FileProviderError::IoError(e.to_string()),
        })?;

        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(e) => paths.push(e.path()),
                Err(e) => return Err(FileProviderError::IoError(e.to_string())),
            }
        }
        Ok(paths)
    }

    fn canonicalize(
        &self,
        path: &std::path::Path,
    ) -> Result<std::path::PathBuf, FileProviderError> {
        let path_buf = path.to_path_buf();

        // Check cache first (read lock)
        {
            let cache = self.canonicalize_cache.read().unwrap();
            if let Some(cached_result) = cache.get(&path_buf) {
                return cached_result.clone();
            }
        }

        // Cache miss - compute the result
        let result = path.canonicalize().or_else(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                // Normalize path components (handle . and ..)
                Ok(normalize_path(path))
            }
            std::io::ErrorKind::PermissionDenied => {
                Err(FileProviderError::PermissionDenied(path.to_path_buf()))
            }
            _ => Err(FileProviderError::IoError(e.to_string())),
        });

        // Store result in cache (write lock)
        {
            let mut cache = self.canonicalize_cache.write().unwrap();
            cache.insert(path_buf, result.clone());
        }

        result
    }

    fn cache_dir(&self) -> std::path::PathBuf {
        dirs::home_dir()
            .expect("Cannot determine home directory")
            .join(".pcb/cache")
    }
}

/// Information about a package alias including its target and source
#[derive(Debug, Clone)]
pub struct AliasInfo {
    /// The target of the alias (e.g., "@github/mycompany/components:main")
    pub target: String,
    /// The canonical path to the pcb.toml file that defined this alias.
    /// None for built-in default aliases.
    pub source_path: Option<PathBuf>,
}

/// Context struct for load resolution operations
/// Contains input parameters and computed state for path resolution
pub struct ResolveContext<'a> {
    // Input parameters
    pub file_provider: &'a dyn FileProvider,
    pub current_file: PathBuf,
    pub current_file_spec: LoadSpec,

    // Resolution history - specs get pushed as they're resolved further
    // Index 0 = original spec, later indices = progressively resolved specs
    pub spec_history: Vec<LoadSpec>,
}

impl<'a> ResolveContext<'a> {
    /// Create a new ResolveContext with the required input parameters
    pub fn new(
        file_provider: &'a dyn FileProvider,
        current_file: PathBuf,
        current_spec: LoadSpec,
        load_spec: LoadSpec,
    ) -> Self {
        Self {
            file_provider,
            current_file,
            current_file_spec: current_spec,
            spec_history: vec![load_spec],
        }
    }

    /// Get the current (most recently resolved) spec
    pub fn latest_spec(&self) -> &LoadSpec {
        self.spec_history
            .last()
            .expect("spec_history should never be empty")
    }

    /// Returns the original spec that was passed to the ResolveContext
    pub fn original_spec(&self) -> &LoadSpec {
        self.spec_history
            .first()
            .expect("spec_history should never be empty")
    }

    /// Push a newly resolved spec onto the resolution history with cycle detection
    pub fn push_spec(&mut self, spec: LoadSpec) -> anyhow::Result<()> {
        // Check for cycles - if we've already seen this spec, it's a cycle
        if self.spec_history.contains(&spec) {
            return Err(anyhow::anyhow!(
                "Circular dependency detected: spec {} creates a cycle in resolution history",
                spec
            ));
        }
        self.spec_history.push(spec);
        Ok(())
    }
}

pub trait LoadResolver: Send + Sync + Any {
    /// Convenience method to resolve a load path string directly
    /// This encapsulates the common pattern of parsing a path and creating a ResolveContext
    fn resolve_path(&self, path: &str, current_file: &Path) -> Result<PathBuf, anyhow::Error> {
        let mut context = self.resolve_context(path, current_file)?;
        self.resolve(&mut context)
    }

    /// Convenience method to resolve a LoadSpec directly
    fn resolve_spec(
        &self,
        load_spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        let mut context = self.resolve_context_from_spec(load_spec, current_file)?;
        self.resolve(&mut context)
    }

    fn resolve_context<'a>(
        &'a self,
        path: &str,
        current_file: &Path,
    ) -> Result<ResolveContext<'a>, anyhow::Error> {
        let let_spec = LoadSpec::parse(path)
            .ok_or_else(|| anyhow::anyhow!("Invalid load path spec: {}", path))?;
        self.resolve_context_from_spec(&let_spec, current_file)
    }

    fn resolve_context_from_spec<'a>(
        &'a self,
        load_spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<ResolveContext<'a>, anyhow::Error> {
        let current_file = self.file_provider().canonicalize(current_file)?;
        self.track_file(&current_file);
        let current_spec = self.get_load_spec(&current_file).ok_or_else(|| {
            anyhow::anyhow!(
                "Current file should have a LoadSpec: {}",
                current_file.display()
            )
        })?;
        let context = ResolveContext::new(
            self.file_provider(),
            current_file,
            current_spec,
            load_spec.clone(),
        );
        Ok(context)
    }

    fn file_provider(&self) -> &dyn FileProvider;

    /// Resolve a LoadSpec to an absolute file path using the provided context
    ///
    /// The context contains the load specification, current file, and other state needed for resolution.
    /// Returns the resolved absolute path that should be loaded.
    fn resolve(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error>;

    /// Manually track a file. Useful for entrypoints.
    fn track_file(&self, path: &Path);

    /// Get the LoadSpec for a specific resolved file path
    fn get_load_spec(&self, path: &Path) -> Option<LoadSpec>;
}

/// File extension constants and utilities
pub mod file_extensions {
    use std::ffi::OsStr;

    /// Supported Starlark-like file extensions
    pub const STARLARK_EXTENSIONS: &[&str] = &["star", "zen"];

    /// KiCad symbol file extension
    pub const KICAD_SYMBOL_EXTENSION: &str = "kicad_sym";

    /// Check if a file has a Starlark-like extension
    pub fn is_starlark_file(extension: Option<&OsStr>) -> bool {
        extension
            .and_then(OsStr::to_str)
            .map(|ext| {
                STARLARK_EXTENSIONS
                    .iter()
                    .any(|&valid_ext| ext.eq_ignore_ascii_case(valid_ext))
            })
            .unwrap_or(false)
    }

    /// Check if a file has a KiCad symbol extension
    pub fn is_kicad_symbol_file(extension: Option<&OsStr>) -> bool {
        extension
            .and_then(OsStr::to_str)
            .map(|ext| ext.eq_ignore_ascii_case(KICAD_SYMBOL_EXTENSION))
            .unwrap_or(false)
    }
}

/// Normalize a path by resolving .. and . components
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => {
                normalized.push(prefix.as_os_str());
            }
            std::path::Component::RootDir => {
                normalized.push("/");
            }
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    // If we can't pop (e.g., at root), keep the parent dir
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(name) => {
                normalized.push(name);
            }
            std::path::Component::CurDir => {
                // Skip current directory
            }
        }
    }
    normalized
}

/// Core load resolver that handles all path resolution logic.
/// This resolver handles workspace paths, relative paths
pub struct CoreLoadResolver {
    file_provider: Arc<dyn FileProvider>,
    /// Resolution map: Package Root -> Import URL -> Resolved Path
    /// Contains workspace packages AND transitive remote deps
    /// BTreeMap enables longest prefix matching for nested package paths
    package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    /// Maps resolved paths to their original LoadSpecs
    /// This allows us to resolve relative paths from remote files correctly
    path_to_spec: Arc<RwLock<HashMap<PathBuf, LoadSpec>>>,
}

impl CoreLoadResolver {
    /// Create a new CoreLoadResolver with the given file provider and remote fetcher.
    pub fn new(
        file_provider: Arc<dyn FileProvider>,
        package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    ) -> Self {
        // Canonicalize package_resolutions keys to match canonicalized file paths during lookup.
        // On Windows, canonicalize() adds \\?\ UNC prefix which must match for HashMap lookups.
        let package_resolutions = package_resolutions
            .into_iter()
            .map(|(root, deps)| {
                let canon_root = file_provider.canonicalize(&root).unwrap_or(root);
                (canon_root, deps)
            })
            .collect();

        Self {
            file_provider,
            path_to_spec: Arc::new(RwLock::new(HashMap::new())),
            package_resolutions,
        }
    }

    fn insert_load_spec(&self, resolved_path: PathBuf, spec: LoadSpec) {
        self.path_to_spec
            .write()
            .unwrap()
            .insert(resolved_path, spec);
    }

    /// Find the package root for a given file by walking up directories
    ///
    /// First tries package_resolutions map (workspace packages), then walks up looking for pcb.toml (cached packages)
    fn find_package_root_for_file(
        &self,
        file: &Path,
        package_resolutions: &HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    ) -> anyhow::Result<PathBuf> {
        let mut current = file.parent();
        while let Some(dir) = current {
            // Check workspace package resolutions first
            if package_resolutions.contains_key(dir) {
                return Ok(dir.to_path_buf());
            }

            // Check for pcb.toml (handles cached packages)
            let pcb_toml = dir.join("pcb.toml");
            if self.file_provider.exists(&pcb_toml) {
                return Ok(dir.to_path_buf());
            }

            current = dir.parent();
        }
        anyhow::bail!(
            "Internal error: current file not in any package: {}",
            file.display()
        )
    }

    fn resolved_map_for_package_root(
        &self,
        package_root: &Path,
    ) -> anyhow::Result<&BTreeMap<String, PathBuf>> {
        self.package_resolutions.get(package_root).ok_or_else(|| {
            anyhow::anyhow!(
                "Dependency map not loaded for package '{}'",
                package_root.display()
            )
        })
    }

    /// Expand alias using the resolution map
    ///
    /// Aliases are auto-generated from the last path segment of dependency URLs.
    /// For example, "github.com/diodeinc/stdlib" generates the alias "stdlib".
    fn expand_alias(&self, context: &ResolveContext, alias: &str) -> Result<String, anyhow::Error> {
        // Find the package root for the current file
        let package_root =
            self.find_package_root_for_file(&context.current_file, &self.package_resolutions)?;

        // Get the resolution map for this package
        let resolved_map = self.resolved_map_for_package_root(&package_root)?;

        // Derive alias from resolution map keys by matching last path segment
        // Also include KiCad asset aliases
        for url in resolved_map.keys() {
            if let Some(last_segment) = url.rsplit('/').next() {
                if last_segment == alias {
                    return Ok(url.clone());
                }
            }
        }

        // Check KiCad asset aliases
        for (kicad_alias, base_url, _) in config::KICAD_ASSETS {
            if *kicad_alias == alias {
                return Ok(base_url.to_string());
            }
        }

        anyhow::bail!("Unknown alias '@{}'", alias)
    }

    /// remote resolution: longest prefix match against package's declared deps
    fn try_resolve_workspace(
        &self,
        context: &ResolveContext,
        package_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        let spec = context.latest_spec();

        // Build full URL from spec
        let (base, path) = match spec {
            LoadSpec::Github {
                user, repo, path, ..
            } => (format!("github.com/{}/{}", user, repo), path),
            LoadSpec::Gitlab {
                project_path, path, ..
            } => (format!("gitlab.com/{}", project_path), path),
            LoadSpec::Package { package, path, .. } => (package.clone(), path),
            _ => unreachable!(),
        };

        let mut full_url = base;
        for component in path.components() {
            if let std::path::Component::Normal(part) = component {
                full_url.push('/');
                full_url.push_str(&part.to_string_lossy());
            }
        }

        let resolved_map = self.resolved_map_for_package_root(package_root)?;

        // Longest prefix match
        let best_match = resolved_map.iter().rev().find(|(dep_url, _)| {
            full_url.starts_with(dep_url.as_str())
                && (full_url.len() == dep_url.len()
                    || full_url.as_bytes().get(dep_url.len()) == Some(&b'/'))
        });

        let Some((matched_dep, root_path)) = best_match else {
            // Strip the filename from full_url to show the package path
            let package_hint = full_url
                .rsplit_once('/')
                .map(|(prefix, _)| prefix)
                .unwrap_or(&full_url);
            anyhow::bail!(
                "No declared dependency matches '{}'\n  \
                Add a dependency that covers this path to [dependencies] in pcb.toml",
                package_hint
            );
        };

        let relative_path = full_url
            .strip_prefix(matched_dep.as_str())
            .and_then(|s| s.strip_prefix('/'))
            .unwrap_or("");

        let full_path = if relative_path.is_empty() {
            root_path.clone()
        } else {
            root_path.join(relative_path)
        };

        if !self.file_provider.exists(&full_path) {
            anyhow::bail!(
                "File not found: {} (resolved to: {}, dep root: {})",
                relative_path,
                full_path.display(),
                root_path.display()
            );
        }

        self.insert_load_spec(full_path.clone(), spec.clone());
        Ok(full_path)
    }

    /// URL resolution: translate canonical URL to cache path using resolution map
    fn resolve_url(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        // Find which package the current file belongs to
        let package_root =
            self.find_package_root_for_file(&context.current_file, &self.package_resolutions)?;

        // Use existing try_resolve_workspace which does longest-prefix matching
        self.try_resolve_workspace(context, &package_root)
    }

    /// relative path resolution: resolve relative to current file with boundary enforcement
    fn resolve_relative(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        let LoadSpec::Path { path, .. } = context.latest_spec() else {
            unreachable!("resolve_relative called on non-Path spec");
        };

        // Find package root for boundary enforcement
        let package_root =
            self.find_package_root_for_file(&context.current_file, &self.package_resolutions)?;

        // Resolve relative to current file's directory
        let current_dir = context
            .current_file
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Current file has no parent directory"))?;

        let resolved_path = current_dir.join(path);

        // Canonicalize both paths for boundary check
        let canonical_resolved = context.file_provider.canonicalize(&resolved_path)?;
        let canonical_root = context.file_provider.canonicalize(&package_root)?;

        // Enforce package boundary: resolved path must stay within package root
        if !canonical_resolved.starts_with(&canonical_root) {
            anyhow::bail!(
                "Cannot load outside package boundary: '{}' would escape package root '{}'",
                path.display(),
                package_root.display()
            );
        }

        // Case sensitivity check: compare original filename to canonical filename
        validate_path_case_with_canonical(path, &canonical_resolved)?;

        Ok(canonical_resolved)
    }
}

impl LoadResolver for CoreLoadResolver {
    fn file_provider(&self) -> &dyn FileProvider {
        &*self.file_provider
    }

    /// resolution: Toolchain + package-level aliases, URLs, and relative paths
    ///
    /// supports three load patterns:
    /// 1. Aliases: load("@stdlib/units.zen") - expanded via toolchain or package aliases
    /// 2. Canonical URLs: load("github.com/user/repo/path.zen") - looked up in resolution map
    /// 3. Relative paths: load("./utils.zen") - resolved relative to current file with boundary checks
    fn resolve(&self, context: &mut ResolveContext) -> Result<PathBuf, anyhow::Error> {
        // Expand aliases: package-level first, then toolchain-level
        if let LoadSpec::Package { package, path, .. } = context.latest_spec() {
            let expanded_url = self.expand_alias(context, package)?;
            let full_url = if path.as_os_str().is_empty() {
                expanded_url
            } else {
                format!("{}/{}", expanded_url, path.display())
            };

            // Reparse the expanded URL as a proper LoadSpec
            let expanded_spec = LoadSpec::parse(&full_url)
                .ok_or_else(|| anyhow::anyhow!("Failed to parse expanded alias: {}", full_url))?;
            context.push_spec(expanded_spec)?;
        }

        let resolved_path = match context.latest_spec() {
            // URL loads: github.com/... or gitlab.com/...
            LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => self.resolve_url(context)?,
            // Relative path loads: ./utils.zen or ../sibling.zen
            LoadSpec::Path { .. } => self.resolve_relative(context)?,
            LoadSpec::Package { .. } => unreachable!("Package checked above"),
        };

        // Validate existence
        if !context.file_provider.exists(&resolved_path)
            && !context.original_spec().allow_not_exist()
        {
            return Err(anyhow::anyhow!(
                "File not found: {}",
                resolved_path.display()
            ));
        }

        // Case sensitivity validation
        if context.file_provider.exists(&resolved_path) {
            validate_path_case(context.file_provider, &resolved_path)?;
        }

        Ok(resolved_path)
    }

    fn track_file(&self, path: &Path) {
        let canonical_path = self.file_provider.canonicalize(path).unwrap();
        if self.get_load_spec(&canonical_path).is_some() {
            // If already tracked, do nothing
            return;
        }
        let load_spec = LoadSpec::local_path(&canonical_path);
        self.insert_load_spec(canonical_path, load_spec);
    }

    fn get_load_spec(&self, path: &Path) -> Option<LoadSpec> {
        self.path_to_spec.read().unwrap().get(path).cloned()
    }
}

/// Validate filename case matches exactly on disk.
/// Prevents macOS/Windows working but Linux CI failing.
fn validate_path_case(file_provider: &dyn FileProvider, path: &Path) -> anyhow::Result<()> {
    // Use canonicalize to get the actual case on disk.
    // On macOS/Windows, canonicalize returns the true filesystem case.
    let canonical = file_provider.canonicalize(path)?;
    validate_path_case_with_canonical(path, &canonical)
}

/// Validate filename case when we already have the canonical path.
fn validate_path_case_with_canonical(original: &Path, canonical: &Path) -> anyhow::Result<()> {
    let Some(expected_filename) = original.file_name() else {
        return Ok(());
    };
    let Some(actual_filename) = canonical.file_name() else {
        return Ok(());
    };

    if actual_filename != expected_filename {
        // Double-check it's actually a case mismatch (not a different file)
        if actual_filename.to_string_lossy().to_lowercase()
            == expected_filename.to_string_lossy().to_lowercase()
        {
            return Err(anyhow::anyhow!(
                "Case mismatch: expected '{}', found '{}'",
                expected_filename.to_string_lossy(),
                actual_filename.to_string_lossy()
            ));
        }
    }

    Ok(())
}
