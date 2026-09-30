//! Conservative, read-only detection of isolated interior through-via lands.
//!
//! This proves electrical isolation, not that a land is mechanically unnecessary.
//! Callers must establish that source geometry was completely understood: an
//! importer diagnostic or silently ignored source extension is not empty copper.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{Context, Result, ensure};
use ipc2581::types::StandardPrimitive;

use crate::dialects::ipc::{ArtworkScope, FeatureRole, FeatureSpan, PlatingKind, PrimitiveRef};
use crate::geom::dfm::{BBoxIndex, region_clearance_within};
use crate::geom::{Polarity, Resolution};
use crate::import::ipc2581::{FeatureDefinitionId, ImportedDesign, LayerId, is_copper};
use crate::import::physical::{Association, PhysicalHoleKind, physical_stackup_layers};

/// Source definitions safe to remove in *every* occurrence in the complete
/// board/array/panel. The input must have passed a source-completeness check.
/// No geometry is mutated. Different-net and unknown-net copper also blocks
/// removal, as do coincident lands and ambiguous hole associations.
pub fn isolated_via_lands(
    design: &ImportedDesign,
    resolution: Resolution,
) -> Result<BTreeSet<FeatureDefinitionId>> {
    ensure!(
        design
            .layer_definitions
            .iter()
            .enumerate()
            .all(|(i, layer)| {
                !(is_copper(layer.layer_function) || layer.layer_function.is_fabrication())
                    || !design.has_layer_diagnostics(LayerId(i as u32))
            }),
        "Cannot prove via isolation: import has geometry diagnostics"
    );
    let layout = &design.geometry.layout;
    let root =
        layout.steps[layout.root_step.context("Missing layout root")? as usize].source_step_ref;
    let placed = layout
        .instances
        .iter()
        .map(|instance| instance.source_step_ref)
        .chain(std::iter::once(root))
        .collect::<HashSet<_>>();
    // Writers may list descendants in Content too. Their standalone layouts
    // add no copper beyond the occurrences already checked under the root.
    ensure!(
        design
            .content
            .step_refs
            .iter()
            .all(|name| placed.contains(name))
            && design.steps.iter().all(|step| {
                placed.contains(&step.name) && (step.step_repeats.is_empty() || step.is_panel())
            }),
        "Cannot prove via isolation: layout has additional roots, unplaced Steps, or unexpanded repeats"
    );
    let order = physical_stackup_layers(&design.stackups, &design.layer_definitions)?
        .context("Via cleanup requires an explicit physical stackup")?;
    let copper = order
        .into_iter()
        .filter(|name| {
            design
                .layer_definitions
                .iter()
                .any(|l| l.name == *name && is_copper(l.layer_function))
        })
        .collect::<Vec<_>>();
    if copper.len() < 3 {
        return Ok(BTreeSet::new());
    }

    let scope = ArtworkScope::ArrayFlattened;
    // Zero significance prevents small contacts from being discarded by area.
    let resolution = resolution.strict();
    let physical = design.physical_view(scope, resolution)?;
    let lands = physical
        .source_lands
        .iter()
        .map(|land| (land.id, land))
        .collect::<HashMap<_, _>>();
    let mut eligible = HashMap::new();
    let mut claims = HashMap::<_, usize>::new();
    for hole in &physical.holes {
        for link in &hole.lands {
            match &link.land {
                Association::Resolved(id) => *claims.entry(*id).or_default() += 1,
                Association::Ambiguous(ids) | Association::Conflicting(ids) => {
                    for id in ids {
                        *claims.entry(*id).or_default() += 2;
                    }
                }
                Association::Unresolved => {}
            }
        }
        let through = match hole.span {
            FeatureSpan::ThroughBoard => true,
            FeatureSpan::FromTo {
                from: Some(from),
                to: Some(to),
            } => {
                (Some(&from) == copper.first() && Some(&to) == copper.last())
                    || (Some(&to) == copper.first() && Some(&from) == copper.last())
            }
            _ => false,
        };
        if !through
            || hole.kind != PhysicalHoleKind::Round
            || hole.plating != PlatingKind::Via
            || hole.padstack.is_none()
        {
            continue;
        }
        for link in &hole.lands {
            let Some(id) = link.land.resolved() else {
                continue;
            };
            let land = lands[id];
            let feature = design
                .feature_definition(id.0.feature)
                .context("Missing land definition")?;
            let layer = &design.layer_definitions[land.layer.0 as usize];
            if !copper[1..copper.len() - 1].contains(&layer.name)
                || feature.intent.role != FeatureRole::Via
                || feature.polarity != Polarity::Dark
                || land.padstack != hole.padstack
                || land.at != hole.at
                || land.pin.is_some()
                || !land.component_refs.is_empty()
            {
                continue;
            }
            // Only explicit, standard circular lands, not circle-like artwork.
            let Some(PrimitiveRef::Standard(primitive)) = feature.primitive_ref else {
                continue;
            };
            let circle = design
                .content
                .dictionary_standard
                .entries
                .iter()
                .any(|entry| {
                    entry.id == primitive && matches!(entry.primitive, StandardPrimitive::Circle(_))
                });
            if circle && land.image.area() > hole.image.area() {
                eligible.insert(id.0, hole.id);
            }
        }
    }

    let holes_near = BBoxIndex::new(
        physical
            .holes
            .iter()
            .map(|hole| hole.image.bbox())
            .collect(),
    );
    let hole_boundaries = physical
        .holes
        .iter()
        .map(|hole| hole.image.prepare_query())
        .collect::<Vec<_>>();
    let hole_error = physical
        .holes
        .iter()
        .map(|hole| hole.image.uncertainty_mm)
        .fold(0.0, f64::max);
    let mut all_isolated = BTreeMap::<FeatureDefinitionId, bool>::new();
    for (index, layer) in design.layer_definitions.iter().enumerate() {
        if !is_copper(layer.layer_function) {
            continue;
        }
        let occurrences = design.feature_occurrences(LayerId(index as u32), scope)?;
        let images = occurrences
            .iter()
            .map(|&o| design.feature_region(o, resolution))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            images.iter().all(|image| !image.is_empty()),
            "Cannot prove via isolation: empty feature image on {}",
            design.resolve(layer.name)
        );
        let bounds = BBoxIndex::new(images.iter().map(|image| image.bbox()).collect());
        // Reuse each boundary index across vias. Only search within the
        // uncertainty margin: the exact distance to faraway copper is irrelevant.
        let boundaries = images
            .iter()
            .map(|image| image.prepare_query())
            .collect::<Vec<_>>();
        let max_error = images
            .iter()
            .map(|image| image.uncertainty_mm)
            .fold(hole_error, f64::max);
        for (i, occurrence) in occurrences.iter().enumerate() {
            let Some(own_hole) = eligible.get(&occurrence.id) else {
                all_isolated.insert(occurrence.id.feature, false);
                continue;
            };
            let image = &images[i];
            let margin = image.uncertainty_mm + max_error + 1e-6;
            let isolated = claims.get(&crate::import::physical::LandId(occurrence.id)) == Some(&1)
                // A neighboring barrel can contact this land without having
                // its own land here. Conservatively block on all other holes,
                // even holes whose span or plating cannot establish contact.
                && holes_near.query(image.bbox().expand(margin)).into_iter().all(|j| {
                    let hole = &physical.holes[j];
                    hole.id == *own_hole || (!hole.image.is_empty()
                        && region_clearance_within(image, &boundaries[i], &hole.image, &hole_boundaries[j], margin)
                            .is_none_or(|gap| gap.mm > gap.uncertainty_mm + 1e-6))
                })
                && bounds
                    .query(image.bbox().expand(margin))
                    .into_iter()
                    .all(|j| {
                        if i == j {
                            return true;
                        }
                        // Treat clear artwork as an obstacle too: we do not remove
                        // partially cleared pads or reason about paint ordering.
                        region_clearance_within(image, &boundaries[i], &images[j], &boundaries[j], margin)
                            .is_none_or(|gap| gap.mm > gap.uncertainty_mm + 1e-6)
                    });
            all_isolated
                .entry(occurrence.id.feature)
                .and_modify(|all| *all &= isolated)
                .or_insert(isolated);
        }
    }
    Ok(all_isolated
        .into_iter()
        .filter_map(|(id, isolated)| isolated.then_some(id))
        .collect())
}
