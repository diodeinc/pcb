use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use anyhow::Context;

pub mod config;
pub mod convert;
pub mod diagnostics;
mod file_provider;
pub mod lang;
pub mod load_spec;

// Re-export commonly used types
pub use config::{BoardConfig, ModuleConfig, PcbToml, WorkspaceConfig};
pub use diagnostics::{Diagnostic, DiagnosticError, Diagnostics, LoadError, WithDiagnostics};
pub use lang::eval::{EvalContext, EvalOutput};
pub use lang::input::{InputMap, InputValue};
pub use load_spec::LoadSpec;

// Re-export file provider types
pub use file_provider::InMemoryFileProvider;

// Re-export types needed by pcb-zen
pub use lang::component::FrozenComponentValue;
pub use lang::module::FrozenModuleValue;
pub use lang::net::{FrozenNetValue, NetId};

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
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error>;

    /// Lookup metadata for a previously fetched remote ref, if cached.
    fn remote_ref_meta(&self, remote_ref: &RemoteRef) -> Option<RemoteRefMeta>;
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

    fn remote_ref_meta(&self, _remote_ref: &RemoteRef) -> Option<RemoteRefMeta> {
        None
    }
}

/// Abstraction for resolving load() paths to file contents
/// Kind of a resolved Git reference after fetching
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Tag,
    Branch,
    Commit,
    Head,
}

/// Remote reference identifier with structured information
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RemoteRef {
    GitHub {
        user: String,
        repo: String,
        rev: String,
    },
    GitLab {
        project_path: String,
        rev: String,
    },
}

impl RemoteRef {
    /// Get the canonical repository URL for this remote reference
    pub fn repo_url(&self) -> Option<String> {
        match self {
            RemoteRef::GitHub { user, repo, .. } => {
                Some(format!("https://github.com/{user}/{repo}"))
            }
            RemoteRef::GitLab { project_path, .. } => {
                Some(format!("https://gitlab.com/{project_path}"))
            }
        }
    }

    pub fn rev(&self) -> &str {
        match self {
            RemoteRef::GitHub { rev, .. } | RemoteRef::GitLab { rev, .. } => rev,
        }
    }
}

/// Metadata about a resolved remote reference
#[derive(Debug, Clone)]
pub struct RemoteRefMeta {
    /// Full 40-character SHA-1 commit id
    pub commit_sha1: String,
    /// Full SHA-256 commit id when repository uses SHA-256 object format
    pub commit_sha256: Option<String>,
    /// Classification of the ref
    pub kind: RefKind,
}

impl RemoteRefMeta {
    pub fn stable(&self) -> bool {
        matches!(self.kind, RefKind::Tag | RefKind::Commit)
    }
}

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

    /// Return the remote ref for a resolved path, if available.
    fn remote_ref(&self, _path: &Path) -> Option<RemoteRef>;

    /// Return stored metadata for a previously fetched remote ref, if available.
    fn remote_ref_meta(&self, _remote_ref: &RemoteRef) -> Option<RemoteRefMeta>;
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

/// Core load resolver that handles all path resolution logic.
/// This resolver handles workspace paths, relative paths, and delegates
/// remote fetching to a RemoteFetcher implementation.
pub struct CoreLoadResolver {
    file_provider: Arc<dyn FileProvider>,
    remote_fetcher: Arc<dyn RemoteFetcher>,
    workspace_root: PathBuf,
    use_vendor_dir: bool,
    /// Maps resolved paths to their original LoadSpecs
    /// This allows us to resolve relative paths from remote files correctly
    path_to_spec: Arc<Mutex<HashMap<PathBuf, LoadSpec>>>,
    /// Tracks all local files that have been resolved (for vendor/release commands)
    tracked_local_files: Arc<Mutex<HashSet<PathBuf>>>,
    /// Hierarchical alias resolution cache
    alias_cache: RwLock<HashMap<PathBuf, HashMap<String, String>>>,
    /// Workspace root cache by directory path
    workspace_root_cache: RwLock<HashMap<PathBuf, PathBuf>>,
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
            workspace_root_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a CoreLoadResolver for a specific file, automatically finding the workspace root.
    pub fn for_file(
        file_provider: Arc<dyn FileProvider>,
        remote_fetcher: Arc<dyn RemoteFetcher>,
        file: &Path,
        use_vendor_dir: bool,
    ) -> Self {
        let workspace_root = config::find_workspace_root(file_provider.as_ref(), file);
        Self::new(
            file_provider,
            remote_fetcher,
            workspace_root,
            use_vendor_dir,
        )
    }

    /// Get the effective workspace root for a given file, with caching.
    /// This determines the correct workspace context for the file, which may differ
    /// from self.workspace_root when dealing with local aliases or remote dependencies.
    fn get_effective_workspace_root(&self, current_file: &Path) -> Result<PathBuf, anyhow::Error> {
        let canonical_file = self.file_provider.canonicalize(current_file)?;
        let dir = canonical_file.parent().unwrap_or(&canonical_file);

        // Check cache first (optimistic read)
        if let Some(cached) = self.workspace_root_cache.read().unwrap().get(dir) {
            return Ok(cached.clone());
        }

        // Determine workspace root - order matters!
        let vendor_dir = self.workspace_root.join("vendor");
        let workspace_root =
            if let Some(spec) = self.path_to_spec.lock().unwrap().get(&canonical_file) {
                // Remote file - use LoadSpec to walk up to repo root
                let mut root = canonical_file.clone();
                for _ in 0..spec.path().components().count() {
                    root = root.parent().unwrap_or(Path::new("")).to_path_buf();
                }
                root
            } else if canonical_file.starts_with(&self.workspace_root)
                && !canonical_file.starts_with(&vendor_dir)
            {
                // Main workspace (but not vendor)
                self.workspace_root.clone()
            } else {
                // Vendored dependency OR local file outside main workspace
                // Both cases: search for pcb.toml with [workspace]
                config::find_workspace_root(self.file_provider.as_ref(), &canonical_file)
            };

        // Cache result for this directory
        self.workspace_root_cache
            .write()
            .unwrap()
            .insert(dir.to_path_buf(), workspace_root.clone());
        Ok(workspace_root)
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
    fn get_aliases_for_file(&self, file: &Path) -> anyhow::Result<HashMap<String, String>> {
        log::debug!("Resolving aliases for file: {}", file.display());
        // Canonicalize file
        let file = self.file_provider.canonicalize(file)?;
        let dir = file.parent().expect("File must have a parent directory");

        // Check cache first (optimistic read)
        if let Some(cached) = self.alias_cache.read().unwrap().get(dir) {
            log::debug!("  Using cached aliases for directory: {}", dir.display());
            return Ok(cached.clone());
        }

        // Determine alias root using centralized workspace detection
        let alias_root = match self.get_effective_workspace_root(&file) {
            Ok(root) => root,
            Err(_) => {
                log::debug!("  Failed to determine workspace root, using defaults");
                return Ok(LoadSpec::default_package_aliases());
            }
        };

        // Still need spec for path_to_spec mapping below
        let spec = self.path_to_spec.lock().unwrap().get(&file).cloned();

        let pcb_toml_files = file
            .ancestors()
            .take_while(|p| p.starts_with(&alias_root))
            .map(|p| p.join("pcb.toml"))
            .filter(|p| self.file_provider.exists(p))
            .collect::<Vec<_>>();

        // Add all discovered pcb.toml files to path_to_spec mapping
        if let Some(spec) = &spec {
            let pcb_toml_specs = pcb_toml_files
                .iter()
                .cloned()
                .map(|p| {
                    // get pcb.toml path relative to alias root
                    let rel_pcb_toml_path = p.strip_prefix(&alias_root).unwrap().to_path_buf();
                    (p, spec.with_path(rel_pcb_toml_path))
                })
                .collect::<HashMap<PathBuf, LoadSpec>>();
            self.path_to_spec.lock().unwrap().extend(pcb_toml_specs);
        } else {
            // If no spec, then the pcb.toml files are local. So, add it to the tracked local files
            // TODO: remove after tracked_local_files, path_to_spec unification
            pcb_toml_files.iter().for_each(|p| {
                self.tracked_local_files.lock().unwrap().insert(p.clone());
            });
        }

        // Iterate in reverse to prioritize the deepest (closest to leaf) pcb.toml files
        let aliases = pcb_toml_files
            .into_iter()
            .map(|p| {
                let content = self.file_provider.read_file(&p)?;
                let toml_aliases = config::PcbToml::parse(&content)?.packages;
                Ok::<_, anyhow::Error>(toml_aliases)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .rev()
            .fold(LoadSpec::default_package_aliases(), |mut acc, aliases| {
                acc.extend(aliases);
                acc
            });

        log::debug!("Inserting aliases for dir: {}", dir.display());
        self.alias_cache
            .write()
            .unwrap()
            .insert(dir.to_path_buf(), aliases.clone());

        log::debug!(
            "Final aliases for {}: {:?}",
            dir.display(),
            aliases.keys().collect::<Vec<_>>()
        );
        Ok(aliases)
    }

    /// Get all files that have been resolved through this resolver
    pub fn get_tracked_files(&self) -> HashSet<PathBuf> {
        let mut files = self
            .path_to_spec
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.is_file())
            .cloned()
            .collect::<HashSet<_>>();
        files.extend(self.tracked_local_files.lock().unwrap().iter().cloned());
        files
    }

    /// Get only local files (not from cache/remote dependencies)
    pub fn get_tracked_local_files(&self) -> HashSet<PathBuf> {
        self.tracked_local_files.lock().unwrap().clone()
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
        // Compute the effective workspace root for this file at the top
        let effective_workspace_root = self.get_effective_workspace_root(current_file)?;
        // Check if the current file is a cached remote file
        let current_file_spec = self.get_load_spec_for_path(current_file);

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
            let aliases = self.get_aliases_for_file(current_file)?;
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
                let canonical_root = file_provider.canonicalize(&effective_workspace_root)?;
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

    fn remote_ref(&self, path: &Path) -> Option<RemoteRef> {
        self.get_load_spec_for_path(path)
            .and_then(|s| s.remote_ref())
    }

    fn remote_ref_meta(&self, remote_ref: &RemoteRef) -> Option<RemoteRefMeta> {
        self.remote_fetcher.remote_ref_meta(remote_ref)
    }
}
