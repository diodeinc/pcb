use super::*;
use anyhow::{Context, Result};

pub(super) fn execute(args: ImportArgs) -> Result<()> {
    let ctx = ImportContext::new(args)?;

    let discovered = Discovered::run(ctx)?;
    prepare_output(&discovered.ctx.paths, &discovered.selection)?;
    let validated = Validated::run(discovered)?;
    let extracted = Extracted::run(validated)?;
    let hierarchized = Hierarchized::run(extracted);
    let analyzed = Analyzed::run(hierarchized);
    let materialized = Materialized::run(analyzed)?;

    generate_and_report(materialized)
}

fn generate_and_report(materialized: Materialized) -> Result<()> {
    let Materialized {
        ctx,
        selection,
        validation,
        ir,
        board,
    } = materialized;

    generate::generate(&board, &selection.board_name, &ir)?;

    let report = report::build_import_report(&ctx.paths, &selection, &validation, ir, &board);
    let report_path = report::write_import_extraction_report(&board.board_dir, &report)?;
    eprintln!(
        "Wrote import extraction report to {}",
        report_path.display()
    );

    Ok(())
}

struct ImportContext {
    args: ImportArgs,
    paths: ImportPaths,
}

impl ImportContext {
    fn new(args: ImportArgs) -> Result<Self> {
        let paths = paths::resolve_paths(&args)?;
        Ok(Self { args, paths })
    }
}

struct Discovered {
    ctx: ImportContext,
    selection: ImportSelection,
}

impl Discovered {
    fn run(ctx: ImportContext) -> Result<Self> {
        let selection = discover::discover_and_select(&ctx.paths, &ctx.args)?;
        Ok(Self { ctx, selection })
    }
}

fn prepare_output(paths: &ImportPaths, selection: &ImportSelection) -> Result<()> {
    let board_dir = paths
        .workspace_root
        .join("boards")
        .join(&selection.board_name);
    if board_dir.exists() {
        std::fs::remove_dir_all(&board_dir).with_context(|| {
            format!(
                "Failed to remove existing board dir {}",
                board_dir.display()
            )
        })?;
    }

    let board_scaffold = crate::new::scaffold_board(&paths.workspace_root, &selection.board_name)?;
    let portable_kicad_project_zip = board_scaffold.board_dir.join(".kicad.archive.zip");
    portable::write_portable_zip(&selection.portable, &portable_kicad_project_zip)
        .context("Failed to write portable KiCad project archive")?;
    Ok(())
}

struct Validated {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
}

impl Validated {
    fn run(discovered: Discovered) -> Result<Self> {
        let Discovered { ctx, selection } = discovered;
        let validation = validate::validate(&ctx.paths, &selection, &ctx.args)?;
        Ok(Self {
            ctx,
            selection,
            validation,
        })
    }
}

struct Extracted {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Extracted {
    fn run(validated: Validated) -> Result<Self> {
        let Validated {
            ctx,
            selection,
            validation,
        } = validated;

        let ir = extract::extract_ir(&ctx.paths, &selection, &validation)?;

        Ok(Self {
            ctx,
            selection,
            validation,
            ir,
        })
    }
}

struct Hierarchized {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Hierarchized {
    fn run(extracted: Extracted) -> Self {
        let Extracted {
            ctx,
            selection,
            validation,
            ir,
        } = extracted;

        let hierarchy_plan = hierarchy::build_hierarchy_plan(&ir);
        let ir = ImportIr {
            hierarchy_plan,
            ..ir
        };

        Self {
            ctx,
            selection,
            validation,
            ir,
        }
    }
}

struct Analyzed {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Analyzed {
    fn run(hierarchized: Hierarchized) -> Self {
        let Hierarchized {
            ctx,
            selection,
            validation,
            ir,
        } = hierarchized;

        let semantic = semantic::analyze(&ir);

        eprintln!(
            "Passive detection (2-pad only): R={} (h:{} m:{} l:{}), C={} (h:{} m:{} l:{}), unknown:{}, non-2-pad:{}",
            semantic.passives.summary.resistor_high
                + semantic.passives.summary.resistor_medium
                + semantic.passives.summary.resistor_low,
            semantic.passives.summary.resistor_high,
            semantic.passives.summary.resistor_medium,
            semantic.passives.summary.resistor_low,
            semantic.passives.summary.capacitor_high
                + semantic.passives.summary.capacitor_medium
                + semantic.passives.summary.capacitor_low,
            semantic.passives.summary.capacitor_high,
            semantic.passives.summary.capacitor_medium,
            semantic.passives.summary.capacitor_low,
            semantic.passives.summary.unknown,
            semantic.passives.summary.non_two_pad,
        );

        let ir = ImportIr { semantic, ..ir };

        Self {
            ctx,
            selection,
            validation,
            ir,
        }
    }
}

struct Materialized {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
    board: MaterializedBoard,
}

impl Materialized {
    fn run(analyzed: Analyzed) -> Result<Self> {
        let Analyzed {
            ctx,
            selection,
            validation,
            ir,
        } = analyzed;

        let board = materialize::materialize_board(&ctx.paths, &selection, &validation)?;

        Ok(Self {
            ctx,
            selection,
            validation,
            ir,
            board,
        })
    }
}
