//! `pcb step`: STEP export of a KiCad board through `pcb-step`. Models
//! come from the board's embedded files; nothing is looked up on disk.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use pcb_step::{Board, Options, Origin, Report};

#[derive(Args)]
pub struct StepArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Export a board to a STEP assembly: body, drills, models, silkscreen and solder mask
    Export(ExportArgs),
}

#[derive(Args)]
pub struct ExportArgs {
    /// KiCad board file to export
    #[arg(value_hint = clap::ValueHint::FilePath)]
    board: PathBuf,
    /// Output STEP file; defaults to the board path with a .step extension
    #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
    output: Option<PathBuf>,
    /// Exclude the board body
    #[arg(long)]
    no_board_body: bool,
    /// Exclude footprint models
    #[arg(long, alias = "board-only")]
    no_components: bool,
    /// Cut via drills into the board body
    #[arg(long)]
    cut_vias_in_body: bool,
    /// Export pad copper and plating
    #[arg(long)]
    include_pads: bool,
    /// Export tracks, via rings and via barrels
    #[arg(long)]
    include_tracks: bool,
    /// Export zone fills
    #[arg(long)]
    include_zones: bool,
    /// Export copper on inner layers too (outer layers only by default)
    #[arg(long)]
    include_inner_copper: bool,
    /// Leave out the silkscreen faces
    #[arg(long)]
    no_silkscreen: bool,
    /// Leave out the solder mask faces
    #[arg(long)]
    no_soldermask: bool,
    /// Exclude models of DNP footprints
    #[arg(long)]
    no_dnp: bool,
    /// Exclude models of footprints with no SMD/THT attribute
    #[arg(long)]
    no_unspecified: bool,
    /// Comma-separated reference designator globs
    #[arg(long, value_delimiter = ',', value_name = "GLOBS")]
    component_filter: Vec<String>,
    /// Use the drill/place origin as the output origin
    #[arg(long, conflicts_with_all = ["grid_origin", "user_origin"])]
    drill_origin: bool,
    /// Use the grid origin as the output origin
    #[arg(long, conflicts_with = "user_origin")]
    grid_origin: bool,
    /// Use a user origin in millimetres, e.g. 25.4x25.4
    #[arg(long, value_name = "XxY", value_parser = parse_user_origin)]
    user_origin: Option<(f64, f64)>,
}

impl ExportArgs {
    fn options(&self) -> Options {
        Options {
            board_body: !self.no_board_body,
            components: !self.no_components,
            cut_vias: self.cut_vias_in_body,
            pads: self.include_pads,
            tracks: self.include_tracks,
            zones: self.include_zones,
            inner_copper: self.include_inner_copper,
            silkscreen: !self.no_silkscreen,
            soldermask: !self.no_soldermask,
            include_dnp: !self.no_dnp,
            include_unspecified: !self.no_unspecified,
            component_filter: self.component_filter.clone(),
            origin: if self.drill_origin {
                Origin::Drill
            } else if self.grid_origin {
                Origin::Grid
            } else if let Some((x, y)) = self.user_origin {
                Origin::User { x, y }
            } else {
                Origin::Board
            },
            ..Options::default()
        }
    }
}

pub fn execute(args: StepArgs) -> Result<()> {
    let Commands::Export(args) = args.command;
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| args.board.with_extension("step"));
    let report = export_board(&args.board, &output, args.options())?;
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if report.failed_models > 0 {
        bail!(
            "{} written, but {} model(s) could not be read",
            output.display(),
            report.failed_models
        );
    }
    Ok(())
}

/// Export `board` to `output`. The assembly is named after the board file
/// and `${NAME}` text variables come from the project file beside it. The
/// file is written beside the output and renamed over it once complete,
/// so a failure leaves any previous output alone.
fn export_board(board: &Path, output: &Path, mut options: Options) -> Result<Report> {
    if output.exists() && fs::canonicalize(board).ok() == fs::canonicalize(output).ok() {
        bail!("Output {} is the board itself", output.display());
    }
    let data = fs::read(board).with_context(|| format!("Failed to read {}", board.display()))?;
    let copper = options.pads || options.tracks || options.zones;
    let graphics = options.silkscreen || options.soldermask;
    let parsed = Board::parse_with(&data, copper, graphics)
        .map_err(anyhow::Error::from)
        .with_context(|| format!("Failed to parse {}", board.display()))?;
    options.name = board
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("board")
        .to_owned();
    options.text_variables = project_text_variables(&board.with_extension("kicad_pro"));

    let partial = output.with_extension("step.part");
    let file = fs::File::create(&partial)
        .with_context(|| format!("Failed to create {}", partial.display()))?;
    let mut sink = BufWriter::with_capacity(1 << 20, file);
    pcb_step::export(&parsed, &options, &mut sink)
        .map_err(anyhow::Error::from)
        .and_then(|report| sink.flush().map(|()| report).map_err(Into::into))
        .and_then(|report| {
            fs::rename(&partial, output)
                .map(|()| report)
                .map_err(Into::into)
        })
        .map_err(|err| {
            let _ = fs::remove_file(&partial);
            err.context(format!("Failed to export {}", board.display()))
        })
}

fn parse_user_origin(value: &str) -> Result<(f64, f64), String> {
    let (x, y) = value
        .split_once('x')
        .ok_or_else(|| format!("bad origin {value}; expected XxY in millimetres"))?;
    let parse = |s: &str| match s.trim().parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(format!("bad origin {value}")),
    };
    Ok((parse(x)?, parse(y)?))
}

/// The `text_variables` of a KiCad project file: a flat object of strings.
fn project_text_variables(project: &Path) -> Vec<(String, String)> {
    let Ok(text) = fs::read_to_string(project) else {
        return Vec::new();
    };
    let Ok(project) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    project
        .get("text_variables")
        .and_then(|vars| vars.as_object())
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
        .collect()
}
