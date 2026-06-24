//! Shared filesystem utilities for walking directories of `.kicad_mod` files.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Recursively collect `.kicad_mod` files under `root` (which may itself be a
/// single file). Results are appended to `out`.
pub fn collect_footprints(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = std::fs::metadata(root).with_context(|| format!("stat {}", root.display()))?;
    if meta.is_file() {
        if is_kicad_mod(root) {
            out.push(root.to_path_buf());
        }
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root).with_context(|| format!("read_dir {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_footprints(&path, out)?;
        } else if file_type.is_file() && is_kicad_mod(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// True when `path` has a `.kicad_mod` extension (case-insensitive).
pub fn is_kicad_mod(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("kicad_mod"))
}
