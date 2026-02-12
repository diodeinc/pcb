use std::{
    collections::HashMap,
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
pub const STDLIB_VERSION: &str = "0.5.8";

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
pub use lang::eval::{EvalContext, EvalContextConfig, EvalOutput};
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
pub(crate) struct ResolveContext<'a> {
    // Input parameters
    pub file_provider: &'a dyn FileProvider,
    pub current_file: PathBuf,

    // Resolution history - specs get pushed as they're resolved further
    // Index 0 = original spec, later indices = progressively resolved specs
    pub spec_history: Vec<LoadSpec>,
}

impl<'a> ResolveContext<'a> {
    /// Create a new ResolveContext with the required input parameters
    pub fn new(
        file_provider: &'a dyn FileProvider,
        current_file: PathBuf,
        load_spec: LoadSpec,
    ) -> Self {
        Self {
            file_provider,
            current_file,
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
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(name) => {
                normalized.push(name);
            }
            std::path::Component::CurDir => {}
        }
    }
    normalized
}

/// Normalize a URL-like path string by resolving `..` and `.` components.
///
/// Unlike filesystem paths, this operates purely on string segments separated by `/`.
/// Used to apply relative paths in URL space (e.g., resolving `../../modules/Led.zen`
/// relative to `github.com/org/repo/boards/DM0002`).
pub fn normalize_url_path(url: &str) -> anyhow::Result<String> {
    let mut result: Vec<&str> = Vec::new();
    for part in url.split('/') {
        match part {
            ".." => {
                if result.pop().is_none() {
                    anyhow::bail!("Relative path escapes beyond package root: '{}'", url);
                }
            }
            "." | "" => {}
            _ => result.push(part),
        }
    }
    Ok(result.join("/"))
}

/// Validate filename case matches exactly on disk.
/// Prevents macOS/Windows working but Linux CI failing.
pub(crate) fn validate_path_case(
    file_provider: &dyn FileProvider,
    path: &Path,
) -> anyhow::Result<()> {
    // Use canonicalize to get the actual case on disk.
    // On macOS/Windows, canonicalize returns the true filesystem case.
    let canonical = file_provider.canonicalize(path)?;
    validate_path_case_with_canonical(path, &canonical)
}

/// Validate filename case when we already have the canonical path.
pub(crate) fn validate_path_case_with_canonical(
    original: &Path,
    canonical: &Path,
) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url_path() {
        assert_eq!(
            normalize_url_path("github.com/org/repo/boards/DM0002/../../modules/Led/Led.zen")
                .unwrap(),
            "github.com/org/repo/modules/Led/Led.zen"
        );
        assert_eq!(
            normalize_url_path("boards/DM0002/../../modules/Led/Led.zen").unwrap(),
            "modules/Led/Led.zen"
        );
        assert_eq!(
            normalize_url_path("github.com/org/repo/src/./utils.zen").unwrap(),
            "github.com/org/repo/src/utils.zen"
        );
    }

    #[test]
    fn test_normalize_url_path_underflow() {
        assert!(normalize_url_path("a/../../b.zen").is_err());
        assert!(normalize_url_path("../../x.zen").is_err());
    }
}
