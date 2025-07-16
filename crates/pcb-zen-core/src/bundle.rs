use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{FileProvider, LoadResolver, LoadSpec};

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
    fn resolve_spec(
        &self,
        _file_provider: &dyn FileProvider,
        spec: &LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf> {
        // Convert current_file to a string key for the load map
        let current_file_str = current_file.to_string_lossy().to_string();

        // Convert the spec to a load string to use as the inner key
        let load_spec_str = spec.to_load_string();

        // Look up in the bundle's load map
        if let Some(file_map) = self.bundle.manifest.load_map.get(&current_file_str) {
            if let Some(resolved_path_str) = file_map.get(&load_spec_str) {
                // The resolved path is relative to the bundle directory
                let resolved_path = self.bundle.bundle_path.join(resolved_path_str);
                return Ok(resolved_path);
            }
        }

        // If not found in the load map, return an error
        Err(anyhow::anyhow!(
            "Load spec '{}' from file '{}' not found in bundle manifest",
            load_spec_str,
            current_file_str
        ))
    }
}
