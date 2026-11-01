use super::*;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub(super) fn execute(args: ImportArgs) -> Result<()> {
    let output_dir = args.output_dir.clone();
    let created_output = !output_dir.exists();
    let ctx = ImportContext::new(args).inspect_err(|_| {
        // Path resolution may create the output directory before failing; it is still empty.
        if created_output {
            let _ = std::fs::remove_dir(&output_dir);
        }
    })?;
    let snapshot = OutputSnapshot::capture(&ctx.paths.workspace_root, created_output)?;
    let result = run(ctx);
    if let Err(error) = &result
        && let Err(restore) = snapshot.restore()
    {
        anyhow::bail!("{error:#}\nAlso failed to restore the output directory: {restore:#}");
    }
    result
}

fn run(ctx: ImportContext) -> Result<()> {
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

    let generation = generate::generate(&board, &selection, &ir)?;
    generated_validate::validate_generated_zen(
        &board,
        &ir,
        &generation.expected_pins_by_refdes,
        &generation.instance_name_by_refdes,
        &generation.not_connected_nets,
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

    // Older imports wrote layout/. Require an explicit move rather than orphaning its PCB.
    let layout_dir = paths.project_dir();
    let legacy_layout_dir = board_repo.join("layout");
    anyhow::ensure!(
        !legacy_layout_dir.is_dir(),
        "Board repository has a legacy layout/ directory. Move its KiCad project into {}/ and reimport.",
        crate::codegen::board::BOARD_PROJECT_DIR
    );

    // Without a source board, reimport retains the project directory, so reject conflicting projects before cleanup.
    let source_board = selection.selected.kicad_pcb.is_some();
    if !source_board && layout_dir.is_dir() {
        let expected = layout_dir.join(selection.selected.layout_kicad_pro());
        for entry in std::fs::read_dir(&layout_dir)? {
            let path = entry?.path();
            anyhow::ensure!(
                path.extension()
                    .is_none_or(|extension| extension != "kicad_pro")
                    || path == expected,
                "Import conflicts with retained KiCad project {}. The source has no board, so --force keeps the existing {}/ layout rather than replacing it. Import that project's matching schematic, remove the project to discard its layout, or choose a new output directory.",
                path.display(),
                crate::codegen::board::BOARD_PROJECT_DIR
            );
        }
    }

    if !existing_board_repo {
        std::fs::create_dir_all(board_repo).with_context(|| {
            format!("Failed to create board repository {}", board_repo.display())
        })?;
        crate::new::init_board_repo(board_repo, &selection.board_name, "")?;
    }

    Ok(())
}

/// The output directory as it was before import, restored when import fails so a failed import
/// leaves no partial output. Git metadata and the `.pcb` cache are neither saved nor restored.
enum OutputSnapshot {
    /// Import created the directory; remove it.
    Created(PathBuf),
    /// The directory was empty apart from `.DS_Store`; empty it again.
    Empty(PathBuf),
    /// An existing board repository; replace its contents with this copy.
    Saved {
        dir: PathBuf,
        copy: tempfile::TempDir,
    },
}

const UNSNAPSHOTTED: [&str; 3] = [".git", ".pcb", ".DS_Store"];

impl OutputSnapshot {
    fn capture(dir: &Path, created: bool) -> Result<Self> {
        let dir = dir.to_path_buf();
        if created {
            return Ok(Self::Created(dir));
        }
        if !dir.join("pcb.toml").exists() {
            return Ok(Self::Empty(dir));
        }
        let copy = tempfile::tempdir().context("Failed to create import backup directory")?;
        for entry in snapshot_entries(&dir)? {
            copy_tree(&entry, &copy.path().join(entry.file_name().unwrap()))?;
        }
        Ok(Self::Saved { dir, copy })
    }

    fn restore(self) -> Result<()> {
        let (dir, copy) = match self {
            Self::Created(dir) => {
                return std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("Failed to remove partial import {}", dir.display()));
            }
            Self::Empty(dir) => {
                // Includes the new repository's `.git`.
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    if entry.file_name() != ".DS_Store" {
                        remove_path(&entry.path())?;
                    }
                }
                return Ok(());
            }
            Self::Saved { dir, copy } => (dir, copy),
        };
        // Keep the backup before touching the repository, so a failure partway through
        // restoring (e.g. a file held open) never loses the previous contents.
        let backup = copy.keep();
        let restored = (|| -> Result<()> {
            for entry in snapshot_entries(&dir)? {
                remove_path(&entry)?;
            }
            for entry in snapshot_entries(&backup)? {
                copy_tree(&entry, &dir.join(entry.file_name().unwrap()))?;
            }
            Ok(())
        })();
        restored.with_context(|| {
            format!(
                "Failed to restore {} after a failed import; its previous contents are in {}",
                dir.display(),
                backup.display()
            )
        })?;
        std::fs::remove_dir_all(&backup)?;
        Ok(())
    }
}

fn snapshot_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))?
    {
        let entry = entry?;
        if !UNSNAPSHOTTED.iter().any(|name| entry.file_name() == *name) {
            entries.push(entry.path());
        }
    }
    Ok(entries)
}

/// Copy a file, symlink, or directory tree exactly, keeping symlinks as links.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(src)
        .with_context(|| format!("Failed to inspect {}", src.display()))?;
    if metadata.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)
            .with_context(|| format!("Failed to copy symlink {}", src.display()))?;
    } else if metadata.is_dir() {
        std::fs::create_dir(dst).with_context(|| format!("Failed to create {}", dst.display()))?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)
            .with_context(|| format!("Failed to copy {} -> {}", src.display(), dst.display()))?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
                .with_context(|| format!("Failed to remove {}", path.display()))
        }
        Ok(_) => std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Failed to inspect {}", path.display())),
    }
}

fn remove_generated_output(
    import_paths: &ImportPaths,
    board_name: &str,
    source_board: bool,
) -> Result<()> {
    let board_dir = &import_paths.workspace_root;
    let mut paths = vec![
        board_dir.join(format!("{board_name}.zen")),
        board_dir.join("modules"),
        board_dir.join("components"),
        board_dir.join(".kicad.import.extraction.json"),
        board_dir.join(".kicad.validation.diagnostics.json"),
        board_dir.join(format!("{board_name}.kicad.archive.zip")),
    ];
    if source_board {
        paths.push(import_paths.project_dir());
    }

    paths.iter().try_for_each(|path| remove_path(path))
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
        let validation = validate::validate(
            &ctx.paths,
            &selection,
            staged_sources.path(),
            ctx.args.skip_drc,
        )?;
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
    staged_sources: tempfile::TempDir,
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
            staged_sources,
            validation,
            ir,
        })
    }
}

struct Hierarchized {
    ctx: ImportContext,
    selection: ImportSelection,
    staged_sources: tempfile::TempDir,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Hierarchized {
    fn run(extracted: Extracted) -> Self {
        let Extracted {
            ctx,
            selection,
            staged_sources,
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
            staged_sources,
            validation,
            ir,
        }
    }
}

struct Analyzed {
    ctx: ImportContext,
    selection: ImportSelection,
    staged_sources: tempfile::TempDir,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Analyzed {
    fn run(hierarchized: Hierarchized) -> Self {
        let Hierarchized {
            ctx,
            selection,
            staged_sources,
            validation,
            ir,
        } = hierarchized;
        let semantic = semantic::analyze(&ir);
        let ir = ImportIr { semantic, ..ir };
        Self {
            ctx,
            selection,
            staged_sources,
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
            staged_sources,
            validation,
            ir,
        } = analyzed;
        if ctx.args.force {
            remove_generated_output(
                &ctx.paths,
                &selection.board_name,
                selection.selected.kicad_pcb.is_some(),
            )?;
        }
        let board = materialize::materialize_board(
            &ctx.paths,
            &selection,
            &validation,
            staged_sources.path(),
        )?;
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

    fn import_paths(board_dir: &Path) -> ImportPaths {
        ImportPaths {
            workspace_root: board_dir.to_path_buf(),
            kicad_project_root: board_dir.to_path_buf(),
            kicad_input_abs: board_dir.join("board.kicad_sch"),
        }
    }

    #[test]
    fn standalone_cleanup_preserves_layout_and_removes_archive() {
        let temp = tempfile::tempdir().expect("tempdir");
        let board_dir = temp.path();
        let layout_file = board_dir.join("eda/user-layout.kicad_pcb");
        let archive_file = board_dir.join("board.kicad.archive.zip");
        std::fs::create_dir_all(layout_file.parent().unwrap()).expect("create layout");
        std::fs::write(&layout_file, "user layout").expect("write layout");
        std::fs::write(&archive_file, "stale project archive").expect("write archive");

        remove_generated_output(&import_paths(board_dir), "board", false)
            .expect("clean standalone output");

        assert_eq!(
            std::fs::read_to_string(layout_file).expect("read preserved layout"),
            "user layout"
        );
        assert!(!archive_file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_removes_dangling_generated_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let board_dir = temp.path();
        let board_zen = board_dir.join("board.zen");
        symlink(board_dir.join("missing-target"), &board_zen).expect("create dangling symlink");

        remove_generated_output(&import_paths(board_dir), "board", false)
            .expect("clean standalone output");

        assert!(std::fs::symlink_metadata(board_zen).is_err());
    }
}
