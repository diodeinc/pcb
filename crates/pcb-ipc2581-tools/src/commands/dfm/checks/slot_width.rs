//! Minimum routed slot width.
//!
//! IPC can state a slot as an oval primitive with nominal tool width `w`, or
//! as an arbitrary filled outline `S`. The primitive width is the
//! authoritative source instruction and is compared directly. For an outline,
//! conservative morphological opening localizes sub-minimum material and the
//! final verdict uses exact distance between the two opposing outline
//! branches. Thus every materialized slot is checked without inferring a
//! nominal tool size that the source never supplied.

use pcb_ir::geom::dfm::thin_features;

use crate::commands::dfm::design::{Slot, SlotWidth};
use crate::commands::dfm::report::{Evidence, Finding, Location, Measurement, Witness};
use crate::commands::dfm::rules::Rule;

use super::{Context, blank_finding, drilled_subject, violates_minimum};

/// Where one slot is narrower than the limit, in the terms its width basis
/// supplies.
struct SlotViolation {
    width_mm: f64,
    detail: &'static str,
    location: Location,
    evidence: Vec<Evidence>,
}

pub(super) fn evaluate(rule: &Rule, ctx: &Context) -> (usize, Vec<Finding>) {
    let slots = &ctx.design.slots;
    let limit = rule.limit.millimeters();
    let findings = slots
        .iter()
        .filter_map(|slot| Some((slot, slot_violation(slot, limit)?)))
        .map(|(slot, violation)| Finding {
            title: "Slot is below minimum width".to_owned(),
            message: format!(
                "{} is {:.6} mm; the PDK requires at least {limit:.6} mm",
                violation.detail, violation.width_mm
            ),
            measurement: Measurement::minimum(violation.width_mm, limit),
            location: violation.location,
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
            evidence: violation.evidence,
            ..blank_finding(rule)
        })
        .collect();
    (slots.len(), findings)
}

/// A nominal width is compared as stated. An outline is measured by
/// [`thin_features`], which owns the verdict for geometric widths; its
/// narrowest reported piece is the slot's width.
fn slot_violation(slot: &Slot, limit: f64) -> Option<SlotViolation> {
    match &slot.width {
        SlotWidth::Nominal(width_mm) => violates_minimum(*width_mm, limit).then(|| SlotViolation {
            width_mm: *width_mm,
            detail: "nominal routed slot width",
            location: Location {
                point: Some(slot.center.into()),
                bounding_box: Some(slot.bbox.into()),
                witnesses: Vec::new(),
            },
            evidence: vec![Evidence::bounds("routed_slot", slot.bbox)],
        }),
        SlotWidth::Geometry(geometry) => thin_features(geometry, limit)
            .into_iter()
            .min_by(|left, right| left.width_mm.total_cmp(&right.width_mm))
            .map(|piece| SlotViolation {
                width_mm: piece.width_mm,
                detail: "minimum local width of routed slot outline",
                location: Location {
                    point: Some(piece.first.midpoint(piece.second).into()),
                    bounding_box: Some(piece.bbox.into()),
                    witnesses: vec![
                        Witness::new("first_slot_boundary", piece.first),
                        Witness::new("second_slot_boundary", piece.second),
                    ],
                },
                evidence: vec![
                    Evidence::bounds("routed_slot", slot.bbox),
                    Evidence::bounds("subminimum_slot_region", piece.bbox),
                ],
            }),
    }
}
