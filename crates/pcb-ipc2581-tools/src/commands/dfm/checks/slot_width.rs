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

use crate::commands::dfm::design::SlotWidth;
use crate::commands::dfm::report::{Evidence, Finding, Location, Measurement, Witness};
use crate::commands::dfm::rules::Rule;

use super::{Context, blank_finding, drilled_subject, violates_minimum};

pub(super) fn evaluate(rule: &Rule, ctx: &Context) -> (usize, Vec<Finding>) {
    let slots = &ctx.design.slots;
    let limit = rule.limit.millimeters();
    let findings = slots
        .iter()
        .filter_map(|slot| {
            let (actual, detail, point, bbox, witnesses, evidence) = match &slot.width {
                SlotWidth::Nominal(actual) if violates_minimum(*actual, limit) => (
                    *actual,
                    "nominal routed slot width",
                    slot.center,
                    slot.bbox,
                    Vec::new(),
                    vec![Evidence::bounds("routed_slot", slot.bbox)],
                ),
                SlotWidth::Nominal(_) => return None,
                SlotWidth::Geometry(geometry) => {
                    let piece = thin_features(geometry, limit)
                        .into_iter()
                        .filter(|piece| violates_minimum(piece.width_mm, limit))
                        .min_by(|left, right| left.width_mm.total_cmp(&right.width_mm))?;
                    (
                        piece.width_mm,
                        "minimum local width of routed slot outline",
                        piece.first.midpoint(piece.second),
                        piece.bbox,
                        vec![
                            Witness::new("first_slot_boundary", piece.first),
                            Witness::new("second_slot_boundary", piece.second),
                        ],
                        vec![
                            Evidence::bounds("routed_slot", slot.bbox),
                            Evidence::bounds("subminimum_slot_region", piece.bbox),
                        ],
                    )
                }
            };
            Some(Finding {
                title: "Slot is below minimum width".to_owned(),
                message: format!(
                    "{detail} is {:.6} mm; the PDK requires at least {:.6} mm",
                    actual, limit
                ),
                measurement: Measurement::minimum(actual, limit),
                location: Location {
                    point: Some(point.into()),
                    bounding_box: Some(bbox.into()),
                    witnesses,
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
                evidence,
                ..blank_finding(rule)
            })
        })
        .collect::<Vec<_>>();
    (slots.len(), findings)
}
