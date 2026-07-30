use ipc2581::edit::Doc;
use pcb_ir::dialects::ipc::{View, root_step};
use pcb_ir::geom::BBox;

use super::*;

const FAB_PANEL_WIDTH_MM: f64 = FabPanelDimensions::INCHES_18_X_24.width_mm();
const FAB_PANEL_HEIGHT_MM: f64 = FabPanelDimensions::INCHES_18_X_24.height_mm();

fn assembly_panel_xml(width_mm: f64, height_mm: f64) -> String {
    assembly_panel_xml_at(0.0, 0.0, width_mm, height_mm)
}

fn assembly_panel_xml_at(min_x: f64, min_y: f64, width_mm: f64, height_mm: f64) -> String {
    let max_x = min_x + width_mm;
    let max_y = min_y + height_mm;
    let radius = 3.0;
    let left_center = min_x + radius;
    let right_center = max_x - radius;
    let bottom_center = min_y + radius;
    let top_center = max_y - radius;
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
      <Stackup name="Primary" overallThickness="0.035" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
        <StackupGroup name="Primary_Group" thickness="0.035" tolPlus="0" tolMinus="0">
          <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
        </StackupGroup>
      </Stackup>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="{min_x}" y="{bottom_center}"/>
            <PolyStepSegment x="{min_x}" y="{top_center}"/>
            <PolyStepCurve x="{left_center}" y="{max_y}" centerX="{left_center}" centerY="{top_center}" clockwise="true"/>
            <PolyStepSegment x="{right_center}" y="{max_y}"/>
            <PolyStepCurve x="{max_x}" y="{top_center}" centerX="{right_center}" centerY="{top_center}" clockwise="true"/>
            <PolyStepSegment x="{max_x}" y="{bottom_center}"/>
            <PolyStepCurve x="{right_center}" y="{min_y}" centerX="{right_center}" centerY="{bottom_center}" clockwise="true"/>
            <PolyStepSegment x="{left_center}" y="{min_y}"/>
            <PolyStepCurve x="{min_x}" y="{bottom_center}" centerX="{left_center}" centerY="{bottom_center}" clockwise="true"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <Line startX="{min_x}" startY="{min_y}" endX="{max_x}" endY="{min_y}">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    )
}

fn assembly_panel_with_non_manufacturing_data() -> String {
    assembly_panel_xml(100.0, 80.0)
        .replace(
            r#"    <LayerRef name="TOP"/>"#,
            r#"    <LayerRef name="TOP"/>
    <LayerRef name="DRILL"/>
    <LayerRef name="ROUTE"/>
    <LayerRef name="SCORE"/>
    <LayerRef name="PASTE"/>
    <LayerRef name="COURTYARD"/>
    <BomRef name="assembly_bom"/>
    <AvlRef name="assembly_avl"/>"#,
        )
        .replace(
            r#"  <Ecad name="assembly">"#,
            r#"  <Bom name="assembly_bom"/>
  <Ecad name="assembly">"#,
        )
        .replace(
            r#"      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>"#,
            r#"      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="TOP" toLayer="TOP"/>
      </Layer>
      <Layer name="ROUTE" layerFunction="ROUTE" side="ALL" polarity="POSITIVE">
        <Span fromLayer="TOP" toLayer="TOP"/>
      </Layer>
      <Layer name="SCORE" layerFunction="SCORE" side="ALL" polarity="POSITIVE"/>
      <Layer name="PASTE" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Layer name="COURTYARD" layerFunction="COURTYARD" side="TOP" polarity="POSITIVE"/>"#,
        )
        .replace(
            r#"        <LayerFeature layerRef="TOP">"#,
            r#"        <Package name="package" type="ELECTRICAL">
          <Outline>
            <Polygon><PolyBegin x="0" y="0"/></Polygon>
            <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
          </Outline>
        </Package>
        <Component refDes="U1" packageRef="package" part="part" layerRef="TOP" mountType="SMT">
          <Location x="1" y="1"/>
        </Component>
        <LogicalNet name="N1"/>
        <LayerFeature layerRef="TOP">"#,
        )
        .replace(
            r#"        </LayerFeature>
      </Step>"#,
            r#"        </LayerFeature>
        <LayerFeature layerRef="DRILL">
          <Set>
            <Hole name="V1" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="10" y="10"/>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="ROUTE">
          <Set>
            <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="20" y="20"/>
              <Oval width="1.7" height="0.6"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="SCORE">
          <Set>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="PASTE">
          <Set>
            <Features>
              <Line startX="0" startY="1" endX="10" endY="1">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="COURTYARD">
          <Set>
            <Features>
              <Line startX="0" startY="2" endX="10" endY="2">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>"#,
        )
        .replace(
            r#"</IPC-2581>"#,
            r#"  <Avl name="assembly_avl"/>
</IPC-2581>"#,
        )
}

#[test]
fn routes_each_assembly_panel_in_the_fab_profile() {
    let sources = vec![
        assembly_panel_xml_at(10.0, 20.0, 100.0, 80.0),
        assembly_panel_xml_at(-15.0, 7.0, 120.0, 90.0),
    ];
    let generated = create_fab_panel_xml(&sources, &[0, 0, 1]).unwrap();

    Ipc2581::validate(&generated).unwrap();
    let parsed = Ipc2581::parse(&generated).unwrap();
    let layout = geometry::extract_layout(&parsed).unwrap();
    let profile = geometry::board_array_fabrication_profile(&parsed, &layout, &[]).unwrap();
    let placed_panels = layout
        .layout
        .instances
        .iter()
        .filter(|instance| {
            instance.parent_instance.is_none()
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
        })
        .map(|instance| instance.bbox)
        .collect::<Vec<_>>();

    assert_eq!(profile.array_outlines.len(), 1);
    assert_eq!(placed_panels.len(), 3);
    assert_eq!(profile.material_removal.len(), 2 * placed_panels.len());
    for panel_bbox in placed_panels {
        assert!(has_contour_bbox(&profile.material_removal, panel_bbox, 0.0));
        assert!(has_contour_bbox(&profile.material_removal, panel_bbox, 1.0));
    }

    let package =
        crate::manufacturing::build_manufacturing_package(&parsed, View::ArrayFlattened).unwrap();
    let profile = package
        .files
        .iter()
        .find(|file| file.filename == "Fab_Panel_Profile.gm1")
        .unwrap();
    assert!(profile.contents.contains("%TF.FileFunction,Profile,NP*%"));
    assert!(profile.contents.contains("%TF.Part,Array*%"));
    assert!(profile.contents.contains("%TA.AperFunction,Profile*%"));
    assert!(profile.contents.contains("%ADD10C,0.05*%"));
    assert!(!profile.contents.contains("%ADD11C,1*%"));
    assert!(
        package
            .files
            .iter()
            .all(|file| file.filename != "Board_Array_Profile.gm1")
    );
    assert!(
        package
            .files
            .iter()
            .filter(|file| file.filename.ends_with(".drl"))
            .all(|file| !file.contents.contains("M15"))
    );
    gerberx2::GerberX2::parse(&profile.contents).unwrap();
}

fn has_contour_bbox(contours: &[pcb_ir::geom::ContourBuf], bbox: BBox, expansion: f64) -> bool {
    const EPSILON: f64 = 0.02;
    contours.iter().any(|contour| {
        (contour.bbox.min.x - (bbox.min.x - expansion)).abs() < EPSILON
            && (contour.bbox.min.y - (bbox.min.y - expansion)).abs() < EPSILON
            && (contour.bbox.max.x - (bbox.max.x + expansion)).abs() < EPSILON
            && (contour.bbox.max.y - (bbox.max.y + expansion)).abs() < EPSILON
    })
}

#[test]
fn shares_the_first_stackup_across_sources_and_builds_full_fab_profile() {
    let sources = vec![
        assembly_panel_xml_at(10.0, 20.0, 100.0, 80.0),
        assembly_panel_xml_at(-15.0, 7.0, 120.0, 90.0),
    ];
    let generated = create_fab_panel_xml(&sources, &[0, 1]).unwrap();

    assert!(generated.contains(r#"<Step name="fab_0_panel" type="PALLET">"#));
    assert!(generated.contains(r#"<Step name="fab_1_panel" type="PALLET">"#));
    assert_eq!(generated.matches(r#"<Layer name="TOP""#).count(), 1);
    assert!(!generated.contains(r#"<Layer name="fab_0_TOP""#));
    assert!(!generated.contains(r#"<Layer name="fab_1_TOP""#));
    assert_eq!(
        generated
            .matches(r#"<Stackup name="fab_0_Primary""#)
            .count(),
        1
    );
    assert!(!generated.contains(r#"<Stackup name="fab_1_Primary""#));
    assert_eq!(
        generated
            .matches(r#"<LayerFeature layerRef="TOP">"#)
            .count(),
        2
    );
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
        .filter(|layer| doc.attr(*layer, "name") == Some("TOP"))
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
fn creates_supported_standard_fabrication_panel_sizes() {
    let sources = vec![assembly_panel_xml(100.0, 80.0)];

    for dimensions in [
        FabPanelDimensions::INCHES_12_X_18,
        FabPanelDimensions::INCHES_16_X_18,
        FabPanelDimensions::INCHES_18_X_24,
        FabPanelDimensions::INCHES_21_X_24,
    ] {
        let generated = create_fab_panel_xml_with_dimensions(&sources, &[0], dimensions).unwrap();
        Ipc2581::validate(&generated).unwrap();
        let parsed = Ipc2581::parse(&generated).unwrap();
        let layout = geometry::extract_layout(&parsed).unwrap();
        let (_, root) = root_step(&layout).unwrap();

        assert!((root.bbox.width() - dimensions.width_mm()).abs() < 1e-9);
        assert!((root.bbox.height() - dimensions.height_mm()).abs() < 1e-9);
    }
}

#[test]
fn strips_non_manufacturing_data_and_preserves_manufacturing_exports() {
    let generated = create_fab_panel_xml(&[assembly_panel_with_non_manufacturing_data()], &[0])
        .expect("fabrication panel should be generated");

    for removed in [
        "<BomRef",
        "<AvlRef",
        "<Bom ",
        "<Avl ",
        "<Package",
        "<Component",
        "<LogicalNet",
        "name=\"fab_0_PASTE\"",
        "name=\"fab_0_COURTYARD\"",
    ] {
        assert!(!generated.contains(removed), "{removed} was not removed");
    }
    assert!(generated.contains(r#"<FunctionMode mode="FABRICATION" sectionKey="SURO"/>"#));
    assert!(generated.contains(r#"<Layer name="TOP""#));
    assert!(generated.contains(r#"<Layer name="fab_0_DRILL""#));
    assert!(generated.contains(r#"<Layer name="fab_0_ROUTE" layerFunction="ROUT""#));
    assert!(generated.contains(r#"<Layer name="fab_0_SCORE" layerFunction="V_CUT""#));
    assert!(generated.contains(r#"<LayerFeature layerRef="TOP">"#));
    assert!(generated.contains(r#"<LayerFeature layerRef="fab_0_DRILL">"#));
    assert!(generated.contains(r#"<LayerFeature layerRef="fab_0_ROUTE">"#));
    assert!(generated.contains(r#"<LayerFeature layerRef="fab_0_SCORE">"#));

    Ipc2581::validate(&generated).expect("fabrication panel should validate against IPC-2581C");
    let parsed = Ipc2581::parse(&generated).expect("fabrication panel should parse");
    let package = crate::manufacturing::build_manufacturing_package(&parsed, View::ArrayFlattened)
        .expect("fabrication panel should export manufacturing files");
    assert!(package.files.iter().any(|file| file.filename == "F_Cu.gtl"));
    assert!(package.files.iter().any(|file| file.filename == "PTH.drl"));
    assert!(
        package
            .files
            .iter()
            .any(|file| file.filename == "V_Cut.gbr")
    );
}

#[test]
fn rejects_a_stackup_that_differs_from_the_first_input() {
    let first = assembly_panel_xml(100.0, 80.0);
    let second = assembly_panel_xml(120.0, 90.0).replace(
        r#"thickness="0.035" tolPlus="0" tolMinus="0" sequence="0""#,
        r#"thickness="0.070" tolPlus="0" tolMinus="0" sequence="0""#,
    );

    let error = create_fab_panel_xml(&[first, second], &[0, 1]).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("stackup layer 1 ('TOP') differs from assembly panel input 1")
    );
}

#[test]
fn rejects_an_input_without_exactly_one_stackup() {
    let first = assembly_panel_xml(100.0, 80.0);
    let second = assembly_panel_xml(120.0, 90.0).replace(
        r#"      <Stackup name="Primary" overallThickness="0.035" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
        <StackupGroup name="Primary_Group" thickness="0.035" tolPlus="0" tolMinus="0">
          <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
        </StackupGroup>
      </Stackup>
"#,
        "",
    );

    let error = create_fab_panel_xml(&[first, second], &[0, 1]).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("assembly panel input 2 must contain exactly one physical stackup")
    );
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
