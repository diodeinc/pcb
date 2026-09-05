use anyhow::Result;
use pcb_ir::dialects::assembly::Scope;
use pcb_ir::dialects::placement::{Document as PlacementDocument, lower_single_board};
use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::import::ipc2581::{ImportedDesign, import_design};

use crate::accessors::IpcAccessor;

pub fn extract_single_board_placements(
    accessor: &IpcAccessor<'_>,
    accuracy: GeometryAccuracy,
) -> Result<PlacementDocument> {
    let ipc = accessor.ipc();
    let imported = import_design(ipc, accuracy)?;
    extract_single_board_placements_from_design(&imported, accuracy)
}

pub fn extract_single_board_placements_from_design(
    imported: &ImportedDesign,
    accuracy: GeometryAccuracy,
) -> Result<PlacementDocument> {
    lower_single_board(&imported.assembly_document(Scope::BoardArray, accuracy)?)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ipc2581::Ipc2581;
    use pcb_ir::dialects::assembly::BomCategory;
    use pcb_ir::dialects::placement::PlacementSide;
    use pcb_ir::geom::Point;

    use super::*;

    #[test]
    fn panel_cpl_uses_repeated_board_local_placements() {
        let accuracy = GeometryAccuracy::default();

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
        let placements =
            extract_single_board_placements(&IpcAccessor::new(&ipc), accuracy).unwrap();

        assert_eq!(placements.components.len(), 1);
        let component = &placements.components[0];
        assert_eq!(component.designator, "R1");
        assert_eq!(component.side, PlacementSide::Top);
        assert_eq!(component.at, Point::new(1.25, 2.5));
        assert_eq!(component.rotation_degrees, 90.0);
    }

    #[test]
    fn dm0002_preserves_bom_categories_in_placement() {
        let accuracy = GeometryAccuracy::default();

        let compressed = include_bytes!("../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
        let xml = zstd::decode_all(Cursor::new(compressed)).unwrap();
        let xml = std::str::from_utf8(&xml).unwrap();
        let imported = import_design(&Ipc2581::parse(xml).unwrap(), accuracy).unwrap();

        let placements = extract_single_board_placements_from_design(&imported, accuracy).unwrap();
        assert_eq!(placements.components.len(), 59);
        assert_eq!(
            placements
                .components
                .iter()
                .filter(|component| component.bom_category == Some(BomCategory::Document))
                .count(),
            6
        );
        assert!(
            placements
                .components
                .iter()
                .filter(|component| component.bom_category != Some(BomCategory::Document))
                .all(|component| component.side == PlacementSide::Top)
        );
    }

    #[test]
    fn panel_cpl_rejects_multiple_component_bearing_steps() {
        let accuracy = GeometryAccuracy::default();

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

        let error = extract_single_board_placements(&IpcAccessor::new(&ipc), accuracy).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("multiple component-bearing repeated Steps")
        );
    }
}
