use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{FileProvider, LoadResolver};

/// Runtime representation of a bundle with additional metadata
#[derive(Debug, Clone)]
pub struct Bundle {
    /// Path to the bundle directory (where files are stored)
    pub bundle_path: PathBuf,

    /// The serializable manifest
    pub manifest: BundleManifest,
}

/// A self-contained bundle manifest that can be serialized
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleManifest {
    /// Entry point path within the bundle (e.g., "main.zen")
    pub entry_point: PathBuf,

    /// Map from file_path to a map of load_spec -> resolved_bundle_path
    pub load_map: HashMap<String, HashMap<String, String>>,

    /// Optional metadata
    pub metadata: BundleMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleMetadata {
    pub created_at: String,
    pub version: String,
    pub description: Option<String>,
    pub build_config: Option<HashMap<String, String>>,
}

impl Default for BundleMetadata {
    fn default() -> Self {
        Self {
            created_at: chrono::Utc::now().to_rfc3339(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: None,
            build_config: None,
        }
    }
}

impl Bundle {
    /// Create a new Bundle with the given path and manifest
    pub fn new(bundle_path: PathBuf, manifest: BundleManifest) -> Self {
        Self {
            bundle_path,
            manifest,
        }
    }

    /// Create a Bundle with an empty manifest
    pub fn empty(bundle_path: PathBuf) -> Self {
        Self {
            bundle_path,
            manifest: BundleManifest::default(),
        }
    }
}

/// A LoadResolver that uses a Bundle's load map
pub struct BundleLoadResolver {
    bundle: Bundle,
}

impl BundleLoadResolver {
    pub fn new(bundle: Bundle) -> Self {
        Self { bundle }
    }
}

impl LoadResolver for BundleLoadResolver {
    fn resolve_path(
        &self,
        file_provider: &dyn FileProvider,
        load_path: &str,
        current_file: &Path,
    ) -> Result<PathBuf> {
        let canonical_current_file = file_provider.canonicalize(current_file)?;
        let stripped_current_file = canonical_current_file
            .strip_prefix(&self.bundle.bundle_path)
            .unwrap_or(canonical_current_file.as_path());

        log::debug!(
            "Resolving path: {} from file: {}",
            load_path,
            stripped_current_file.display()
        );

        let load_map = self
            .bundle
            .manifest
            .load_map
            .get(stripped_current_file.to_string_lossy().as_ref());

        log::debug!("Load map: {load_map:?}");

        if let Some(load_map) = load_map {
            let resolved_path = load_map.get(load_path);
            log::debug!("Load map resolved path: {resolved_path:?}");
            if let Some(resolved_path) = resolved_path {
                return Ok(PathBuf::from(resolved_path));
            }
        }

        Ok(PathBuf::from(load_path))
    }
}
