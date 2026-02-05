//! KiCad import flow.

mod discover;
mod extract;
mod generate;
mod hierarchy;
mod materialize;
mod paths;
mod report;
mod semantic;
mod types;
mod validate;

pub use types::ImportArgs;

use anyhow::Result;

// Re-export internal types so submodules can `use super::*;`.
pub(super) use types::*;

pub fn execute(args: ImportArgs) -> Result<()> {
    let paths = paths::resolve_paths(&args)?;

    let selection = discover::discover_and_select(&paths, &args)?;

    let validation = validate::validate(&paths, &selection, &args)?;

    let mut ir = extract::extract_ir(&paths, &selection, &validation)?;
    ir.hierarchy_plan = hierarchy::build_hierarchy_plan(&ir);
    ir.semantic = semantic::analyze(&ir);
    eprintln!(
        "Passive detection (2-pad only): R={} (h:{} m:{} l:{}), C={} (h:{} m:{} l:{}), unknown:{}, non-2-pad:{}",
        ir.semantic.passives.summary.resistor_high
            + ir.semantic.passives.summary.resistor_medium
            + ir.semantic.passives.summary.resistor_low,
        ir.semantic.passives.summary.resistor_high,
        ir.semantic.passives.summary.resistor_medium,
        ir.semantic.passives.summary.resistor_low,
        ir.semantic.passives.summary.capacitor_high
            + ir.semantic.passives.summary.capacitor_medium
            + ir.semantic.passives.summary.capacitor_low,
        ir.semantic.passives.summary.capacitor_high,
        ir.semantic.passives.summary.capacitor_medium,
        ir.semantic.passives.summary.capacitor_low,
        ir.semantic.passives.summary.unknown,
        ir.semantic.passives.summary.non_two_pad,
    );

    let materialized = materialize::materialize_board(&paths, &selection, &validation)?;

    generate::generate(&materialized, &selection.board_name, &ir)?;

    let report = report::build_import_report(&paths, &selection, &validation, ir, &materialized);
    let report_path = report::write_import_extraction_report(&materialized.board_dir, &report)?;
    eprintln!(
        "Wrote import extraction report to {}",
        report_path.display()
    );

    Ok(())
}
