use std::{
    collections::{HashMap, HashSet},
    error::Error as StdError,
    fmt::Display,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::Context;
use serde::ser::SerializeStruct;
use starlark::{
    codemap::ResolvedSpan,
    errors::{EvalMessage, EvalSeverity},
    eval::CallStack,
};

pub mod bundle;
pub mod convert;
mod file_provider;
pub mod lang;
pub mod load_spec;

// Re-export commonly used types
pub use lang::eval::{EvalContext, EvalOutput};
pub use lang::input::{InputMap, InputValue};
pub use load_spec::LoadSpec;

// Re-export file provider types
pub use file_provider::InMemoryFileProvider;

// Re-export types needed by pcb-zen
pub use lang::component::FrozenComponentValue;
pub use lang::module::FrozenModuleValue;
pub use lang::net::{FrozenNetValue, NetId};

/// A wrapper error type that carries a Diagnostic through the starlark error chain.
/// This allows us to preserve the full diagnostic information when errors cross
/// module boundaries during load() operations.
#[derive(Debug, Clone)]
pub struct DiagnosticError(pub Diagnostic);

impl Display for DiagnosticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Just display the inner diagnostic
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DiagnosticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Wrapper error that has DiagnosticError as its source, allowing it to be
/// discovered through the error chain.
#[derive(Debug)]
pub struct LoadError {
    pub message: String,
    pub diagnostic: DiagnosticError,
}

impl Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.diagnostic)
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub path: String,
    pub span: Option<ResolvedSpan>,
    pub severity: EvalSeverity,
    pub body: String,
    pub call_stack: Option<CallStack>,

    /// Optional child diagnostic representing a nested error that occurred in a
    /// downstream (e.g. loaded) module.  When present, this allows callers to
    /// reconstruct a chain of diagnostics across module/evaluation boundaries
    /// without needing to rely on parsing rendered strings.
    pub child: Option<Box<Diagnostic>>,
}

impl serde::Serialize for Diagnostic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Diagnostic", 6)?;
        state.serialize_field("path", &self.path)?;
        state.serialize_field("span", &self.span.map(|span| span.to_string()))?;
        state.serialize_field("severity", &self.severity)?;
        state.serialize_field("body", &self.body)?;
        state.serialize_field(
            "call_stack",
            &self.call_stack.as_ref().map(|stack| stack.to_string()),
        )?;
        state.serialize_field("child", &self.child)?;
        state.end()
    }
}

impl Diagnostic {
    pub fn from_eval_message(msg: EvalMessage) -> Self {
        Self {
            path: msg.path,
            span: msg.span,
            severity: msg.severity,
            body: msg.description,
            call_stack: None,
            child: None,
        }
    }

    pub fn from_error(err: starlark::Error) -> Self {
        // Check the source chain of the error kind
        if let Some(source) = err.kind().source() {
            let mut current: Option<&(dyn StdError + 'static)> = Some(source);
            while let Some(src) = current {
                // Check if this source is our DiagnosticError
                if let Some(diag_err) = src.downcast_ref::<DiagnosticError>() {
                    return diag_err.0.clone();
                }
                current = src.source();
            }
        }

        // No hidden diagnostic found - create one from the starlark error
        Self {
            path: err
                .span()
                .map(|span| span.file.filename().to_string())
                .unwrap_or_default(),
            span: err.span().map(|span| span.resolve_span()),
            severity: EvalSeverity::Error,
            body: err.kind().to_string(),
            call_stack: Some(err.call_stack().clone()),
            child: None,
        }
    }

    pub fn with_child(self, child: Diagnostic) -> Self {
        Self {
            child: Some(Box::new(child)),
            ..self
        }
    }

    /// Return `true` if the diagnostic severity is `Error`.
    pub fn is_error(&self) -> bool {
        matches!(self.severity, EvalSeverity::Error)
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Format: "Error: path:line:col-line:col message"
        write!(f, "{}: ", self.severity)?;

        if !self.path.is_empty() {
            write!(f, "{}", self.path)?;
            if let Some(span) = &self.span {
                write!(f, ":{span}")?;
            }
            write!(f, " ")?;
        }

        write!(f, "{}", self.body)?;

        let mut current = &self.child;
        while let Some(diag) = current {
            write!(f, "\n{}: ", diag.severity)?;

            if !diag.path.is_empty() {
                write!(f, "{}", diag.path)?;
                if let Some(span) = &diag.span {
                    write!(f, ":{span}")?;
                }
                write!(f, " ")?;
            }

            write!(f, "{}", diag.body)?;
            current = &diag.child;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // We don't have a source error, as Diagnostic is our root error type
        None
    }
}

#[derive(Debug, Clone)]
pub struct WithDiagnostics<T> {
    pub diagnostics: Vec<Diagnostic>,
    pub output: Option<T>,
}

impl<T: Display> Display for WithDiagnostics<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(output) = &self.output {
            write!(f, "{output}")?;
        }
        for diagnostic in &self.diagnostics {
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl<T> WithDiagnostics<T> {
    /// Convenience constructor for a *successful* evaluation.
    pub fn success(output: T, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            output: Some(output),
        }
    }

    /// Convenience constructor for a *failed* evaluation.
    pub fn failure(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            diagnostics,
            output: None,
        }
    }

    /// Return `true` if any diagnostic in the list represents an error.
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.is_error())
    }

    /// Return `true` if evaluation produced an output **and** did not emit
    /// any error-level diagnostics.
    pub fn is_success(&self) -> bool {
        self.output.is_some() && !self.has_errors()
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> WithDiagnostics<U> {
        if let Some(output) = self.output {
            WithDiagnostics::success(f(output), self.diagnostics)
        } else {
            WithDiagnostics::failure(self.diagnostics)
        }
    }

    pub fn flat_map<U>(self, f: impl FnOnce(T) -> WithDiagnostics<U>) -> WithDiagnostics<U> {
        match self.output {
            Some(output) => {
                let mut result = f(output);
                let mut diagnostics = self.diagnostics;
                diagnostics.append(&mut result.diagnostics);
                if result.output.is_some() {
                    WithDiagnostics::success(result.output.unwrap(), diagnostics)
                } else {
                    WithDiagnostics::failure(diagnostics)
                }
            }
            None => WithDiagnostics::failure(self.diagnostics),
        }
    }
}

/// Abstraction for file system access to make the core WASM-compatible
pub trait FileProvider: Send + Sync {
    /// Read the contents of a file at the given path
    fn read_file(&self, path: &std::path::Path) -> Result<String, FileProviderError>;

    /// Check if a file exists
    fn exists(&self, path: &std::path::Path) -> bool;

    /// Check if a path is a directory
    fn is_directory(&self, path: &std::path::Path) -> bool;

    /// List files in a directory (for directory imports)
    fn list_directory(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, FileProviderError>;

    /// Canonicalize a path (make it absolute)
    fn canonicalize(&self, path: &std::path::Path)
        -> Result<std::path::PathBuf, FileProviderError>;
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

/// Default implementation of FileProvider that uses the actual file system
#[cfg(feature = "native")]
#[derive(Debug, Clone)]
pub struct DefaultFileProvider;

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
        path.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FileProviderError::NotFound(path.to_path_buf()),
            std::io::ErrorKind::PermissionDenied => {
                FileProviderError::PermissionDenied(path.to_path_buf())
            }
            _ => FileProviderError::IoError(e.to_string()),
        })
    }
}

/// Abstraction for fetching remote resources (packages, GitHub repos, etc.)
/// This allows pcb-zen-core to handle all resolution logic while delegating
/// the actual network/filesystem operations to the implementor.
pub trait RemoteFetcher: Send + Sync {
    /// Fetch a remote resource and return the local path where it was materialized.
    /// This could involve downloading, caching, unpacking, etc.
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error>;
}

#[derive(Debug, Clone, Default)]
pub struct NoopRemoteFetcher;

impl RemoteFetcher for NoopRemoteFetcher {
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        _workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        Err(anyhow::anyhow!(
            "Remote fetch for {:?} blocked because --offline mode is enabled. \
            Run 'pcb vendor' to download dependencies locally.",
            spec
        ))
    }
}

/// Abstraction for resolving load() paths to file contents
pub trait LoadResolver: Send + Sync {
    /// Resolve a LoadSpec to an absolute file path
    ///
    /// The `spec` is a parsed load specification.
    /// The `current_file` is the file that contains the load() statement.
    ///
    /// Returns the resolved absolute path that should be loaded.
    fn resolve_spec(
        &self,
        file_provider: &dyn FileProvider,
        spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error>;

    /// Resolve a load path to an absolute file path
    ///
    /// The `load_path` is the string passed to load() in Starlark code.
    /// The `current_file` is the file that contains the load() statement.
    ///
    /// Returns the resolved absolute path that should be loaded.
    fn resolve_path(
        &self,
        file_provider: &dyn FileProvider,
        load_path: &str,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        let spec = LoadSpec::parse(load_path)
            .ok_or_else(|| anyhow::anyhow!("Invalid load spec: {}", load_path))?;
        self.resolve_spec(file_provider, &spec, current_file)
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

/// Workspace-related utilities
pub mod workspace {
    use super::FileProvider;
    use std::path::{Path, PathBuf};

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
                if let Ok(contents) = file_provider.read_file(&pcb_toml) {
                    #[derive(serde::Deserialize)]
                    struct WorkspaceToml {
                        workspace: Option<toml::Value>,
                    }

                    if let Ok(parsed) = toml::from_str::<WorkspaceToml>(&contents) {
                        if parsed.workspace.is_some() {
                            return dir;
                        }
                    }
                }
            }
            current = dir.parent().map(|p| p.to_path_buf());
        }
        start.parent().unwrap_or(start).to_path_buf()
    }
}

/// Normalize a path by resolving .. and . components
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::Normal(name) => {
                components.push(name);
            }
            std::path::Component::CurDir => {
                // Skip current directory
            }
            _ => {}
        }
    }
    components.iter().collect()
}

/// Helper to parse package aliases from TOML content
fn parse_package_aliases_from_toml(content: &str) -> HashMap<String, String> {
    #[derive(Debug, serde::Deserialize)]
    struct PkgRoot {
        packages: Option<HashMap<String, String>>,
    }

    match toml::from_str::<PkgRoot>(content) {
        Ok(parsed) => parsed.packages.unwrap_or_default(),
        Err(e) => {
            log::warn!("Failed to parse pcb.toml: {e}");
            HashMap::new()
        }
    }
}

/// Core load resolver that handles all path resolution logic.
/// This resolver handles workspace paths, relative paths, and delegates
/// remote fetching to a RemoteFetcher implementation.
pub struct CoreLoadResolver {
    file_provider: Arc<dyn FileProvider>,
    remote_fetcher: Arc<dyn RemoteFetcher>,
    workspace_root: PathBuf,
    /// Maps resolved paths to their original LoadSpecs
    /// This allows us to resolve relative paths from remote files correctly
    path_to_spec: Arc<Mutex<HashMap<PathBuf, LoadSpec>>>,
    /// Tracks all local files that have been resolved (for vendor/release commands)
    tracked_local_files: Arc<Mutex<HashSet<PathBuf>>>,
    use_vendor_dir: bool,

    // Hierarchical alias resolution cache
    alias_cache: RwLock<HashMap<PathBuf, Arc<HashMap<String, String>>>>,
}

impl CoreLoadResolver {
    /// Create a new CoreLoadResolver with the given file provider and remote fetcher.
    pub fn new(
        file_provider: Arc<dyn FileProvider>,
        remote_fetcher: Arc<dyn RemoteFetcher>,
        workspace_root: PathBuf,
        use_vendor_dir: bool,
    ) -> Self {
        // Canonicalize workspace root once to avoid path comparison issues
        let workspace_root = file_provider
            .canonicalize(&workspace_root)
            .unwrap_or(workspace_root);

        Self {
            file_provider,
            remote_fetcher,
            workspace_root,
            path_to_spec: Arc::new(Mutex::new(HashMap::new())),
            tracked_local_files: Arc::new(Mutex::new(HashSet::new())),
            use_vendor_dir,
            alias_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a CoreLoadResolver for a specific file, automatically finding the workspace root.
    pub fn for_file(
        file_provider: Arc<dyn FileProvider>,
        remote_fetcher: Arc<dyn RemoteFetcher>,
        file: &Path,
        use_vendor_dir: bool,
    ) -> Self {
        let workspace_root = workspace::find_workspace_root(file_provider.as_ref(), file);
        Self::new(
            file_provider,
            remote_fetcher,
            workspace_root,
            use_vendor_dir,
        )
    }

    /// Try to resolve a LoadSpec from the vendor directory
    fn try_resolve_from_vendor(&self, spec: &LoadSpec) -> Result<PathBuf, anyhow::Error> {
        // Helper to build vendor paths with common logic
        fn make_vendor_path<I>(root: &str, segments: I, file_path: &Path) -> PathBuf
        where
            I: IntoIterator,
            I::Item: AsRef<Path>,
        {
            let mut p = PathBuf::from(root);
            for s in segments {
                let segment = s.as_ref();
                if !segment.as_os_str().is_empty() {
                    p.push(segment);
                }
            }
            if !file_path.as_os_str().is_empty() && file_path != Path::new(".") {
                p.push(file_path);
            }
            p
        }

        let vendor_dir = self.workspace_root.join("vendor");

        // Convert spec to vendor directory path
        let canonical_spec = match spec {
            LoadSpec::Package { .. } => spec
                .resolve(None)
                .context("Failed to resolve package alias to canonical form")?,
            _ => spec.clone(),
        };

        let relative_vendor_path = match &canonical_spec {
            LoadSpec::Github {
                user,
                repo,
                rev,
                path,
            } => make_vendor_path("github.com", [&user, &repo, &rev], path),
            LoadSpec::Gitlab {
                project_path,
                rev,
                path,
            } => make_vendor_path("gitlab.com", [&project_path, &rev], path),
            LoadSpec::Package { package, tag, path } => {
                make_vendor_path("packages", [&package, &tag], path)
            }
            _ => anyhow::bail!("Local specs not handled in vendor directory"),
        };

        let full_vendor_path = vendor_dir.join(relative_vendor_path);
        if self.file_provider.exists(&full_vendor_path) {
            let canonical_path = self.file_provider.canonicalize(&full_vendor_path)?;
            self.path_to_spec
                .lock()
                .unwrap()
                .insert(canonical_path.clone(), canonical_spec);
            Ok(canonical_path)
        } else {
            anyhow::bail!("Not found in vendor directory")
        }
    }

    /// Get hierarchical package aliases for a specific file.
    /// This walks from the appropriate root (workspace or repo) to the file's directory,
    /// merging aliases with deeper directories taking priority.
    fn get_aliases_for_file(&self, file: &Path) -> Arc<HashMap<String, String>> {
        log::debug!("Resolving aliases for file: {}", file.display());
        // Canonicalize the directory once
        let canonical_dir = self
            .file_provider
            .canonicalize(file.parent().unwrap_or(Path::new("")))
            .unwrap_or_else(|_| file.parent().unwrap_or(Path::new("")).to_path_buf());

        // Check cache first (optimistic read)
        if let Some(cached) = self.alias_cache.read().unwrap().get(&canonical_dir) {
            log::debug!(
                "  Using cached aliases for directory: {}",
                canonical_dir.display()
            );
            return cached.clone();
        }

        // Determine alias root (workspace or remote repo root)
        let alias_root = if canonical_dir.starts_with(&self.workspace_root) {
            log::debug!("  Using workspace root: {}", self.workspace_root.display());
            self.workspace_root.clone()
        } else {
            // For remote files, find repo root using existing LoadSpec mapping
            let canonical_file = self
                .file_provider
                .canonicalize(file)
                .unwrap_or_else(|_| file.to_path_buf());
            if let Some(spec) = self.path_to_spec.lock().unwrap().get(&canonical_file) {
                match spec {
                    LoadSpec::Github {
                        path: spec_path, ..
                    }
                    | LoadSpec::Gitlab {
                        path: spec_path, ..
                    }
                    | LoadSpec::Package {
                        path: spec_path, ..
                    } => {
                        // Walk up from file by the number of components in spec_path
                        let mut root = canonical_file;
                        for _ in 0..spec_path.components().count() {
                            root = root.parent().unwrap_or(Path::new("")).to_path_buf();
                        }
                        log::debug!("  Using remote repo root: {}", root.display());
                        root
                    }
                    _ => {
                        log::debug!("  File has non-remote LoadSpec, using defaults");
                        return Arc::new(LoadSpec::default_package_aliases());
                    }
                }
            } else {
                log::debug!("  File not in path_to_spec mapping, using defaults");
                return Arc::new(LoadSpec::default_package_aliases());
            }
        };

        // Build aliases by walking ancestors from root to target
        let mut aliases = LoadSpec::default_package_aliases();
        let ancestors: Vec<_> = canonical_dir
            .ancestors()
            .take_while(|p| p.starts_with(&alias_root))
            .collect();
        for ancestor in ancestors.into_iter().rev() {
            if let Ok(content) = self.file_provider.read_file(&ancestor.join("pcb.toml")) {
                let toml_aliases = parse_package_aliases_from_toml(&content);
                log::debug!(
                    "  Merging {} aliases from: {}",
                    toml_aliases.len(),
                    ancestor.display()
                );
                aliases.extend(toml_aliases);
            }
        }

        log::debug!(
            "  Final aliases for {}: {:?}",
            canonical_dir.display(),
            aliases.keys().collect::<Vec<_>>()
        );

        self.alias_cache
            .write()
            .unwrap()
            .entry(canonical_dir)
            .or_insert_with(|| Arc::new(aliases))
            .clone()
    }

    /// Get all files that have been resolved through this resolver
    pub fn get_tracked_files(&self) -> HashSet<PathBuf> {
        let mut files = self
            .path_to_spec
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        files.extend(self.tracked_local_files.lock().unwrap().iter().cloned());
        files
    }

    /// Get the LoadSpec for a specific resolved file path
    pub fn get_load_spec_for_path(&self, path: &Path) -> Option<LoadSpec> {
        self.path_to_spec.lock().unwrap().get(path).cloned()
    }

    /// Manually track a file (useful for entry points)
    pub fn track_file(&self, path: PathBuf) {
        self.tracked_local_files.lock().unwrap().insert(path);
    }
}

impl LoadResolver for CoreLoadResolver {
    fn resolve_spec(
        &self,
        file_provider: &dyn FileProvider,
        spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        // Check if the current file is a cached remote file
        let current_file_spec = self.path_to_spec.lock().unwrap().get(current_file).cloned();

        // If we're resolving from a remote file, we need to handle relative and workspace paths specially
        if let Some(remote_spec) = current_file_spec {
            match spec {
                LoadSpec::Path { path } if !path.is_absolute() => {
                    // Relative path from a remote file - resolve it relative to the remote spec
                    match &remote_spec {
                        LoadSpec::Github {
                            user,
                            repo,
                            rev,
                            path: remote_path,
                        } => {
                            // Get the directory of the remote file
                            let remote_dir = remote_path.parent().unwrap_or(Path::new(""));
                            // Join with the relative path and normalize
                            let new_path = normalize_path(&remote_dir.join(path));
                            // Create a new GitHub spec with the resolved path
                            let new_spec = LoadSpec::Github {
                                user: user.clone(),
                                repo: repo.clone(),
                                rev: rev.clone(),
                                path: new_path,
                            };
                            // Recursively resolve this new spec
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        LoadSpec::Gitlab {
                            project_path,
                            rev,
                            path: remote_path,
                        } => {
                            let remote_dir = remote_path.parent().unwrap_or(Path::new(""));
                            let new_path = normalize_path(&remote_dir.join(path));
                            let new_spec = LoadSpec::Gitlab {
                                project_path: project_path.clone(),
                                rev: rev.clone(),
                                path: new_path,
                            };
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        LoadSpec::Package {
                            package,
                            tag,
                            path: remote_path,
                        } => {
                            let remote_dir = remote_path.parent().unwrap_or(Path::new(""));
                            let new_path = normalize_path(&remote_dir.join(path));
                            let new_spec = LoadSpec::Package {
                                package: package.clone(),
                                tag: tag.clone(),
                                path: new_path,
                            };
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        _ => {
                            // For other types, fall through to normal handling
                        }
                    }
                }
                LoadSpec::WorkspacePath { path } => {
                    // Workspace path from a remote file - resolve it relative to the remote root
                    match &remote_spec {
                        LoadSpec::Github {
                            user, repo, rev, ..
                        } => {
                            let new_spec = LoadSpec::Github {
                                user: user.clone(),
                                repo: repo.clone(),
                                rev: rev.clone(),
                                path: path.clone(),
                            };
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        LoadSpec::Gitlab {
                            project_path, rev, ..
                        } => {
                            let new_spec = LoadSpec::Gitlab {
                                project_path: project_path.clone(),
                                rev: rev.clone(),
                                path: path.clone(),
                            };
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        LoadSpec::Package { package, tag, .. } => {
                            let new_spec = LoadSpec::Package {
                                package: package.clone(),
                                tag: tag.clone(),
                                path: path.clone(),
                            };
                            return self.resolve_spec(file_provider, &new_spec, current_file);
                        }
                        _ => {
                            // For other types, fall through to normal handling
                        }
                    }
                }
                _ => {
                    // Other spec types proceed normally
                }
            }
        }

        // First, resolve any package aliases
        let (resolved_spec, is_from_alias) = if let LoadSpec::Package { .. } = spec {
            // Always use hierarchical alias resolution - works for workspace OR remote repo
            let aliases = self.get_aliases_for_file(current_file);
            log::debug!(
                "Resolving package spec: {} from file: {}",
                spec.to_load_string(),
                current_file.display()
            );
            let resolved = spec.resolve(Some(&aliases))?;
            if resolved != *spec {
                log::debug!(
                    "  Package alias resolved: {} -> {}",
                    spec.to_load_string(),
                    resolved.to_load_string()
                );
            }
            // Check if the resolution changed the spec type (indicating it was an alias)
            let from_alias = !matches!(&resolved, LoadSpec::Package { .. });
            (resolved, from_alias)
        } else {
            (spec.clone(), false)
        };

        match &resolved_spec {
            // Remote specs need to be fetched
            LoadSpec::Package { .. } | LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => {
                // First try vendor directory if available
                if self.use_vendor_dir {
                    if let Ok(vendor_path) = self.try_resolve_from_vendor(&resolved_spec) {
                        return Ok(vendor_path);
                    }
                }

                let resolved_path = self
                    .remote_fetcher
                    .fetch_remote(&resolved_spec, &self.workspace_root)?;

                let canonical_resolved_path = file_provider.canonicalize(&resolved_path)?;

                // Store the mapping from resolved path to original spec
                self.path_to_spec
                    .lock()
                    .unwrap()
                    .insert(canonical_resolved_path.clone(), resolved_spec.clone());

                Ok(canonical_resolved_path)
            }

            // Workspace-relative paths (starts with //)
            LoadSpec::WorkspacePath { path } => {
                let canonical_root = file_provider.canonicalize(&self.workspace_root)?;
                let resolved_path = canonical_root.join(path);

                // Canonicalize the resolved path to handle .. and symlinks
                let canonical_path = file_provider.canonicalize(&resolved_path)?;

                if file_provider.exists(&canonical_path) {
                    // Track local file
                    self.tracked_local_files
                        .lock()
                        .unwrap()
                        .insert(canonical_path.clone());
                    Ok(canonical_path)
                } else {
                    Err(anyhow::anyhow!(
                        "File not found: {}",
                        canonical_path.display()
                    ))
                }
            }

            // Regular paths (relative or absolute)
            LoadSpec::Path { path } => {
                if path.is_absolute() {
                    // Absolute paths are used as-is
                    let canonical_path = file_provider.canonicalize(path)?;

                    if file_provider.exists(&canonical_path) {
                        // Track local file
                        self.tracked_local_files
                            .lock()
                            .unwrap()
                            .insert(canonical_path.clone());
                        Ok(canonical_path)
                    } else {
                        Err(anyhow::anyhow!(
                            "File not found: {}",
                            canonical_path.display()
                        ))
                    }
                } else if is_from_alias {
                    // If this path came from an alias resolution, treat it as workspace-relative
                    let canonical_root = file_provider.canonicalize(&self.workspace_root)?;
                    let resolved_path = canonical_root.join(path);

                    // Canonicalize the resolved path to handle .. and symlinks
                    let canonical_path = file_provider.canonicalize(&resolved_path)?;

                    if file_provider.exists(&canonical_path) {
                        // Track local file
                        self.tracked_local_files
                            .lock()
                            .unwrap()
                            .insert(canonical_path.clone());
                        Ok(canonical_path)
                    } else {
                        Err(anyhow::anyhow!(
                            "File not found: {}",
                            canonical_path.display()
                        ))
                    }
                } else {
                    // Regular relative paths are resolved from the current file's directory
                    let current_dir = current_file
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("Current file has no parent directory"))?;

                    let resolved_path = current_dir.join(path);

                    // Canonicalize the resolved path to handle .. and symlinks
                    let canonical_path = file_provider.canonicalize(&resolved_path)?;

                    if file_provider.exists(&canonical_path) {
                        // Track local file
                        self.tracked_local_files
                            .lock()
                            .unwrap()
                            .insert(canonical_path.clone());
                        Ok(canonical_path)
                    } else {
                        Err(anyhow::anyhow!(
                            "File not found: {}",
                            canonical_path.display()
                        ))
                    }
                }
            }
        }
    }
}
