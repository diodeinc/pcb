//! Automatic board-array copper balancing.
//!
//! The planner derives a certified safe region and the composed copper image
//! of every copper layer from a completed board array, then runs one joint
//! spatial solve across the stackup via [`crate::copper_balance`]. The steps
//! every panelizer shares live here: the existing copper of each layer, the
//! certification of a safe region, and the layer the solve is handed.

use anyhow::{Context, Result, bail};
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    ArtworkScope, BalancingRegionOptions, BoardArrayBalancingInput, BoardArraySupportDocument,
    BoardArraySupportLayerPolicy, board_array_balancing_region,
    collect_board_array_balancing_input,
};
use pcb_ir::geom::attachment::transform_region;
use pcb_ir::geom::copper_balance::{DenseCopperBalanceProfile, map_layers};
use pcb_ir::geom::{Affine2, ContourSet, Resolution};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId, import_design};

use crate::copper_balance::{
    BalanceVoidTemplate, CERTIFICATE_AREA_TOLERANCE_MM2, CopperBalancePlan, PreparedCopperLayer,
    physical_copper_stack_weights, solve_copper_balance,
};
use crate::generated::GeneratedLayerFeature;
use crate::geometry;
use crate::ipc2581::Ipc2581;

/// Plan best-effort copper balancing for every copper layer in a board array.
///
/// Each layer targets the copper density measured inside the repeated board
/// footprints, extending the board's own density into controllable panel
/// material instead of imposing one universal density across the stackup.
///
/// `ipc` must describe the completed, not-yet-balanced array so that generated
/// rails, V-scores, tooling holes, and fiducials participate in safe-region
/// discovery while balance copper itself does not. Balance geometry is
/// prepared to the profile's own accuracy; `tolerance_mm` only sets which
/// features are significant.
pub fn generate_automatic_board_array_copper_balance(
    ipc: &Ipc2581,
    tolerance_mm: f64,
) -> Result<CopperBalancePlan> {
    let resolution = Resolution::new(tolerance_mm, DenseCopperBalanceProfile::V1.accuracy);
    let imported = import_design(ipc, resolution)?;
    let layout = &imported.geometry;
    let score_lines = geometry::board_array_vscore_lines(&imported)
        .context("failed to extract board-array V-scores for copper balancing")?;
    let fabrication_profile =
        geometry::board_array_fabrication_profile(&imported, layout, &score_lines, resolution)
            .context("failed to derive board-array fabrication profile for copper balancing")?;
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = crate::layers::copper_layers(ecad);
    let support_layers = extract_array_support_layers(&imported)?;
    let collection = collect_board_array_balancing_input(
        layout,
        &fabrication_profile,
        &copper_layers,
        support_layers
            .iter()
            .map(|source| BoardArraySupportDocument::new(&source.document, source.policy)),
        resolution,
    )
    .context("failed to collect board-array balancing obstacles")?;
    let panel_outer = &collection.panel_outer;
    let board_footprints = &collection.board_footprints;
    let layer_names = copper_layers
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<Vec<_>>();
    let existing = existing_copper(&imported, &layer_names, panel_outer, resolution)?;
    // Existing copper outside the board footprints participates as an
    // obstacle, so copper the array-support geometry does not capture
    // shrinks the certified safe region instead of failing the solve.
    let frame = existing
        .iter()
        .map(|copper| copper.difference(board_footprints))
        .collect::<Result<Vec<_>, _>>()?;
    // Layers with no frame copper and the same support scope share a region.
    let mut inputs = Vec::new();
    let mut region_of = Vec::with_capacity(copper_layers.len());
    for (index, layer) in copper_layers.iter().enumerate() {
        let representative = (0..index).find(|&earlier| {
            frame[index].is_empty()
                && frame[earlier].is_empty()
                && collection.has_same_support_scope(copper_layers[earlier].name, layer.name)
        });
        region_of.push(match representative {
            Some(earlier) => region_of[earlier],
            None => {
                let mut input = collection.input_for_layer(layer.name)?;
                input.support_features = input.support_features.union(&frame[index])?;
                inputs.push((layer_names[index].as_str(), input));
                inputs.len() - 1
            }
        });
    }
    let regions = map_layers(inputs, |(layer_name, input)| {
        certified_safe_region(&input, &format!("board-array layer '{layer_name}'"))
    })
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let stack_weights = physical_copper_stack_weights(ipc);
    let prepared = map_layers(
        layer_names.iter().zip(existing).zip(frame).zip(region_of),
        |(((layer_name, existing), frame), region)| {
            prepared_layer(
                layer_name,
                existing,
                &frame,
                regions[region].clone(),
                board_footprints,
                stack_weights.as_ref(),
            )
        },
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    solve_copper_balance(
        panel_outer,
        board_footprints.clone(),
        stack_weights.is_some(),
        prepared,
    )
}

/// The existing copper of each named layer inside `panel`, one layer per
/// thread. A step's artwork is composed once and placed at every occurrence
/// of the step: the occurrences of a panel do not paint over one another, so
/// their union is the image a flattened composition would reach after
/// stroking and uniting every board's copper again for every instance.
pub(crate) fn existing_copper(
    imported: &ImportedDesign,
    layer_names: &[String],
    panel: &ContourSet,
    resolution: Resolution,
) -> Result<Vec<ContourSet>> {
    let layout = &imported.geometry.layout;
    let root = layout
        .root_step
        .context("IPC-2581 primary step has no canonical layout root")?;
    map_layers(layer_names, |layer_name| {
        let layer = imported
            .layer_id(layer_name)
            .context("missing copper layer")?;
        let definition = imported
            .layer_definition(layer)
            .context("missing copper layer")?;
        let images = (0..layout.steps.len() as u32)
            .map(|step| {
                imported
                    .materialize_step_layer(step, layer)?
                    .into_layer_image(
                        0,
                        crate::layers::layer_role(definition.layer_function),
                        crate::layers::ir_side(definition.side),
                        resolution,
                    )
            })
            .collect::<Result<Vec<_>>>()?;
        let placed = std::iter::once((root, Affine2::IDENTITY))
            .chain(
                layout
                    .instances
                    .iter()
                    .map(|instance| (instance.child_step, instance.transform)),
            )
            .filter(|(step, _)| !images[*step as usize].is_empty())
            .map(|(step, transform)| Ok(transform_region(&images[step as usize], transform)?))
            .collect::<Result<Vec<_>>>()?;
        Ok(ContourSet::union_all(resolution, placed)?.intersection(panel)?)
    })
    .into_iter()
    .collect()
}

/// The region `input` leaves safe for generated copper, or an error naming
/// `what` when its clearance certificate does not hold.
pub(crate) fn certified_safe_region(
    input: &BoardArrayBalancingInput,
    what: &str,
) -> Result<ContourSet> {
    let region = board_array_balancing_region(input, BalancingRegionOptions::default())
        .with_context(|| format!("failed to compute balancing region for {what}"))?;
    if !region.certificate.passes(CERTIFICATE_AREA_TOLERANCE_MM2) {
        bail!("computed balancing region for {what} failed clearance certification");
    }
    Ok(region.safe_region)
}

/// One layer as the joint solve takes it. The target is the density of the
/// copper inside the immutable `footprints`; `stray` is this layer's copper
/// outside them, which belongs to its density domain although nothing may be
/// generated there.
pub(crate) fn prepared_layer(
    layer_name: &str,
    existing_copper: ContourSet,
    stray: &ContourSet,
    safe_region: ContourSet,
    footprints: &ContourSet,
    stack_weights: Option<&std::collections::HashMap<String, f64>>,
) -> Result<PreparedCopperLayer> {
    Ok(PreparedCopperLayer {
        layer_name: layer_name.to_string(),
        target_density: (existing_copper.intersection(footprints)?.area() / footprints.area())
            .clamp(0.0, 1.0),
        stack_weight_mm2: stack_weights
            .and_then(|weights| weights.get(layer_name).copied())
            .unwrap_or(0.0),
        density_domain: footprints.union(stray)?.union(&safe_region)?,
        existing_copper,
        safe_region,
    })
}

/// A plan's generated feature sets, layer by layer, and the void templates
/// they reference, each once.
pub(crate) fn generated_features(
    plan: CopperBalancePlan,
) -> (Vec<BalanceVoidTemplate>, Vec<GeneratedLayerFeature>) {
    let mut templates: Vec<BalanceVoidTemplate> = Vec::new();
    let features = plan
        .layers
        .into_iter()
        .flat_map(|layer| {
            let (layer_templates, features) = layer.features.into_layer_features(&layer.layer_name);
            for template in layer_templates {
                if !templates.iter().any(|entry| entry.id == template.id) {
                    templates.push(template);
                }
            }
            features
        })
        .collect();
    (templates, features)
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
    pub document: pcb_ir::import::ipc2581::GeometryDocument,
}

/// Extract every ECAD layer as an `ArraySupport` document for safe-region
/// discovery.
pub fn extract_array_support_layers(
    imported: &ImportedDesign,
) -> Result<Vec<ArraySupportLayerSource>> {
    imported
        .layer_definitions
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            let name = imported.resolve(layer.name).to_string();
            let document = imported
                .materialize_layer(LayerId(index as u32), ArtworkScope::ArraySupport)
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
