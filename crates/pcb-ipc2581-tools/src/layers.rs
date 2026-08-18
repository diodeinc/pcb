//! Shared IPC-2581 layer-function classification.

use ipc2581::{
    Symbol,
    types::{Ecad, LayerFunction, Side as IpcSide},
};
use pcb_ir::dialects::ipc::{
    BoardArrayCopperLayer, PhysicalLayer, SurfaceLayerError, TwoSidedSurfaceLayers,
    resolve_two_sided_surface_layers,
};
use pcb_ir::dialects::{LayerRole, Side};

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

/// Canonical copper-layer identities and sides for per-layer geometry work.
pub fn copper_layers(ecad: &Ecad) -> Vec<BoardArrayCopperLayer<Symbol>> {
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| is_copper(layer.layer_function))
        .map(|layer| BoardArrayCopperLayer::new(layer.name, ir_side(layer.side)))
        .collect()
}

/// Resolve existing outer copper and solder-mask layers for two-sided features.
pub fn two_sided_surface_layers(
    ecad: &Ecad,
) -> Result<TwoSidedSurfaceLayers<Symbol>, SurfaceLayerError> {
    resolve_two_sided_surface_layers(ecad.cad_data.layers.iter().map(|layer| {
        PhysicalLayer::new(
            layer.name,
            layer_role(layer.layer_function),
            ir_side(layer.side),
        )
    }))
}

/// Map an IPC layer side to the IR side vocabulary.
pub fn ir_side(side: Option<IpcSide>) -> Side {
    match side {
        Some(IpcSide::Top) => Side::Top,
        Some(IpcSide::Bottom) => Side::Bottom,
        Some(IpcSide::Internal) => Side::Inner,
        _ => Side::None,
    }
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
