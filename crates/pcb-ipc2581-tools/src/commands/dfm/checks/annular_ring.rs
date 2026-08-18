//! Minimum annular ring: radial copper enclosure of a drilled hole.
//!
//! Let `M ⊂ ℝ²` be one layer's composed copper image — a regularized
//! closed filled region — and let the hole be the disk `D(p, r)`. The
//! annular enclosure is the largest uniform ring of copper the hole is
//! guaranteed on that layer:
//!
//! ```text
//! a = sup { t : D(p, r + t) ⊆ M }.
//! ```
//!
//! For `p ∈ M` the largest disk centered at `p` inside `M` has radius
//! `d(p, ∂M)`, the Euclidean distance from the center to the copper
//! boundary, so `a = d(p, ∂M) − r`; the value is signed, a negative `a`
//! being the depth by which the drill breaches the copper. The check
//! requires
//!
//! ```text
//! a ≥ A_min    on every spanned copper layer where p ∈ M,
//! ```
//!
//! equivalently `D(p, r + A_min) ⊆ M` — the indexed form of a morphological
//! erosion test. For `p ∉ M` the artwork deliberately places no copper at
//! the hole — a plane anti-pad, a thermal-relief clearance, a removed
//! unused land — so there is no ring to measure and the layer is not
//! checked. One finding per hole reports the layer minimizing `a`.
//!
//! Computation: `p ∈ M` by a batched winding-number sweep over all hole
//! centers; `d(p, ∂M)` by a nearest-boundary query against a uniform grid
//! over the boundary segments, searched only to `r + A_min` since farther
//! boundaries cannot violate. The polygon flattening tolerance is
//! subtracted from the requirement so arc discretization cannot
//! manufacture violations.

use pcb_ir::geom::{BBox, Point, tol};
use rayon::prelude::*;

use crate::commands::dfm::design::{CopperLayer, Hole, HoleClass, Land};
use crate::commands::dfm::report::{
    Evidence, Finding, Location, Measurement, SourceLocator, Subject, Witness,
};
use crate::commands::dfm::rules::Rule;

use super::{
    COMPARISON_EPSILON_MM, Context, blank_finding, hole_subject, holes_of_class, unique_layers,
};

/// One (hole, copper layer) enclosure violation candidate.
struct AnnularViolation<'a> {
    copper_index: usize,
    land: Option<&'a Land>,
    enclosure_mm: f64,
    cutout_boundary: Point,
    material_boundary: Point,
}

pub(super) fn evaluate(rule: &Rule, class: HoleClass, ctx: &Context) -> (usize, Vec<Finding>) {
    let design = ctx.design;
    let holes = holes_of_class(design, class);
    let limit = rule.limit.millimeters();
    let tolerance = tol::FLATTEN_MM + COMPARISON_EPSILON_MM;
    let centers = holes.iter().map(|hole| hole.center).collect::<Vec<_>>();
    let contains = design
        .copper_layers
        .par_iter()
        .map(|layer| layer.image.contains_points_batch(&centers))
        .collect::<Vec<_>>();
    let boundaries = ctx.copper_boundaries();

    let per_hole = holes
        .par_iter()
        .enumerate()
        .map(|(hole_index, hole)| {
            let radius = hole.diameter_mm / 2.0;
            let mut checked = 0;
            let mut worst: Option<AnnularViolation> = None;
            for (copper_index, copper) in design.copper_layers.iter().enumerate() {
                if !hole.spans_copper(copper_index as u16) {
                    continue;
                }
                if !contains[copper_index][hole_index] {
                    // No copper at the hole here — a plane anti-pad, a
                    // thermal-relief clearance, or a removed unused land.
                    // There is no ring to measure.
                    continue;
                }
                checked += 1;
                let violation = boundaries[copper_index]
                    .circular_enclosure(hole.center, radius, limit - tolerance)
                    .filter(|measurement| measurement.enclosure_mm + tolerance < limit)
                    .map(|measurement| AnnularViolation {
                        copper_index,
                        land: hole
                            .land_on(copper_index)
                            .map(|link| &copper.lands[link.land_index as usize]),
                        enclosure_mm: measurement.enclosure_mm,
                        cutout_boundary: measurement.cutout_boundary,
                        material_boundary: measurement.material_boundary,
                    });
                if let Some(violation) = violation
                    && worst
                        .as_ref()
                        .is_none_or(|current| violation.enclosure_mm < current.enclosure_mm)
                {
                    worst = Some(violation);
                }
            }
            (
                checked,
                worst.map(|worst| annular_finding(rule, class, hole, ctx, worst)),
            )
        })
        .collect::<Vec<_>>();

    (
        per_hole.iter().map(|(checked, _)| checked).sum(),
        per_hole
            .into_iter()
            .filter_map(|(_, finding)| finding)
            .collect(),
    )
}

fn annular_finding(
    rule: &Rule,
    class: HoleClass,
    hole: &Hole,
    ctx: &Context,
    violation: AnnularViolation,
) -> Finding {
    let limit = rule.limit.millimeters();
    let copper = &ctx.design.copper_layers[violation.copper_index];
    let required_radius = hole.diameter_mm / 2.0 + limit;
    let witnesses = vec![
        Witness::new("hole_boundary", violation.cutout_boundary),
        Witness::new("copper_boundary", violation.material_boundary),
    ];
    let location_point = violation
        .cutout_boundary
        .midpoint(violation.material_boundary);
    let detail = if violation.enclosure_mm < 0.0 {
        format!(
            "the drilled hole breaches the copper image by {:.6} mm",
            -violation.enclosure_mm
        )
    } else {
        format!(
            "only {:.6} mm of copper remains outside the drilled hole",
            violation.enclosure_mm
        )
    };
    Finding {
        title: format!("{} annular ring is below minimum", class.label()),
        message: format!(
            "{} minimum radial copper enclosure is {:.6} mm on {}; the PDK requires {:.6} mm ({detail})",
            class.label(),
            violation.enclosure_mm,
            copper.layer.name,
            limit
        ),
        measurement: Measurement::minimum(violation.enclosure_mm, limit),
        location: Location {
            point: Some(location_point.into()),
            bounding_box: Some(BBox::from_point(hole.center).expand(required_radius).into()),
            witnesses,
        },
        layers: unique_layers(&hole.layer, &copper.layer),
        subjects: annular_ring_subjects(ctx, hole, copper, violation.land),
        evidence: annular_ring_evidence(hole, violation.land, 2.0 * required_radius),
        ..blank_finding(rule)
    }
}

fn annular_ring_subjects(
    ctx: &Context,
    hole: &Hole,
    copper: &CopperLayer,
    land: Option<&Land>,
) -> Vec<Subject> {
    let mut subjects = vec![hole_subject(ctx, hole, "hole")];
    subjects.push(match land {
        Some(land) => Subject {
            role: "land",
            kind: "padstack_land",
            name: ctx.resolve(land.primitive_ref),
            reference_designator: ctx.resolve(land.reference_designator),
            pin: ctx.resolve(land.pin),
            net: ctx.resolve(land.net),
            padstack_ref: ctx.resolve(Some(land.padstack)),
            source: Some(SourceLocator {
                step: ctx.resolve(land.step),
                layer: Some(copper.layer.name.clone()),
                set_index: Some(land.source_set_index),
                feature_index: Some(land.source_feature_index),
                instance_index: None,
            }),
        },
        None => Subject {
            role: "land",
            kind: "composed_copper",
            name: Some(copper.layer.name.clone()),
            ..Subject::default()
        },
    });
    subjects
}

fn annular_ring_evidence(
    hole: &Hole,
    land: Option<&Land>,
    required_diameter_mm: f64,
) -> Vec<Evidence> {
    let mut evidence = vec![
        Evidence::circle("drilled_hole", hole.center, hole.diameter_mm),
        Evidence::circle(
            "required_copper_envelope",
            hole.center,
            required_diameter_mm,
        ),
    ];
    if let Some(land) = land {
        evidence.push(Evidence::bounds("source_padstack_land_bounds", land.bbox));
    }
    evidence
}
