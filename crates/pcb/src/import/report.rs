use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub(super) fn write_import_extraction_report(
    board_dir: &Path,
    payload: &super::ImportReport,
) -> Result<PathBuf> {
    let out_path = board_dir.join(".kicad.import.extraction.json");
    fs::write(&out_path, serde_json::to_string_pretty(payload)?)
        .with_context(|| format!("Failed to write {}", out_path.display()))?;
    Ok(out_path)
}
