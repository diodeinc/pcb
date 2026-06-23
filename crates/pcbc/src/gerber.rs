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
        } => render(&file, output.as_deref(), format),
    }
}

fn render(file: &Path, output: Option<&Path>, format: RenderFormat) -> Result<()> {
    let target = resolve_target(output, format)?;
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("Failed to parse Gerber file {}", file.display()))?;
    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    pcb_ir::dialects::gerber::process::compose_for_rendering(&mut geometry);

    for diagnostic in &geometry.diagnostics {
        eprintln!("warning: {}", diagnostic.message);
    }

    match target {
        RenderTarget::Svg => {
            let svg = pcb_ir::dialects::gerber::svg::render_svg(&geometry);
            if let Some(output) = output {
                std::fs::write(output, svg)
                    .with_context(|| format!("Failed to write SVG to {}", output.display()))?;
                println!("✓ Gerber layer rendered to {}", output.display());
            } else {
                print!("{svg}");
            }
        }
        RenderTarget::Png => {
            let png = pcb_ir::dialects::gerber::raster::render_png(&geometry)
                .map_err(gerberx2::GerberError::Render)?;
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
        RenderTarget::Terminal => {
            pcb_ir::dialects::gerber::terminal::render_to_terminal(&geometry)
                .map_err(gerberx2::GerberError::Render)?;
        }
    }

    Ok(())
}

fn resolve_target(output: Option<&Path>, format: RenderFormat) -> Result<RenderTarget> {
    match format {
        RenderFormat::Auto => {
            if let Some(output) = output {
                infer_format_from_output(output)
            } else if pcb_ir::dialects::mask::can_render_to_terminal() {
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
