//! Automatic fab-panel copper balancing.
//!
//! The gutters between the placed assembly panels are the only balancing
//! region: the reserved process margins stay bare, and every placed panel is
//! immutable. The fabrication-panel step adds no per-layer geometry of its
//! own, so one certified safe region serves the whole copper stack.

use anyhow::{Context, Result, bail};
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::dialects::ipc::{
    BalancingRegionOptions, board_array_balancing_region, collect_fab_panel_balancing_input,
};
use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::geom::copper_balance::map_layers;
use pcb_ir::geom::{BBox, ContourSet, tol};
use pcb_ir::import::ipc2581::import_design;

use crate::copper_balance::{
    CERTIFICATE_AREA_TOLERANCE_MM2, CopperBalancePlan, PreparedCopperLayer,
    physical_copper_stack_weights, solve_copper_balance,
};
use crate::geometry;
use crate::ipc2581::Ipc2581;

/// Plan best-effort copper balancing for every copper layer of a fabrication
/// panel.
///
/// Each layer targets the aggregate copper density measured inside the placed
/// assembly panels, extending their already-balanced density into the gutters
/// between them. `ipc` must describe the completed, not-yet-balanced
/// fabrication panel, and `usable` the stock region between the reserved
/// process margins; the margins never enter the density domain and stay bare.
pub(super) fn generate_automatic_fab_panel_copper_balance(
    ipc: &Ipc2581,
    usable: BBox,
    accuracy: GeometryAccuracy,
) -> Result<CopperBalancePlan> {
    let imported = import_design(ipc, accuracy)?;
    let layout = &imported.geometry;
    // V-scores and profile cutouts all lie inside the placed assembly panels,
    // so the fabrication profile needs no relief geometry here.
    let fabrication_profile =
        geometry::board_array_fabrication_profile_from_design(&imported, layout, &[], accuracy)
            .context("failed to derive fabrication-panel profile for copper balancing")?;
    let usable_region = ContourSet::rectangle(usable, tol::REGION_MM);
    let mut input =
        collect_fab_panel_balancing_input(usable_region.clone(), &fabrication_profile, accuracy)
            .context("failed to collect fabrication-panel balancing obstacles")?;
    let footprints = input.board_footprints.clone();
    let footprint_area_mm2 = footprints.area();

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let layer_names = crate::layers::copper_layers(ecad)
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<Vec<_>>();
    let extract = |layer_name: &String| {
        imported
            .composed_layer_image(
                imported
                    .layer_id(layer_name)
                    .context("missing copper layer")?,
                ArtworkScope::ArrayFlattened,
                accuracy,
            )
            .map(|image| (layer_name.clone(), image.intersection(&usable_region)))
    };
    let copper_images = map_layers(&layer_names, extract)
        .into_iter()
        .collect::<Result<Vec<_>>>()?;
    // Copper found outside the placed panels joins the shared obstacle set,
    // so unexpected overhang shrinks the certified safe region for every
    // layer instead of failing the solve. Each layer keeps its own overhang:
    // it is real copper and belongs in that layer's density domain even
    // though no generated copper may be placed there.
    let stray_copper = copper_images
        .iter()
        .map(|(_, image)| image.difference(&footprints))
        .collect::<Vec<_>>();
    for stray in &stray_copper {
        input.support_features = input.support_features.union(stray);
    }

    let balancing_region =
        board_array_balancing_region(&input, BalancingRegionOptions::default(), accuracy)
            .context("failed to compute fabrication-panel balancing region")?;
    if !balancing_region
        .certificate
        .passes(CERTIFICATE_AREA_TOLERANCE_MM2)
    {
        bail!("computed fabrication-panel balancing region failed clearance certification");
    }
    let safe_region = balancing_region.safe_region;

    // The gutters the solver may fill, plus the panels whose density set the
    // target. Everything else inside the usable region — clearance around each
    // placed panel, material removal, gaps too narrow for a void — can never
    // hold generated copper and so stays out of the density denominator.
    let panel_domain = footprints.union(&safe_region);

    let stack_weights = physical_copper_stack_weights(ipc);
    let stack_weights_available = stack_weights.is_some();
    let prepared = copper_images
        .into_iter()
        .zip(stray_copper)
        .map(|((layer_name, existing_copper), stray)| {
            let target_density = (existing_copper.intersection(&footprints).area()
                / footprint_area_mm2)
                .clamp(0.0, 1.0);
            let stack_weight_mm2 = stack_weights
                .as_ref()
                .and_then(|weights| weights.get(&layer_name).copied())
                .unwrap_or(0.0);
            PreparedCopperLayer {
                layer_name,
                target_density,
                stack_weight_mm2,
                existing_copper,
                safe_region: safe_region.clone(),
                density_domain: panel_domain.union(&stray),
            }
        })
        .collect();

    solve_copper_balance(
        &usable_region,
        footprints,
        stack_weights_available,
        prepared,
        accuracy,
    )
}
