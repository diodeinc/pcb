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

    generate::generate(&board, &selection.board_name, &ir)?;
    eprintln!("Wrote imported board to {}", board.board_zen.display());

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
        crate::new::init_board_repo(board_repo, &selection.board_name, "")?;
    }

    let portable_kicad_project_zip =
        board_repo.join(format!("{}.kicad.archive.zip", selection.board_name));
    portable::write_portable_zip(&selection.portable, &portable_kicad_project_zip)
        .context("Failed to write portable KiCad project archive")?;
    Ok(())
}

fn remove_generated_output(board_dir: &std::path::Path, board_name: &str) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn force_import_preserves_existing_board_repo_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let board_repo = temp.path().join("board");
        std::fs::create_dir(&board_repo)?;

        let pcb_toml = board_repo.join("pcb.toml");
        let readme = board_repo.join("README.md");
        let gitignore = board_repo.join(".gitignore");
        let board_zen = board_repo.join("ImportedBoard.zen");
        let modules = board_repo.join("modules");

        let pcb_toml_contents = r#"[workspace]
repository = "github.com/example/custom-board"
endpoint = "https://example.invalid"

[board]
name = "ImportedBoard"
path = "ImportedBoard.zen"
description = "Custom board description."

[dependencies]
foo = { path = "modules/foo" }
"#;
        let readme_contents = "# Custom README\n";
        let gitignore_contents = "custom-ignore\n";

        std::fs::write(&pcb_toml, pcb_toml_contents)?;
        std::fs::write(&readme, readme_contents)?;
        std::fs::write(&gitignore, gitignore_contents)?;
        std::fs::write(&board_zen, "old generated board\n")?;
        std::fs::create_dir(&modules)?;
        std::fs::write(modules.join("old.zen"), "old generated module\n")?;

        let paths = ImportPaths {
            workspace_root: board_repo.clone(),
            kicad_project_root: temp.path().to_path_buf(),
            kicad_pro_abs: temp.path().join("ImportedBoard.kicad_pro"),
        };
        let selection = ImportSelection {
            board_name: "ImportedBoard".to_string(),
            board_name_source: BoardNameSource::KicadProArgument,
            files: KicadDiscoveredFiles::default(),
            selected: SelectedKicadFiles {
                kicad_pro: PathBuf::from("ImportedBoard.kicad_pro"),
                kicad_sch: PathBuf::from("ImportedBoard.kicad_sch"),
                kicad_pcb: PathBuf::from("ImportedBoard.kicad_pcb"),
            },
            portable: PortableKicadProject {
                project_dir: temp.path().to_path_buf(),
                project_name: "ImportedBoard".to_string(),
                kicad_pro_rel: PathBuf::from("ImportedBoard.kicad_pro"),
                root_schematic_rel: PathBuf::from("ImportedBoard.kicad_sch"),
                primary_kicad_pcb_rel: PathBuf::from("ImportedBoard.kicad_pcb"),
                schematic_files_rel: Vec::new(),
                files_to_bundle_rel: Vec::new(),
                extra_files_to_bundle: Vec::new(),
                manifest_json: "{}".to_string(),
            },
        };
        let args = ImportArgs {
            kicad_pro: paths.kicad_pro_abs.clone(),
            output_dir: board_repo.clone(),
            force: true,
        };

        prepare_output(&paths, &selection, &args)?;

        assert_eq!(std::fs::read_to_string(&pcb_toml)?, pcb_toml_contents);
        assert_eq!(std::fs::read_to_string(&readme)?, readme_contents);
        assert_eq!(std::fs::read_to_string(&gitignore)?, gitignore_contents);
        assert!(!board_zen.exists());
        assert!(!modules.exists());
        assert!(board_repo.join("ImportedBoard.kicad.archive.zip").is_file());

        Ok(())
    }
}
