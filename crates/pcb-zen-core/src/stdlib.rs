use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static EXCLUDED_PATHS: LazyLock<GlobSet> = LazyLock::new(|| {
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

pub fn include_path(path: &Path) -> bool {
    !EXCLUDED_PATHS.is_match(path)
}

/// Return all repository stdlib `.zen` files as a map from relative path to contents.
///
/// This is intended for test harnesses that use an in-memory file provider.
pub fn files_for_tests() -> HashMap<PathBuf, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/std");
    let mut out = HashMap::new();
    collect_zen_files(&root, &root, &mut out).expect("failed to read repository stdlib files");
    out
}

fn collect_zen_files(
    root: &Path,
    dir: &Path,
    out: &mut HashMap<PathBuf, String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_zen_files(root, &path, out)?;
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("zen") {
            let rel = path.strip_prefix(root).expect("stdlib path is under root");
            out.insert(rel.to_path_buf(), fs::read_to_string(&path)?);
        }
    }
    Ok(())
}

#[cfg(feature = "native")]
pub mod native {
    use super::include_path;
    use anyhow::{Context, Result};
    use filetime::FileTime;
    use std::fs;
    use std::path::{Path, PathBuf};
    use walkdir::{DirEntry, WalkDir};

    const MAX_SOURCE_SEARCH_ANCESTORS: usize = 5;

    pub fn discover_source() -> Result<PathBuf> {
        let exe = std::env::current_exe().context("failed to determine current executable path")?;
        discover_source_from_exe(&exe)
    }

    pub fn discover_source_from_exe(exe: &Path) -> Result<PathBuf> {
        for ancestor in exe.ancestors().take(MAX_SOURCE_SEARCH_ANCESTORS) {
            let candidate = ancestor.join("lib/std");
            if candidate.join("pcb.toml").is_file() {
                return Ok(candidate);
            }
        }

        anyhow::bail!(
            "could not find stdlib source next to {}; expected an ancestor containing lib/std/pcb.toml",
            exe.display()
        )
    }

    /// Whether `target` is a current copy of `source`: the same files, with the
    /// sizes and modification times [`copy_source`] mirrors. Resolution checks
    /// this on every command, so it must not read either tree.
    pub fn source_matches_target(source: &Path, target: &Path) -> Result<bool> {
        let (source, target) = rayon::join(|| file_stamps(source), || file_stamps(target));
        Ok(source? == target?)
    }

    pub fn copy_source(source: &Path, target: &Path) -> Result<()> {
        anyhow::ensure!(
            source.join("pcb.toml").is_file(),
            "stdlib source {} is missing pcb.toml",
            source.display()
        );
        fs::create_dir_all(target)
            .with_context(|| format!("Failed to create stdlib target {}", target.display()))?;

        for file in stdlib_files(source) {
            let (rel, entry) = file?;
            let dst = target.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory {}", parent.display()))?;
            }
            fs::copy(entry.path(), &dst).with_context(|| {
                format!(
                    "Failed to copy {} to {}",
                    entry.path().display(),
                    dst.display()
                )
            })?;
            let mtime = FileTime::from_last_modification_time(&entry.metadata()?);
            filetime::set_file_mtime(&dst, mtime)
                .with_context(|| format!("Failed to set mtime of {}", dst.display()))?;
        }
        Ok(())
    }

    /// Relative path, size and modification time of every stdlib file under
    /// `root`. Times are whole seconds: a copy can land on a filesystem that
    /// keeps nothing finer.
    fn file_stamps(root: &Path) -> Result<Vec<(PathBuf, u64, i64)>> {
        stdlib_files(root)
            .map(|file| {
                let (rel, entry) = file?;
                let metadata = entry
                    .metadata()
                    .with_context(|| format!("Failed to stat {}", entry.path().display()))?;
                let mtime = FileTime::from_last_modification_time(&metadata).unix_seconds();
                Ok((rel, metadata.len(), mtime))
            })
            .collect()
    }

    /// The stdlib files under `root` with their relative paths, in an order that
    /// depends only on the paths. Excluded directories are not entered.
    fn stdlib_files(root: &Path) -> impl Iterator<Item = Result<(PathBuf, DirEntry)>> {
        let relative =
            move |entry: &DirEntry| entry.path().strip_prefix(root).map(Path::to_path_buf);
        WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(move |entry| relative(entry).is_ok_and(|rel| include_path(&rel)))
            .filter_map(move |entry| match entry {
                Ok(entry) if entry.file_type().is_file() => {
                    Some(Ok((relative(&entry).ok()?, entry)))
                }
                Ok(_) => None,
                Err(e) => {
                    Some(Err(e).with_context(|| format!("Failed to walk {}", root.display())))
                }
            })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn discovers_installed_toolchain_stdlib() {
            let temp = tempfile::tempdir().expect("create temp dir");
            let toolchain = temp.path().join("toolchains/1.2.3/aarch64-test");
            std::fs::create_dir_all(toolchain.join("lib/std")).expect("create stdlib");
            std::fs::write(toolchain.join("lib/std/pcb.toml"), "[dependencies]\n")
                .expect("write manifest");

            let exe = toolchain.join("pcbc");
            assert_eq!(
                super::discover_source_from_exe(&exe).expect("discover stdlib"),
                toolchain.join("lib/std")
            );
        }

        #[test]
        fn copy_matches_until_either_tree_changes() {
            use super::{copy_source, source_matches_target};
            use filetime::{FileTime, set_file_mtime};
            use std::fs;

            let temp = tempfile::tempdir().expect("create temp dir");
            let (source, target) = (temp.path().join("source"), temp.path().join("target"));
            fs::create_dir_all(source.join("generics/test")).expect("create source");
            for file in ["pcb.toml", "generics/Led.zen", "generics/test/skipped.zen"] {
                fs::write(source.join(file), "v1").expect("write source file");
                // An installed toolchain is older than any edit to its copy.
                set_file_mtime(
                    source.join(file),
                    FileTime::from_unix_time(1_000_000_000, 0),
                )
                .expect("age source file");
            }
            let matches = || source_matches_target(&source, &target).unwrap_or(false);
            let recopy = || {
                fs::remove_dir_all(&target).expect("remove target");
                copy_source(&source, &target).expect("copy stdlib");
            };

            assert!(!matches(), "a missing target does not match");
            copy_source(&source, &target).expect("copy stdlib");
            assert!(matches());
            assert!(!target.join("generics/test").exists());

            // Same size, so only the modification time gives the edit away.
            fs::write(target.join("generics/Led.zen"), "v2").expect("edit target");
            assert!(!matches(), "an edited copy does not match");
            recopy();

            fs::remove_file(target.join("generics/Led.zen")).expect("delete from target");
            assert!(!matches(), "an incomplete copy does not match");
            recopy();

            fs::write(source.join("generics/Led.zen"), "v3").expect("edit source");
            assert!(!matches(), "a changed source does not match");
            recopy();
            assert!(matches());
        }
    }
}
