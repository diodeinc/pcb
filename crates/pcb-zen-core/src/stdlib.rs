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
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};

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
