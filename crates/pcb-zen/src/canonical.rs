//! Canonical tar archive and content hashing utilities.
//!
//! This module implements deterministic tar archives and BLAKE3 content hashing
//! for package integrity verification.

use anyhow::Result;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Collect entries for canonical tar (shared between create and list)
fn collect_canonical_entries(dir: &Path) -> Result<Vec<(PathBuf, std::fs::FileType)>> {
    let mut entries = Vec::new();
    let package_root = dir.to_path_buf();
    for result in WalkBuilder::new(dir)
        .filter_entry(move |entry| {
            let path = entry.path();
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && path != package_root
                && path.join("pcb.toml").is_file()
            {
                return false;
            }
            true
        })
        .build()
    {
        let entry = result?;
        let path = entry.path();
        let rel_path = match path.strip_prefix(dir) {
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
            entries.push((rel_path.to_path_buf(), file_type));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// List entries that would be included in canonical tar (for debugging)
pub fn list_canonical_tar_entries(dir: &Path) -> Result<Vec<String>> {
    let entries = collect_canonical_entries(dir)?;
    Ok(entries
        .into_iter()
        .map(|(p, _ft)| p.display().to_string())
        .collect())
}

/// Create a canonical, deterministic tar archive from a directory
///
/// Rules from packaging.md:
/// - Regular files only (directories are implicit from paths)
/// - Relative paths, forward slashes, lexicographic order
/// - Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
/// - File mode: 0644
/// - End with two 512-byte zero blocks
/// - Respect .gitignore and filter internal marker files
/// - Exclude nested packages (subdirs with pcb.toml + [package])
pub fn create_canonical_tar<W: std::io::Write>(dir: &Path, writer: W) -> Result<()> {
    use std::fs;
    use tar::{Builder, Header};

    let mut builder = Builder::new(writer);
    builder.mode(tar::HeaderMode::Deterministic);

    let entries = collect_canonical_entries(dir)?;

    for (rel_path, _file_type) in entries {
        let full_path = dir.join(&rel_path);
        let path_str = rel_path.to_str().unwrap().replace('\\', "/");

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

        builder.append_data(&mut header, &path_str, file)?;
    }

    builder.finish()?;

    Ok(())
}

/// Compute content hash from a directory
///
/// Creates canonical GNU tarball from directory, streams to BLAKE3 hasher.
/// Format: h1:<base64-encoded-blake3>
pub fn compute_content_hash_from_dir(cache_dir: &Path) -> Result<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

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
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let hash = blake3::hash(manifest_content.as_bytes());

    format!("h1:{}", STANDARD.encode(hash.as_bytes()))
}
