use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use include_dir::{Dir, include_dir};
use once_cell::sync::Lazy;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "native")]
use once_cell::sync::OnceCell;
#[cfg(feature = "native")]
use walkdir::WalkDir;

/// Embedded stdlib tree sourced directly from repository stdlib/.
static EMBEDDED_STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../stdlib");
static EXCLUDED_STDLIB_PATHS: Lazy<GlobSet> = Lazy::new(|| {
    let mut builder = GlobSetBuilder::new();
    for pattern in [
        ".gitignore",
        "**/.gitignore",
        "**/*.log",
        "**/*layout.json",
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

/// Filter out tool-generated files and local ignore metadata so embedded and
/// materialized stdlib hashing stays stable across workspaces.
fn include_stdlib_path(path: &Path) -> bool {
    !EXCLUDED_STDLIB_PATHS.is_match(path)
}

#[cfg(feature = "native")]
pub fn embedded_stdlib_hash() -> &'static str {
    static EMBEDDED_STDLIB_HASH: OnceCell<String> = OnceCell::new();
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
        assert!(stdlib.get_file("interfaces.zen").is_some());
        assert!(stdlib.get_file("units.zen").is_some());
        assert!(stdlib.get_file("generics/Resistor.zen").is_some());
        assert!(stdlib.get_file("docs/spec.md").is_some());
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
