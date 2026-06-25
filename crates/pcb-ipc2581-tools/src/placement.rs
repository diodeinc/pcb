use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ipc2581::types::{MountType, Side};
use pcb_ir::common::Point;
use pcb_ir::dialects::placement::{
    ComponentPlacement, PlacementDocument, PlacementMount, PlacementSide,
};

use crate::accessors::{CharacteristicsData, IpcAccessor};

pub fn extract_primary_step_placements(accessor: &IpcAccessor<'_>) -> Result<PlacementDocument> {
    let ipc = accessor.ipc();
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step = accessor
        .primary_step()
        .context("IPC-2581 file has no primary Step")?;

    if step.components.is_empty() && !step.step_repeats.is_empty() {
        anyhow::bail!(
            "primary Step contains only StepRepeat instances; panel CPL expansion is not implemented yet"
        );
    }

    let layer_sides = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| (ipc.resolve(layer.name).to_string(), layer.side))
        .collect::<BTreeMap<_, _>>();
    let bom_lookup = build_bom_lookup(accessor);

    let mut components = Vec::new();
    for component in &step.components {
        let Some(ref_des) = component.ref_des else {
            continue;
        };
        let designator = ipc.resolve(ref_des).to_string();
        if designator.is_empty() {
            continue;
        }

        let bom = bom_lookup.get(&designator);
        let component_package = component
            .package_ref
            .map(|package_ref| ipc.resolve(package_ref).to_string())
            .filter(|package| !package.is_empty());
        let package = bom
            .and_then(|data| data.package.clone())
            .or(component_package);
        let value = bom.and_then(|data| data.value.clone());
        let populate = bom.map(|data| data.populate);
        let xform = component.xform.unwrap_or_default();
        let layer_ref = ipc.resolve(component.layer_ref).to_string();
        let side = layer_sides
            .get(&layer_ref)
            .copied()
            .flatten()
            .map(map_side)
            .unwrap_or(PlacementSide::Unknown);

        components.push(ComponentPlacement {
            designator,
            value,
            package,
            part: ipc.resolve(component.part).to_string(),
            layer_ref,
            side,
            mount: map_mount(component.mount_type),
            at: Point::new(component.location.x, component.location.y),
            rotation_degrees: xform.rotation,
            x_offset: xform.x_offset,
            y_offset: xform.y_offset,
            mirror: xform.mirror,
            face_up: xform.face_up,
            scale: xform.scale,
            populate,
        });
    }

    Ok(PlacementDocument { components })
}

#[derive(Debug, Clone)]
struct BomPlacementData {
    value: Option<String>,
    package: Option<String>,
    populate: bool,
}

fn build_bom_lookup(accessor: &IpcAccessor<'_>) -> BTreeMap<String, BomPlacementData> {
    let ipc = accessor.ipc();
    let mut lookup = BTreeMap::new();

    let Some(bom) = ipc.bom() else {
        return lookup;
    };

    for item in &bom.items {
        let characteristics = item
            .characteristics
            .as_ref()
            .map(|chars| accessor.extract_characteristics(chars))
            .unwrap_or_else(CharacteristicsData::default);

        for ref_des in &item.ref_des_list {
            let designator = ipc.resolve(ref_des.name).to_string();
            if designator.is_empty() {
                continue;
            }

            let package = Some(ipc.resolve(ref_des.package_ref).to_string())
                .filter(|package| !package.is_empty())
                .or_else(|| characteristics.package.clone());

            lookup.insert(
                designator,
                BomPlacementData {
                    value: characteristics.value.clone(),
                    package,
                    populate: ref_des.populate,
                },
            );
        }
    }

    lookup
}

fn map_side(side: Side) -> PlacementSide {
    match side {
        Side::Top => PlacementSide::Top,
        Side::Bottom => PlacementSide::Bottom,
        Side::Internal => PlacementSide::Internal,
        Side::Both | Side::All | Side::None => PlacementSide::Unknown,
    }
}

fn map_mount(mount: MountType) -> PlacementMount {
    match mount {
        MountType::Smt => PlacementMount::Smt,
        MountType::Thmt => PlacementMount::ThroughHole,
        MountType::Embedded => PlacementMount::Embedded,
        MountType::PressFit => PlacementMount::PressFit,
        MountType::WireBonded => PlacementMount::WireBonded,
        MountType::Glued => PlacementMount::Glued,
        MountType::Clamped => PlacementMount::Clamped,
        MountType::Socketed => PlacementMount::Socketed,
        MountType::Formed => PlacementMount::Formed,
        MountType::Other => PlacementMount::Other,
    }
}
