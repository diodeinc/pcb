//! Copper layer count from the physical IPC-2581 stackup.
//!
//! The stackup extractor has already required one unambiguous stackup and a
//! one-to-one match with every declared copper layer. This evaluator therefore
//! measures one exact integer, independent of layout materialization.

use super::CountEvaluation;
use crate::commands::dfm::design::Design;
use crate::commands::dfm::report::Subject;

pub(super) fn evaluate(design: &Design) -> CountEvaluation {
    let stackup = design
        .stackup
        .as_ref()
        .expect("layer-count rules request stackup extraction");
    CountEvaluation {
        actual: u32::try_from(stackup.copper_layers.len())
            .expect("IPC-2581 copper layer count fits in u32"),
        layers: stackup.copper_layers.clone(),
        subjects: vec![Subject {
            role: "measured",
            kind: "stackup",
            name: Some(stackup.name.clone()),
            ..Subject::default()
        }],
    }
}
