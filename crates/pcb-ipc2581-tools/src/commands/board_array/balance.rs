//! Automatic board-array copper balancing.
//!
//! The planner derives a certified safe region and the composed copper image
//! of every copper layer from a completed board array, then runs one joint
//! spatial solve across the stackup via [`crate::copper_balance`].

use anyhow::{Context, Result, bail};
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    ArtworkScope, BalancingRegionOptions, BoardArraySupportDocument, BoardArraySupportLayerPolicy,
    board_array_balancing_region, collect_board_array_balancing_input,
};
use pcb_ir::geom::copper_balance::map_layers;
use pcb_ir::geom::{ContourSet, GeometryAccuracy};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId, import_design};

use crate::copper_balance::{
    CERTIFICATE_AREA_TOLERANCE_MM2, CopperBalancePlan, PreparedCopperLayer,
    physical_copper_stack_weights, solve_copper_balance,
};
use crate::geometry;
use crate::ipc2581::{Ipc2581, Symbol};

/// Plan best-effort copper balancing for every copper layer in a board array.
///
/// Each layer targets the copper density measured inside the repeated board
/// footprints, extending the board's own density into controllable panel
/// material instead of imposing one universal density across the stackup.
///
/// `ipc` must describe the completed, not-yet-balanced array so that generated
/// rails, V-scores, tooling holes, and fiducials participate in safe-region
/// discovery while balance copper itself does not.
pub fn generate_automatic_board_array_copper_balance(
    ipc: &Ipc2581,
    accuracy: GeometryAccuracy,
) -> Result<CopperBalancePlan> {
    let imported = import_design(ipc, accuracy)?;
    let layout = &imported.geometry;
    let score_lines = geometry::board_array_vscore_lines_from_design(&imported, accuracy)
        .context("failed to extract board-array V-scores for copper balancing")?;
    let fabrication_profile = geometry::board_array_fabrication_profile_from_design(
        &imported,
        layout,
        &score_lines,
        accuracy,
    )
    .context("failed to derive board-array fabrication profile for copper balancing")?;
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = crate::layers::copper_layers(ecad);
    let support_layers = extract_array_support_layers(&imported, accuracy)?;
    let collection = collect_board_array_balancing_input(
        layout,
        &fabrication_profile,
        &copper_layers,
        support_layers
            .iter()
            .map(|source| BoardArraySupportDocument::new(&source.document, source.policy)),
        accuracy,
    )
    .context("failed to collect board-array balancing obstacles")?;
    let panel_outer = &collection.panel_outer;
    let board_footprints = &collection.board_footprints;
    let board_area_mm2 = board_footprints.area();
    let stack_weights = physical_copper_stack_weights(ipc);
    let stack_weights_available = stack_weights.is_some();
    let mut prepared: Vec<LayerCopper> = Vec::with_capacity(copper_layers.len());
    let mut inputs = Vec::new();
    for layer in &copper_layers {
        let layer_name = ipc.resolve(layer.name).to_string();
        let existing_copper = imported
            .composed_layer_image(
                imported
                    .layer_id(&layer_name)
                    .context("missing copper layer")?,
                ArtworkScope::ArrayFlattened,
                accuracy,
            )?
            .intersection(panel_outer);
        // Existing copper outside the board footprints participates as an
        // obstacle, so copper the array-support geometry does not capture
        // shrinks the certified safe region instead of failing the solve.
        let frame_copper = existing_copper.difference(board_footprints);
        let frame_copper_empty = frame_copper.is_empty();
        let representative = prepared
            .iter()
            .find(|candidate| {
                frame_copper_empty
                    && candidate.frame.is_empty()
                    && collection.has_same_support_scope(candidate.layer, layer.name)
            })
            .map(|candidate| candidate.region);
        let input_index = representative.unwrap_or_else(|| {
            let index = inputs.len();
            let mut input = collection.input_for_layer(layer.name);
            input.support_features = input.support_features.union(&frame_copper);
            inputs.push((layer_name.clone(), input));
            index
        });
        prepared.push(LayerCopper {
            layer: layer.name,
            name: layer_name,
            existing: existing_copper,
            frame: frame_copper,
            region: input_index,
        });
    }
    let regions = map_layers(inputs, |(layer_name, input)| {
        let region = board_array_balancing_region(&input, BalancingRegionOptions::default(), accuracy)
            .with_context(|| format!("failed to compute balancing region for layer '{layer_name}'"))?;
        if !region.certificate.passes(CERTIFICATE_AREA_TOLERANCE_MM2) {
            bail!("computed board-array balancing region for layer '{layer_name}' failed clearance certification");
        }
        Ok(region.safe_region)
    }).into_iter().collect::<Result<Vec<_>>>()?;
    let prepared = prepared
        .into_iter()
        .map(|layer| {
            let safe_region = regions[layer.region].clone();
            PreparedCopperLayer {
                target_density: (layer.existing.intersection(board_footprints).area()
                    / board_area_mm2)
                    .clamp(0.0, 1.0),
                stack_weight_mm2: stack_weights
                    .as_ref()
                    .and_then(|weights| weights.get(&layer.name).copied())
                    .unwrap_or(0.0),
                density_domain: board_footprints.union(&layer.frame).union(&safe_region),
                layer_name: layer.name,
                existing_copper: layer.existing,
                safe_region,
            }
        })
        .collect();

    solve_copper_balance(
        panel_outer,
        board_footprints.clone(),
        stack_weights_available,
        prepared,
        accuracy,
    )
}

struct LayerCopper {
    layer: Symbol,
    name: String,
    existing: ContourSet,
    frame: ContourSet,
    region: usize,
}

/// One ECAD layer's `ArraySupport` view plus its physical-obstacle policy.
///
/// V-cut layers restrict physical obstacles to `V_Cut` operation features so
/// same-layer callout arrows and labels stay documentation; every other layer
/// contributes all painted features.
pub struct ArraySupportLayerSource {
    pub name: String,
    pub layer_function: LayerFunction,
    pub policy: BoardArraySupportLayerPolicy,
    pub document: SupportDocument,
}

type SupportDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;

/// Extract every ECAD layer as an `ArraySupport` document for safe-region
/// discovery.
pub fn extract_array_support_layers(
    imported: &ImportedDesign,
    accuracy: GeometryAccuracy,
) -> Result<Vec<ArraySupportLayerSource>> {
    imported
        .layer_definitions
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            let name = imported.resolve(layer.name).to_string();
            let document = imported
                .materialize_layer(LayerId(index as u32), ArtworkScope::ArraySupport, accuracy)
                .with_context(|| {
                    format!("failed to extract IPC-2581 array-support layer '{name}'")
                })?;
            let policy = if layer.layer_function == LayerFunction::VCut {
                BoardArraySupportLayerPolicy::VCutOperationsOnly
            } else {
                BoardArraySupportLayerPolicy::AllPaintedFeatures
            };
            Ok(ArraySupportLayerSource {
                name,
                layer_function: layer.layer_function,
                policy,
                document,
            })
        })
        .collect()
}
