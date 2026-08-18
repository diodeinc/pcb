//! Minimum feature width and minimum gap, by mathematical morphology.
//!
//! Let `M` be one layer's composed image and `B_ρ` the closed disk of
//! radius `ρ = L/2`. The morphological opening and closing of `M` with
//! `B_ρ` are
//!
//! ```text
//! M ∘ B_ρ = (M ⊖ B_ρ) ⊕ B_ρ        (erosion, then dilation)
//! M • B_ρ = (M ⊕ B_ρ) ⊖ B_ρ        (dilation, then erosion)
//! ```
//!
//! The opening equals the union of all translates of `B_ρ` contained in
//! `M`, so the residue `M \ (M ∘ B_ρ)` is exactly the material through
//! which no disk of diameter `L` passes — the sub-minimum features.
//! Dually, the closing residue `(M • B_ρ) \ M` is exactly the complement
//! material narrower than `L` — the sub-minimum gaps, notches, and webs.
//!
//! Each connected residue component is one finding, with the
//! isoperimetric width estimate `w = 2·area / perimeter` (exact for long
//! ribbons) compared against `L`. Residue at the arc-flattening scale is
//! dropped as noise, and so is one-sided residue — the bite an isolated
//! corner or shallow arc sheds, whose perimeter mostly leaves the source
//! boundary — since a violation is walled by material on both sides.
//!
//! For copper, `Residue::Feature` flags copper that will not survive
//! etching and `Residue::Gap` flags spacing that will not resolve. A
//! soldermask image is the mask *openings*, so `Residue::Gap` flags the
//! web of mask remaining between them.

use pcb_ir::geom::ContourSet;
use pcb_ir::geom::dfm::{ThinPiece, thin_features, thin_gaps};
use rayon::prelude::*;

use crate::commands::dfm::report::{Evidence, Finding, LayerRef, Location, Measurement, Subject};
use crate::commands::dfm::rules::{ImageSel, Rule};

use super::{Context, blank_finding};

/// Which morphological residue the rule reports.
#[derive(Clone, Copy)]
pub(super) enum Residue {
    Feature,
    Gap,
}

pub(super) fn evaluate(
    rule: &Rule,
    sel: ImageSel,
    residue: Residue,
    ctx: &Context,
) -> (usize, Vec<Finding>) {
    let images: Vec<(&LayerRef, &ContourSet)> = match sel {
        ImageSel::Copper => ctx
            .design
            .copper_layers
            .iter()
            .map(|layer| (&layer.layer, &layer.image))
            .collect(),
        ImageSel::Soldermask => ctx
            .design
            .mask_layers
            .iter()
            .map(|layer| (&layer.layer, &layer.image))
            .collect(),
    };
    let limit = rule.limit.millimeters();
    let (title, noun, offender_kind) = match (sel, residue) {
        (ImageSel::Copper, Residue::Feature) => (
            "Copper feature is below minimum width",
            "copper feature",
            "copper_image",
        ),
        (ImageSel::Copper, Residue::Gap) => (
            "Copper spacing is below minimum",
            "copper-to-copper gap",
            "copper_image",
        ),
        (ImageSel::Soldermask, Residue::Feature) => (
            "Soldermask feature is below minimum width",
            "soldermask feature",
            "soldermask_image",
        ),
        (ImageSel::Soldermask, Residue::Gap) => (
            "Soldermask web is below minimum",
            "soldermask web",
            "soldermask_image",
        ),
    };

    let findings = images
        .par_iter()
        .map(|(layer, image)| {
            let pieces = match residue {
                Residue::Feature => thin_features(image, limit),
                Residue::Gap => thin_gaps(image, limit),
            };
            pieces
                .into_iter()
                .map(|piece| thin_finding(rule, layer, &piece, title, noun, offender_kind))
                .collect::<Vec<_>>()
        })
        .flatten()
        .collect::<Vec<_>>();
    (images.len(), findings)
}

fn thin_finding(
    rule: &Rule,
    layer: &LayerRef,
    piece: &ThinPiece,
    title: &str,
    noun: &str,
    offender_kind: &'static str,
) -> Finding {
    Finding {
        title: title.to_owned(),
        message: format!(
            "{noun} measures {:.6} mm over {:.6} mm on {}; the PDK requires at least {:.6} mm",
            piece.width_mm,
            piece.length_mm,
            layer.name,
            rule.limit.millimeters()
        ),
        measurement: Measurement::minimum(piece.width_mm, rule.limit.millimeters()),
        location: Location {
            point: Some(piece.bbox.center().into()),
            bounding_box: Some(piece.bbox.into()),
            witnesses: Vec::new(),
        },
        layers: vec![layer.clone()],
        subjects: vec![Subject {
            role: "offender",
            kind: offender_kind,
            name: Some(layer.name.clone()),
            ..Subject::default()
        }],
        evidence: vec![Evidence::bounds("thin_piece", piece.bbox)],
        ..blank_finding(rule)
    }
}
