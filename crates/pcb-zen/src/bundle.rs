use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use pcb_zen_core::bundle::{Bundle, BundleManifest};
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::{
    CoreLoadResolver, DefaultFileProvider, EvalContext, FileProvider, InputMap, LoadResolver,
};
use zip::write::FileOptions;
use zip::ZipWriter;

use crate::load::DefaultRemoteFetcher;

/// A LoadResolver that wraps another resolver and tracks all resolved files
struct BundleTrackingResolver {
    inner: Arc<dyn LoadResolver>,
    file_provider: Arc<dyn FileProvider>,
    source_dir: PathBuf,
    bundle_dir: PathBuf,
    tracked_files: Arc<Mutex<HashMap<PathBuf, PathBuf>>>, // canonical_path -> bundle_path
    load_map: Arc<Mutex<HashMap<String, HashMap<String, String>>>>, // file -> load_spec -> resolved_path
}

impl BundleTrackingResolver {
    fn new(
        inner: Arc<dyn LoadResolver>,
        file_provider: Arc<dyn FileProvider>,
        source_dir: PathBuf,
        bundle_dir: PathBuf,
    ) -> Self {
        Self {
            inner,
            file_provider,
            source_dir,
            bundle_dir,
            tracked_files: Arc::new(Mutex::new(HashMap::new())),
            load_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Convert to a Bundle after evaluation
    fn into_bundle(self, entry_point: PathBuf) -> Bundle {
        let load_map = match Arc::try_unwrap(self.load_map) {
            Ok(mutex) => mutex.into_inner().unwrap(),
            Err(arc) => arc.lock().unwrap().clone(),
        };

        let manifest = BundleManifest {
            entry_point,
            load_map,
            metadata: Default::default(),
        };

        Bundle::new(self.bundle_dir, manifest)
    }

    /// Copy a file or directory to the bundle, returning the relative path within the bundle
    fn copy_to_bundle(&self, source_path: &Path) -> Result<PathBuf> {
        let canonical_source = self.file_provider.canonicalize(source_path)?;

        // Check if we've already copied this file
        let mut tracked = self.tracked_files.lock().unwrap();
        if let Some(bundle_path) = tracked.get(&canonical_source) {
            return Ok(bundle_path.clone());
        }

        // Determine where to place this file in the bundle
        let bundle_path = if canonical_source.starts_with(&self.source_dir) {
            // File is within the source directory - preserve relative path
            canonical_source
                .strip_prefix(&self.source_dir)
                .unwrap()
                .to_path_buf()
        } else {
            // External dependency - place in deps folder
            let file_name = canonical_source
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?
                .to_string_lossy();

            // Use a hash of the full path to ensure uniqueness
            let hash = format!(
                "{:x}",
                md5::compute(canonical_source.to_string_lossy().as_bytes())
            );

            // Create filename with hash prefix
            let hashed_filename = format!("{}_{}", &hash[..8], file_name);

            // Create path in deps folder
            PathBuf::from("deps").join(hashed_filename)
        };

        // Create the target directory
        let full_bundle_path = self.bundle_dir.join(&bundle_path);
        if let Some(parent) = full_bundle_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Copy the file
        if self.file_provider.is_directory(&canonical_source) {
            // For directories, we need to copy recursively
            copy_dir_all(&canonical_source, &full_bundle_path)?;
        } else {
            fs::copy(&canonical_source, &full_bundle_path).with_context(|| {
                format!("Failed to copy {} to bundle", canonical_source.display())
            })?;
        }

        tracked.insert(canonical_source, bundle_path.clone());
        Ok(bundle_path)
    }
}

impl LoadResolver for BundleTrackingResolver {
    fn resolve_spec(
        &self,
        file_provider: &dyn FileProvider,
        spec: &pcb_zen_core::LoadSpec,
        current_file: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        // First resolve using the inner resolver
        let resolved_path = self.inner.resolve_spec(file_provider, spec, current_file)?;

        // Copy the resolved file to the bundle
        let bundle_path = self.copy_to_bundle(&resolved_path)?;

        // Also ensure the current file is copied to bundle if it's not already
        let current_file_bundle_path = self.copy_to_bundle(current_file)?;

        // Track the load mapping
        {
            let mut load_map = self.load_map.lock().unwrap();

            // Use the bundle-relative path of the current file as the key
            // Always use forward slashes for cross-platform compatibility
            let current_file_key = current_file_bundle_path
                .to_string_lossy()
                .replace('\\', "/");

            load_map.entry(current_file_key).or_default().insert(
                spec.to_load_string(),
                bundle_path.to_string_lossy().replace('\\', "/"),
            );
        }

        // Return the original resolved path (not the bundle path)
        Ok(resolved_path)
    }
}

/// Copy a directory recursively
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if file_type.is_symlink() {
            // Resolve the symlink
            match fs::read_link(&src_path) {
                Ok(target) => {
                    // Get the absolute path of the symlink target
                    let resolved_target = if target.is_absolute() {
                        target
                    } else {
                        src_path.parent().unwrap().join(target)
                    };

                    // Check what the symlink points to
                    if resolved_target.is_dir() {
                        // Symlink to directory - copy the directory contents
                        copy_dir_all(&resolved_target, &dst_path)?;
                    } else if resolved_target.is_file() {
                        // Symlink to file - copy the file
                        fs::copy(&resolved_target, &dst_path).with_context(|| {
                            format!(
                                "Failed to copy symlink target {} to {}",
                                resolved_target.display(),
                                dst_path.display()
                            )
                        })?;
                    }
                }
                Err(e) => {
                    log::warn!("Failed to read symlink {}: {}", src_path.display(), e);
                    // Skip broken symlinks
                }
            }
        } else if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
        } else {
            // Skip special files (sockets, device files, etc.)
            log::debug!("Skipping special file: {}", src_path.display());
        }
    }

    Ok(())
}

/// Create a bundle from a Starlark file, discovering all dependencies
pub fn create_bundle(input_path: &Path, output_path: &Path) -> Result<()> {
    // Get the canonical path of the input file
    let canonical_input = input_path
        .canonicalize()
        .context("Failed to canonicalize input path")?;

    // Determine the source directory (parent of the input file)
    let source_dir = canonical_input
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Input file has no parent directory"))?
        .to_path_buf();

    // Create the file provider
    let file_provider = Arc::new(DefaultFileProvider);

    // Find the workspace root by looking for pcb.toml, fall back to source dir if not found
    let workspace_root = find_workspace_root(file_provider.as_ref(), &canonical_input);

    // Create a temporary directory for the bundle
    let temp_dir = tempfile::tempdir()?;
    let bundle_dir = temp_dir.path().to_path_buf();

    // Copy the entire source directory to the bundle
    copy_dir_all(&source_dir, &bundle_dir).with_context(|| {
        format!(
            "Failed to copy source directory {} to bundle",
            source_dir.display()
        )
    })?;

    // Calculate the entry point relative to the source directory
    let entry_point = canonical_input
        .strip_prefix(&source_dir)
        .unwrap()
        .to_path_buf();

    // Create the base load resolver with the correct workspace root
    let remote_fetcher = Arc::new(DefaultRemoteFetcher);
    let base_resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher,
        workspace_root,
        true,
    ));

    // Create a tracking resolver that wraps the base resolver
    let tracking_resolver = Arc::new(BundleTrackingResolver::new(
        base_resolver,
        file_provider.clone(),
        source_dir.clone(),
        bundle_dir.clone(),
    ));

    // Create evaluation context with the tracking resolver
    let eval_context = EvalContext::new()
        .set_file_provider(file_provider.clone())
        .set_load_resolver(tracking_resolver.clone())
        .set_source_path(canonical_input.clone())
        .set_inputs(InputMap::new());

    // Evaluate the input file to discover all dependencies
    let eval_result = eval_context.eval();

    // Check for errors
    if !eval_result.is_success() {
        let errors: Vec<String> = eval_result
            .diagnostics
            .iter()
            .filter(|d| d.is_error())
            .map(|d| d.to_string())
            .collect();
        anyhow::bail!("Evaluation failed with errors:\n{}", errors.join("\n"));
    }

    // Convert the tracking resolver to a bundle
    let bundle = Arc::try_unwrap(tracking_resolver)
        .map_err(|_| anyhow::anyhow!("Failed to unwrap tracking resolver Arc"))?
        .into_bundle(entry_point);

    // Write the bundle to a zip file
    write_bundle_zip(&bundle, &bundle_dir, output_path)?;

    Ok(())
}

/// Write a bundle to a zip archive
pub fn write_bundle_zip(bundle: &Bundle, bundle_dir: &Path, output_path: &Path) -> Result<()> {
    let file = File::create(output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
    let mut zip = ZipWriter::new(file);

    // Write the bundle manifest
    let manifest_toml =
        toml::to_string(&bundle.manifest).context("Failed to serialize bundle manifest")?;
    zip.start_file("bundle.toml", FileOptions::<()>::default())
        .context("Failed to start writing bundle.toml")?;
    zip.write_all(manifest_toml.as_bytes())
        .context("Failed to write bundle.toml")?;

    // Add all files from the bundle directory
    add_directory_to_zip(&mut zip, bundle_dir, bundle_dir)?;

    zip.finish().context("Failed to finalize zip file")?;
    Ok(())
}

/// Recursively add a directory to a zip archive
fn add_directory_to_zip<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    base_dir: &Path,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative_path = path.strip_prefix(base_dir)?;

        if path.is_dir() {
            // Recursively add subdirectory
            add_directory_to_zip(zip, &path, base_dir)?;
        } else {
            // Add file to zip
            // Always use forward slashes in ZIP archives (ZIP standard)
            let file_name = relative_path.to_string_lossy().replace('\\', "/");
            zip.start_file(file_name, FileOptions::<()>::default())?;

            let mut file = File::open(&path)?;
            std::io::copy(&mut file, zip)?;
        }
    }

    Ok(())
}
