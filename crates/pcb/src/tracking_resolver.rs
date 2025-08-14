use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use pcb_zen_core::{FileProvider, LoadResolver, LoadSpec};

/// Wraps another resolver and remembers every file it resolves.
pub struct TrackingLoadResolver {
    inner: Arc<dyn LoadResolver>,
    file_provider: Arc<dyn FileProvider>,
    loaded: Arc<Mutex<HashSet<PathBuf>>>,
    load_specs: Arc<Mutex<HashMap<PathBuf, LoadSpec>>>,
}

impl TrackingLoadResolver {
    pub fn new(inner: Arc<dyn LoadResolver>, file_provider: Arc<dyn FileProvider>) -> Self {
        Self {
            inner,
            file_provider,
            loaded: Arc::new(Mutex::new(HashSet::new())),
            load_specs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert something manually (e.g. the entrypoint itself)
    pub fn track(&self, p: PathBuf) {
        self.loaded.lock().unwrap().insert(p);
    }

    /// Return a copy of everything that was tracked.
    pub fn files(&self) -> HashSet<PathBuf> {
        self.loaded.lock().unwrap().clone()
    }

    /// Return the LoadSpec for a given resolved path
    pub fn get_load_spec(&self, path: &Path) -> Option<LoadSpec> {
        self.load_specs.lock().unwrap().get(path).cloned()
    }

    /// Derive canonical LoadSpec by expanding relative paths within remote contexts
    fn derive_canonical_spec(&self, spec: &LoadSpec, current_file: &Path) -> LoadSpec {
        match spec {
            // Resolve packages to canonical form first
            LoadSpec::Package { .. } => spec.resolve(None, None).unwrap_or_else(|_| spec.clone()),

            // Already canonical
            LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => spec.clone(),

            // Expand relative paths within remote contexts
            LoadSpec::Path { path } | LoadSpec::WorkspacePath { path } => {
                let current_canonical = self
                    .file_provider
                    .canonicalize(current_file)
                    .unwrap_or_else(|_| current_file.to_path_buf());

                // Look up parent's LoadSpec (should be canonical by now)
                if let Some(parent_spec) = self.load_specs.lock().unwrap().get(&current_canonical) {
                    match parent_spec {
                        LoadSpec::Github {
                            user,
                            repo,
                            rev,
                            path: parent_path,
                        } => {
                            let new_path = if path.is_absolute() {
                                path.clone()
                            } else {
                                parent_path.parent().unwrap_or(Path::new("")).join(path)
                            };
                            LoadSpec::Github {
                                user: user.clone(),
                                repo: repo.clone(),
                                rev: rev.clone(),
                                path: new_path,
                            }
                        }
                        LoadSpec::Gitlab {
                            project_path,
                            rev,
                            path: parent_path,
                        } => {
                            let new_path = if path.is_absolute() {
                                path.clone()
                            } else {
                                parent_path.parent().unwrap_or(Path::new("")).join(path)
                            };
                            LoadSpec::Gitlab {
                                project_path: project_path.clone(),
                                rev: rev.clone(),
                                path: new_path,
                            }
                        }
                        _ => spec.clone(),
                    }
                } else {
                    spec.clone()
                }
            }
        }
    }
}

impl LoadResolver for TrackingLoadResolver {
    fn resolve_spec(
        &self,
        fp: &dyn FileProvider,
        spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf> {
        // Delegate real work to the wrapped resolver
        let resolved = self.inner.resolve_spec(fp, spec, current_file)?;

        // Canonicalise to avoid duplicates caused by "../"
        let canonical = self.file_provider.canonicalize(&resolved)?;
        self.loaded.lock().unwrap().insert(canonical.clone());

        // Store canonical LoadSpec instead of original
        let canonical_spec = self.derive_canonical_spec(spec, current_file);
        self.load_specs
            .lock()
            .unwrap()
            .insert(canonical, canonical_spec);

        Ok(resolved)
    }
}
