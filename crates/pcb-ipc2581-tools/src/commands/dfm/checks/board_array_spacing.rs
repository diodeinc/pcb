//! Minimum spacing between sibling board arrays on a fabrication panel.
//!
//! Each array is a filled region `Aᵢ ⊂ ℝ²`, the union of its transformed
//! profile outlines. The measured quantity between two arrays is the
//! Euclidean set distance
//!
//! ```text
//! dist(Aᵢ, Aⱼ) = inf { ‖x − y‖ : x ∈ Aᵢ, y ∈ Aⱼ },
//! ```
//!
//! which is `0` when the regions touch, overlap, or nest, and is otherwise
//! attained between boundary points, so `dist(Aᵢ, Aⱼ) = dist(∂Aᵢ, ∂Aⱼ)`,
//! the minimum over boundary segment pairs. Overlap and nesting are decided
//! by testing each region's vertices against the other (winding number);
//! partial overlap without vertex containment is caught by the
//! boundary-crossing test inside the segment-pair distance.
//!
//! The check requires `dist(Aᵢ, Aⱼ) ≥ L` for every unordered pair of
//! direct board-array instances of the panel's root step. A panel-kind
//! child that places a single board is per-board packaging, not an array,
//! and is excluded at extraction. Bounds distance is a lower bound on
//! region distance, so a pair whose bounds already clear `L` is proven
//! clear without walking its boundary segments; every pair counts as
//! checked either way.

use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::geom::dfm::{Distance, region_clearance, region_clearance_sites};

use crate::commands::dfm::design::{BoardArray, Design};
use crate::commands::dfm::report::{Evidence, SourceLocator, Subject};

use super::{Evaluation, Measured, linework_clearance, violates};

pub(super) fn evaluate(
    limit_mm: f64,
    design: &Design,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Evaluation> {
    let arrays = &design.board_arrays;
    let pairs = arrays.iter().enumerate().flat_map(|(index, first)| {
        arrays[index + 1..]
            .iter()
            .map(move |second| (first, second))
    });
    let measured = pairs
        .clone()
        .filter(|(first, second)| {
            let bound = first.region.bbox.distance_to(second.region.bbox);
            Distance::exact(
                bound,
                first.region.bbox.center(),
                second.region.bbox.center(),
            )
            .certainly_below(limit_mm)
        })
        .map(|(first, second)| {
            let distance = region_clearance(&first.region, &second.region)
                .expect("board arrays are extracted with non-empty profiles");
            Ok::<_, anyhow::Error>(Measured {
                distance,
                bbox: first.region.bbox.union(second.region.bbox),
                layers: Vec::new(),
                subjects: vec![
                    board_array_subject(first, "first"),
                    board_array_subject(second, "second"),
                ],
                evidence: vec![
                    Evidence::bounds("first_board_array", first.region.bbox),
                    Evidence::bounds("second_board_array", second.region.bbox),
                ],
                sites: if violates(&distance, limit_mm) {
                    region_clearance_sites(&first.region, &second.region, limit_mm)
                        .into_iter()
                        .map(|site| {
                            linework_clearance::report_site(site, Vec::new(), limit_mm, accuracy)
                        })
                        .collect::<anyhow::Result<Vec<_>>>()?
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                },
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .collect();
    Ok(Evaluation {
        checked: pairs.count(),
        measured,
    })
}

fn board_array_subject(array: &BoardArray, role: &'static str) -> Subject {
    Subject {
        role,
        kind: "board_array_outline",
        name: Some(array.name.clone()),
        source: Some(SourceLocator {
            step: Some(array.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: Some(array.instance_index),
        }),
        provenance: Some(SourceLocator {
            step: Some(array.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: Some(array.instance_index),
        }),
        ..Subject::default()
    }
}
