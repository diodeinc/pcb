use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use include_dir::{Dir, include_dir};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

#[cfg(feature = "native")]
use std::sync::OnceLock;
#[cfg(feature = "native")]
use walkdir::WalkDir;

/// Embedded stdlib tree sourced directly from repository stdlib/.
static EMBEDDED_STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../stdlib");
static EXCLUDED_STDLIB_PATHS: LazyLock<GlobSet> = LazyLock::new(|| {
    let mut builder = GlobSetBuilder::new();
    for pattern in [
        ".gitignore",
        "**/.gitignore",
        "**/*.log",
        "**/*.layout.json",
        "**/test",
        "**/test/**",
    ] {
        builder.add(Glob::new(pattern).expect("valid stdlib exclude glob"));
    }
    builder
        .build()
        .expect("valid stdlib exclude globset configuration")
});

pub fn embedded_stdlib_dir() -> &'static Dir<'static> {
    &EMBEDDED_STDLIB
}

/// Return all embedded stdlib `.zen` files as a map from relative path to contents.
///
/// Paths are relative to the stdlib root (e.g. `"interfaces.zen"`,
/// `"generics/Resistor.zen"`). Only UTF-8 `.zen` files are included.
/// This is intended for test harnesses that use an in-memory file provider.
pub fn stdlib_files_for_tests() -> std::collections::HashMap<PathBuf, String> {
    let mut out = std::collections::HashMap::new();
    collect_stdlib_test_files(&EMBEDDED_STDLIB, &mut out);
    out
}

fn collect_stdlib_test_files(
    dir: &Dir<'static>,
    out: &mut std::collections::HashMap<PathBuf, String>,
) {
    for file in dir.files() {
        let path = file.path();
        if path.extension().and_then(|e| e.to_str()) == Some("zen")
            && let Ok(contents) = std::str::from_utf8(file.contents())
        {
            out.insert(path.to_path_buf(), contents.to_string());
        }
    }
    for subdir in dir.dirs() {
        collect_stdlib_test_files(subdir, out);
    }
}

/// Filter out tool-generated files and local ignore metadata so embedded and
/// materialized stdlib hashing stays stable across workspaces.
///
/// Shared by native and WASM stdlib providers to keep visibility consistent.
pub fn include_stdlib_path(path: &Path) -> bool {
    !EXCLUDED_STDLIB_PATHS.is_match(path)
}

#[cfg(feature = "native")]
pub fn embedded_stdlib_hash() -> &'static str {
    static EMBEDDED_STDLIB_HASH: OnceLock<String> = OnceLock::new();
    EMBEDDED_STDLIB_HASH
        .get_or_init(|| {
            let mut files: Vec<(&'static Path, &'static [u8])> = Vec::new();
            collect_embedded_files(&EMBEDDED_STDLIB, &mut files);
            pcb_canonical::compute_content_hash_from_memory_files(files)
                .expect("failed to hash in-memory embedded stdlib")
        })
        .as_str()
}

#[cfg(feature = "native")]
pub fn compute_stdlib_dir_hash(root: &Path) -> Result<String> {
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    collect_stdlib_disk_files(root, &mut files)?;
    let refs: Vec<(&Path, &[u8])> = files
        .iter()
        .map(|(path, contents)| (path.as_path(), contents.as_slice()))
        .collect();
    pcb_canonical::compute_content_hash_from_memory_files(refs)
}

/// Extract the embedded stdlib tree into `target_dir`.
pub fn extract_embedded_stdlib(target_dir: &Path) -> Result<()> {
    fs::create_dir_all(target_dir).with_context(|| {
        format!(
            "Failed to create target directory for embedded stdlib: {}",
            target_dir.display()
        )
    })?;
    EMBEDDED_STDLIB.extract(target_dir).with_context(|| {
        format!(
            "Failed to extract embedded stdlib into {}",
            target_dir.display()
        )
    })?;
    #[cfg(feature = "native")]
    prune_excluded_paths(target_dir)?;
    Ok(())
}

#[cfg(feature = "native")]
fn collect_embedded_files(dir: &Dir<'static>, out: &mut Vec<(&'static Path, &'static [u8])>) {
    out.extend(
        dir.files()
            .filter(|file| include_stdlib_path(file.path()))
            .map(|file| (file.path(), file.contents())),
    );
    for subdir in dir.dirs() {
        collect_embedded_files(subdir, out);
    }
}

#[cfg(feature = "native")]
fn collect_stdlib_disk_files(root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.with_context(|| format!("Failed to walk {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
        if !include_stdlib_path(rel) {
            continue;
        }

        let contents =
            fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
        out.push((rel.to_path_buf(), contents));
    }
    Ok(())
}

#[cfg(feature = "native")]
fn prune_excluded_paths(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).contents_first(true).min_depth(1) {
        let entry = entry.with_context(|| format!("Failed to walk {}", root.display()))?;
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
        if include_stdlib_path(rel) {
            continue;
        }

        if entry.file_type().is_dir() {
            fs::remove_dir_all(path)
                .with_context(|| format!("Failed to remove directory {}", path.display()))?;
        } else {
            fs::remove_file(path)
                .with_context(|| format!("Failed to remove file {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn embeds_expected_stdlib_files() {
        let stdlib = &super::EMBEDDED_STDLIB;
        assert!(stdlib.get_file("io.zen").is_some());
        assert!(stdlib.get_file("interfaces.zen").is_some());
        assert!(stdlib.get_file("units.zen").is_some());
        assert!(stdlib.get_file("generics/Resistor.zen").is_some());

        assert!(stdlib.get_dir(".pcb").is_none());
    }

    #[test]
    fn stdlib_filter_excludes_hidden_and_generated_noise() {
        assert!(super::include_stdlib_path(Path::new("interfaces.zen")));
        assert!(!super::include_stdlib_path(Path::new(
            "test/test_checks.zen"
        )));
        assert!(!super::include_stdlib_path(Path::new(".gitignore")));
        assert!(!super::include_stdlib_path(Path::new(
            "test/layout/layout.log"
        )));
        assert!(!super::include_stdlib_path(Path::new(
            "test/layout/snapshot.layout.json",
        )));
        assert!(super::include_stdlib_path(Path::new(
            "layout/datalayout.json",
        )));
    }

    #[cfg(feature = "native")]
    #[test]
    fn filtered_embedded_hash_matches_filtered_extracted_hash() {
        let temp = tempfile::tempdir().expect("create temp dir");
        super::extract_embedded_stdlib(temp.path()).expect("extract stdlib");
        assert!(!temp.path().join("test").exists());
        assert!(!temp.path().join(".gitignore").exists());

        let expected = super::embedded_stdlib_hash().to_string();
        let actual = super::compute_stdlib_dir_hash(temp.path()).expect("hash extracted stdlib");
        assert_eq!(expected, actual);
    }
}
