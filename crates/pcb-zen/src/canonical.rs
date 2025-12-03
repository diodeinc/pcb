//! Canonical tar archive and content hashing utilities.
//!
//! This module implements deterministic tar archives and BLAKE3 content hashing
//! for package integrity verification.
//!
//! ## Canonicalization Rules
//!
//! To ensure byte-identical archives across platforms:
//! - Paths are normalized to NFC Unicode form (macOS uses NFD, Linux uses NFC)
//! - Paths use forward slashes and are sorted by byte value (not path components)
//! - Metadata is normalized: mtime=0, uid=0, gid=0, mode=0644, empty user/group names
//! - Only regular files are included (directories are implicit)

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ignore::WalkBuilder;
use tar::{Builder, Header};
use unicode_normalization::UnicodeNormalization;

/// Convert a path to a canonical tar path string.
///
/// - Converts to UTF-8 (errors on non-UTF-8 paths)
/// - Normalizes to NFC Unicode form for cross-platform consistency
/// - Uses forward slashes
fn canonicalize_path(path: &Path) -> Result<String> {
    let s = path
        .to_str()
        .with_context(|| format!("non-UTF-8 path: {:?}", path))?;
    // NFC normalization: macOS stores filenames as NFD, Linux as NFC.
    // Normalizing to NFC ensures identical hashes across platforms.
    Ok(s.nfc().collect::<String>().replace('\\', "/"))
}

/// Collect entries for canonical tar (shared between create and list)
///
/// Handles both directories (walks all files) and single files.
/// For single files, returns just that file with its filename as the path.
fn collect_canonical_entries(path: &Path) -> Result<Vec<(PathBuf, String)>> {
    // Handle single file case - include it with just its filename
    if path.is_file() {
        let filename = path
            .file_name()
            .with_context(|| format!("path has no filename: {:?}", path))?;
        let canonical = canonicalize_path(Path::new(filename))?;
        return Ok(vec![(PathBuf::from(filename), canonical)]);
    }

    let mut entries = Vec::new();
    let package_root = path.to_path_buf();
    for result in WalkBuilder::new(path)
        .filter_entry(move |entry| {
            let entry_path = entry.path();
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && entry_path != package_root
                && entry_path.join("pcb.toml").is_file()
            {
                return false;
            }
            true
        })
        .build()
    {
        let entry = result?;
        let entry_path = entry.path();
        let rel_path = match entry_path.strip_prefix(path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if rel_path == Path::new("") {
            continue;
        }
        let file_type = entry.file_type().unwrap();
        // Only include files - directories are implicit from file paths in tar
        // This avoids issues with empty directories (which git doesn't track anyway)
        if file_type.is_file() {
            let canonical = canonicalize_path(rel_path)?;
            entries.push((rel_path.to_path_buf(), canonical));
        }
    }
    // Sort by canonical path string bytes, not by PathBuf components.
    // This matters for paths like "a/b" vs "a-c" where component order differs from byte order.
    entries.sort_by(|a, b| a.1.as_bytes().cmp(b.1.as_bytes()));
    Ok(entries)
}

/// List entries that would be included in canonical tar (for debugging)
pub fn list_canonical_tar_entries(dir: &Path) -> Result<Vec<String>> {
    let entries = collect_canonical_entries(dir)?;
    Ok(entries
        .into_iter()
        .map(|(_, canonical)| canonical)
        .collect())
}

/// Create a canonical, deterministic tar archive from a directory or file
///
/// Rules from packaging.md:
/// - Regular files only (directories are implicit from paths)
/// - Relative paths, forward slashes, lexicographic byte order
/// - NFC Unicode normalization for cross-platform consistency
/// - Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
/// - File mode: 0644
/// - End with two 512-byte zero blocks
/// - Respect .gitignore and filter internal marker files
/// - Exclude nested packages (subdirs with pcb.toml)
///
/// For single files, creates a tar with just that file using its filename as the path.
pub fn create_canonical_tar<W: std::io::Write>(path: &Path, writer: W) -> Result<()> {
    let mut builder = Builder::new(writer);
    builder.mode(tar::HeaderMode::Deterministic);

    let is_file = path.is_file();
    let entries = collect_canonical_entries(path)?;

    for (rel_path, canonical_path) in entries {
        // For single files, the original path IS the file; for directories, join the relative path
        let full_path = if is_file {
            path.to_path_buf()
        } else {
            path.join(&rel_path)
        };

        let file = fs::File::open(&full_path)?;
        let len = file.metadata()?.len();
        let mut header = Header::new_gnu();
        header.set_size(len);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("")?;
        header.set_groupname("")?;
        header.set_entry_type(tar::EntryType::Regular);

        builder.append_data(&mut header, &canonical_path, file)?;
    }

    builder.finish()?;

    Ok(())
}

/// Compute content hash from a directory
///
/// Creates canonical GNU tarball from directory, streams to BLAKE3 hasher.
/// Format: h1:<base64-encoded-blake3>
pub fn compute_content_hash_from_dir(cache_dir: &Path) -> Result<String> {
    // Stream canonical tar directly to BLAKE3 hasher (avoids buffering entire tar in memory)
    let mut hasher = blake3::Hasher::new();
    create_canonical_tar(cache_dir, &mut hasher)?;
    let hash = hasher.finalize();
    Ok(format!("h1:{}", STANDARD.encode(hash.as_bytes())))
}

/// Compute manifest hash for a pcb.toml file
///
/// Format: h1:<base64-encoded-blake3>
pub fn compute_manifest_hash(manifest_content: &str) -> String {
    let hash = blake3::hash(manifest_content.as_bytes());
    format!("h1:{}", STANDARD.encode(hash.as_bytes()))
}
