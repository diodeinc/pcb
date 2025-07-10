//! # pcb-buildifier
//!
//! This crate bundles the buildifier binary from the bazelbuild/buildtools project.
//!
//! ## License Notice
//!
//! This crate includes a bundled buildifier binary which is part of the buildtools project
//! by the Bazel Authors, licensed under the Apache License, Version 2.0.
//! See the LICENSE file in this crate's directory for the full license text.
//!
//! The buildtools project can be found at: https://github.com/bazelbuild/buildtools

use anyhow::{Context, Result};
use once_cell::sync::OnceCell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

// Include the buildifier binary at compile time
const BUILDIFIER_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/buildifier"));

// Version of buildifier bundled in this crate (used for cache invalidation)
const BUILDIFIER_VERSION: &str = "7.3.1";

// Global cache for the buildifier binary path
static CACHED_BINARY_PATH: OnceCell<Mutex<Option<PathBuf>>> = OnceCell::new();

/// Get or create the cached buildifier binary path
fn get_cached_binary_path() -> Result<PathBuf> {
    let mutex = CACHED_BINARY_PATH.get_or_init(|| Mutex::new(None));
    let mut cache = mutex.lock().unwrap();

    if let Some(path) = cache.as_ref() {
        // Verify the cached binary still exists
        if path.exists() {
            return Ok(path.clone());
        }
    }

    // Determine cache directory
    let cache_dir = dirs::cache_dir()
        .context("Failed to determine cache directory")?
        .join("pcb")
        .join("buildifier")
        .join(format!("v{BUILDIFIER_VERSION}"));

    // Create cache directory if it doesn't exist
    fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

    // Determine the binary name based on the platform
    let binary_name = if cfg!(windows) {
        "buildifier.exe"
    } else {
        "buildifier"
    };

    let binary_path = cache_dir.join(binary_name);

    // Check if binary already exists in cache
    if !binary_path.exists() {
        log::info!("Extracting buildifier to cache: {}", binary_path.display());

        // Write the binary to the cache directory
        let mut file =
            fs::File::create(&binary_path).context("Failed to create buildifier binary file")?;
        file.write_all(BUILDIFIER_BINARY)
            .context("Failed to write buildifier binary")?;

        // Make it executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&binary_path, perms)
                .context("Failed to set executable permissions")?;
        }
    } else {
        log::debug!("Using cached buildifier: {}", binary_path.display());
    }

    // Update cache
    *cache = Some(binary_path.clone());

    Ok(binary_path)
}

/// Represents a buildifier instance that can format Starlark files
pub struct Buildifier {
    binary_path: PathBuf,
}

impl Buildifier {
    /// Create a new Buildifier instance using the cached binary
    pub fn new() -> Result<Self> {
        let binary_path = get_cached_binary_path()?;
        Ok(Self { binary_path })
    }

    /// Get the path to the buildifier binary
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    /// Run buildifier with the given arguments
    pub fn run(&self, args: &[String]) -> Result<Output> {
        Command::new(&self.binary_path)
            .args(args)
            .output()
            .context("Failed to execute buildifier")
    }

    /// Run buildifier and capture stdout/stderr separately
    pub fn run_with_io(&self, args: &[String]) -> Result<Output> {
        Command::new(&self.binary_path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to execute buildifier")
    }

    /// Check if a file needs formatting (returns true if already formatted)
    pub fn check_file(&self, file_path: &Path) -> Result<bool> {
        let output = self.run(&[
            "--mode=check".to_string(),
            file_path.to_string_lossy().to_string(),
        ])?;

        // Exit code 0 means file is already formatted
        // Exit code 4 means file needs formatting
        match output.status.code() {
            Some(0) => Ok(true),
            Some(4) => Ok(false),
            Some(code) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Buildifier check failed with code {}: {}", code, stderr)
            }
            None => anyhow::bail!("Buildifier was terminated by signal"),
        }
    }

    /// Format a file in place
    pub fn format_file(&self, file_path: &Path) -> Result<()> {
        let output = self.run(&[
            "--mode=fix".to_string(),
            file_path.to_string_lossy().to_string(),
        ])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to format file: {stderr}");
        }

        Ok(())
    }

    /// Get the diff that would be applied to format a file
    pub fn diff_file(&self, file_path: &Path) -> Result<String> {
        let output = self.run_with_io(&[
            "--mode=diff".to_string(),
            file_path.to_string_lossy().to_string(),
        ])?;

        if output.status.code() == Some(0) || output.status.code() == Some(4) {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to get diff: {}", stderr);
        }
    }

    /// Get the buildifier version
    pub fn version(&self) -> Result<String> {
        let output = self.run_with_io(&["--version".to_string()])?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            anyhow::bail!("Failed to get buildifier version");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buildifier_creation() {
        let buildifier = Buildifier::new().unwrap();
        assert!(buildifier.binary_path().exists());
    }

    #[test]
    fn test_buildifier_version() {
        let buildifier = Buildifier::new().unwrap();
        let version = buildifier.version().unwrap();
        assert!(version.contains("buildifier"));
    }

    #[test]
    fn test_caching() {
        // Create two instances and verify they use the same cached binary
        let buildifier1 = Buildifier::new().unwrap();
        let path1 = buildifier1.binary_path().to_path_buf();

        let buildifier2 = Buildifier::new().unwrap();
        let path2 = buildifier2.binary_path().to_path_buf();

        // Both should point to the same cached binary
        assert_eq!(path1, path2);
    }
}
