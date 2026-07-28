use anyhow::{Context, Result};
use pcb_fmt::RuffFormatter;
use std::io::Write;
use std::path::Path;
use tempfile::Builder;

pub fn write_zen_formatted(path: &Path, content: &str) -> Result<()> {
    let dir = path
        .parent()
        .context("Expected .zen path to have a parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;

    let mut tmp = Builder::new()
        .prefix(".pcb.codegen.")
        .suffix(".zen")
        .tempfile_in(dir)
        .with_context(|| format!("Failed to create temp file in {}", dir.display()))?;

    tmp.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write temp file for {}", path.display()))?;
    tmp.flush()
        .with_context(|| format!("Failed to flush temp file for {}", path.display()))?;

    let formatter = RuffFormatter::default();
    formatter
        .format_file(tmp.path())
        .with_context(|| format!("Failed to format generated {}", path.display()))?;

    tmp.persist(path)
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| format!("Failed to persist {}", path.display()))?;

    Ok(())
}
