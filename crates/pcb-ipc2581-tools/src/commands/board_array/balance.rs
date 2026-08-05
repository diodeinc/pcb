//! Automatic board-array copper balancing.
//!
//! The planner derives a certified safe region and the composed copper image
//! of every copper layer from a completed board array, then runs one joint
//! spatial solve across the stackup via [`crate::copper_balance`].

use anyhow::{Context, Result, bail};
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    BalancingRegionOptions, BoardArraySupportDocument, BoardArraySupportLayerPolicy, View,
    board_array_balancing_region, collect_board_array_balancing_input,
};

use crate::copper_balance::{
    CERTIFICATE_AREA_TOLERANCE_MM2, CopperBalancePlan, PreparedCopperLayer, composed_copper_image,
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
pub fn generate_automatic_board_array_copper_balance(ipc: &Ipc2581) -> Result<CopperBalancePlan> {
    let layout = geometry::extract_layout(ipc)
        .context("failed to extract board-array layout for copper balancing")?;
    let score_lines = geometry::board_array_vscore_lines(ipc)
        .context("failed to extract board-array V-scores for copper balancing")?;
    let fabrication_profile = geometry::board_array_fabrication_profile(ipc, &layout, &score_lines)
        .context("failed to derive board-array fabrication profile for copper balancing")?;
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = crate::layers::copper_layers(ecad);
    let support_layers = extract_array_support_layers(ipc)?;
    let collection = collect_board_array_balancing_input(
        &layout,
        &fabrication_profile,
        &copper_layers,
        support_layers
            .iter()
            .map(|source| BoardArraySupportDocument::new(&source.document, source.policy)),
    )
    .context("failed to collect board-array balancing obstacles")?;
    let panel_outer = &collection.panel_outer;
    let board_footprints = &collection.board_footprints;
    let board_area_mm2 = board_footprints.area();
    let stack_weights = physical_copper_stack_weights(ipc);
    let stack_weights_available = stack_weights.is_some();
    let mut prepared: Vec<PreparedLayerBalance> = Vec::with_capacity(copper_layers.len());
    for layer in &copper_layers {
        let layer_name = ipc.resolve(layer.name).to_string();
        let existing_copper = composed_copper_image(ipc, &layer_name)?.intersection(panel_outer);
        let board_target_density = (existing_copper.intersection(board_footprints).area()
            / board_area_mm2)
            .clamp(0.0, 1.0);
        // Existing copper outside the board footprints participates as an
        // obstacle, so copper the array-support geometry does not capture
        // shrinks the certified safe region instead of failing the solve.
        let frame_copper = existing_copper.difference(board_footprints);
        let frame_copper_empty = frame_copper.is_empty();
        let stack_weight_mm2 = stack_weights
            .as_ref()
            .and_then(|weights| weights.get(&layer_name).copied())
            .unwrap_or(0.0);
        let safe_region = if frame_copper_empty
            && let Some(representative) = prepared.iter().find(|candidate| {
                candidate.frame_copper_empty
                    && collection.has_same_support_scope(candidate.layer, layer.name)
            }) {
            representative.inner.safe_region.clone()
        } else {
            let mut balancing_input = collection.input_for_layer(layer.name);
            balancing_input.support_features =
                balancing_input.support_features.union(&frame_copper);
            let balancing_region =
                board_array_balancing_region(&balancing_input, BalancingRegionOptions::default());
            let balancing_region = balancing_region.context(format!(
                "failed to compute balancing region for layer '{layer_name}'"
            ))?;
            if !balancing_region
                .certificate
                .passes(CERTIFICATE_AREA_TOLERANCE_MM2)
            {
                bail!(
                    "computed board-array balancing region for layer '{layer_name}' failed clearance certification"
                );
            }
            balancing_region.safe_region
        };
        prepared.push(PreparedLayerBalance {
            layer: layer.name,
            frame_copper_empty,
            inner: PreparedCopperLayer {
                layer_name,
                target_density: board_target_density,
                stack_weight_mm2,
                existing_copper,
                safe_region,
            },
        });
    }

    solve_copper_balance(
        panel_outer,
        board_footprints.clone(),
        stack_weights_available,
        prepared.into_iter().map(|layer| layer.inner).collect(),
    )
}

struct PreparedLayerBalance {
    layer: Symbol,
    frame_copper_empty: bool,
    inner: PreparedCopperLayer,
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
pub fn extract_array_support_layers(ipc: &Ipc2581) -> Result<Vec<ArraySupportLayerSource>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    ecad.cad_data
        .layers
        .iter()
        .map(|layer| {
            let name = ipc.resolve(layer.name).to_string();
            let document = geometry::extract_layer_for_view(ipc, &name, View::ArraySupport)
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
