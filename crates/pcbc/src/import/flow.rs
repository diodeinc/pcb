use super::*;
use anyhow::{Context, Result};

pub(super) fn execute(args: ImportArgs) -> Result<()> {
    let ctx = ImportContext::new(args)?;

    let discovered = Discovered::run(ctx)?;
    prepare_output(
        &discovered.ctx.paths,
        &discovered.selection,
        &discovered.ctx.args,
    )?;
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

    let generation = generate::generate(&board, &selection.board_name, &ir)?;
    generated_validate::validate_generated_zen(
        &board,
        &ir,
        &generation.expected_pins_by_refdes,
        &generation.instance_name_by_refdes,
    )?;
    eprintln!("Wrote imported board to {}", board.board_zen.display());

    let report = report::build_import_report(&ctx.paths, &selection, &validation, ir, &board);
    report::write_import_extraction_report(&board.import_extraction_json, &report)?;
    eprintln!(
        "Wrote import extraction report to {}",
        board.import_extraction_json.display()
    );
    eprintln!(
        "Wrote import validation diagnostics to {}",
        board.validation_diagnostics_json.display()
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
    staged_sources: tempfile::TempDir,
}

impl Discovered {
    fn run(ctx: ImportContext) -> Result<Self> {
        let selection = discover::discover_and_select(&ctx.paths)?;
        let staged_sources = portable::stage_project_files(&selection.portable)?;
        Ok(Self {
            ctx,
            selection,
            staged_sources,
        })
    }
}

fn prepare_output(
    paths: &ImportPaths,
    selection: &ImportSelection,
    args: &ImportArgs,
) -> Result<()> {
    let board_repo = &paths.workspace_root;
    let pcb_toml = board_repo.join("pcb.toml");
    let existing_board_repo = pcb_toml.exists();

    if existing_board_repo && !args.force {
        anyhow::bail!(
            "Board repository already exists: {}. Use --force to overwrite generated files.",
            board_repo.display()
        );
    }

    if args.force {
        remove_generated_output(board_repo, &selection.board_name)?;
    }

    if !existing_board_repo {
        std::fs::create_dir_all(board_repo).with_context(|| {
            format!("Failed to create board repository {}", board_repo.display())
        })?;
        crate::new::init_board_repo(board_repo, &selection.board_name, "")?;
    }

    if selection.portable.source_kind == ImportSourceKind::Project {
        let output_zip = board_repo.join(format!("{}.kicad.archive.zip", selection.board_name));
        portable::write_portable_zip(&selection.portable, &output_zip)
            .context("Failed to write portable KiCad project archive")?;
    }

    Ok(())
}

fn remove_generated_output(board_dir: &Path, board_name: &str) -> Result<()> {
    for path in [
        board_dir.join(format!("{board_name}.zen")),
        board_dir.join("modules"),
        board_dir.join("components"),
        board_dir.join("layout"),
        board_dir.join(".kicad.import.extraction.json"),
        board_dir.join(".kicad.validation.diagnostics.json"),
        board_dir.join(format!("{board_name}.kicad.archive.zip")),
    ] {
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        } else if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("Failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

struct Validated {
    ctx: ImportContext,
    selection: ImportSelection,
    staged_sources: tempfile::TempDir,
    validation: ImportValidationRun,
}

impl Validated {
    fn run(discovered: Discovered) -> Result<Self> {
        let Discovered {
            ctx,
            selection,
            staged_sources,
        } = discovered;
        let validation = validate::validate(&ctx.paths, &selection, staged_sources.path())?;
        Ok(Self {
            ctx,
            selection,
            staged_sources,
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
            staged_sources,
            validation,
        } = validated;
        let ir = extract::extract_ir(&ctx.paths, &selection, &validation, staged_sources.path())?;
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
            "Passive detection (2-pad only): R={} (h:{} m:{} l:{}), C={} (h:{} m:{} l:{}), unknown:{}, known-non-2-pad:{}, pad-count-unknown:{}",
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
            semantic.passives.summary.unknown_pad_count,
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
