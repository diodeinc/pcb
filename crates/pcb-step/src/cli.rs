//! The `export` command line shared by every writer of a [`Scene`]:
//! `pcb step export` and `pcb gltf export` take the same arguments, as
//! `kicad-cli pcb export step` and `glb` do, except which copper to export:
//! STEP takes it as [`CopperArgs`], GLB always exports outer copper.
//!
//! [`Scene`]: crate::scene::Scene

use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};

use crate::{Board, ModelResolver, Options, Origin, OutlinePolicy, ProxyMode, Report};

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
    /// Read models that are not embedded from files under DIR (repeatable).
    /// Without this, `--model-var` and `--project-models`, only the board's
    /// embedded models are read and the board cannot make the exporter read
    /// any other file.
    #[arg(long, value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    model_dir: Vec<PathBuf>,
    /// Define a model path variable, e.g. KICAD9_3DMODEL_DIR=/usr/share/kicad/3dmodels
    /// (repeatable). The directory is also read from.
    #[arg(long, value_name = "NAME=DIR", value_parser = parse_model_var)]
    model_var: Vec<(String, PathBuf)>,
    /// Read models from the board's own directory, as KiCad's ${KIPRJMOD}
    #[arg(long)]
    project_models: bool,
    /// Write the per-footprint model coverage to this JSON file, even when
    /// the export fails
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    coverage: Option<PathBuf>,
    /// Fail, after writing the output, if a model a footprint names is
    /// absent, unsupported or failed
    #[arg(long)]
    require_complete: bool,
}

#[derive(clap::Args, Clone, Default)]
pub struct VisualArgs {
    /// Give a board whose Edge.Cuts is missing or unusable a body that is
    /// the rounded hull of what is on it, instead of failing. Unfinished and
    /// module boards; never for manufacturing output.
    #[arg(long)]
    synthesize_outline: bool,
    /// Draw a box over the pads of every footprint whose model is named but
    /// cannot be drawn
    #[arg(long, conflicts_with = "footprint_only")]
    proxy_missing_models: bool,
    /// Draw a box over the pads of every footprint and no models, without
    /// reading any
    #[arg(long)]
    footprint_only: bool,
    /// Export copper on inner layers too (outer layers only by default)
    #[arg(long)]
    include_inner_copper: bool,
}

/// Which copper to export.
#[derive(clap::Args, Clone, Copy)]
pub struct CopperArgs {
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
}

impl CopperArgs {
    /// Pads, tracks, vias and zones on the outer layers.
    pub const OUTER: Self = Self {
        include_pads: true,
        include_tracks: true,
        include_zones: true,
        include_inner_copper: false,
    };
}

impl ExportArgs {
    fn resolver(&self) -> Result<ModelResolver> {
        let mut resolver = ModelResolver::default();
        if self.project_models {
            let dir = std::path::absolute(&self.board)
                .ok()
                .and_then(|b| b.parent().map(Path::to_path_buf))
                .context("cannot find the board's directory")?;
            resolver = resolver.define("KIPRJMOD", &dir)?;
        }
        for dir in &self.model_dir {
            resolver = resolver
                .allow_root(dir)
                .with_context(|| format!("cannot use model directory {}", dir.display()))?;
        }
        for (name, dir) in &self.model_var {
            resolver = resolver
                .define(name, dir)
                .with_context(|| format!("cannot use model directory {}", dir.display()))?;
        }
        Ok(resolver)
    }

    fn options(&self, copper: CopperArgs, visual: Option<&VisualArgs>) -> Result<Options> {
        let visual_args = visual.cloned().unwrap_or_default();
        Ok(Options {
            board_body: !self.no_board_body,
            components: !self.no_components,
            cut_vias: self.cut_vias_in_body,
            visual: visual.is_some(),
            pads: copper.include_pads,
            tracks: copper.include_tracks,
            zones: copper.include_zones,
            inner_copper: copper.include_inner_copper || visual_args.include_inner_copper,
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
            models: self.resolver()?,
            outline: if visual_args.synthesize_outline {
                OutlinePolicy::Synthesize
            } else {
                OutlinePolicy::Strict
            },
            proxies: if visual_args.footprint_only {
                ProxyMode::All
            } else if visual_args.proxy_missing_models {
                ProxyMode::Missing
            } else {
                ProxyMode::Off
            },
        })
    }

    /// Export the board and its `copper` with `write`, to the output path
    /// or the board path with `extension`. The assembly is named after the
    /// board file and `${NAME}` text variables come from the project file
    /// beside it. The output is replaced atomically, so a failure leaves any
    /// previous output alone. Warnings are printed, whether or not the export
    /// succeeds; models that could not be read fail the command once the
    /// file is written.
    pub fn run(
        &self,
        extension: &str,
        copper: CopperArgs,
        visual: Option<&VisualArgs>,
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
        let options = self.options(copper, visual)?;
        let data =
            fs::read(board).with_context(|| format!("Failed to read {}", board.display()))?;
        let any_copper = options.pads || options.tracks || options.zones;
        let graphics = options.silkscreen || options.soldermask || options.visual;
        let parsed = Board::parse_with(&data, any_copper, graphics)
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
        if let Some(path) = &self.coverage {
            let mut doc = report.coverage.to_json();
            if let Err(err) = &written {
                doc["complete"] = false.into();
                doc["error"] = format!("{err:#}").into();
            }
            fs::write(path, serde_json::to_vec_pretty(&doc)?)
                .with_context(|| format!("Failed to write {}", path.display()))?;
        }
        written.with_context(|| format!("Failed to export {}", board.display()))?;
        if report.failed_models > 0 {
            bail!(
                "{} written, but {} model(s) could not be read",
                output.display(),
                report.failed_models
            );
        }
        if self.require_complete && !report.coverage.is_complete() {
            let missing: Vec<String> = report
                .coverage
                .incomplete()
                .map(|m| format!("{} ({})", m.reference, m.model))
                .collect();
            bail!(
                "{} written, but {} models are missing, e.g. {}",
                output.display(),
                missing.len(),
                missing[..missing.len().min(5)].join(", ")
            );
        }
        Ok(())
    }
}

fn parse_model_var(value: &str) -> Result<(String, PathBuf), String> {
    let (name, dir) = value
        .split_once('=')
        .ok_or_else(|| format!("bad model variable {value}; expected NAME=DIR"))?;
    if name.is_empty() || dir.is_empty() {
        return Err(format!("bad model variable {value}; expected NAME=DIR"));
    }
    Ok((name.to_owned(), PathBuf::from(dir)))
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
