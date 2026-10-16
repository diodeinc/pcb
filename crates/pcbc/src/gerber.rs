use pcb_ir::geom::{GeometryAccuracy, Resolution};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use pcb_ipc2581_tools::commands::render::{RenderTarget, render_artwork};
use pcb_ipc2581_tools::geometry::composite::gerber::{FabricationFile, gerber_composite};
use pcb_ipc2581_tools::{BoardSide, RenderFormat};

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
    /// Render one Gerber X2 layer, or the finished board a directory or zip of
    /// fabrication files describes, to SVG, PNG, or terminal graphics
    Render {
        /// Gerber layer file, or a directory or zip of Gerber, drill and job files
        #[arg(value_hint = clap::ValueHint::AnyPath)]
        file: PathBuf,
        /// Render only this side of the finished board
        #[arg(long)]
        side: Option<BoardSide>,
        /// Output file path. If omitted, auto renders to the terminal when possible.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Render format. Auto infers SVG/PNG from output extension or uses terminal graphics.
        #[arg(short, long, default_value = "auto")]
        format: RenderFormat,
    },
}

pub fn execute(args: GerberArgs, resolution: Resolution) -> Result<()> {
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
            resolution,
        ),
        Commands::Normalize { file, output } => {
            normalize(&file, output.as_deref(), resolution.accuracy)
        }
        Commands::Render {
            file,
            side,
            output,
            format,
        } => render(&file, side, output.as_deref(), format, resolution),
    }
}

fn normalize(file: &Path, output: Option<&Path>, accuracy: GeometryAccuracy) -> Result<()> {
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("failed to parse Gerber file {}", file.display()))?;
    let normalized = gerberx2::from_artwork::normalize_layer(&gerber, accuracy)
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
    resolution: Resolution,
) -> Result<()> {
    let reference_geometry = load_geometry(reference, resolution.accuracy)?;
    let candidate_geometry = load_geometry(candidate, resolution.accuracy)?;
    let report = pcb_ir::dialects::artwork::compare::compare_documents(
        &reference_geometry,
        &candidate_geometry,
        pcb_ir::dialects::artwork::compare::CompareTolerance {
            bbox_mm: bbox_tolerance_mm,
            area_mm2: area_tolerance_mm2,
        },
        resolution,
    )?;

    println!(
        "reference area {:.6} mm², candidate area {:.6} mm², delta {:.6} mm²",
        report.reference.area_mm2,
        report.candidate.area_mm2,
        report.candidate.area_mm2 - report.reference.area_mm2
    );
    println!(
        "reference bbox {}, candidate bbox {}",
        format_bbox(report.reference.bbox),
        format_bbox(report.candidate.bbox)
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
            "{label} component {}: area {:.6} mm², bbox {}",
            index + 1,
            component.area_mm2,
            format_bbox(component.bbox)
        );
    }
}

fn format_bbox(bbox: pcb_ir::geom::BBox) -> String {
    format!(
        "[{:.6},{:.6}]..[{:.6},{:.6}]",
        bbox.min.x, bbox.min.y, bbox.max.x, bbox.max.y
    )
}

/// Render the extracted artwork as it is: flashes, strokes and polarity runs
/// draw natively, so viewing a layer never composes it.
fn render(
    file: &Path,
    side: Option<BoardSide>,
    output: Option<&Path>,
    format: RenderFormat,
    resolution: Resolution,
) -> Result<()> {
    let is_zip = file
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    if file.is_dir() || is_zip {
        return render_board(file, side, output, format, resolution);
    }
    if side.is_some() {
        bail!("--side renders a finished board; pass a directory or zip of fabrication files");
    }
    let target = RenderTarget::resolve(output, format, "Gerber layer")?;
    render_artwork(
        &load_geometry(file, resolution.accuracy)?,
        target,
        output,
        "Gerber layer",
        &pcb_ir::render::RenderOptions::default().with_accuracy(resolution.accuracy),
    )
}

/// Render the finished board from its fabrication files, as `pcb ipc2581
/// render` renders it from a design.
fn render_board(
    source: &Path,
    side: Option<BoardSide>,
    output: Option<&Path>,
    format: RenderFormat,
    resolution: Resolution,
) -> Result<()> {
    let sides = side.map_or(vec![BoardSide::Top, BoardSide::Bottom], |side| vec![side]);
    let subject = format!(
        "Gerber {} view",
        sides
            .iter()
            .map(BoardSide::to_string)
            .collect::<Vec<_>>()
            .join(" and ")
    );
    let target = RenderTarget::resolve(output, format, &subject)?;
    let files = read_fabrication_files(source)?;
    let composite = gerber_composite(&files, &sides, resolution)
        .with_context(|| format!("Failed to render {}", source.display()))?;
    let options = pcb_ir::render::RenderOptions::default()
        .with_accuracy(resolution.accuracy)
        .with_styles(composite.styles)
        .with_viewport(composite.viewport);
    render_artwork(&composite.artwork, target, output, &subject, &options)
}

/// Every file in a directory, or in a zip archive, by its path within it.
fn read_fabrication_files(source: &Path) -> Result<Vec<FabricationFile>> {
    let read = |path: &Path| {
        std::fs::read(path).with_context(|| format!("Failed to read {}", path.display()))
    };
    if source.is_dir() {
        let mut files = Vec::new();
        let mut directories = vec![source.to_path_buf()];
        while let Some(directory) = directories.pop() {
            let entries = std::fs::read_dir(&directory)
                .with_context(|| format!("Failed to read {}", directory.display()))?;
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    files.push(FabricationFile {
                        name: path.strip_prefix(source)?.to_string_lossy().into_owned(),
                        contents: read(&path)?,
                    });
                }
            }
        }
        return Ok(files);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(read(source)?))
        .with_context(|| format!("Failed to open {} as a zip archive", source.display()))?;
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if !entry.is_file() {
            continue;
        }
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut contents)
            .with_context(|| format!("Failed to read {} from the archive", entry.name()))?;
        files.push(FabricationFile {
            name: entry.name().to_owned(),
            contents,
        });
    }
    Ok(files)
}

fn load_geometry(
    file: &Path,
    accuracy: GeometryAccuracy,
) -> Result<gerberx2::geometry::GerberArtworkDocument> {
    let gerber = gerberx2::GerberX2::parse_file(file)
        .with_context(|| format!("Failed to parse Gerber file {}", file.display()))?;
    Ok(gerberx2::geometry::extract_document(&gerber, accuracy)?)
}
