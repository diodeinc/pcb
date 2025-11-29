//! Content-addressed store for vendored assets.
//!
//! Vendored assets are stored as `.tar.zst` archives in the vendor directory.
//! When resolved, they are unpacked to a content-addressed store at `~/.pcb/store/{content_hash}/`.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use tar::Archive;
use tempfile::TempDir;

use crate::canonical::{compute_content_hash_from_dir, create_canonical_tar};

const ZSTD_COMPRESSION_LEVEL: i32 = 10;

/// Get the base path for the content-addressed store: `~/.pcb/store/`
pub fn store_base() -> PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".pcb")
        .join("store")
}

/// Convert a content hash to a filesystem-safe directory name.
///
/// The h1: prefix uses standard base64 which contains `/` and `+` characters.
/// We re-encode to URL-safe base64 (uses `-` and `_` instead, no padding).
fn hash_to_dirname(content_hash: &str) -> String {
    let hash_data = content_hash
        .strip_prefix("h1:")
        .expect("hash must have h1: prefix");
    let bytes = STANDARD
        .decode(hash_data)
        .expect("hash must be valid base64");
    format!("h1:{}", URL_SAFE_NO_PAD.encode(&bytes))
}

/// Create a compressed `.tar.zst` archive from a source directory.
///
/// Uses canonical tar format (deterministic, sorted entries) and zstd level 10 compression.
pub fn create_asset_archive(source_dir: &Path, dest_path: &Path) -> Result<()> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(dest_path)
        .with_context(|| format!("Failed to create archive: {}", dest_path.display()))?;
    let writer = BufWriter::new(file);
    let encoder = zstd::Encoder::new(writer, ZSTD_COMPRESSION_LEVEL)
        .context("Failed to create zstd encoder")?;

    create_canonical_tar(source_dir, encoder.auto_finish())?;

    Ok(())
}

/// Unpack a vendored `.tar.zst` archive to the content-addressed store.
///
/// Returns the path to the unpacked directory: `~/.pcb/store/{content_hash}/`
///
/// Uses atomic directory rename to handle concurrent unpacking:
/// 1. Unpack to a temporary directory
/// 2. Verify content hash matches expected
/// 3. Rename to final location
///
/// If the store directory already exists, returns immediately (idempotent).
pub fn unpack_vendored_asset(archive_path: &Path, expected_hash: &str) -> Result<PathBuf> {
    let store_dir = store_base().join(hash_to_dirname(expected_hash));

    if store_dir.exists() {
        return Ok(store_dir);
    }

    fs::create_dir_all(store_base())?;

    let temp_dir =
        TempDir::new_in(store_base()).context("Failed to create temp directory for unpacking")?;

    let file = File::open(archive_path)
        .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
    let reader = BufReader::new(file);
    let decoder = zstd::Decoder::new(reader).context("Failed to create zstd decoder")?;

    let mut archive = Archive::new(decoder);
    archive
        .unpack(temp_dir.path())
        .with_context(|| format!("Failed to unpack archive: {}", archive_path.display()))?;

    let actual_hash = compute_content_hash_from_dir(temp_dir.path())
        .context("Failed to compute hash of unpacked content")?;

    if actual_hash != expected_hash {
        anyhow::bail!(
            "Vendored asset hash mismatch\n  \
            Archive: {}\n  \
            Expected: {}\n  \
            Got: {}",
            archive_path.display(),
            expected_hash,
            actual_hash
        );
    }

    match fs::rename(temp_dir.path(), &store_dir) {
        Ok(()) => {
            std::mem::forget(temp_dir);
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Another process beat us to it - that's fine
        }
        Err(e) if store_dir.exists() => {
            // Race condition: directory was created between our check and rename
            drop(e);
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "Failed to move unpacked content to store: {}",
                    store_dir.display()
                )
            });
        }
    }

    Ok(store_dir)
}

/// Get the vendor archive path for an asset.
///
/// Returns `vendor/{module_path}/{ref}.tar.zst`
pub fn vendor_archive_path(workspace_root: &Path, module_path: &str, ref_str: &str) -> PathBuf {
    workspace_root
        .join("vendor")
        .join(module_path)
        .join(format!("{}.tar.zst", ref_str))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_create_and_unpack_archive() -> Result<()> {
        let source = tempdir()?;
        fs::write(source.path().join("test.txt"), "hello world")?;
        fs::create_dir(source.path().join("subdir"))?;
        fs::write(source.path().join("subdir").join("nested.txt"), "nested")?;

        let archive_dir = tempdir()?;
        let archive_path = archive_dir.path().join("test.tar.zst");

        create_asset_archive(source.path(), &archive_path)?;
        assert!(archive_path.exists());

        let expected_hash = compute_content_hash_from_dir(source.path())?;

        let store = tempdir()?;
        let store_path = store.path().join(&expected_hash);

        // Manually set store base for test (we can't easily override store_base())
        fs::create_dir_all(&store_path.parent().unwrap())?;

        let file = File::open(&archive_path)?;
        let reader = BufReader::new(file);
        let decoder = zstd::Decoder::new(reader)?;
        let mut archive = Archive::new(decoder);
        archive.unpack(&store_path)?;

        assert!(store_path.join("test.txt").exists());
        assert!(store_path.join("subdir").join("nested.txt").exists());

        let unpacked_hash = compute_content_hash_from_dir(&store_path)?;
        assert_eq!(expected_hash, unpacked_hash);

        Ok(())
    }
}
