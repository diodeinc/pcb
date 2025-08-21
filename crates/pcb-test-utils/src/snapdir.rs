//! Snapshot a whole directory with `insta` (simple API).
//! - Respects `.gitignore` and `.ignore` files
//! - Only includes UTF-8 text files (CRLF→LF), ignores binary files  
//! - Large files (>8KB) and .kicad_mod files show hash and size instead of content
//! - Deterministic path order
//!
//!   Review changes: `cargo insta review`

use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

/// Threshold for file content inlining (in bytes)
/// Files larger than this will show hash + size instead of content
const LARGE_FILE_THRESHOLD: usize = 8192; // 8KB

pub fn build_manifest(root: &Path, hash_globs: &[&str], ignore_globs: &[&str]) -> String {
    let base = fs::canonicalize(root).expect("failed to canonicalize root path");

    // Build globsets for hash and ignore patterns
    let mut hash_builder = GlobSetBuilder::new();
    for pattern in hash_globs {
        hash_builder.add(Glob::new(pattern).expect("invalid hash glob pattern"));
    }
    let hash_set = hash_builder.build().expect("failed to build hash globset");

    let mut ignore_builder = GlobSetBuilder::new();
    for pattern in ignore_globs {
        ignore_builder.add(Glob::new(pattern).expect("invalid ignore glob pattern"));
    }
    let ignore_set = ignore_builder
        .build()
        .expect("failed to build ignore globset");

    // Gitignore-aware file walker, but deterministic and confined to `base`
    let mut wb = WalkBuilder::new(&base);
    wb.hidden(true)
        .git_ignore(true) // Respect .gitignore files
        .ignore(true) // Respect .ignore files
        .git_exclude(true) // Keep host-independent
        .git_global(false) // No global git config
        .parents(false); // Don't traverse up directory tree

    let mut entries: Vec<(String, String)> = Vec::new();

    for dent in wb.build().filter_map(Result::ok) {
        let p = dent.path();
        if p == base {
            continue;
        }

        let rel = p
            .strip_prefix(&base)
            .expect("path should be within base")
            .to_string_lossy()
            .replace('\\', "/");

        // Check if file should be ignored
        if ignore_set.is_match(&rel) {
            continue;
        }

        let Some(ft) = dent.file_type() else { continue };

        if ft.is_dir() {
            // Skip directory entries - only show files
        } else if ft.is_file() {
            let mut buf = Vec::new();
            fs::File::open(p)
                .expect("failed to open file")
                .read_to_end(&mut buf)
                .expect("failed to read file");

            // Only include UTF-8 files, ignore non-UTF-8 files
            if let Ok(s) = std::str::from_utf8(&buf) {
                let should_hash = buf.len() > LARGE_FILE_THRESHOLD || hash_set.is_match(&rel);
                if should_hash {
                    // Large file or .kicad_mod: store hash info with path
                    let hash = Sha256::digest(&buf);
                    let hash_short = format!("{hash:x}")[..7].to_string();
                    let hash_info = format!(" <{} bytes, sha256: {}>", buf.len(), hash_short);
                    entries.push((rel + &hash_info, String::new()));
                } else {
                    // Small file: include full content
                    let mut content = s.replace("\r\n", "\n");
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                    entries.push((rel, content));
                };
            }
            // Non-UTF-8 files are ignored
        }
    }

    // Stable order
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Manifest
    let mut out = String::new();
    for (rel, body) in entries {
        out.push_str(&format!("=== {rel}\n"));
        out.push_str(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_file_hashing() {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path();

        // Small regular file - should be inlined
        fs::write(temp_path.join("small.txt"), "Hello").unwrap();

        // Large file - should be hashed
        fs::write(temp_path.join("large.txt"), "x".repeat(10000)).unwrap();

        // .kicad_mod file - should be hashed regardless of size
        fs::write(temp_path.join("test.kicad_mod"), "(module test)").unwrap();

        let manifest = build_manifest(temp_path, &["*.kicad_mod"], &[]);

        // Small file inlined
        assert!(manifest.contains("=== small.txt\nHello"));

        // Large file and .kicad_mod should be in header with hash
        assert!(manifest.contains("=== large.txt <10000 bytes, sha256:"));
        assert!(manifest.contains("=== test.kicad_mod <13 bytes, sha256:"));
    }

    #[test]
    fn test_ignore_globs() {
        let temp_dir = tempdir().unwrap();
        let temp_path = temp_dir.path();

        fs::write(temp_path.join("keep.txt"), "keep this").unwrap();
        fs::write(temp_path.join("ignore.log"), "ignore this").unwrap();

        let manifest = build_manifest(temp_path, &[], &["*.log"]);

        assert!(manifest.contains("keep.txt"));
        assert!(!manifest.contains("ignore.log"));
    }
}
