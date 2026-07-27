use super::*;
use anyhow::{Context, Result};
use pcb_zen_core::Diagnostics;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Name of the validation diagnostics file `materialize` writes on every import, inside
/// [`create_diagnostics_dir`].
pub(super) const IMPORT_VALIDATION_DIAGNOSTICS_NAME: &str = ".kicad.validation.diagnostics.json";

/// Directory holding the files that describe the *conversion* rather than the design: the extraction
/// report and the validation diagnostics.
pub(super) const IMPORT_DIAGNOSTICS_DIR: &str = ".pcb/import";

/// Create the directory holding this board's conversion diagnostics.
///
/// It lives under the output repository's gitignored `.pcb/`, next to the board it describes, and both
/// files are overwritten on every run rather than going through the no-overwrite policy.
///
/// Created through the writer's guard, not `create_dir_all`: these paths are inside the output directory
/// like every other, so a symlink standing in for one of them must be refused here too. Import promises
/// not to write outside the directory it was given, and that promise cannot have an exception.
fn create_diagnostics_dir(board_dir: &Path) -> Result<PathBuf> {
    let dir = board_dir.join(IMPORT_DIAGNOSTICS_DIR);
    output::create_dir_checked(&dir, board_dir).with_context(|| {
        format!(
            "Failed to create import diagnostics directory {}",
            dir.display()
        )
    })?;
    Ok(dir)
}

pub(super) fn materialize_board(
    paths: &ImportPaths,
    selection: &ImportSelection,
    validation: &ImportValidationRun,
    portable_kicad_project_zip: Option<PathBuf>,
    writer: &mut output::ImportWriter,
) -> Result<MaterializedBoard> {
    let board_dir = paths.workspace_root.clone();
    let board_zen = board_dir.join(format!("{}.zen", selection.board_name));

    let diagnostics_dir = create_diagnostics_dir(&board_dir)?;
    let import_extraction_json = diagnostics_dir.join(report::IMPORT_EXTRACTION_REPORT_NAME);
    let validation_diagnostics_json = diagnostics_dir.join(IMPORT_VALIDATION_DIAGNOSTICS_NAME);
    write_validation_diagnostics(
        &validation_diagnostics_json,
        &paths.kicad_project_root,
        &validation.summary,
        &validation.diagnostics,
    );

    let (layout_dir, layout_kicad_pro, layout_kicad_pcb) =
        if selection.portable.source_kind == ImportSourceKind::Project {
            let (layout_dir, kicad_pro, kicad_pcb) = copy_layout_sources(
                &paths.kicad_project_root,
                &validation.summary.selected,
                &board_dir,
                writer,
            )?;
            (layout_dir, Some(kicad_pro), Some(kicad_pcb))
        } else {
            // Keep only the declared layout path. `pcb layout` will create KiCad layout files
            // later; schematic-only import must not fabricate an empty project or PCB.
            (board_dir.join("layout"), None, None)
        };

    Ok(MaterializedBoard {
        board_dir,
        board_zen,
        layout_dir,
        layout_kicad_pro,
        layout_kicad_pcb,
        portable_kicad_project_zip,
        validation_diagnostics_json,
        import_extraction_json,
    })
}

/// Write the validation diagnostics, warning rather than failing when the write does not land.
///
/// The diagnostics describe the conversion rather than the design and nothing downstream reads them,
/// so a full or unwritable temp filesystem must not fail an import whose conversion succeeded. The
/// caller prints the path only when the file is actually there.
fn write_validation_diagnostics(
    out_path: &Path,
    kicad_project_root: &Path,
    validation: &ImportValidation,
    diagnostics: &Diagnostics,
) {
    if let Err(error) =
        try_write_validation_diagnostics(out_path, kicad_project_root, validation, diagnostics)
    {
        eprintln!("Failed to write import validation diagnostics (continuing): {error:#}");
    }
}

fn try_write_validation_diagnostics(
    out_path: &Path,
    kicad_project_root: &Path,
    validation: &ImportValidation,
    diagnostics: &Diagnostics,
) -> Result<()> {
    #[derive(Serialize)]
    struct ImportValidationDiagnosticsFile<'a> {
        kicad_project_root: &'a Path,
        selected: &'a SelectedKicadFiles,
        diagnostics: &'a Diagnostics,
    }

    let payload = ImportValidationDiagnosticsFile {
        kicad_project_root,
        selected: &validation.selected,
        diagnostics,
    };

    // Renamed into place rather than truncated: a crash mid-write would otherwise leave invalid JSON
    // at a path consumers read, and the rename replaces a symlinked destination instead of following it.
    output::write_atomic(out_path, serde_json::to_string_pretty(&payload)?.as_bytes())
}

fn copy_layout_sources(
    kicad_project_root: &Path,
    selected: &SelectedKicadFiles,
    board_dir: &Path,
    writer: &mut output::ImportWriter,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    // This matches the default `pcb new board` template: `layout_path = "layout"`.
    let layout_dir = board_dir.join("layout");

    let selected_pro = selected
        .kicad_pro
        .as_ref()
        .context("Project import is missing a selected .kicad_pro file")?;
    let selected_pcb = selected
        .kicad_pcb
        .as_ref()
        .context("Project import is missing a selected .kicad_pcb file")?;
    let src_pro = kicad_project_root.join(selected_pro);
    let src_pcb = kicad_project_root.join(selected_pcb);

    let dst_pro = layout_dir.join(selected_pro);
    let dst_pcb = layout_dir.join(selected_pcb);

    // The writer applies the no-overwrite rule and creates parents, so an authored `layout/` keeps
    // whatever it already has.
    writer.copy(&src_pro, &dst_pro)?;
    writer.copy(&src_pcb, &dst_pcb)?;
    let src_dru = src_pro.with_extension("kicad_dru");
    if src_dru.is_file() {
        writer.copy(&src_dru, &dst_pro.with_extension("kicad_dru"))?;
    }

    Ok((layout_dir, dst_pro, dst_pcb))
}

#[cfg(test)]
mod tests {
    use std::fs;

    /// A writer over an already-created board directory, for the layout-copy tests.
    fn test_writer(board_dir: &Path) -> output::ImportWriter {
        output::ImportWriter::new(board_dir, "Board", false).expect("writer")
    }
    use super::*;

    fn selected_files() -> SelectedKicadFiles {
        SelectedKicadFiles {
            kicad_pro: Some(PathBuf::from("board.kicad_pro")),
            kicad_sch: PathBuf::from("board.kicad_sch"),
            kicad_pcb: Some(PathBuf::from("board.kicad_pcb")),
        }
    }

    fn setup_sources(with_dru: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let src_root = dir.path().join("src");
        let board_dir = dir.path().join("out");
        fs::create_dir_all(&src_root).expect("mkdir src");

        fs::write(src_root.join("board.kicad_pro"), "(kicad_pro)").expect("write pro");
        fs::write(src_root.join("board.kicad_pcb"), "(kicad_pcb)").expect("write pcb");
        if with_dru {
            fs::write(src_root.join("board.kicad_dru"), "(kicad_dru)").expect("write dru");
        }

        (dir, src_root, board_dir)
    }

    fn validation_summary() -> ImportValidation {
        ImportValidation {
            selected: selected_files(),
            schematic_parity_ok: true,
            schematic_parity_violations: 0,
            schematic_parity_tolerated: 0,
            schematic_parity_blocking: 0,
            erc_errors: 0,
            erc_warnings: 0,
            drc_errors: 0,
            drc_warnings: 0,
        }
    }

    /// A symlink standing in for the diagnostics directory would redirect the write out of the output
    /// tree, which is the one thing import promises never to do. The guard applies here like anywhere
    /// else, with no exception for the gitignored `.pcb/`.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_diagnostics_directory_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let board_dir = temp.path().join("board");
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir_all(board_dir.join(".pcb")).expect("board dir");
        fs::create_dir_all(&elsewhere).expect("outside dir");
        std::os::unix::fs::symlink(&elsewhere, board_dir.join(IMPORT_DIAGNOSTICS_DIR))
            .expect("symlink");

        let error = create_diagnostics_dir(&board_dir)
            .expect_err("a symlinked diagnostics directory must be refused");
        assert!(
            format!("{error:#}").contains("symlink"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            fs::read_dir(&elsewhere).expect("read outside dir").count(),
            0,
            "nothing may be written through the link"
        );
    }

    /// Diagnostics live under the output repository's gitignored `.pcb/`, and re-running overwrites
    /// them in place, so nothing accumulates across runs.
    #[test]
    fn diagnostics_live_under_the_output_pcb_dir_and_are_reused_across_runs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let board_dir = temp.path().join("board");
        fs::create_dir_all(&board_dir).expect("board dir");

        let first = create_diagnostics_dir(&board_dir).expect("diagnostics dir");
        assert_eq!(first, board_dir.join(IMPORT_DIAGNOSTICS_DIR));
        assert!(
            first.starts_with(board_dir.join(".pcb")),
            "diagnostics must be inside the gitignored build directory: {}",
            first.display()
        );

        write_validation_diagnostics(
            &first.join(IMPORT_VALIDATION_DIAGNOSTICS_NAME),
            Path::new("/tmp/kicad-project"),
            &validation_summary(),
            &Diagnostics::default(),
        );
        let second = create_diagnostics_dir(&board_dir).expect("second run");

        assert_eq!(second, first, "a second run must reuse the same directory");
        assert_eq!(
            fs::read_dir(temp.path().join("board/.pcb"))
                .expect("read .pcb")
                .count(),
            1,
            "a second run must not add a directory"
        );
    }

    /// An unwritable diagnostics location must not fail an import whose conversion succeeded, and it
    /// must not leave a file behind for the caller to print a path to.
    #[test]
    fn an_unwritable_validation_diagnostics_location_is_not_fatal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let diagnostics_dir = temp.path().join("diagnostics");
        let out_path = diagnostics_dir.join(IMPORT_VALIDATION_DIAGNOSTICS_NAME);
        // A directory where the file should go: writing a file over it fails for any user. An earlier
        // version made the parent read-only, which a privileged user ignores — so it passed locally and
        // failed on CI.
        fs::create_dir_all(&out_path).expect("mkdir in place of the diagnostics file");

        write_validation_diagnostics(
            &out_path,
            Path::new("/tmp/kicad-project"),
            &validation_summary(),
            &Diagnostics::default(),
        );

        assert!(
            !out_path.is_file(),
            "a failed write must not leave a file to print a path to"
        );
    }

    #[test]
    fn copy_layout_sources_copies_kicad_dru_when_present() {
        let (_dir, src_root, board_dir) = setup_sources(true);

        let (_layout_dir, dst_pro, _dst_pcb) = copy_layout_sources(
            &src_root,
            &selected_files(),
            &board_dir,
            &mut test_writer(&board_dir),
        )
        .expect("copy layout");

        let dst_dru = dst_pro.with_extension("kicad_dru");
        assert!(dst_dru.is_file());
        assert_eq!(
            fs::read_to_string(&dst_dru).expect("read dst dru"),
            "(kicad_dru)"
        );
    }

    #[test]
    fn copy_layout_sources_preserves_existing_layout_files() {
        let (_dir, src_root, board_dir) = setup_sources(true);
        let layout_dir = board_dir.join("layout");
        fs::create_dir_all(&layout_dir).expect("mkdir layout");
        fs::write(layout_dir.join("board.kicad_pro"), "authored pro").expect("write pro");
        fs::write(layout_dir.join("board.kicad_pcb"), "authored pcb").expect("write pcb");
        fs::write(layout_dir.join("board.kicad_dru"), "authored dru").expect("write dru");

        copy_layout_sources(
            &src_root,
            &selected_files(),
            &board_dir,
            &mut test_writer(&board_dir),
        )
        .expect("reuse layout");

        assert_eq!(
            fs::read_to_string(layout_dir.join("board.kicad_pro")).unwrap(),
            "authored pro"
        );
        assert_eq!(
            fs::read_to_string(layout_dir.join("board.kicad_pcb")).unwrap(),
            "authored pcb"
        );
        assert_eq!(
            fs::read_to_string(layout_dir.join("board.kicad_dru")).unwrap(),
            "authored dru"
        );
    }

    #[test]
    fn copy_layout_sources_skips_kicad_dru_when_missing() {
        let (_dir, src_root, board_dir) = setup_sources(false);

        let (_layout_dir, dst_pro, _dst_pcb) = copy_layout_sources(
            &src_root,
            &selected_files(),
            &board_dir,
            &mut test_writer(&board_dir),
        )
        .expect("copy layout");

        assert!(!dst_pro.with_extension("kicad_dru").exists());
    }
}
