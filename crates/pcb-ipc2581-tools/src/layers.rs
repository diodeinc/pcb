//! Shared IPC-2581 layer-function classification.

use ipc2581::{Symbol, types::Ecad};
use pcb_ir::dialects::ipc::{
    BoardArrayCopperLayer, PhysicalLayer, SurfaceLayerError, TwoSidedSurfaceLayers,
    resolve_two_sided_surface_layers,
};

pub use pcb_ir::import::ipc2581::{is_copper, layer_role, side_for_layer as ir_side};

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
