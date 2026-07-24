use ipc2581::edit::Doc;
use pcb_ir::dialects::ipc::root_step;

use super::*;

fn assembly_panel_xml(width_mm: f64, height_mm: f64) -> String {
    assembly_panel_xml_at(0.0, 0.0, width_mm, height_mm)
}

fn assembly_panel_xml_at(min_x: f64, min_y: f64, width_mm: f64, height_mm: f64) -> String {
    let max_x = min_x + width_mm;
    let max_y = min_y + height_mm;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="designer">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
  </Content>
  <LogisticHeader>
    <Role id="designer" roleFunction="DESIGNER"/>
    <Enterprise id="enterprise" name="Example" code="EXAMPLE"/>
    <Person name="designer" enterpriseRef="enterprise" roleRef="designer"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-07-24T00:00:00Z" software="test" lastChange="2026-07-24T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="Test input" label="">
      <SoftwarePackage name="test" vendor="test" revision="1">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Ecad name="assembly">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="{min_x}" y="{min_y}"/>
            <PolyStepSegment x="{max_x}" y="{min_y}"/>
            <PolyStepSegment x="{max_x}" y="{max_y}"/>
            <PolyStepSegment x="{min_x}" y="{max_y}"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

#[test]
fn merges_colliding_source_names_and_builds_full_fab_profile() {
    let sources = vec![
        assembly_panel_xml_at(10.0, 20.0, 100.0, 80.0),
        assembly_panel_xml_at(-15.0, 7.0, 120.0, 90.0),
    ];
    let generated = create_fab_panel_xml(&sources, &[0, 1]).unwrap();

    assert!(generated.contains(r#"<Step name="fab_0_panel" type="PALLET">"#));
    assert!(generated.contains(r#"<Step name="fab_1_panel" type="PALLET">"#));
    assert!(generated.contains(r#"<Layer name="fab_0_TOP""#));
    assert!(generated.contains(r#"<Layer name="fab_1_TOP""#));
    assert!(generated.contains(r#"stepRef="fab_0_panel""#));
    assert!(generated.contains(r#"stepRef="fab_1_panel""#));

    Ipc2581::validate(&generated).unwrap();
    let parsed = Ipc2581::parse(&generated).unwrap();
    let layout = geometry::extract_layout(&parsed).unwrap();
    let (_, root) = root_step(&layout).unwrap();
    assert!((root.bbox.width() - FAB_PANEL_WIDTH_MM).abs() < 1e-9);
    assert!((root.bbox.height() - FAB_PANEL_HEIGHT_MM).abs() < 1e-9);

    let instances = layout
        .layout
        .instances
        .iter()
        .filter(|instance| {
            instance.parent_instance.is_none()
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
        })
        .collect::<Vec<_>>();
    assert_eq!(instances.len(), 2);
    for instance in &instances {
        assert!(instance.bbox.min.x >= EDGE_RAIL_MM - 1e-9);
        assert!(instance.bbox.min.y >= EDGE_RAIL_MM - 1e-9);
        assert!(instance.bbox.max.x <= FAB_PANEL_WIDTH_MM - EDGE_RAIL_MM + 1e-9);
        assert!(instance.bbox.max.y <= FAB_PANEL_HEIGHT_MM - EDGE_RAIL_MM + 1e-9);
    }
    let first = instances[0].bbox;
    let second = instances[1].bbox;
    let separated = first.max.x + PANEL_GAP_MM <= second.min.x + 1e-9
        || second.max.x + PANEL_GAP_MM <= first.min.x + 1e-9
        || first.max.y + PANEL_GAP_MM <= second.min.y + 1e-9
        || second.max.y + PANEL_GAP_MM <= first.min.y + 1e-9;
    assert!(separated);
}

#[test]
fn repeating_an_input_reuses_its_definitions_and_adds_placements() {
    let sources = vec![assembly_panel_xml(100.0, 80.0)];
    let generated = create_fab_panel_xml(&sources, &[0, 0, 0]).unwrap();
    let doc = Doc::parse(&generated).unwrap();

    let imported_steps = doc
        .find_all("Step")
        .into_iter()
        .filter(|step| doc.attr(*step, "name") == Some("fab_0_panel"))
        .count();
    let imported_layers = doc
        .find_all("Layer")
        .into_iter()
        .filter(|layer| doc.attr(*layer, "name") == Some("fab_0_TOP"))
        .count();
    let placements = doc
        .find_all("StepRepeat")
        .into_iter()
        .filter(|repeat| doc.attr(*repeat, "stepRef") == Some("fab_0_panel"))
        .count();

    assert_eq!(imported_steps, 1);
    assert_eq!(imported_layers, 1);
    assert_eq!(placements, 3);
}

#[test]
fn rotates_and_translates_a_nonzero_source_profile() {
    let sources = vec![assembly_panel_xml_at(10.0, 20.0, 500.0, 400.0)];
    let generated = create_fab_panel_xml(&sources, &[0]).unwrap();
    let parsed = Ipc2581::parse(&generated).unwrap();
    let layout = geometry::extract_layout(&parsed).unwrap();
    let instance = layout
        .layout
        .instances
        .iter()
        .find(|instance| instance.parent_instance.is_none())
        .unwrap();

    assert!((instance.bbox.width() - 400.0).abs() < 1e-9);
    assert!((instance.bbox.height() - 500.0).abs() < 1e-9);
    assert!(instance.bbox.min.x >= EDGE_RAIL_MM - 1e-9);
    assert!(instance.bbox.min.y >= EDGE_RAIL_MM - 1e-9);
    assert!(instance.bbox.max.x <= FAB_PANEL_WIDTH_MM - EDGE_RAIL_MM + 1e-9);
    assert!(instance.bbox.max.y <= FAB_PANEL_HEIGHT_MM - EDGE_RAIL_MM + 1e-9);
}

#[test]
fn rejects_a_board_instead_of_an_assembly_panel() {
    let board = assembly_panel_xml(100.0, 80.0).replace(
        r#"<Step name="panel" type="PALLET">"#,
        r#"<Step name="panel" type="BOARD">"#,
    );
    let error = create_fab_panel_xml(&[board], &[0]).unwrap_err();
    assert!(error.to_string().contains("expected a board array"));
}
