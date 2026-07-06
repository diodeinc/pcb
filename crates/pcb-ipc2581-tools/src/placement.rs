use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ipc2581::types::{MountType, Side, Step};
use ipc2581::{Ipc2581, Symbol};
use pcb_ir::dialects::placement::{
    Document as PlacementDocument, Placement, PlacementMount, PlacementSide,
};
use pcb_ir::geom::Point;

use crate::accessors::{CharacteristicsData, IpcAccessor};

pub fn extract_single_board_placements(accessor: &IpcAccessor<'_>) -> Result<PlacementDocument> {
    let ipc = accessor.ipc();
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let primary_step = accessor
        .primary_step()
        .context("IPC-2581 file has no primary Step")?;
    let step = cpl_source_step(ipc, primary_step, &ecad.cad_data.steps)?;

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

        components.push(Placement {
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

fn cpl_source_step<'a>(
    ipc: &Ipc2581,
    primary_step: &'a Step,
    steps: &'a [Step],
) -> Result<&'a Step> {
    if !primary_step.components.is_empty() || primary_step.step_repeats.is_empty() {
        return Ok(primary_step);
    }

    let mut visited = Vec::new();
    let mut component_steps = Vec::new();
    collect_component_steps(ipc, primary_step, steps, &mut visited, &mut component_steps)?;

    match component_steps.as_slice() {
        [] => Ok(primary_step),
        [step] => Ok(step),
        _ => {
            let names = component_steps
                .iter()
                .map(|step| ipc.resolve(step.name))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "CPL export found multiple component-bearing repeated Steps ({names}); single-board CPL is ambiguous"
            );
        }
    }
}

fn collect_component_steps<'a>(
    ipc: &Ipc2581,
    step: &'a Step,
    steps: &'a [Step],
    visited: &mut Vec<Symbol>,
    component_steps: &mut Vec<&'a Step>,
) -> Result<()> {
    if visited.contains(&step.name) {
        return Ok(());
    }
    visited.push(step.name);

    for repeat in &step.step_repeats {
        let child = steps
            .iter()
            .find(|step| step.name == repeat.step_ref)
            .with_context(|| {
                format!(
                    "StepRepeat references unknown Step '{}'",
                    ipc.resolve(repeat.step_ref)
                )
            })?;
        if !child.components.is_empty()
            && !component_steps
                .iter()
                .any(|component_step| component_step.name == child.name)
        {
            component_steps.push(child);
        }
        collect_component_steps(ipc, child, steps, visited, component_steps)?;
    }

    Ok(())
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
