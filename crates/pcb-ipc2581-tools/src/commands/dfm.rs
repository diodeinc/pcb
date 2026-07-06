//! Manufacturability screening of exported Gerber geometry.
//!
//! Runs the normal Gerber export pipeline in memory, composes each layer's
//! final filled image, and reports features and gaps narrower than the
//! fabrication minimum — the slivers a manufacturer's DFM check would flag.

use std::path::Path;

use anyhow::{Context, Result, bail};
use pcb_ir::dialects::ipc::View;
use pcb_ir::geom::region::rings_from_contours;
use pcb_ir::geom::{ContourSet, FillRule, dfm};

use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

pub fn execute(file: &Path, view: View, min_width_mm: f64) -> Result<()> {
    if !(min_width_mm.is_finite() && min_width_mm > 0.0) {
        bail!("minimum width must be positive; got {min_width_mm}");
    }
    let content = file_utils::load_ipc_file(file)?;
    let ipc = Ipc2581::parse(&content).context("Failed to parse IPC-2581 file")?;
    let files = crate::gerber::build_gerber_x2_files(&ipc, view)?;

    let mut findings = 0usize;
    for gerber_file in &files {
        let Some(kind) = layer_kind(&gerber_file.layer) else {
            continue;
        };
        let region = compose_layer(&gerber_file.contents)
            .with_context(|| format!("failed to compose {}", gerber_file.filename))?;
        let thin = dfm::thin_features(&region, min_width_mm);
        let gaps = dfm::thin_gaps(&region, min_width_mm);
        findings += thin.len() + gaps.len();
        report_layer(&gerber_file.filename, kind, &thin, &gaps);
    }

    if findings > 0 {
        bail!("DFM check found {findings} feature(s) narrower than {min_width_mm} mm");
    }
    eprintln!("✓ No features or gaps narrower than {min_width_mm} mm");
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum LayerKind {
    Copper,
    Soldermask,
    Paste,
}

impl LayerKind {
    fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Copper => ("copper sliver", "clearance sliver"),
            Self::Soldermask => ("mask opening sliver", "mask web sliver"),
            Self::Paste => ("paste sliver", "paste gap sliver"),
        }
    }
}

fn layer_kind(layer: &gerberx2::GerberLayer) -> Option<LayerKind> {
    let function = layer
        .file_attributes
        .iter()
        .find(|attribute| attribute.name == ".FileFunction")?
        .fields
        .first()?;
    match function.as_str() {
        "Copper" => Some(LayerKind::Copper),
        "Soldermask" => Some(LayerKind::Soldermask),
        "Paste" => Some(LayerKind::Paste),
        _ => None,
    }
}

/// Compose a written Gerber layer back into its final filled image.
fn compose_layer(contents: &str) -> Result<ContourSet> {
    let gerber = gerberx2::GerberX2::parse(contents)?;
    let doc = gerberx2::geometry::extract_document(&gerber);
    let mask = pcb_ir::dialects::artwork::compose_to_mask(&doc);
    let mut rings = Vec::new();
    for layer in &mask.layers {
        for shape in mask.shapes(layer) {
            rings.extend(rings_from_contours(&mask.arena.path_contours(shape)));
        }
    }
    Ok(ContourSet::new(rings, FillRule::NonZero, 1e-4))
}

fn report_layer(filename: &str, kind: LayerKind, thin: &[dfm::ThinPiece], gaps: &[dfm::ThinPiece]) {
    let (thin_label, gap_label) = kind.labels();
    if thin.is_empty() && gaps.is_empty() {
        eprintln!("{filename}: ok");
        return;
    }
    eprintln!(
        "{filename}: {} {thin_label}(s), {} {gap_label}(s)",
        thin.len(),
        gaps.len()
    );
    for (label, pieces) in [(thin_label, thin), (gap_label, gaps)] {
        for piece in pieces {
            eprintln!(
                "  {label}: {:.3} mm wide, {:.2} mm long at [{:.3},{:.3}]..[{:.3},{:.3}]",
                piece.width_mm,
                piece.length_mm,
                piece.bbox.min.x,
                piece.bbox.min.y,
                piece.bbox.max.x,
                piece.bbox.max.y,
            );
        }
    }
}
