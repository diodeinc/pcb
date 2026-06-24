pub mod dxf;
mod extract;
pub mod render;

use anyhow::{Context, Result};
use ipc2581::{Ipc2581, types::LayerFunction};
use pcb_ir::dialects::ipc::{
    GeometryView,
    relief::{VScoreLine, vscore_lines_for},
};

pub use extract::{extract_layer, extract_layer_for_view, extract_layout};

pub fn board_array_vscore_lines(ipc: &Ipc2581) -> Result<Vec<VScoreLine>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut lines = Vec::new();
    for source_layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::VCut)
    {
        let layer_name = ipc.resolve(source_layer.name);
        let doc = extract_layer_for_view(ipc, layer_name, GeometryView::ArrayFlattened)
            .with_context(|| format!("failed to extract IPC-2581 V-cut layer '{layer_name}'"))?;
        lines.extend(vscore_lines_for(&doc));
    }
    Ok(lines)
}
