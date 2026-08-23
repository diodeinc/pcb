//! Minimum clearance from reference linework to copper.
//!
//! The reference is a set of segments `S = {s₁ … sₙ}` — a V-score tool
//! centerline, or a board profile's outer and cutout rings — and the target
//! is one layer's composed copper image `M`, a regularized closed filled
//! region. The measured quantity is the Euclidean set distance
//!
//! ```text
//! dist(S, M) = minᵢ inf { ‖x − y‖ : x ∈ sᵢ, y ∈ M },
//! ```
//!
//! and the check requires `dist(S, M) ≥ L` per (reference item, layer).
//!
//! Because `M` is closed and filled, the distance decomposes exactly:
//! `dist(S, M) = 0` iff some segment meets `M`, which happens iff a segment
//! endpoint lies in `M` (winding number, batched per layer) or a segment
//! crosses `∂M` (a segment with both endpoints outside a filled region
//! intersects it iff it crosses its boundary). Otherwise `S` is disjoint
//! from `M` and `dist(S, M) = dist(S, ∂M)`, the minimum segment-to-segment
//! distance against the indexed boundary, searched only within `L` since
//! farther boundaries cannot violate. A profile's rings are flattened
//! curves and count toward the measurement's uncertainty; a score line is
//! exact.

use std::ops::Range;

use pcb_ir::geom::dfm::Distance;
use pcb_ir::geom::region::ring_edges;
use pcb_ir::geom::{BBox, Point};

use crate::commands::dfm::design::{BoardOutline, CopperLayer, Design};
use crate::commands::dfm::report::{Evidence, LayerRef, SourceLocator, Subject};
use crate::commands::dfm::rules::Linework;

use super::{Evaluation, Measured, layers};

/// One reference item: a V-score centerline or a board outline, as a range
/// of bare segments plus its report identity.
struct LineworkItem {
    segments: Range<usize>,
    flattened_boundaries: u32,
    layer: Option<LayerRef>,
    subject: Subject,
    evidence: Evidence,
}

/// The reference items and their segments, flattened into one pool so the
/// endpoint containment sweep runs once per layer.
struct LineworkPool {
    items: Vec<LineworkItem>,
    segments: Vec<(Point, Point)>,
}

fn linework_items(linework: Linework, design: &Design) -> LineworkPool {
    let mut segments = Vec::new();
    let mut push = |item_segments: Vec<(Point, Point)>| {
        let start = segments.len();
        segments.extend(item_segments);
        start..segments.len()
    };
    let items = match linework {
        Linework::VScore => design
            .scores
            .iter()
            .map(|score| LineworkItem {
                segments: push(vec![(score.start, score.end)]),
                flattened_boundaries: 0,
                layer: Some(score.layer.clone()),
                subject: Subject {
                    role: "reference",
                    kind: "vscore_centerline",
                    name: Some(score.layer.name.clone()),
                    ..Subject::default()
                },
                evidence: Evidence::segment("vscore_centerline", score.start, score.end),
            })
            .collect(),
        Linework::BoardEdge => design
            .board_outlines
            .iter()
            .map(|outline| LineworkItem {
                segments: push(outline.contours.iter().flat_map(ring_edges).collect()),
                flattened_boundaries: 1,
                layer: None,
                subject: outline_subject(outline, "reference"),
                evidence: Evidence::bounds("board_outline", outline.bbox),
            })
            .collect(),
    };
    LineworkPool { items, segments }
}

pub(super) fn evaluate(limit_mm: f64, linework: Linework, design: &Design) -> Evaluation {
    let copper_layers = &design.copper_layers;
    let boundaries = &design.copper_boundaries;
    let pool = linework_items(linework, design);
    let (items, segments) = (&pool.items, &pool.segments);
    let endpoints = segments
        .iter()
        .flat_map(|&(start, end)| [start, end])
        .collect::<Vec<_>>();

    let measured = copper_layers
        .iter()
        .enumerate()
        .flat_map(|(copper_index, copper)| {
            let inside = copper.image.contains_points_batch(&endpoints);
            items.iter().filter_map(move |item| {
                let nearest = item
                    .segments
                    .clone()
                    .filter_map(|segment_index| {
                        let (start, end) = segments[segment_index];
                        let (start_inside, end_inside) =
                            (inside[2 * segment_index], inside[2 * segment_index + 1]);
                        // A segment touching copper measures zero at the
                        // contained endpoint; otherwise both ends are outside
                        // and the boundary distance is the set distance.
                        match (start_inside, end_inside) {
                            (true, _) => Some(Distance::flattened(0.0, start, start, 1)),
                            (_, true) => Some(Distance::flattened(0.0, end, end, 1)),
                            _ => boundaries[copper_index]
                                .segment_nearest_within(start, end, limit_mm),
                        }
                    })
                    .min_by(|left, right| left.mm.total_cmp(&right.mm))?;
                let distance = nearest.also_flattened(item.flattened_boundaries);
                Some(Measured {
                    distance,
                    bbox: BBox::from_point(distance.first).union(BBox::from_point(distance.second)),
                    layers: layers(item.layer.iter().chain([&copper.layer])),
                    subjects: vec![item.subject.clone(), copper_subject(copper)],
                    evidence: vec![item.evidence.clone()],
                })
            })
        })
        .collect();
    Evaluation {
        checked: copper_layers.len() * items.len(),
        measured,
    }
}

fn copper_subject(copper: &CopperLayer) -> Subject {
    Subject {
        role: "offender",
        kind: "copper_image",
        name: Some(copper.layer.name.clone()),
        ..Subject::default()
    }
}

fn outline_subject(outline: &BoardOutline, role: &'static str) -> Subject {
    Subject {
        role,
        kind: "board_outline",
        name: Some(outline.name.clone()),
        source: Some(SourceLocator {
            step: Some(outline.name.clone()),
            layer: None,
            set_index: None,
            feature_index: None,
            instance_index: outline.instance_index,
        }),
        ..Subject::default()
    }
}
