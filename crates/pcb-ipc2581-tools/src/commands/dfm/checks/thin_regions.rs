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
//! Morphology is the conservative broad phase: it runs with an offset guard
//! large enough to retain candidates across curve/offset tessellation. A
//! candidate becomes a measurement only when two opposing source-boundary
//! branches wall it; their exact separation is the piece's width. One-sided
//! residue — the bite an isolated corner sheds — has no opposing branches
//! and is discarded. Thus the composed image supplies the authoritative
//! measurement while morphology only localizes the work.
//!
//! For copper, `Residue::Feature` flags copper that will not survive
//! etching and `Residue::Gap` flags spacing that will not resolve. A
//! soldermask image is the mask *openings*, so `Residue::Gap` flags the
//! web of mask remaining between them.

use anyhow::Result;
use pcb_ir::geom::ContourSet;
use pcb_ir::geom::dfm::{ThinPiece, thin_features, thin_gaps};
use rayon::prelude::*;

use crate::commands::dfm::report::{Evidence, LayerRef, Subject};
use crate::commands::dfm::rules::ImageSel;

use super::{Context, Evaluation, Measured};

/// Which morphological residue the rule reports.
#[derive(Clone, Copy)]
pub(super) enum Residue {
    Feature,
    Gap,
}

pub(super) fn evaluate(
    limit_mm: f64,
    sel: ImageSel,
    residue: Residue,
    ctx: &Context,
) -> Result<Evaluation> {
    let (images, offender_kind): (Vec<(&LayerRef, &ContourSet)>, &'static str) = match sel {
        ImageSel::Copper => (
            ctx.design
                .copper_layers()?
                .iter()
                .map(|layer| (&layer.layer, &layer.image))
                .collect(),
            "copper_image",
        ),
        ImageSel::Soldermask => (
            ctx.design
                .mask_layers()?
                .iter()
                .map(|layer| (&layer.layer, &layer.image))
                .collect(),
            "soldermask_image",
        ),
    };
    let thin: fn(&ContourSet, f64) -> Vec<ThinPiece> = match residue {
        Residue::Feature => thin_features,
        Residue::Gap => thin_gaps,
    };
    let measured = images
        .par_iter()
        .flat_map_iter(|(layer, image)| {
            thin(image, limit_mm).into_iter().map(|piece| Measured {
                distance: piece.width,
                bbox: piece.bbox,
                layers: vec![(*layer).clone()],
                subjects: vec![Subject {
                    role: "offender",
                    kind: offender_kind,
                    name: Some(layer.name.clone()),
                    ..Subject::default()
                }],
                evidence: vec![Evidence::bounds("thin_piece", piece.bbox)],
            })
        })
        .collect();
    Ok(Evaluation {
        checked: images.len(),
        measured,
    })
}
