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
    use std::fs;
    use std::path::{Path, PathBuf};
    use walkdir::WalkDir;

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

    pub fn source_matches_target(source: &Path, target: &Path) -> Result<bool> {
        if !target.join("pcb.toml").is_file() {
            return Ok(false);
        }

        Ok(collect_disk_files(source)? == collect_disk_files(target)?)
    }

    pub fn copy_source(source: &Path, target: &Path) -> Result<()> {
        anyhow::ensure!(
            source.join("pcb.toml").is_file(),
            "stdlib source {} is missing pcb.toml",
            source.display()
        );
        fs::create_dir_all(target)
            .with_context(|| format!("Failed to create stdlib target {}", target.display()))?;

        for entry in WalkDir::new(source).follow_links(false) {
            let entry = entry.with_context(|| format!("Failed to walk {}", source.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let rel = path
                .strip_prefix(source)
                .with_context(|| format!("{} is not under {}", path.display(), source.display()))?;
            if !include_path(rel) {
                continue;
            }

            let dst = target.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directory {}", parent.display()))?;
            }
            fs::copy(path, &dst).with_context(|| {
                format!("Failed to copy {} to {}", path.display(), dst.display())
            })?;
        }
        Ok(())
    }

    fn collect_disk_files(root: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let mut out = Vec::new();
        for entry in WalkDir::new(root).follow_links(false) {
            let entry = entry.with_context(|| format!("Failed to walk {}", root.display()))?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("{} is not under {}", path.display(), root.display()))?;
            if !include_path(rel) {
                continue;
            }

            let contents =
                fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
            out.push((rel.to_path_buf(), contents));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
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
    }
}
