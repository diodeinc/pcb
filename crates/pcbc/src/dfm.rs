use pcb_ir::geom::{GeometryAccuracy, Resolution};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pcb_ipc2581_tools::{LayoutTarget, commands};

use crate::layout::LayoutArgs;

// Findings are calibrated at 10 µm, independently of the CLI export budget.
pub(crate) const DFM_RESOLUTION: Resolution = Resolution {
    tolerance_mm: pcb_ir::geom::tol::REGION_MM,
    accuracy: GeometryAccuracy::micrometres(10),
};

#[derive(Args, Debug)]
#[command(about = "Run DFM checks for a .zen board")]
pub struct DfmArgs {
    /// Path to the board .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Built-in PDK name or fabrication PDK TOML path
    #[arg(long, default_value = "standard")]
    pub pdk: PathBuf,

    /// Output self-contained JSON report path. Omit to write to stdout.
    #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
    pub output: Option<PathBuf>,

    /// Open the report in dfm.diode.computer after writing it
    #[arg(long)]
    pub open: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Check the existing layout as-is without regenerating it. Fails when no
    /// layout exists. Only use this when the layout is known to match the
    /// source: unlike the default path it never refreshes copper fills, so a
    /// stale layout checks stale geometry.
    #[arg(long = "no-sync")]
    pub no_sync: bool,
}

pub fn execute(args: DfmArgs) -> Result<()> {
    let temporary_output = if args.open && args.output.is_none() {
        Some(tempfile::tempdir().context("failed to create temporary DFM report directory")?)
    } else {
        None
    };
    let output = args.output.clone().or_else(|| {
        temporary_output.as_ref().map(|directory| {
            let filename = args
                .file
                .with_extension("dfm.json")
                .file_name()
                .unwrap_or_default()
                .to_owned();
            directory.path().join(filename)
        })
    });
    let options = commands::dfm::CheckOptions {
        pdk: args.pdk.clone(),
        waivers: None,
        output,
        layout_target: LayoutTarget::Board,
    };
    commands::dfm::validate_output(&args.file, &options)?;
    let dfm_result = match export_layout(&args) {
        Ok((_temporary_dir, ipc_path)) => {
            match commands::dfm::execute_check(&ipc_path, &options, DFM_RESOLUTION)? {
                commands::dfm::CheckOutcome::Passed => Ok(()),
                commands::dfm::CheckOutcome::Failed(error) => Err(error),
            }
        }
        Err(error) => {
            commands::dfm::write_error_report(&args.file, &options, &error)
                .with_context(|| format!("DFM check was incomplete: {error:#}"))?;
            Err(error)
        }
    };

    if !args.open {
        return dfm_result;
    }

    if let Err(open_error) = crate::open::open_dfm_report(
        options
            .output
            .as_deref()
            .expect("--open always selects a report file"),
    ) {
        if let Some(directory) = temporary_output {
            let _ = directory.keep();
            anstream::eprintln!(
                "DFM report kept at {}",
                options.output.as_deref().unwrap().display()
            );
        }
        if dfm_result.is_ok() {
            return Err(open_error);
        }
        anstream::eprintln!("Warning: {open_error:#}");
    }
    dfm_result
}

fn export_layout(args: &DfmArgs) -> Result<(tempfile::TempDir, PathBuf)> {
    let layout_args = LayoutArgs {
        file: args.file.clone(),
        no_open: true,
        offline: args.offline,
        // DFM never reads hydrated part data, and hydration only renames
        // footprint Value text, which lives on Fab/Silk layers DFM does not
        // consume (no footprint Value sits on copper corpus-wide, and sync
        // hides Value on every created footprint). Skipping it pins the
        // fallback geometry a failed BOM match already produces today.
        skip_bom_hydration: true,
        no_sync: args.no_sync,
        ..Default::default()
    };
    let design = crate::layout::prepare_design(&layout_args)?;
    if args.no_sync {
        return export_generated_layout(args, crate::layout::apply_prepared(&layout_args, design)?);
    }
    let layout_dir = pcb_layout::utils::resolve_layout_dir(&design.schematic)?
        .with_context(|| format!("{} does not declare a layout", args.file.display()))?;
    if layout_dir.exists() {
        // Synchronize a disposable copy so DFM never rewrites, replaces, or
        // changes metadata on a pre-existing user layout.
        let working = tempfile::tempdir().context("failed to create temporary layout directory")?;
        let working_layout = working.path().join("layout");
        std::fs::create_dir(&working_layout)
            .context("failed to create temporary layout working copy")?;
        copy_layout(&layout_dir, &working_layout)?;
        let layout = crate::layout::apply_prepared_to(&layout_args, design, &working_layout)?;
        return export_generated_layout(args, layout);
    }
    // Preserve the existing first-run behavior: a layout created from
    // scratch remains available after DFM completes.
    let layout = crate::layout::apply_prepared(&layout_args, design)?;
    export_generated_layout(args, layout)
}

fn export_generated_layout(
    args: &DfmArgs,
    layout: crate::layout::LayoutCommandResult,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let pcb_file = layout
        .pcb_file_abs
        .as_deref()
        .with_context(|| format!("{} does not declare a layout", args.file.display()))?;
    export_ipc(pcb_file)
}

/// Export a board file to a temporary IPC-2581 document for checking.
fn export_ipc(pcb_file: &std::path::Path) -> Result<(tempfile::TempDir, PathBuf)> {
    let temporary_dir = tempfile::tempdir().context("failed to create temporary DFM directory")?;
    let ipc_path = temporary_dir.path().join("ipc2581.xml");
    crate::release::export_ipc2581(pcb_file, &ipc_path)?;
    Ok((temporary_dir, ipc_path))
}

fn copy_layout(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    let files = pcb_layout::utils::resolve_kicad_files(source)?;
    let pcb = files.kicad_pcb();
    for path in [files.kicad_pro, pcb] {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect layout file {}", path.display()));
            }
        }
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("failed to inspect layout file {}", path.display()))?;
        anyhow::ensure!(
            metadata.is_file(),
            "layout file is not a regular file: {}",
            path.display()
        );
        let target = destination.join(
            path.file_name()
                .with_context(|| format!("layout file {} has no name", path.display()))?,
        );
        std::fs::copy(&path, &target).with_context(|| {
            format!("failed to copy {} to {}", path.display(), target.display())
        })?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::copy_layout;

    #[test]
    fn temporary_layout_copy_reads_selected_symlinks_without_modifying_targets() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let external_project = tempfile::NamedTempFile::new().unwrap();
        let external_board = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(external_project.path(), "project").unwrap();
        std::fs::write(external_board.path(), "board").unwrap();
        symlink(
            external_project.path(),
            source.path().join("layout.kicad_pro"),
        )
        .unwrap();
        symlink(
            external_board.path(),
            source.path().join("layout.kicad_pcb"),
        )
        .unwrap();
        symlink(external_board.path(), source.path().join("unrelated-link")).unwrap();

        copy_layout(source.path(), destination.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(external_project.path()).unwrap(),
            "project"
        );
        assert_eq!(
            std::fs::read_to_string(external_board.path()).unwrap(),
            "board"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("layout.kicad_pro")).unwrap(),
            "project"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("layout.kicad_pcb")).unwrap(),
            "board"
        );
        assert!(!destination.path().join("unrelated-link").exists());
    }

    #[test]
    fn temporary_layout_copy_rejects_unusable_selected_files() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("layout.kicad_pro"), "project").unwrap();
        symlink("missing", source.path().join("layout.kicad_pcb")).unwrap();
        let error = copy_layout(source.path(), destination.path()).unwrap_err();
        assert!(error.to_string().contains("failed to inspect layout file"));

        std::fs::remove_file(source.path().join("layout.kicad_pcb")).unwrap();
        std::fs::create_dir(source.path().join("layout.kicad_pcb")).unwrap();
        let error = copy_layout(source.path(), destination.path()).unwrap_err();
        assert!(error.to_string().contains("not a regular file"));
    }
}
