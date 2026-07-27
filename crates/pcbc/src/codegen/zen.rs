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

/// Format `content` as Zener source and return the result, writing nothing.
///
/// For callers that must decide *whether* to write: import compares the formatted bytes against what is
/// already on disk, which needs the formatted output before the decision. The scratch file the formatter
/// requires goes in the system temp directory, deliberately not the destination — it is read back and
/// discarded, never renamed into place, so keeping it off the destination means comparing against a
/// read-only output tree stays a silent no-op instead of failing for want of somewhere to write.
pub fn format_zen(content: &str) -> Result<Vec<u8>> {
    let mut tmp = Builder::new()
        .prefix(".pcb.codegen.")
        .suffix(".zen")
        .tempfile()
        .context("Failed to create a temp file to format Zener in")?;
    tmp.write_all(content.as_bytes())
        .context("Failed to write Zener to a temp file")?;
    tmp.flush().context("Failed to flush Zener temp file")?;

    RuffFormatter::default()
        .format_file(tmp.path())
        .context("Failed to format generated Zener")?;

    std::fs::read(tmp.path()).context("Failed to read back formatted Zener")
}
