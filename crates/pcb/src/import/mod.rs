//! KiCad import flow.

mod discover;
mod extract;
mod generate;
mod materialize;
mod paths;
mod report;
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

    let ir = extract::extract_ir(&paths, &selection, &validation)?;

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
