use anyhow::{Context, Result};
use pcb_zen_core::diagnostics::DiagnosticsPass;
use pcb_zen_core::passes::{FilterHiddenPass, SortPass, SuppressPass};
use std::path::Path;

/// Run KiCad DRC checks and return diagnostics (without printing)
pub fn run_drc(
    kicad_pcb_path: &Path,
    suppress_kinds: &[String],
) -> Result<pcb_zen_core::Diagnostics> {
    let drc_report = pcb_kicad::run_drc(kicad_pcb_path).context("Failed to run KiCad DRC")?;
    let mut diagnostics = drc_report.to_diagnostics(&kicad_pcb_path.to_string_lossy());

    for pass in [
        &FilterHiddenPass as &dyn DiagnosticsPass,
        &SuppressPass::new(suppress_kinds.to_vec()),
        &SortPass,
    ] {
        pass.apply(&mut diagnostics);
    }

    Ok(diagnostics)
}
