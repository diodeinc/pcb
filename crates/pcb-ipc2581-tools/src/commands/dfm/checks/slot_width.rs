//! Minimum routed slot width.
//!
//! A routed slot's width is fixed at extraction ([`Slot::width`]): the
//! stated primitive width when the source gives one — exact, and verified
//! against the materialized outline — and otherwise the outline's narrowest
//! local width, the separation of its two facing walls. The check is the
//! pointwise predicate `wₛ ≥ W_min` for every slot `s`.

use crate::commands::dfm::design::{Design, Slot};
use crate::commands::dfm::report::Evidence;

use super::{Evaluation, Measured, drilled_subject};

pub(super) fn evaluate(design: &Design) -> Evaluation {
    let slots = &design.slots;
    let measured = slots
        .iter()
        .map(|slot: &Slot| Measured {
            distance: slot.width,
            bbox: slot.bbox,
            layers: vec![slot.layer.clone()],
            subjects: vec![drilled_subject(
                design,
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
        })
        .collect();
    Evaluation {
        checked: slots.len(),
        measured,
    }
}
