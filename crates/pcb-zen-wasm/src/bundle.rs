use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use log::warn;
use pcb_zen_core::bundle::{Bundle, BundleManifest};
use pcb_zen_core::InMemoryFileProvider;

/// A loaded bundle that can be evaluated multiple times with different inputs
pub struct LoadedBundle {
    pub bundle: Bundle,
    pub file_provider: Arc<InMemoryFileProvider>,
}

impl LoadedBundle {
    /// Load a bundle from zip bytes
    pub fn from_zip_bytes(bytes: &[u8]) -> Result<Self> {
        use zip::ZipArchive;

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = ZipArchive::new(cursor).context("Failed to read zip archive")?;

        // First, read the manifest
        let manifest_contents = {
            let mut manifest_file = archive
                .by_name("bundle.toml")
                .context("Missing bundle.toml in archive")?;

            let mut contents = String::new();
            manifest_file
                .read_to_string(&mut contents)
                .context("Failed to read bundle.toml")?;
            contents
        };

        let manifest: BundleManifest =
            toml::from_str(&manifest_contents).context("Failed to parse bundle manifest")?;

        // Now extract all files into an InMemoryFileProvider
        let mut files = HashMap::new();

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name().to_string();

            // Skip the manifest file
            if name == "bundle.toml" {
                continue;
            }

            // Try to read file contents as UTF-8
            let mut contents = String::new();
            match file.read_to_string(&mut contents) {
                Ok(_) => {
                    // Successfully read as text, store with absolute path
                    files.insert(format!("/{name}"), contents);
                }
                Err(e) => {
                    // Failed to read as UTF-8, skip the file
                    warn!("Skipping non-text file '{name}' in bundle: {e}");
                }
            }
        }

        let file_provider = Arc::new(InMemoryFileProvider::new(files));

        // Create bundle with root path "/"
        let bundle = Bundle::new(PathBuf::from("/"), manifest);

        Ok(LoadedBundle {
            bundle,
            file_provider,
        })
    }

    /// Get the entry point path for this bundle
    pub fn entry_point(&self) -> &std::path::Path {
        &self.bundle.manifest.entry_point
    }
}
