use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use std::fs;
use std::path::Path;

#[cfg(feature = "native")]
use once_cell::sync::OnceCell;

/// Embedded stdlib tree sourced directly from repository stdlib/.
static EMBEDDED_STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../stdlib");

pub fn embedded_stdlib_dir() -> &'static Dir<'static> {
    &EMBEDDED_STDLIB
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
    })
}

#[cfg(feature = "native")]
fn collect_embedded_files(dir: &Dir<'static>, out: &mut Vec<(&'static Path, &'static [u8])>) {
    out.extend(dir.files().map(|file| (file.path(), file.contents())));
    for subdir in dir.dirs() {
        collect_embedded_files(subdir, out);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn embeds_expected_stdlib_files() {
        let stdlib = &super::EMBEDDED_STDLIB;
        assert!(stdlib.get_file("interfaces.zen").is_some());
        assert!(stdlib.get_file("units.zen").is_some());
        assert!(stdlib.get_file("generics/Resistor.zen").is_some());
        assert!(stdlib.get_file("docs/spec.md").is_some());
        assert!(stdlib.get_dir(".pcb").is_none());
    }
}
