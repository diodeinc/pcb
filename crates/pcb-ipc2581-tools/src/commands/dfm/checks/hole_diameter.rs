//! Minimum hole diameter.
//!
//! Model each drilled hole `h` as the closed disk `D(cₕ, rₕ)`. The measured
//! quantity is the finished diameter `dₕ = 2·rₕ` as the source `Hole`
//! element declares it — no geometry is derived. The check is the pointwise
//! predicate
//!
//! ```text
//! dₕ ≥ D_min    for every hole h of the rule's plating class,
//! ```
//!
//! with the engine's shared comparison epsilon absorbing floating-point
//! unit conversion.

use crate::commands::dfm::design::HoleClass;
use crate::commands::dfm::report::{Evidence, Finding, Location, Measurement};
use crate::commands::dfm::rules::Rule;

use super::{Context, blank_finding, hole_subject, holes_of_class, violates_minimum};

pub(super) fn evaluate(rule: &Rule, class: HoleClass, ctx: &Context) -> (usize, Vec<Finding>) {
    let holes = holes_of_class(ctx.design, class);
    let limit = rule.limit.millimeters();
    let findings = holes
        .iter()
        .filter(|hole| violates_minimum(hole.diameter_mm, limit))
        .map(|hole| Finding {
            title: format!("{} hole is below minimum diameter", class.label()),
            message: format!(
                "{} hole diameter is {:.6} mm; the PDK requires at least {:.6} mm",
                class.label(),
                hole.diameter_mm,
                limit
            ),
            measurement: Measurement::minimum(hole.diameter_mm, limit),
            location: Location {
                point: Some(hole.center.into()),
                bounding_box: Some(hole.bbox.into()),
                witnesses: Vec::new(),
            },
            layers: vec![hole.layer.clone()],
            subjects: vec![hole_subject(ctx, hole, "offender")],
            evidence: vec![Evidence::circle(
                "drilled_hole",
                hole.center,
                hole.diameter_mm,
            )],
            ..blank_finding(rule)
        })
        .collect::<Vec<_>>();
    (holes.len(), findings)
}
