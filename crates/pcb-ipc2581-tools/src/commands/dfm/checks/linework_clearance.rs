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
//! farther boundaries cannot violate.

use pcb_ir::geom::Point;
use pcb_ir::geom::dfm::ClearanceMeasurement;
use pcb_ir::geom::region::ring_edges;

use crate::commands::dfm::design::{BoardOutline, CopperLayer, Design};
use crate::commands::dfm::report::{Evidence, Finding, LayerRef, SourceLocator, Subject};
use crate::commands::dfm::rules::{Linework, Rule};

use super::{ClearanceViolation, Context, unique_layers, violates_minimum};

/// One reference item: a V-score centerline or a board outline, as bare
/// segments plus its report identity.
struct LineworkItem {
    segments: Vec<(Point, Point)>,
    layer: Option<LayerRef>,
    subject: Subject,
    evidence: Evidence,
}

fn linework_items(linework: Linework, design: &Design) -> Vec<LineworkItem> {
    match linework {
        Linework::VScore => design
            .scores
            .iter()
            .map(|score| LineworkItem {
                segments: vec![(score.start, score.end)],
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
                segments: outline.contours.iter().flat_map(ring_edges).collect(),
                layer: None,
                subject: outline_subject(outline, "reference"),
                evidence: Evidence::bounds("board_outline", outline.bbox),
            })
            .collect(),
    }
}

pub(super) fn evaluate(rule: &Rule, linework: Linework, ctx: &Context) -> (usize, Vec<Finding>) {
    let design = ctx.design;
    let items = linework_items(linework, design);
    let (title, witness_role, reference_label) = match linework {
        Linework::VScore => (
            "V-score centerline is too close to copper",
            "vscore_centerline",
            "V-score centerline",
        ),
        Linework::BoardEdge => (
            "Board edge is too close to copper",
            "board_outline",
            "board edge",
        ),
    };
    let limit = rule.limit.millimeters();
    let boundaries = ctx.copper_boundaries();
    let endpoints = items
        .iter()
        .flat_map(|item| item.segments.iter().flat_map(|&(start, end)| [start, end]))
        .collect::<Vec<_>>();

    let mut checked = 0;
    let mut findings = Vec::new();
    for (copper_index, copper) in design.copper_layers.iter().enumerate() {
        let inside = copper.image.contains_points_batch(&endpoints);
        let mut cursor = 0;
        for item in &items {
            checked += 1;
            let mut nearest: Option<ClearanceMeasurement> = None;
            for &(start, end) in &item.segments {
                let (start_inside, end_inside) = (inside[cursor], inside[cursor + 1]);
                cursor += 2;
                let candidate = if start_inside || end_inside {
                    let point = if start_inside { start } else { end };
                    Some(ClearanceMeasurement {
                        distance_mm: 0.0,
                        first: point,
                        second: point,
                    })
                } else {
                    boundaries[copper_index].segment_nearest_within(start, end, limit)
                };
                if let Some(candidate) = candidate
                    && nearest
                        .as_ref()
                        .is_none_or(|current| candidate.distance_mm < current.distance_mm)
                {
                    nearest = Some(candidate);
                }
            }
            let Some(clearance) = nearest else {
                continue;
            };
            if !violates_minimum(clearance.distance_mm, limit) {
                continue;
            }
            findings.push(
                ClearanceViolation {
                    rule,
                    title,
                    message: format!(
                        "{reference_label} is {:.6} mm from copper on {}; the PDK requires at least {:.6} mm",
                        clearance.distance_mm, copper.layer.name, limit
                    ),
                    witness_roles: [witness_role, "copper_boundary"],
                    clearance,
                    layers: match &item.layer {
                        Some(layer) => unique_layers(layer, &copper.layer),
                        None => vec![copper.layer.clone()],
                    },
                    subjects: vec![item.subject.clone(), copper_subject(copper)],
                    evidence: vec![item.evidence.clone()],
                }
                .into_finding(),
            );
        }
    }
    (checked, findings)
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
