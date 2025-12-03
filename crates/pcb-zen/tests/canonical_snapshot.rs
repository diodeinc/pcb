//! Snapshot tests for canonical tar archives and content hashing.
//!
//! These tests ensure byte-stable outputs for package integrity verification.

use std::fs;
use std::path::{Path, PathBuf};

use pcb_zen::canonical::{
    compute_content_hash_from_dir, compute_manifest_hash, list_canonical_tar_entries,
};

/// Test helper for creating isolated directories with files.
struct CanonicalTestDir {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl CanonicalTestDir {
    fn new() -> Self {
        let _temp = tempfile::tempdir().expect("failed to create temp dir");
        let root = _temp.path().to_path_buf();
        Self { _temp, root }
    }

    fn add_file(&self, rel_path: &str, contents: &str) {
        let path = self.root.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        fs::write(&path, contents).expect("failed to write file");
    }

    fn add_binary_file(&self, rel_path: &str, contents: &[u8]) {
        let path = self.root.join(rel_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        fs::write(&path, contents).expect("failed to write binary file");
    }

    fn add_empty_dir(&self, rel_path: &str) {
        let path = self.root.join(rel_path);
        fs::create_dir_all(&path).expect("failed to create dir");
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

/// Snapshot macro that captures entries and content hash.
macro_rules! canonical_snapshot {
    ($dir:expr) => {{
        let entries = list_canonical_tar_entries($dir.root()).unwrap();
        let content_hash = compute_content_hash_from_dir($dir.root()).unwrap();
        insta::assert_snapshot!(format!(
            "entries:\n{}\n\nhash: {}",
            if entries.is_empty() {
                "(none)".to_string()
            } else {
                entries.join("\n")
            },
            content_hash
        ));
    }};
}

#[test]
fn pcb_toml_manifest_hashes() {
    // Test various pcb.toml contents to ensure stable hashing
    let simple = compute_manifest_hash("[package]\nname = \"test\"\nversion = \"1.0.0\"\n");
    let empty = compute_manifest_hash("");

    // Whitespace sensitivity - these should all produce different hashes
    let no_newline = compute_manifest_hash("name = \"test\"");
    let extra_spaces = compute_manifest_hash("name  =  \"test\"");
    let with_newline = compute_manifest_hash("name = \"test\"\n");

    // Full manifest with dependencies
    let full = compute_manifest_hash(
        r#"[package]
name = "example"
version = "1.0.0"
description = "An example package"

[dependencies]
"@stdlib/resistor" = "^1.0.0"
"#,
    );

    insta::assert_snapshot!(format!(
        "simple: {simple}
empty: {empty}
no_newline: {no_newline}
extra_spaces: {extra_spaces}
with_newline: {with_newline}
full: {full}"
    ));
}

#[test]
fn file_ordering_and_nesting() {
    let dir = CanonicalTestDir::new();

    // Test lexicographic ordering (added in non-sorted order)
    dir.add_file("z.txt", "z");
    dir.add_file("a.txt", "a");
    dir.add_file("m.txt", "m");

    // Test numeric ordering (lexicographic: file1 < file10 < file2)
    dir.add_file("file10.txt", "10");
    dir.add_file("file2.txt", "2");
    dir.add_file("file1.txt", "1");

    // Test uppercase vs lowercase ordering (uppercase byte values are lower)
    dir.add_file("UPPER.txt", "upper");
    dir.add_file("lower.txt", "lower");

    // Test nested directories
    dir.add_file("src/main.zen", "main");
    dir.add_file("src/lib/util.zen", "util");
    dir.add_file("deep/a/b/c/d.txt", "deep");

    // Empty directories should be excluded
    dir.add_empty_dir("empty_dir");

    canonical_snapshot!(dir);
}

#[test]
fn nested_package_exclusion() {
    let dir = CanonicalTestDir::new();

    // Root package files (should be included)
    dir.add_file(
        "pcb.toml",
        "[package]\nname = \"root\"\nversion = \"1.0.0\"\n",
    );
    dir.add_file("main.zen", "# root module");
    dir.add_file("lib/helper.zen", "# helper");

    // Nested package at level 1 (should be excluded)
    dir.add_file("subpkg/pcb.toml", "[package]\nname = \"subpkg\"");
    dir.add_file("subpkg/code.zen", "# excluded");

    // Another nested package (should be excluded)
    dir.add_file("examples/demo/pcb.toml", "[package]");
    dir.add_file("examples/demo/demo.zen", "# excluded");

    // Deeply nested package (should be excluded)
    dir.add_file("a/b/c/nested/pcb.toml", "[package]");
    dir.add_file("a/b/c/nested/deep.zen", "# excluded");

    // Files alongside nested packages (should be included)
    dir.add_file("a/b/c/included.zen", "# included");
    dir.add_file("examples/shared.zen", "# included");

    canonical_snapshot!(dir);
}

#[test]
fn gitignore_patterns() {
    let dir = CanonicalTestDir::new();

    // Root .gitignore
    dir.add_file(".gitignore", "*.log\nbuild/\n!important.log\n");
    dir.add_file("main.zen", "# main");
    dir.add_file("debug.log", "excluded by *.log");
    dir.add_file("important.log", "included via negation");
    dir.add_file("build/output.txt", "excluded by build/");

    // Subdirectory .gitignore
    dir.add_file("sub/.gitignore", "*.tmp\n");
    dir.add_file("sub/code.zen", "included");
    dir.add_file("sub/cache.tmp", "excluded by sub/.gitignore");
    dir.add_file("other/cache.tmp", "included - no gitignore here");

    canonical_snapshot!(dir);
}

#[test]
fn content_edge_cases() {
    let dir = CanonicalTestDir::new();

    // Empty file
    dir.add_file("empty.txt", "");

    // Binary content
    dir.add_binary_file("data.bin", &[0x00, 0x01, 0x02, 0xFF, 0xFE, 0xFD]);

    // Windows line endings (should hash as-is, not normalized)
    dir.add_file("unix.txt", "line1\nline2\n");
    dir.add_file("windows.txt", "line1\r\nline2\r\n");

    // Special characters in filenames
    dir.add_file("file with spaces.txt", "spaces");
    dir.add_file("file-with-dashes.txt", "dashes");

    canonical_snapshot!(dir);
}

#[test]
fn deterministic_hashing() {
    // Create two identical directories and verify they produce the same hash
    let dir1 = CanonicalTestDir::new();
    dir1.add_file("a.txt", "content a");
    dir1.add_file("b.txt", "content b");
    dir1.add_file("sub/c.txt", "content c");

    let dir2 = CanonicalTestDir::new();
    dir2.add_file("a.txt", "content a");
    dir2.add_file("b.txt", "content b");
    dir2.add_file("sub/c.txt", "content c");

    let hash1 = compute_content_hash_from_dir(dir1.root()).unwrap();
    let hash2 = compute_content_hash_from_dir(dir2.root()).unwrap();

    assert_eq!(
        hash1, hash2,
        "identical directories should produce identical hashes"
    );

    // Verify path normalization (forward slashes, no leading ./)
    let entries = list_canonical_tar_entries(dir1.root()).unwrap();
    assert!(
        entries.iter().all(|e| !e.contains('\\')),
        "should use forward slashes"
    );
    assert!(
        entries.iter().all(|e| !e.starts_with("./")),
        "should not have leading ./"
    );

    insta::assert_snapshot!(hash1);
}

#[test]
fn content_change_changes_hash() {
    let dir1 = CanonicalTestDir::new();
    dir1.add_file("file.txt", "content1");

    let dir2 = CanonicalTestDir::new();
    dir2.add_file("file.txt", "content2");

    let hash1 = compute_content_hash_from_dir(dir1.root()).unwrap();
    let hash2 = compute_content_hash_from_dir(dir2.root()).unwrap();

    assert_ne!(
        hash1, hash2,
        "different content should produce different hashes"
    );
}

#[test]
fn golden_full_package() {
    // A realistic package structure - hash should never change
    let dir = CanonicalTestDir::new();

    dir.add_file(
        "pcb.toml",
        r#"[package]
name = "golden-package"
version = "1.0.0"
description = "A golden test package for hash stability"

[dependencies]
"#,
    );

    dir.add_file(
        "main.zen",
        r#"# Main module
load("lib/util.zen", "helper")

R1 = Resistor(resistance = 10k)
helper()
"#,
    );

    dir.add_file(
        "lib/util.zen",
        r#"# Utility functions
def helper():
    print("Hello from helper")
"#,
    );

    dir.add_file(".gitignore", "*.log\nbuild/\n");

    canonical_snapshot!(dir);
}

#[test]
fn empty_directory() {
    let dir = CanonicalTestDir::new();
    // No files - should produce empty entries and a known hash
    canonical_snapshot!(dir);
}

#[test]
fn single_file_hashing() {
    // Test that single files (not directories) are hashed correctly
    let dir = CanonicalTestDir::new();
    dir.add_file("test.txt", "hello world");
    dir.add_file("other.txt", "different content");

    let file1 = dir.root().join("test.txt");
    let file2 = dir.root().join("other.txt");

    // Single files should produce entries with just the filename
    let entries1 = list_canonical_tar_entries(&file1).unwrap();
    let entries2 = list_canonical_tar_entries(&file2).unwrap();

    assert_eq!(entries1, vec!["test.txt"]);
    assert_eq!(entries2, vec!["other.txt"]);

    // Different files should produce different hashes
    let hash1 = compute_content_hash_from_dir(&file1).unwrap();
    let hash2 = compute_content_hash_from_dir(&file2).unwrap();

    assert_ne!(hash1, hash2, "different files should have different hashes");

    // Same content in different filenames should produce different hashes
    // (because the filename is part of the tar entry)
    dir.add_file("same_content_a.txt", "identical");
    dir.add_file("same_content_b.txt", "identical");

    let hash_a = compute_content_hash_from_dir(&dir.root().join("same_content_a.txt")).unwrap();
    let hash_b = compute_content_hash_from_dir(&dir.root().join("same_content_b.txt")).unwrap();

    assert_ne!(
        hash_a, hash_b,
        "same content with different filenames should have different hashes"
    );

    // Snapshot the hashes for stability
    insta::assert_snapshot!(format!(
        "test.txt: {hash1}\nother.txt: {hash2}\nsame_content_a.txt: {hash_a}\nsame_content_b.txt: {hash_b}"
    ));
}
