//! PCB auto-routing command using local FreeRouting

use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use pcb_kicad::PythonScriptBuilder;
use pcb_layout::utils;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::file_walker;

/// Which router backend `pcb route` should use.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[value(rename_all = "kebab-case")]
pub enum RouteEngine {
    #[default]
    Freerouting,
}

#[derive(Args, Debug, Clone)]
#[command(about = "Auto-route a PCB using local FreeRouting")]
pub struct RouteArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Routing engine to use
    #[arg(long, value_enum, default_value_t = RouteEngine::Freerouting)]
    pub engine: RouteEngine,

    /// Don't open KiCad after routing
    #[arg(long)]
    pub no_open: bool,

    /// Routing timeout in minutes (default: 20, max: 60)
    #[arg(long, short = 't', default_value = "20")]
    pub timeout: u32,
}

pub fn execute(args: RouteArgs) -> Result<()> {
    file_walker::require_zen_file(&args.file)?;

    if args.timeout > 60 {
        anyhow::bail!("Timeout cannot exceed 60 minutes");
    }

    let (board_path, project_path) = resolve_board(&args.file)?;
    let board_name = board_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();

    crate::freerouting::execute(&args, &board_path, &project_path, &board_name)
}

/// Evaluate the .zen file, find its `.kicad_pcb` + `.kicad_pro`, and
/// validate that the board exists.
fn resolve_board(zen_path: &Path) -> Result<(PathBuf, PathBuf)> {
    let resolution_result = crate::resolve::resolve(Some(zen_path), false)?;

    let (output, diagnostics) =
        pcb_zen::run(zen_path, resolution_result, Default::default()).unpack();

    if diagnostics.has_errors() {
        anyhow::bail!("Failed to evaluate {}: build errors", zen_path.display());
    }

    let schematic = output.context("No schematic output from evaluation")?;

    let layout_dir = utils::resolve_layout_dir(&schematic)?
        .context("No layout path defined in schematic. Add layout=\"path\" to your module.")?;

    let kicad_files = utils::require_kicad_files(&layout_dir)?;
    let board_path = kicad_files.kicad_pcb();
    let project_path = kicad_files.kicad_pro;

    if !board_path.exists() {
        anyhow::bail!(
            "No layout found at {}\n\nRun {} first to generate the board.",
            board_path.display(),
            "pcb layout".yellow()
        );
    }

    Ok((board_path, project_path))
}

/// Import a Specctra SES session file into the board via `pcbnew`, filling
/// zones and saving the result.
pub(crate) fn import_ses(board_path: &Path, ses_path: &Path) -> Result<()> {
    let script = r#"
import pcbnew
import sys

brd_filename = sys.argv[1]
ses_filename = sys.argv[2]
brd = pcbnew.LoadBoard(brd_filename)
if not pcbnew.ImportSpecctraSES(brd, ses_filename):
    sys.exit("Failed to import SES file into board")

filler = pcbnew.ZONE_FILLER(brd)
if not filler.Fill(brd.Zones()):
    sys.exit("Failed to fill zones after SES import")

if not pcbnew.SaveBoard(brd_filename, brd):
    sys.exit("Failed to save board after SES import")
"#;

    PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .arg(ses_path.to_string_lossy())
        .run()
        .context("Failed to import SES file")?;

    Ok(())
}

pub(crate) fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{}:{:02}", mins, secs)
}
