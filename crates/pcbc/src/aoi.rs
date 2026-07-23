use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use crate::file_walker;

#[derive(Args, Debug)]
#[command(
    about = "Automated optical inspection: compare a board design against a photo of the assembled PCB",
    long_about = "Automated Optical Inspection (AOI) compares the expected board — rendered from a \
Zener design — against a captured photo of the physical PCB and flags component-level discrepancies \
such as missing, misplaced, rotated, or tombstoned parts.\n\n\
Note: the render-vs-photo comparison is a scaffold. Input handling and reporting are wired up, but \
the computer-vision diff step is not yet implemented and reports no findings."
)]
pub struct AoiArgs {
    /// Path to the .zen design describing the expected board
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub design: PathBuf,

    /// Captured photo of the assembled PCB to inspect
    #[arg(short = 'i', long = "image", value_name = "IMAGE", value_hint = clap::ValueHint::FilePath)]
    pub image: PathBuf,

    /// Write the inspection report as JSON to PATH, or '-' for stdout
    #[arg(short = 'o', long = "output", value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub output: Option<PathBuf>,

    /// Per-component difference threshold in [0.0, 1.0]; higher tolerates more visual deviation
    #[arg(
        short = 't',
        long = "threshold",
        value_name = "RATIO",
        default_value_t = 0.05
    )]
    pub threshold: f64,
}

/// Kind of component-level discrepancy the inspection can report.
#[allow(dead_code)] // Constructed once the CV diff step is implemented.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum DiscrepancyKind {
    Missing,
    Misplaced,
    Rotated,
    Tombstoned,
}

/// A single component-level finding.
#[allow(dead_code)] // Constructed once the CV diff step is implemented.
#[derive(Debug, Serialize)]
struct ComponentFinding {
    /// Component reference designator, e.g. "R1".
    reference: String,
    kind: DiscrepancyKind,
    detail: String,
}

/// Whether a report reflects a real inspection or a scaffold that has not yet
/// run the comparison. Distinguishes "no defects found" from "not inspected".
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum InspectionStatus {
    /// The render-vs-photo comparison is not implemented; `findings` is not a
    /// clean-pass result.
    NotImplemented,
    /// A full inspection ran and `findings` is authoritative.
    #[allow(dead_code)] // Set once the CV diff step is implemented.
    Completed,
}

/// Result of an AOI compare run.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AoiReport {
    status: InspectionStatus,
    design: PathBuf,
    captured_image: PathBuf,
    threshold: f64,
    findings: Vec<ComponentFinding>,
}

/// Expected board image rendered from the design.
struct ExpectedRender {
    source: PathBuf,
}

/// Captured photo loaded from disk.
struct CapturedImage {
    source: PathBuf,
}

pub fn execute(args: AoiArgs) -> Result<()> {
    if !(0.0..=1.0).contains(&args.threshold) {
        bail!(
            "--threshold must be between 0.0 and 1.0, got {}",
            args.threshold
        );
    }
    file_walker::require_zen_file(&args.design)?;

    eprintln!(
        "Inspecting {} against {}",
        args.design.display(),
        args.image.display()
    );

    // (a) Render/obtain the expected board image from the design.
    let expected = render_expected_image(&args.design)?;
    // (b) Load the captured photo of the physical board.
    let captured = load_captured_image(&args.image)?;
    // (c)+(d) Align, diff, and collect component-level discrepancies.
    let report = compare_render_to_photo(&expected, &captured, args.threshold)?;

    emit_report(&report, args.output.as_deref())
}

/// (a) Render the expected board image from the Zener design.
///
/// TODO: build the design and rasterize its layout (see `pcb layout` and the
/// `pcb-ir` render pipeline) into a reference image. For now this only records
/// the source path so the pipeline is wired end to end.
fn render_expected_image(design: &Path) -> Result<ExpectedRender> {
    Ok(ExpectedRender {
        source: design.to_path_buf(),
    })
}

/// (b) Load the captured photo of the assembled PCB.
///
/// TODO: decode the image into a pixel buffer for alignment. For now this
/// verifies the file is present and readable.
fn load_captured_image(image: &Path) -> Result<CapturedImage> {
    if !image.is_file() {
        bail!("Captured image not found: {}", image.display());
    }
    std::fs::File::open(image)
        .with_context(|| format!("Failed to open captured image {}", image.display()))?;
    Ok(CapturedImage {
        source: image.to_path_buf(),
    })
}

/// (c)+(d) Align the rendered board to the captured photo, diff them, and
/// collect component-level discrepancies (missing / misplaced / rotated /
/// tombstoned).
///
/// TODO: this is the core computer-vision step and is not yet implemented. A
/// real implementation would align the two images, compute a per-component
/// difference against `threshold`, and classify each discrepancy. No findings
/// are fabricated here.
fn compare_render_to_photo(
    expected: &ExpectedRender,
    captured: &CapturedImage,
    threshold: f64,
) -> Result<AoiReport> {
    eprintln!(
        "note: render-vs-photo comparison is not yet implemented; no components were inspected"
    );
    Ok(AoiReport {
        status: InspectionStatus::NotImplemented,
        design: expected.source.clone(),
        captured_image: captured.source.clone(),
        threshold,
        findings: Vec::new(),
    })
}

fn emit_report(report: &AoiReport, output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) if path == Path::new("-") => {
            println!("{}", serde_json::to_string_pretty(report)?);
        }
        Some(path) => {
            std::fs::write(path, serde_json::to_string_pretty(report)?)
                .with_context(|| format!("Failed to write report to {}", path.display()))?;
            eprintln!("✓ AOI report written to {}", path.display());
        }
        None => match report.status {
            InspectionStatus::NotImplemented => {
                println!(
                    "status: not implemented — the render-vs-photo comparison did not run; \
                     this is not a clean-pass result"
                );
            }
            InspectionStatus::Completed if report.findings.is_empty() => {
                println!("status: completed — no component discrepancies found");
            }
            InspectionStatus::Completed => {
                println!(
                    "status: completed — {} discrepancy(ies) found",
                    report.findings.len()
                );
                for finding in &report.findings {
                    println!(
                        "{}: {:?} — {}",
                        finding.reference, finding.kind, finding.detail
                    );
                }
            }
        },
    }
    Ok(())
}
