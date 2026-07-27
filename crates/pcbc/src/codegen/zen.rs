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

/// Format `content` as Zener source and return the result, writing nothing to `path`.
///
/// The formatter works on files, so this needs a scratch file; `dir` is where it goes, so the caller
/// can keep it on the destination's own filesystem. Callers that must decide *whether* to write —
/// import compares the formatted bytes against what is already there — need the formatted output
/// before the decision, which [`write_zen_formatted`] cannot give them.
pub fn format_zen(dir: &Path, content: &str) -> Result<Vec<u8>> {
    let mut tmp = Builder::new()
        .prefix(".pcb.codegen.")
        .suffix(".zen")
        .tempfile_in(dir)
        .with_context(|| format!("Failed to create temp file in {}", dir.display()))?;
    tmp.write_all(content.as_bytes())
        .with_context(|| format!("Failed to write temp file in {}", dir.display()))?;
    tmp.flush()
        .with_context(|| format!("Failed to flush temp file in {}", dir.display()))?;

    RuffFormatter::default()
        .format_file(tmp.path())
        .with_context(|| format!("Failed to format generated Zener in {}", dir.display()))?;

    std::fs::read(tmp.path())
        .with_context(|| format!("Failed to read formatted Zener in {}", dir.display()))
}
