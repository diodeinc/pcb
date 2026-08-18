//! Minimum routed slot width.
//!
//! A routed slot is the Minkowski sum of its routing centerline `P` with
//! the cutting tool's disk: `S = P ⊕ D(0, w/2)`, so the slot's width `w`
//! equals the tool diameter — the minor dimension of the slot's oval
//! primitive. The check is the pointwise predicate
//!
//! ```text
//! wₛ ≥ W_min    for every slot s with a stated primitive width.
//! ```
//!
//! Outline-shaped slots state no nominal width and are not measured; they
//! are absent from the checked count rather than silently passed as zero.

use crate::commands::dfm::report::{Evidence, Finding, Location, Measurement};
use crate::commands::dfm::rules::Rule;

use super::{Context, blank_finding, drilled_subject, violates_minimum};

pub(super) fn evaluate(rule: &Rule, ctx: &Context) -> (usize, Vec<Finding>) {
    let slots = &ctx.design.slots;
    let limit = rule.limit.millimeters();
    let findings = slots
        .iter()
        .filter(|slot| violates_minimum(slot.width_mm, limit))
        .map(|slot| Finding {
            title: "Slot is below minimum width".to_owned(),
            message: format!(
                "routed slot width is {:.6} mm; the PDK requires at least {:.6} mm",
                slot.width_mm, limit
            ),
            measurement: Measurement::minimum(slot.width_mm, limit),
            location: Location {
                point: Some(slot.center.into()),
                bounding_box: Some(slot.bbox.into()),
                witnesses: Vec::new(),
            },
            layers: vec![slot.layer.clone()],
            subjects: vec![drilled_subject(
                ctx,
                "offender",
                "routed_slot",
                slot.net,
                slot.padstack,
                slot.step,
                &slot.layer,
                slot.source_set_index,
                slot.source_feature_index,
            )],
            evidence: vec![Evidence::bounds("routed_slot", slot.bbox)],
            ..blank_finding(rule)
        })
        .collect::<Vec<_>>();
    (slots.len(), findings)
}
