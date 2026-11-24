//! Shared IPC-2581 layer-function classification.

use std::collections::HashSet;

use ipc2581::Symbol;
use ipc2581::types::Ecad;
use pcb_ir::dialects::Side;
use pcb_ir::dialects::ipc::{
    BoardArrayCopperLayer, PhysicalLayer, SurfaceLayerError, TwoSidedSurfaceLayers,
    resolve_two_sided_surface_layers,
};

use pcb_ir::import::ipc2581::ImportedDesign;
pub use pcb_ir::import::ipc2581::{is_copper, layer_role, side_for_layer as ir_side};

/// Canonical copper-layer identities and sides for per-layer geometry work.
pub fn copper_layers(ecad: &Ecad) -> Vec<BoardArrayCopperLayer> {
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| is_copper(layer.layer_function))
        .map(|layer| BoardArrayCopperLayer::new(layer.name, ir_side(layer.side)))
        .collect()
}

/// Resolve existing outer copper and solder-mask layers for two-sided features.
pub fn two_sided_surface_layers(ecad: &Ecad) -> Result<TwoSidedSurfaceLayers, SurfaceLayerError> {
    resolve_two_sided_surface_layers(ecad.cad_data.layers.iter().map(|layer| {
        PhysicalLayer::new(
            layer.name,
            layer_role(layer.layer_function),
            ir_side(layer.side),
        )
    }))
}

/// Copper layers top to bottom: as the stackup orders them, else as declared.
pub fn copper_order(imported: &ImportedDesign) -> Vec<Symbol> {
    let mut copper = imported
        .layer_definitions
        .iter()
        .filter(|layer| is_copper(layer.layer_function))
        .map(|layer| layer.name)
        .collect::<Vec<_>>();
    let side = |name: Symbol| {
        let mut layers = imported.layer_definitions.iter();
        match layers
            .find(|layer| layer.name == name)
            .map(|layer| ir_side(layer.side))
        {
            Some(Side::Top) => 0,
            Some(Side::Bottom) => 2,
            _ => 1,
        }
    };
    copper.sort_by_key(|name| side(*name));
    let Some(stackup) = imported.stackups.first() else {
        return copper;
    };
    // A sequence orders the stack only when every layer has its own.
    let mut stack = stackup.layers.iter().collect::<Vec<_>>();
    let numbers = stack
        .iter()
        .filter_map(|layer| layer.layer_number)
        .collect::<HashSet<_>>();
    if numbers.len() == stack.len() {
        stack.sort_by_key(|layer| layer.layer_number);
    }
    let place = |name: Symbol| {
        let name = imported.resolve(name);
        stack
            .iter()
            .position(|layer| imported.resolve(layer.layer_ref) == name)
    };
    // A stackup that leaves a copper layer out orders none of them.
    if copper.iter().all(|name| place(*name).is_some()) {
        copper.sort_by_key(|name| place(*name));
    }
    copper
}
