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
    /// Compare two Gerber X2 layers after lowering to final rendered geometry
    Compare {
        /// Reference Gerber layer file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        reference: PathBuf,
        /// Candidate Gerber layer file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        candidate: PathBuf,
        /// Bounding-box tolerance in millimeters
        #[arg(long, default_value_t = 0.01)]
        bbox_tolerance_mm: f64,
        /// Area and symmetric-difference tolerance in square millimeters
        #[arg(long, default_value_t = 0.01)]
        area_tolerance_mm2: f64,
    },
    /// Re-emit a Gerber X2 layer through the pcb-ir artwork pipeline
    Normalize {
        /// Gerber layer file to normalize
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output file path; prints to stdout when omitted
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
    },
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
        Commands::Compare {
            reference,
            candidate,
            bbox_tolerance_mm,
            area_tolerance_mm2,
        } => compare(
            &reference,
            &candidate,
            bbox_tolerance_mm,
            area_tolerance_mm2,
        ),
        Commands::Normalize { file, output } => normalize(&file, output.as_deref()),
        Commands::Render {
            file,
            output,
            format,
        } => render(&file, output.as_deref(), format),
    }
}

fn normalize(file: &Path, output: Option<&Path>) -> Result<()> {
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("failed to parse Gerber file {}", file.display()))?;
    let normalized = gerberx2::from_artwork::normalize_layer(&gerber)
        .with_context(|| format!("failed to normalize Gerber file {}", file.display()))?;
    match output {
        Some(path) => std::fs::write(path, normalized)
            .with_context(|| format!("failed to write {}", path.display()))?,
        None => print!("{normalized}"),
    }
    Ok(())
}

fn compare(
    reference: &Path,
    candidate: &Path,
    bbox_tolerance_mm: f64,
    area_tolerance_mm2: f64,
) -> Result<()> {
    let reference_geometry = load_geometry(reference)?;
    let candidate_geometry = load_geometry(candidate)?;
    let report = pcb_ir::dialects::artwork::compare::compare_documents(
        &reference_geometry,
        &candidate_geometry,
        pcb_ir::dialects::artwork::compare::CompareTolerance {
            bbox_mm: bbox_tolerance_mm,
            area_mm2: area_tolerance_mm2,
        },
    );

    println!(
        "reference area {:.6} mm², candidate area {:.6} mm², delta {:.6} mm²",
        report.reference.area_mm2,
        report.candidate.area_mm2,
        report.candidate.area_mm2 - report.reference.area_mm2
    );
    println!(
        "reference bbox [{:.6},{:.6}]..[{:.6},{:.6}], candidate bbox [{:.6},{:.6}]..[{:.6},{:.6}]",
        report.reference.bbox.min.x,
        report.reference.bbox.min.y,
        report.reference.bbox.max.x,
        report.reference.bbox.max.y,
        report.candidate.bbox.min.x,
        report.candidate.bbox.min.y,
        report.candidate.bbox.max.x,
        report.candidate.bbox.max.y
    );
    println!(
        "reference objects {}, paths {}; candidate objects {}, paths {}",
        report.reference.object_count,
        report.reference.path_count,
        report.candidate.object_count,
        report.candidate.path_count
    );
    println!(
        "reference-only area {:.6} mm², candidate-only area {:.6} mm², symmetric difference {:.6} mm²",
        report.difference.reference_only.area_mm2,
        report.difference.candidate_only.area_mm2,
        report.difference.symmetric_area_mm2
    );

    if report.is_match() {
        println!("✓ Gerber geometry matches within tolerance");
        Ok(())
    } else {
        for mismatch in &report.mismatches {
            println!("mismatch: {mismatch}");
        }
        print_difference_components("reference-only", &report.difference.reference_only);
        print_difference_components("candidate-only", &report.difference.candidate_only);
        bail!("Gerber geometry differs")
    }
}

fn print_difference_components(
    label: &str,
    summary: &pcb_ir::dialects::artwork::compare::DirectionalDifferenceSummary,
) {
    for (index, component) in summary.components.iter().take(12).enumerate() {
        println!(
            "{label} component {}: area {:.6} mm², bbox [{:.6},{:.6}]..[{:.6},{:.6}]",
            index + 1,
            component.area_mm2,
            component.bbox.min.x,
            component.bbox.min.y,
            component.bbox.max.x,
            component.bbox.max.y
        );
    }
}

fn render(file: &Path, output: Option<&Path>, format: RenderFormat) -> Result<()> {
    let target = resolve_target(output, format)?;
    let geometry = load_geometry(file)?;

    for diagnostic in &geometry.diagnostics {
        eprintln!("warning: {}", diagnostic.message);
    }

    match target {
        RenderTarget::Svg => {
            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry);
            let svg = pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::default());
            if let Some(output) = output {
                std::fs::write(output, svg)
                    .with_context(|| format!("Failed to write SVG to {}", output.display()))?;
                println!("✓ Gerber layer rendered to {}", output.display());
            } else {
                print!("{svg}");
            }
        }
        RenderTarget::Png => {
            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry);
            let png = pcb_ir::render::png(&mask, &pcb_ir::render::RenderOptions::default())
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
            let mask = pcb_ir::dialects::artwork::compose_to_mask(&geometry);
            pcb_ir::render::to_terminal(&mask, &pcb_ir::render::RenderOptions::default())
                .map_err(gerberx2::GerberError::Render)?;
        }
    }

    Ok(())
}

fn load_geometry(file: &Path) -> Result<gerberx2::geometry::GerberArtworkDocument> {
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("Failed to parse Gerber file {}", file.display()))?;
    Ok(gerberx2::geometry::extract_document(&gerber))
}

fn resolve_target(output: Option<&Path>, format: RenderFormat) -> Result<RenderTarget> {
    match format {
        RenderFormat::Auto => {
            if let Some(output) = output {
                infer_format_from_output(output)
            } else if pcb_ir::render::can_render_to_terminal() {
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
