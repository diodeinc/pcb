use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use ipc2581::types::{MountType, Side};
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::dialects::placement::{
    Document as PlacementDocument, Placement, PlacementMount, PlacementSide,
};
use pcb_ir::geom::{Affine2, Point};
use pcb_ir::import::ipc2581::{ImportedDesign, PopulationState, import_design};

use crate::accessors::{CharacteristicsData, IpcAccessor};

pub fn extract_single_board_placements(accessor: &IpcAccessor<'_>) -> Result<PlacementDocument> {
    let ipc = accessor.ipc();
    let imported = import_design(ipc)?;
    extract_single_board_placements_from_design(accessor, &imported)
}

pub fn extract_single_board_placements_from_design(
    accessor: &IpcAccessor<'_>,
    imported: &ImportedDesign,
) -> Result<PlacementDocument> {
    let layer_sides = imported
        .layer_definitions
        .iter()
        .map(|layer| (imported.resolve(layer.name).to_string(), layer.side))
        .collect::<BTreeMap<_, _>>();
    let bom_lookup = build_bom_lookup(accessor);
    let occurrences = imported.component_occurrences(ArtworkScope::ArrayFlattened)?;
    let root_step = imported.geometry.layout.root_step;
    let root_has_components = root_step.is_some_and(|root| {
        occurrences.iter().any(|occurrence| {
            imported
                .component_definition(occurrence.id.component)
                .is_some_and(|component| component.step == root)
        })
    });
    let component_steps = occurrences
        .iter()
        .filter_map(|occurrence| {
            imported
                .component_definition(occurrence.id.component)
                .map(|component| component.step)
        })
        .collect::<BTreeSet<_>>();
    let selected_step = if root_has_components {
        root_step
    } else {
        match component_steps
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] => None,
            [step] => Some(*step),
            steps => {
                let names = steps
                    .iter()
                    .map(|step| {
                        imported
                            .resolve(imported.geometry.layout.steps[*step as usize].source_step_ref)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "CPL export found multiple component-bearing repeated Steps ({names}); single-board CPL is ambiguous"
                );
            }
        }
    };
    let Some(selected_step) = selected_step else {
        return Ok(PlacementDocument::default());
    };

    let mut components = Vec::new();
    let mut emitted = BTreeSet::new();
    for occurrence in occurrences {
        let component = imported
            .component_definition(occurrence.id.component)
            .context("component occurrence references a missing definition")?;
        if component.step != selected_step || !emitted.insert(occurrence.id.component) {
            continue;
        }
        let component = &component.source;
        let Some(ref_des) = component.ref_des else {
            continue;
        };
        let designator = imported.resolve(ref_des).to_string();
        if designator.is_empty() {
            continue;
        }

        let bom = bom_lookup.get(&designator);
        let component_package = component
            .package_ref
            .map(|package_ref| imported.resolve(package_ref).to_string())
            .filter(|package| !package.is_empty());
        let package = bom
            .and_then(|data| data.package.clone())
            .or(component_package);
        let value = bom.and_then(|data| data.value.clone());
        let populate = match occurrence.population {
            PopulationState::Unspecified => bom.and_then(|data| data.populate),
            PopulationState::Populate => Some(true),
            PopulationState::DoNotPopulate => Some(false),
            PopulationState::Conflicting => {
                bail!("component '{designator}' has conflicting population state")
            }
        };
        let source_xform = component.xform.unwrap_or_default();
        let layer_ref = imported.resolve(component.layer_ref).to_string();
        let side = layer_sides
            .get(&layer_ref)
            .copied()
            .flatten()
            .map(map_side)
            .unwrap_or(PlacementSide::Unknown);
        let transform = occurrence
            .board_from_component
            .unwrap_or(occurrence.root_from_component);
        let placement = decompose_placement(transform)?;

        components.push(Placement {
            designator,
            value,
            package,
            part: imported.resolve(component.part).to_string(),
            layer_ref,
            side,
            mount: map_mount(component.mount_type),
            at: placement.at,
            rotation_degrees: placement.rotation_degrees,
            x_offset: 0.0,
            y_offset: 0.0,
            mirror: placement.mirror,
            face_up: source_xform.face_up,
            scale: placement.scale,
            populate,
        });
    }

    Ok(PlacementDocument { components })
}

struct DecomposedPlacement {
    at: Point,
    rotation_degrees: f64,
    mirror: bool,
    scale: f64,
}

fn decompose_placement(transform: Affine2) -> Result<DecomposedPlacement> {
    let scale = transform.m00.hypot(transform.m10);
    let other_scale = transform.m01.hypot(transform.m11);
    let dot = transform.m00 * transform.m01 + transform.m10 * transform.m11;
    let epsilon = 1e-9 * scale.max(other_scale).max(1.0);
    if scale <= 0.0 || (scale - other_scale).abs() > epsilon || dot.abs() > epsilon {
        bail!("component occurrence transform is not a rigid uniform placement");
    }
    let mirror = transform.determinant() < 0.0;
    let signed_scale = if mirror { -scale } else { scale };
    Ok(DecomposedPlacement {
        at: Point::new(transform.m02, transform.m12),
        rotation_degrees: (transform.m10 / signed_scale)
            .atan2(transform.m00 / signed_scale)
            .to_degrees(),
        mirror,
        scale,
    })
}

#[derive(Debug, Clone)]
struct BomPlacementData {
    value: Option<String>,
    package: Option<String>,
    populate: Option<bool>,
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

        for ref_des in item.reference_designators() {
            let designator = ipc.resolve(ref_des.name).to_string();
            if designator.is_empty() {
                continue;
            }

            let package = ref_des
                .package_ref
                .map(|package| ipc.resolve(package).to_string())
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

#[cfg(test)]
mod tests {
    use ipc2581::Ipc2581;

    use super::*;

    #[test]
    fn panel_cpl_uses_repeated_board_local_placements() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="COMPONENT_TOP" side="TOP"/>
      <Step name="board" type="BOARD">
        <Component refDes="R1" packageRef="R_0603" part="10k" layerRef="F.Cu" mountType="SMT">
          <Xform rotation="90"/>
          <Location x="1.25" y="2.5"/>
        </Component>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="50" y="60" nx="2" ny="1" dx="20" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let placements = extract_single_board_placements(&IpcAccessor::new(&ipc)).unwrap();

        assert_eq!(placements.components.len(), 1);
        let component = &placements.components[0];
        assert_eq!(component.designator, "R1");
        assert_eq!(component.side, PlacementSide::Top);
        assert_eq!(component.at, Point::new(1.25, 2.5));
        assert_eq!(component.rotation_degrees, 90.0);
    }

    #[test]
    fn panel_cpl_rejects_multiple_component_bearing_steps() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="COMPONENT_TOP" side="TOP"/>
      <Step name="left_board" type="BOARD">
        <Component refDes="R1" part="10k" layerRef="F.Cu" mountType="SMT">
          <Location x="1" y="2"/>
        </Component>
      </Step>
      <Step name="right_board" type="BOARD">
        <Component refDes="R1" part="10k" layerRef="F.Cu" mountType="SMT">
          <Location x="3" y="4"/>
        </Component>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="left_board" x="0" y="0"/>
        <StepRepeat stepRef="right_board" x="10" y="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let error = extract_single_board_placements(&IpcAccessor::new(&ipc)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("multiple component-bearing repeated Steps")
        );
    }
}
