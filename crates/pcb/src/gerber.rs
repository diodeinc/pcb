use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

#[derive(Args)]
pub struct GerberArgs {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Render a single Gerber X2 layer to SVG, PNG, or terminal graphics
    Render {
        /// Gerber layer file to render
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output file path. If omitted, auto renders to the terminal when possible.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Render format. Auto infers SVG/PNG from output extension or uses terminal graphics.
        #[arg(short, long, default_value = "auto")]
        format: RenderFormat,
        /// Render pre-composition feature buckets for debugging geometry extraction.
        #[arg(long)]
        debug_geometry: bool,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum RenderFormat {
    Auto,
    Svg,
    Png,
}

enum RenderTarget {
    Svg,
    Png,
    Terminal,
}

pub fn execute(args: GerberArgs) -> Result<()> {
    match args.command {
        Commands::Render {
            file,
            output,
            format,
            debug_geometry,
        } => render(&file, output.as_deref(), format, debug_geometry),
    }
}

fn render(
    file: &Path,
    output: Option<&Path>,
    format: RenderFormat,
    debug_geometry: bool,
) -> Result<()> {
    let target = resolve_target(output, format)?;
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("Failed to parse Gerber file {}", file.display()))?;
    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    if !debug_geometry {
        gerberx2::geometry::process::process_document(&mut geometry);
    }

    for diagnostic in &geometry.diagnostics {
        eprintln!("warning: {}", diagnostic.message);
    }

    match target {
        RenderTarget::Svg => {
            let options = gerberx2::geometry::svg::SvgOptions {
                mode: if debug_geometry {
                    gerberx2::geometry::svg::RenderMode::Debug
                } else {
                    gerberx2::geometry::svg::RenderMode::Final
                },
                width_px: None,
                height_px: None,
            };
            let svg = gerberx2::geometry::svg::render_svg_with_options(&geometry, options);
            if let Some(output) = output {
                std::fs::write(output, svg)
                    .with_context(|| format!("Failed to write SVG to {}", output.display()))?;
                println!("✓ Gerber layer rendered to {}", output.display());
            } else {
                print!("{svg}");
            }
        }
        RenderTarget::Png => {
            let png = gerberx2::geometry::raster::render_png(&geometry)?;
            if let Some(output) = output {
                std::fs::write(output, png)
                    .with_context(|| format!("Failed to write PNG to {}", output.display()))?;
                println!("✓ Gerber layer rendered to {}", output.display());
            } else {
                std::io::stdout()
                    .lock()
                    .write_all(&png)
                    .context("Failed to write PNG to stdout")?;
            }
        }
        RenderTarget::Terminal => gerberx2::geometry::terminal::render_to_terminal(&geometry)?,
    }

    Ok(())
}

fn resolve_target(output: Option<&Path>, format: RenderFormat) -> Result<RenderTarget> {
    match format {
        RenderFormat::Auto => {
            if let Some(output) = output {
                infer_format_from_output(output)
            } else if gerberx2::geometry::terminal::can_render_to_terminal() {
                Ok(RenderTarget::Terminal)
            } else {
                bail!(
                    "Could not render Gerber layer to stdout; run from an interactive terminal or pass --output <path>.svg or <path>.png"
                )
            }
        }
        RenderFormat::Svg => Ok(RenderTarget::Svg),
        RenderFormat::Png => Ok(RenderTarget::Png),
    }
}

fn infer_format_from_output(output: &Path) -> Result<RenderTarget> {
    match output
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => Ok(RenderTarget::Svg),
        Some("png") => Ok(RenderTarget::Png),
        _ => bail!(
            "Could not infer Gerber render format from {}; pass --format svg or --format png",
            output.display()
        ),
    }
}
