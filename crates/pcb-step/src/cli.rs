//! The `export` command line shared by every writer of a [`Scene`]:
//! `pcb step export` and `pcb gltf export` take the same arguments, as
//! `kicad-cli pcb export step` and `glb` do.
//!
//! [`Scene`]: crate::scene::Scene

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};

use crate::{Board, Options, Origin, Report};

#[derive(clap::Args)]
pub struct ExportArgs {
    /// KiCad board file to export
    #[arg(value_hint = clap::ValueHint::FilePath)]
    board: PathBuf,
    /// Output file; defaults to the board path with the format's extension
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
            name: self
                .board
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("board")
                .to_owned(),
            text_variables: project_text_variables(&self.board.with_extension("kicad_pro")),
        }
    }

    /// Export the board with `write`, to the output path or the board path
    /// with `extension`. The assembly is named after the board file and
    /// `${NAME}` text variables come from the project file beside it. The
    /// output is replaced atomically, so a failure leaves any previous
    /// output alone. Warnings are printed, whether or not the export
    /// succeeds; models that could not be read fail the command once the
    /// file is written.
    pub fn run(
        &self,
        extension: &str,
        write: impl FnOnce(&Board, &Options, &mut dyn Write, &mut Report) -> Result<()>,
    ) -> Result<()> {
        let board = &self.board;
        let output = self
            .output
            .clone()
            .unwrap_or_else(|| board.with_extension(extension));
        if output.exists() && fs::canonicalize(board).ok() == fs::canonicalize(&output).ok() {
            bail!("Output {} is the board itself", output.display());
        }
        let options = self.options();
        let data =
            fs::read(board).with_context(|| format!("Failed to read {}", board.display()))?;
        let copper = options.pads || options.tracks || options.zones;
        let graphics = options.silkscreen || options.soldermask;
        let parsed = Board::parse_with(&data, copper, graphics)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("Failed to parse {}", board.display()))?;

        let mut report = Report::default();
        let written = AtomicFile::new(&output, OverwriteBehavior::AllowOverwrite)
            .write(|file| {
                let mut sink = BufWriter::with_capacity(1 << 20, file);
                write(&parsed, &options, &mut sink, &mut report)?;
                sink.flush()?;
                anyhow::Ok(())
            })
            .map_err(|err| match err {
                atomicwrites::Error::Internal(err) => err.into(),
                atomicwrites::Error::User(err) => err,
            });
        // Warnings explain a failed export too, such as models that could
        // not be read leaving nothing to export.
        for warning in &report.warnings {
            eprintln!("warning: {warning}");
        }
        written.with_context(|| format!("Failed to export {}", board.display()))?;
        if report.failed_models > 0 {
            bail!(
                "{} written, but {} model(s) could not be read",
                output.display(),
                report.failed_models
            );
        }
        Ok(())
    }
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
