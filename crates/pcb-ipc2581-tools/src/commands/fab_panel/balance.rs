//! Automatic fab-panel copper balancing.
//!
//! The gutters between the placed assembly panels are the only balancing
//! region: the reserved process margins stay bare, and every placed panel is
//! immutable. The fabrication-panel step adds no per-layer geometry of its
//! own, so one certified safe region serves the whole copper stack.

use anyhow::{Context, Result};
use pcb_ir::dialects::ipc::collect_fab_panel_balancing_input;
use pcb_ir::geom::Resolution;
use pcb_ir::geom::copper_balance::DenseCopperBalanceProfile;
use pcb_ir::geom::{BBox, ContourSet};
use pcb_ir::import::ipc2581::import_design;

use crate::commands::board_array::balance::{
    certified_safe_region, existing_copper, prepared_layer,
};
use crate::copper_balance::{
    CopperBalancePlan, physical_copper_stack_weights, solve_copper_balance,
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
/// Balance geometry is prepared to the profile's own accuracy; `tolerance_mm`
/// only sets which features are significant.
pub(super) fn generate_automatic_fab_panel_copper_balance(
    ipc: &Ipc2581,
    usable: BBox,
    tolerance_mm: f64,
) -> Result<CopperBalancePlan> {
    let resolution = Resolution::new(tolerance_mm, DenseCopperBalanceProfile::V1.accuracy);
    let imported = import_design(ipc, resolution)?;
    let layout = &imported.geometry;
    // V-scores and profile cutouts all lie inside the placed assembly panels,
    // so the fabrication profile needs no relief geometry here.
    let fabrication_profile =
        geometry::board_array_fabrication_profile(&imported, layout, &[], resolution)
            .context("failed to derive fabrication-panel profile for copper balancing")?;
    let usable_region = ContourSet::rectangle(usable, resolution);
    let mut input = collect_fab_panel_balancing_input(usable_region.clone(), &fabrication_profile)
        .context("failed to collect fabrication-panel balancing obstacles")?;
    let footprints = input.board_footprints.clone();

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let layer_names = crate::layers::copper_layers(ecad)
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<Vec<_>>();
    let copper_images = existing_copper(&imported, &layer_names, &usable_region, resolution)?;
    // Copper found outside the placed panels joins the shared obstacle set,
    // so unexpected overhang shrinks the certified safe region for every
    // layer instead of failing the solve. Each layer keeps its own overhang:
    // it is real copper and belongs in that layer's density domain even
    // though no generated copper may be placed there.
    let stray_copper = copper_images
        .iter()
        .map(|image| image.difference(&footprints))
        .collect::<Result<Vec<_>, _>>()?;
    for stray in &stray_copper {
        input.support_features = input.support_features.union(stray)?;
    }

    // The gutters the solver may fill. Everything else inside the usable
    // region — clearance around each placed panel, material removal, gaps too
    // narrow for a void — can never hold generated copper and so stays out of
    // every layer's density domain.
    let safe_region = certified_safe_region(&input, "the fabrication panel")?;

    let stack_weights = physical_copper_stack_weights(ipc);
    let prepared = layer_names
        .iter()
        .zip(copper_images)
        .zip(&stray_copper)
        .map(|((layer_name, existing), stray)| {
            prepared_layer(
                layer_name,
                existing,
                stray,
                safe_region.clone(),
                &footprints,
                stack_weights.as_ref(),
            )
        })
        .collect::<Result<Vec<_>>>()?;

    solve_copper_balance(
        &usable_region,
        footprints,
        stack_weights.is_some(),
        prepared,
    )
}
