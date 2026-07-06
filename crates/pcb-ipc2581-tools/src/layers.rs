//! Shared IPC-2581 layer-function classification.

use ipc2581::types::LayerFunction;
use pcb_ir::dialects::LayerRole;

/// True for layer functions that carry copper imagery.
pub fn is_copper(function: LayerFunction) -> bool {
    matches!(
        function,
        LayerFunction::Conductor
            | LayerFunction::CondFilm
            | LayerFunction::CondFoil
            | LayerFunction::Plane
            | LayerFunction::Signal
            | LayerFunction::Mixed
    )
}

/// Map a layer function to its pcb-ir rendering role.
pub fn layer_role(function: LayerFunction) -> LayerRole {
    if is_copper(function) {
        return LayerRole::Copper;
    }
    match function {
        LayerFunction::Solderpaste | LayerFunction::Pastemask => LayerRole::Paste,
        LayerFunction::Soldermask => LayerRole::Soldermask,
        LayerFunction::Silkscreen | LayerFunction::Legend => LayerRole::Legend,
        LayerFunction::Drill => LayerRole::Drill,
        LayerFunction::Rout
        | LayerFunction::VCut
        | LayerFunction::Score
        | LayerFunction::EdgeChamfer
        | LayerFunction::EdgePlating
        | LayerFunction::BoardOutline => LayerRole::Profile,
        LayerFunction::Assembly
        | LayerFunction::BoardFab
        | LayerFunction::Courtyard
        | LayerFunction::Document
        | LayerFunction::Graphic
        | LayerFunction::Fixture
        | LayerFunction::Probe
        | LayerFunction::Rework => LayerRole::Mechanical,
        _ => LayerRole::Other,
    }
}
