//! Snapshot tests for canonical tar archives and content hashing.
//!
//! These tests ensure byte-stable outputs for package integrity verification.

use std::fs;
use std::path::{Path, PathBuf};

use pcb_canonical::{
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
        let entries = list_canonical_tar_entries($dir.root(), None).unwrap();
        let content_hash = compute_content_hash_from_dir($dir.root()).unwrap();
        insta::with_settings!({ prepend_module_to_snapshot => false }, {
            insta::assert_snapshot!(
                crate::insta_snapshot_name!(),
                format!(
                    "entries:\n{}\n\nhash: {}",
                    if entries.is_empty() {
                        "(none)".to_string()
                    } else {
                        entries.join("\n")
                    },
                    content_hash
                ),
            );
        });
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

    insta::with_settings!({ prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!(crate::insta_snapshot_name!(), format!(
            "simple: {simple}
empty: {empty}
no_newline: {no_newline}
extra_spaces: {extra_spaces}
with_newline: {with_newline}
full: {full}"
        ));
    });
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
    let entries = list_canonical_tar_entries(dir1.root(), None).unwrap();
    assert!(
        entries.iter().all(|e| !e.contains('\\')),
        "should use forward slashes"
    );
    assert!(
        entries.iter().all(|e| !e.starts_with("./")),
        "should not have leading ./"
    );

    insta::with_settings!({ prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!(crate::insta_snapshot_name!(), hash1);
    });
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
fn pcb_sum_is_excluded_from_content_hashes() {
    let clean = CanonicalTestDir::new();
    clean.add_file("pcb.toml", "[dependencies]\n");
    clean.add_file("main.zen", "x = 1\n");

    let with_lockfile = CanonicalTestDir::new();
    with_lockfile.add_file("pcb.toml", "[dependencies]\n");
    with_lockfile.add_file("main.zen", "x = 1\n");
    with_lockfile.add_file("pcb.sum", "github.com/acme/dep v1.0.0 h1:old\n");
    with_lockfile.add_file("nested/pcb.sum", "ignored nested lockfile\n");

    let entries = list_canonical_tar_entries(with_lockfile.root(), None).unwrap();
    assert_eq!(entries, vec!["main.zen", "pcb.toml"]);
    assert_eq!(
        compute_content_hash_from_dir(clean.root()).unwrap(),
        compute_content_hash_from_dir(with_lockfile.root()).unwrap(),
        "pcb.sum files should not affect directory content hashes"
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
    let entries1 = list_canonical_tar_entries(&file1, None).unwrap();
    let entries2 = list_canonical_tar_entries(&file2, None).unwrap();

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
    insta::with_settings!({ prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!(crate::insta_snapshot_name!(), format!(
            "test.txt: {hash1}\nother.txt: {hash2}\nsame_content_a.txt: {hash_a}\nsame_content_b.txt: {hash_b}"
        ));
    });
}

/// Guard helper: run `git` with an isolated HOME/git-config so the test never
/// touches the developer's global git config, system config, or credential
/// helpers (mirrors the isolation approach in `pcbc/tests/auth_git.rs`).
struct IsolatedGitRepo {
    root: PathBuf,
    _home: tempfile::TempDir,
    _repo: tempfile::TempDir,
    git_config: PathBuf,
}

impl IsolatedGitRepo {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("temp home");
        let git_config = home.path().join("gitconfig");
        let repo = tempfile::tempdir().expect("repo tempdir");
        Self {
            root: repo.path().to_path_buf(),
            _home: home,
            _repo: repo,
            git_config,
        }
    }

    fn git(&self, args: &[&str]) -> std::process::Command {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&self.root)
            .env("HOME", self._home.path())
            .env("XDG_CONFIG_HOME", self._home.path().join("xdg"))
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        for arg in args {
            cmd.arg(arg);
        }
        cmd
    }

    fn run(&self, args: &[&str]) {
        let out = self.git(args).output().expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&path, contents).expect("write file");
    }
}

/// Regression test for the publisher/consumer content-hash divergence.
///
/// `pcbc publish` computes the content hash inside the workspace git repo
/// (`.git` present) and records it in an annotated tag (`publish.rs:972`).
/// `pcb-zen`'s resolver recomputes the hash over a `git archive` extract
/// (`.git` absent) and bails if it differs from the tag (`resolve.rs:364`).
///
/// For a tracked file that matches a committed `.gitignore` rule (here a
/// force-added `debug.log` against `*.log`), the two walks must agree on the
/// file set so their hashes match. `git archive` emits tracked files
/// regardless of `.gitignore`, so `debug.log` is present in the extract; the
/// walker must therefore honor `.gitignore` on *both* sides (independent of a
/// `.git` directory) to drop it consistently. Before the fix, `ignore`'s
/// default `require_git(true)` applied `.gitignore` only on the publisher side,
/// so the hashes diverged and every consumer's `verify_tag_hashes` failed.
#[test]
fn publisher_and_consumer_hashes_agree_for_tracked_but_ignored_file() {
    use std::process::{Command, Stdio};

    if !pcb_zen::git::is_available() {
        eprintln!("skipping: git not on PATH");
        return;
    }

    let repo = IsolatedGitRepo::new();
    repo.run(&["init", "-b", "main"]);
    repo.run(&["config", "user.name", "Test"]);
    repo.run(&["config", "user.email", "test@example.com"]);

    // Committed `.gitignore` plus a force-added file that matches it.
    repo.write(".gitignore", "*.log\n");
    repo.write(
        "pcb.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    repo.write("main.zen", "# main\n");
    repo.write("debug.log", "force-added, matches *.log\n");
    repo.run(&["add", ".gitignore", "pcb.toml", "main.zen"]);
    repo.run(&["add", "-f", "debug.log"]);
    repo.run(&["commit", "-m", "initial"]);

    // Publisher side: walk the package directory inside the real git repo
    // (`.git` present), as `pcbc publish` does.
    let publisher_entries = list_canonical_tar_entries(&repo.root, None).unwrap();
    let publisher_hash = compute_content_hash_from_dir(&repo.root).unwrap();

    // Consumer side: extract `git archive HEAD` into a `.git`-less tempdir,
    // replicating `pcb_zen::git::archive_to_dir` (`crates/pcb-zen/src/git.rs`).
    let extract = tempfile::tempdir().expect("extract tempdir");
    let extract_root = extract.path().to_path_buf();
    let mut archive = Command::new("git");
    archive
        .current_dir(&repo.root)
        .env("HOME", repo._home.path())
        .env("GIT_CONFIG_GLOBAL", &repo.git_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["archive", "--format=tar", "HEAD"])
        .stdout(Stdio::piped());
    let mut child = archive.spawn().expect("spawn git archive");
    let stdout = child.stdout.take().expect("piped stdout");
    tar::Archive::new(stdout)
        .unpack(&extract_root)
        .expect("unpack git archive");
    let status = child.wait().expect("wait git archive");
    assert!(status.success(), "git archive failed");

    // The consumer extract has no `.git`, but `git archive` emits tracked
    // files even when they match `.gitignore` — so `debug.log` is present.
    assert!(!extract_root.join(".git").exists());
    assert!(extract_root.join("debug.log").exists());

    let consumer_entries = list_canonical_tar_entries(&extract_root, None).unwrap();
    let consumer_hash = compute_content_hash_from_dir(&extract_root).unwrap();

    // Both sides honor the committed `.gitignore`, so the file set and hash
    // agree regardless of whether a `.git` directory exists above the walk.
    assert_eq!(publisher_entries, vec!["main.zen", "pcb.toml"]);
    assert_eq!(consumer_entries, vec!["main.zen", "pcb.toml"]);
    assert_eq!(
        publisher_hash, consumer_hash,
        "publisher (in-repo) and consumer (git-archive extract) content hashes must agree"
    );
}
