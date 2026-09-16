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
        // Trust the on-disk layout without touching it: resolve the existing
        // board file and export it directly.
        let layout = crate::layout::resolve_existing_layout(&layout_args.file, &design.schematic)?;
        let pcb_file = layout
            .pcb_file_abs
            .as_deref()
            .with_context(|| format!("{} does not declare a layout", args.file.display()))?;

        return export_ipc(pcb_file);
    }
    // DFM regenerates the layout as an intermediate artifact. Snapshot a
    // pre-existing layout directory so the unhydrated regeneration below does
    // not clobber the user's hydrated files; a layout created from scratch is
    // left in place, as today.
    let snapshot = LayoutSnapshot::capture(&design.schematic)?;
    let export = (|| {
        let layout = crate::layout::apply_prepared(&layout_args, design)?;
        let pcb_file = layout
            .pcb_file_abs
            .as_deref()
            .with_context(|| format!("{} does not declare a layout", args.file.display()))?
            .to_path_buf();

        export_ipc(&pcb_file)
    })();
    // The export error takes precedence, but a failed restore (which leaves
    // regenerated files behind) must still surface.
    match (export, snapshot.restore()) {
        (Ok(export), Ok(())) => Ok(export),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(restore)) => Err(restore),
        (Err(error), Err(restore)) => Err(anyhow::anyhow!(
            "{error:#}; also failed to restore the layout directory: {restore:#}"
        )),
    }
}

/// Export a board file to a temporary IPC-2581 document for checking.
fn export_ipc(pcb_file: &std::path::Path) -> Result<(tempfile::TempDir, PathBuf)> {
    let temporary_dir = tempfile::tempdir().context("failed to create temporary DFM directory")?;
    let ipc_path = temporary_dir.path().join("ipc2581.xml");
    crate::release::export_ipc2581(pcb_file, &ipc_path)?;
    Ok((temporary_dir, ipc_path))
}

/// A pre-existing layout directory preserved across DFM's intermediate
/// regeneration, or nothing when DFM creates the layout from scratch.
struct LayoutSnapshot {
    directory: Option<PathBuf>,
    backup: Option<tempfile::TempDir>,
}

impl LayoutSnapshot {
    fn capture(schematic: &pcb_sch::Schematic) -> Result<Self> {
        let directory = pcb_layout::utils::resolve_layout_dir(schematic)?;
        let backup = directory
            .as_deref()
            .filter(|directory| directory.exists())
            .map(|directory| {
                // Stage the backup beside the layout directory so restoring
                // it is a same-filesystem rename.
                let parent = directory.parent().with_context(|| {
                    format!("layout directory {} has no parent", directory.display())
                })?;
                let backup =
                    tempfile::tempdir_in(parent).context("failed to snapshot layout directory")?;
                copy_dir(directory, backup.path())?;
                Ok::<_, anyhow::Error>(backup)
            })
            .transpose()?;
        Ok(Self { directory, backup })
    }

    fn restore(self) -> Result<()> {
        let (Some(directory), Some(backup)) = (self.directory, self.backup) else {
            return Ok(());
        };
        // Relinquish automatic cleanup before touching the live directory:
        // if the restore fails halfway, the backup survives and the error
        // below tells the user where to find it.
        let backup_path = backup.keep();
        let restore_context = || {
            format!(
                "failed to restore layout directory {}; the pre-existing contents are preserved at {}",
                directory.display(),
                backup_path.display(),
            )
        };
        if directory.exists() {
            std::fs::remove_dir_all(&directory).with_context(restore_context)?;
        }
        // Persist the backup in place of the regenerated directory with a
        // same-filesystem rename.
        std::fs::rename(&backup_path, &directory).with_context(restore_context)?;
        Ok(())
    }
}

fn copy_dir(source: &std::path::Path, destination: &std::path::Path) -> Result<()> {
    let entries = walkdir::WalkDir::new(source)
        .min_depth(1)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to scan {}", source.display()))?;
    for entry in entries {
        let relative = entry
            .path()
            .strip_prefix(source)
            .with_context(|| format!("failed to relativize {}", entry.path().display()))?;
        let target = destination.join(relative);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
        } else if file_type.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else if file_type.is_symlink() {
            copy_symlink(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_symlink(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    let link = std::fs::read_link(source)
        .with_context(|| format!("failed to read link {}", source.display()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::os::unix::fs::symlink(link, target)
        .with_context(|| format!("failed to create link {}", target.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_symlink(source: &std::path::Path, target: &std::path::Path) -> Result<()> {
    // Layout directories do not use symlinks; fail closed rather than
    // silently dropping one during the snapshot round trip.
    anyhow::bail!("cannot snapshot symbolic link {}", source.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_failure_preserves_the_backup_and_reports_where() {
        let parent = tempfile::tempdir().unwrap();
        // A missing destination parent makes rename fail regardless of the
        // test process's permissions (CI may run as root).
        let live = parent.path().join("missing").join("layout");
        let backup = tempfile::tempdir_in(parent.path()).unwrap();
        std::fs::write(backup.path().join("board.kicad_pcb"), "original").unwrap();

        let snapshot = LayoutSnapshot {
            directory: Some(live.clone()),
            backup: Some(backup),
        };
        let error = snapshot.restore().unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("preserved at"),
            "restore error reports the surviving backup: {message}"
        );
        // The backup persists with its contents.
        let survivors = std::fs::read_dir(parent.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path != &live)
            .collect::<Vec<_>>();
        assert_eq!(survivors.len(), 1, "exactly the backup survives");
        assert_eq!(
            std::fs::read(survivors[0].join("board.kicad_pcb")).unwrap(),
            b"original"
        );
    }
}
