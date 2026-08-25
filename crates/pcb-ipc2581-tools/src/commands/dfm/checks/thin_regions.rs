//! Minimum copper feature width and soldermask web width, by morphology.
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
//! material narrower than `L`, used here for soldermask webs.
//!
//! Morphology is the conservative broad phase: it runs with an offset guard
//! large enough to retain candidates across curve/offset tessellation. A
//! candidate becomes a measurement only when two opposing source-boundary
//! branches wall it; their exact separation is the piece's width. One-sided
//! residue — the bite an isolated corner sheds — has no opposing branches
//! and is discarded. Thus the composed image supplies the authoritative
//! measurement while morphology only localizes the work.
//!
//! The opening residue flags copper that will not survive etching. A
//! soldermask image is the mask *openings*, so its closing residue flags the
//! web of mask remaining between them. Copper clearance is a separate
//! electrical-conductor boundary-distance check; morphology must not
//! reinterpret a same-net notch as spacing between conductors.

use pcb_ir::geom::ContourSet;
use pcb_ir::geom::dfm::{ThinPiece, thin_features, thin_gaps};
use rayon::prelude::*;

use super::{Evaluation, Measured};
use crate::commands::dfm::design::Design;
use crate::commands::dfm::report::{Evidence, LayerRef, Subject};

pub(super) fn copper_feature_width(limit_mm: f64, design: &Design) -> Evaluation {
    evaluate(
        limit_mm,
        design
            .copper_layers
            .iter()
            .map(|layer| (&layer.layer, &layer.image))
            .collect(),
        "copper_image",
        thin_features,
    )
}

pub(super) fn soldermask_web(limit_mm: f64, design: &Design) -> Evaluation {
    evaluate(
        limit_mm,
        design
            .mask_layers
            .iter()
            .map(|layer| (&layer.layer, &layer.image))
            .collect(),
        "soldermask_image",
        thin_gaps,
    )
}

fn evaluate(
    limit_mm: f64,
    images: Vec<(&LayerRef, &ContourSet)>,
    offender_kind: &'static str,
    thin: fn(&ContourSet, f64) -> Vec<ThinPiece>,
) -> Evaluation {
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
    Evaluation {
        checked: images.len(),
        measured,
    }
}
