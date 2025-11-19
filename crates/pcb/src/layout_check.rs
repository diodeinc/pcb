use anyhow::{Context, Result};
use pcb_kicad::PythonScriptBuilder;
use pcb_layout::sync_check::SyncReport;
use pcb_zen_core::diagnostics::DiagnosticsPass;
use pcb_zen_core::passes::{FilterHiddenPass, SortPass, SuppressPass};
use std::path::Path;
use tempfile::NamedTempFile;

/// Run layout sync check and return diagnostics (without printing)
pub fn run_layout_check(
    _zen_path: &Path,
    pcb_path: &Path,
    netlist_path: &Path,
    board_config_path: Option<&Path>,
    suppress_kinds: &[String],
) -> Result<pcb_zen_core::Diagnostics> {
    let temp_file =
        NamedTempFile::new().context("Failed to create temporary file for changes JSON")?;
    let changes_path = temp_file.path();

    let script = include_str!("../../pcb-layout/src/scripts/update_layout_file.py");

    let mut script_builder = PythonScriptBuilder::new(script)
        .arg("--check")
        .arg("--changes-output")
        .arg(changes_path.to_str().unwrap())
        .arg("-j")
        .arg(netlist_path.to_str().unwrap())
        .arg("-o")
        .arg(pcb_path.to_str().unwrap());

    if let Some(board_config) = board_config_path {
        script_builder = script_builder
            .arg("--board-config")
            .arg(board_config.to_str().unwrap());
    }

    script_builder
        .run()
        .context("Failed to run layout sync check")?;

    let sync_report = SyncReport::from_json_file(changes_path)
        .context("Failed to parse layout sync changes JSON")?;

    let mut diagnostics = sync_report
        .to_diagnostics()
        .context("Failed to convert sync report to diagnostics")?;

    for pass in [
        &FilterHiddenPass as &dyn DiagnosticsPass,
        &SuppressPass::new(suppress_kinds.to_vec()),
        &SortPass,
    ] {
        pass.apply(&mut diagnostics);
    }

    Ok(diagnostics)
}
