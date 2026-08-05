use ipc2581::edit::Doc;
use pcb_ir::dialects::ipc::{View, root_step};
use pcb_ir::geom::{BBox, ContourSet, tol};

use crate::copper_balance::{CopperBalanceMode, composed_copper_image};

use super::*;

const FAB_PANEL_WIDTH_MM: f64 = FabPanelSpec::INCHES_18_X_24.width_mm();
const FAB_PANEL_HEIGHT_MM: f64 = FabPanelSpec::INCHES_18_X_24.height_mm();

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

fn assembly_panel_with_profile_cutouts_xml(width_mm: f64, height_mm: f64) -> String {
    assembly_panel_xml(width_mm, height_mm)
        .replace(
            r#"      <Step name="panel" type="PALLET">"#,
            r#"      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="10" y="10"/>
            <PolyStepSegment x="30" y="10"/>
            <PolyStepSegment x="30" y="30"/>
            <PolyStepSegment x="10" y="30"/>
            <PolyStepSegment x="10" y="10"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="19" y="19"/>
            <PolyStepSegment x="21" y="19"/>
            <PolyStepSegment x="21" y="21"/>
            <PolyStepSegment x="19" y="21"/>
            <PolyStepSegment x="19" y="19"/>
          </Cutout>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">"#,
        )
        .replace(
            r#"        </Profile>
        <LayerFeature layerRef="TOP">"#,
            r#"          <Cutout>
            <PolyBegin x="39" y="39"/>
            <PolyStepSegment x="41" y="39"/>
            <PolyStepSegment x="41" y="41"/>
            <PolyStepSegment x="39" y="41"/>
            <PolyStepSegment x="39" y="39"/>
          </Cutout>
        </Profile>
        <StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1" dx="0" dy="0" angle="0" mirror="false"/>
        <LayerFeature layerRef="TOP">"#,
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
fn exports_separate_nominal_panel_outlines_and_board_cutouts() {
    let sources = vec![
        assembly_panel_with_profile_cutouts_xml(100.0, 80.0),
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

    assert_eq!(
        profile.purpose,
        pcb_ir::dialects::ipc::LayoutPurpose::FabricationPanel
    );
    assert_eq!(profile.array_outlines.len(), 1);
    assert_eq!(placed_panels.len(), 3);
    assert_eq!(profile.assembly_panel_outlines.len(), placed_panels.len());
    assert_eq!(
        profile.material_removal.len(),
        4,
        "both board and assembly-panel profile cutouts must survive two placements"
    );
    let assembly_panel_outlines = profile
        .assembly_panel_outlines
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    for panel_bbox in placed_panels {
        assert!(has_contour_bbox(&assembly_panel_outlines, panel_bbox, 0.0));
        assert!(!has_contour_bbox(&assembly_panel_outlines, panel_bbox, 1.0));
        assert!(!has_contour_bbox(
            &profile.material_removal,
            panel_bbox,
            0.0
        ));
        assert!(!has_contour_bbox(
            &profile.material_removal,
            panel_bbox,
            1.0
        ));
    }

    let profile_stroke_radius = 0.025;
    let expected_fab_bbox =
        contour_bbox(profile.array_outlines.iter().flatten()).expand(profile_stroke_radius);
    let expected_assembly_bbox = contour_bbox(profile.assembly_panel_outlines.iter().flatten())
        .expand(profile_stroke_radius);
    let expected_cutout_bbox =
        contour_bbox(profile.material_removal.iter()).expand(profile_stroke_radius);
    let package =
        crate::manufacturing::build_manufacturing_package(&parsed, View::ArrayFlattened).unwrap();
    for (filename, expected_bbox) in [
        ("Fab_Panel_Outline.gm1", expected_fab_bbox),
        ("Assembly_Panel_Outlines.gm1", expected_assembly_bbox),
        ("Board_Cutouts.gm1", expected_cutout_bbox),
    ] {
        let file = package
            .files
            .iter()
            .find(|file| file.filename == filename)
            .unwrap_or_else(|| panic!("missing {filename}"));
        assert!(file.contents.contains("%TF.FileFunction,Profile,NP*%"));
        assert!(file.contents.contains("%TF.Part,Array*%"));
        assert!(file.contents.contains("%TA.AperFunction,Profile*%"));
        assert!(file.contents.contains("%ADD10C,0.05*%"));
        assert!(!file.contents.contains("C,1*%"));
        let parsed_gerber = gerberx2::GerberX2::parse(&file.contents).unwrap();
        let artwork = gerberx2::geometry::extract_document(&parsed_gerber);
        assert_bbox_close(artwork.layers[0].bbox, expected_bbox);
        let crate::manufacturing::ManufacturingFileKind::GerberX2(layer) = &file.kind else {
            panic!("{filename} is not a Gerber layer");
        };
        assert!(!layer.objects.is_empty());
    }
    assert_eq!(
        package
            .files
            .iter()
            .filter(|file| file.contents.contains("%TF.FileFunction,Profile,NP*%"))
            .count(),
        3
    );
    assert!(
        package
            .files
            .iter()
            .all(|file| file.filename != "Fab_Panel_Profile.gm1")
    );
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
}

fn contour_bbox<'a>(contours: impl IntoIterator<Item = &'a pcb_ir::geom::ContourBuf>) -> BBox {
    contours
        .into_iter()
        .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox))
}

fn assert_bbox_close(actual: BBox, expected: BBox) {
    const EPSILON: f64 = 0.002;
    assert!((actual.min.x - expected.min.x).abs() < EPSILON);
    assert!((actual.min.y - expected.min.y).abs() < EPSILON);
    assert!((actual.max.x - expected.max.x).abs() < EPSILON);
    assert!((actual.max.y - expected.max.y).abs() < EPSILON);
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
        assert!(instance.bbox.min.x >= DEFAULT_EDGE_MARGIN_MM.left - 1e-9);
        assert!(instance.bbox.min.y >= DEFAULT_EDGE_MARGIN_MM.bottom - 1e-9);
        assert!(instance.bbox.max.x <= FAB_PANEL_WIDTH_MM - DEFAULT_EDGE_MARGIN_MM.right + 1e-9);
        assert!(instance.bbox.max.y <= FAB_PANEL_HEIGHT_MM - DEFAULT_EDGE_MARGIN_MM.top + 1e-9);
    }
    let first = instances[0].bbox;
    let second = instances[1].bbox;
    let separated = first.max.x + DEFAULT_PANEL_GAP_MM <= second.min.x + 1e-9
        || second.max.x + DEFAULT_PANEL_GAP_MM <= first.min.x + 1e-9
        || first.max.y + DEFAULT_PANEL_GAP_MM <= second.min.y + 1e-9
        || second.max.y + DEFAULT_PANEL_GAP_MM <= first.min.y + 1e-9;
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

    for (spec, usable_width, usable_height) in [
        (FabPanelSpec::INCHES_12_X_18, 254.0, 355.6),
        (FabPanelSpec::INCHES_16_X_18, 355.6, 355.6),
        (FabPanelSpec::INCHES_18_X_24, 406.4, 508.0),
        (FabPanelSpec::INCHES_21_X_24, 482.6, 508.0),
    ] {
        let generated = create_fab_panel(&sources, &[0], spec).unwrap().xml;
        Ipc2581::validate(&generated).unwrap();
        let parsed = Ipc2581::parse(&generated).unwrap();
        let layout = geometry::extract_layout(&parsed).unwrap();
        let (_, root) = root_step(&layout).unwrap();
        let usable = spec.usable_bbox().unwrap();

        assert!((root.bbox.width() - spec.width_mm()).abs() < 1e-9);
        assert!((root.bbox.height() - spec.height_mm()).abs() < 1e-9);
        assert!((usable.width() - usable_width).abs() < 1e-9);
        assert!((usable.height() - usable_height).abs() < 1e-9);
    }
}

#[test]
fn writes_default_process_margin_and_usable_area_metadata() {
    let generated = create_fab_panel_xml(&[assembly_panel_xml(100.0, 80.0)], &[0]).unwrap();

    for metadata in [
        r#"<NonstandardAttribute name="diode.fab_panel.schema_version" type="INTEGER" value="2"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.usable_width_mm" type="DOUBLE" value="406.4"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.usable_height_mm" type="DOUBLE" value="508"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.edge_margin_top_mm" type="DOUBLE" value="50.8"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.edge_margin_right_mm" type="DOUBLE" value="25.4"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.edge_margin_bottom_mm" type="DOUBLE" value="50.8"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.edge_margin_left_mm" type="DOUBLE" value="25.4"/>"#,
        r#"<NonstandardAttribute name="diode.fab_panel.gap_mm" type="DOUBLE" value="5"/>"#,
    ] {
        assert!(generated.contains(metadata), "missing {metadata}");
    }
    assert!(!generated.contains("diode.fab_panel.edge_rail_mm"));
}

#[test]
fn applies_asymmetric_process_margin_and_gap_overrides() {
    let spec = FabPanelSpec {
        edge_margin_mm: EdgeInsetsMm::new(30.0, 20.0, 10.0, 40.0),
        panel_gap_mm: 7.0,
        ..FabPanelSpec::INCHES_12_X_18
    };
    let generated = create_fab_panel(&[assembly_panel_xml(100.0, 80.0)], &[0], spec)
        .unwrap()
        .xml;
    let parsed = Ipc2581::parse(&generated).unwrap();
    let layout = geometry::extract_layout(&parsed).unwrap();
    let instance = layout
        .layout
        .instances
        .iter()
        .find(|instance| instance.parent_instance.is_none())
        .unwrap();
    let usable = spec.usable_bbox().unwrap();

    assert!(instance.bbox.min.x >= usable.min.x - 1e-9);
    assert!(instance.bbox.min.y >= usable.min.y - 1e-9);
    assert!(instance.bbox.max.x <= usable.max.x + 1e-9);
    assert!(instance.bbox.max.y <= usable.max.y + 1e-9);
    assert!((instance.bbox.center().x - usable.center().x).abs() < 0.001);
    assert!((instance.bbox.center().y - usable.center().y).abs() < 0.001);
    assert!(generated.contains(
        r#"<NonstandardAttribute name="diode.fab_panel.edge_margin_top_mm" type="DOUBLE" value="30"/>"#
    ));
    assert!(generated.contains(
        r#"<NonstandardAttribute name="diode.fab_panel.gap_mm" type="DOUBLE" value="7"/>"#
    ));
}

#[test]
fn accepts_subtool_spacing_and_rejects_invalid_fabrication_panel_domains() {
    let sources = [assembly_panel_xml(100.0, 80.0)];

    let subtool_spacing = FabPanelSpec {
        edge_margin_mm: EdgeInsetsMm::new(50.8, 25.4, 50.8, 0.5),
        panel_gap_mm: 0.5,
        ..FabPanelSpec::INCHES_18_X_24
    };
    create_fab_panel(&sources, &[0], subtool_spacing).unwrap();

    let negative_margin = FabPanelSpec {
        edge_margin_mm: EdgeInsetsMm::new(50.8, 25.4, 50.8, -0.5),
        ..FabPanelSpec::INCHES_18_X_24
    };
    let error = create_fab_panel(&sources, &[0], negative_margin).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("left edge margin must be non-negative")
    );

    let negative_gap = FabPanelSpec {
        panel_gap_mm: -0.5,
        ..FabPanelSpec::INCHES_18_X_24
    };
    let error = create_fab_panel(&sources, &[0], negative_gap).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("fabrication panel gap must be non-negative")
    );

    let no_usable_width = FabPanelSpec {
        edge_margin_mm: EdgeInsetsMm::new(50.8, 250.0, 50.8, 250.0),
        ..FabPanelSpec::INCHES_18_X_24
    };
    let error = create_fab_panel(&sources, &[0], no_usable_width).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("edge margins leave no usable width")
    );
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
    assert!(instance.bbox.min.x >= DEFAULT_EDGE_MARGIN_MM.left - 1e-9);
    assert!(instance.bbox.min.y >= DEFAULT_EDGE_MARGIN_MM.bottom - 1e-9);
    assert!(instance.bbox.max.x <= FAB_PANEL_WIDTH_MM - DEFAULT_EDGE_MARGIN_MM.right + 1e-9);
    assert!(instance.bbox.max.y <= FAB_PANEL_HEIGHT_MM - DEFAULT_EDGE_MARGIN_MM.top + 1e-9);
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

/// A compact stock so balance tests solve quickly: 120 x 220 mm usable.
const BALANCE_SPEC: FabPanelSpec = FabPanelSpec {
    width_mm: 150.0,
    height_mm: 250.0,
    edge_margin_mm: EdgeInsetsMm::all(15.0),
    panel_gap_mm: 5.0,
};

/// An assembly panel whose left half carries solid copper, inset 1 mm so the
/// copper stays strictly inside the rounded panel outline.
fn dense_assembly_panel_xml(width_mm: f64, height_mm: f64) -> String {
    let copper_max_x = width_mm / 2.0;
    let copper_max_y = height_mm - 1.0;
    let sparse = assembly_panel_xml(width_mm, height_mm);
    let dense = sparse.replace(
        &format!(
            r#"<Line startX="0" startY="0" endX="{width_mm}" endY="0">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>"#
        ),
        &format!(
            r#"<Contour>
                <Polygon>
                  <PolyBegin x="1" y="1"/>
                  <PolyStepSegment x="{copper_max_x}" y="1"/>
                  <PolyStepSegment x="{copper_max_x}" y="{copper_max_y}"/>
                  <PolyStepSegment x="1" y="{copper_max_y}"/>
                  <PolyStepSegment x="1" y="1"/>
                </Polygon>
              </Contour>"#
        ),
    );
    assert!(dense.contains("<Contour>"), "copper fixture replace failed");
    dense
}

#[test]
fn balances_gutters_at_the_assembly_panel_density_and_leaves_margins_bare() {
    let sources = vec![
        dense_assembly_panel_xml(200.0, 80.0),
        dense_assembly_panel_xml(30.0, 25.0),
    ];
    let creation = create_fab_panel(&sources, &[0, 1], BALANCE_SPEC).unwrap();

    let report = &creation.copper_balance;
    assert!(report.stack_weights_available);
    assert_eq!(report.layers.len(), 1);
    let layer = &report.layers[0];
    assert_eq!(layer.layer_name, "TOP");
    assert_eq!(layer.mode, CopperBalanceMode::Perforated);
    assert!(layer.void_count > 0);
    assert!((0.4..0.55).contains(&layer.target_density));
    assert!(
        layer.residual_error <= 5e-3,
        "gutter fill should reach the panel density: target {:.4}, achieved {:.4}",
        layer.target_density,
        layer.achieved_density
    );
    assert_eq!(
        creation
            .xml
            .matches(r#"<LayerFeature layerRef="TOP">"#)
            .count(),
        4,
        "both sources plus the fab step's positive and negative balance sets should carry TOP copper"
    );

    Ipc2581::validate(&creation.xml).unwrap();
    let parsed = Ipc2581::parse(&creation.xml).unwrap();
    let layout = geometry::extract_layout(&parsed).unwrap();
    // The 200 mm panel exceeds the 120 mm usable width, so packing must
    // rotate it; balancing has to respect the rotated footprint.
    assert!(
        layout
            .layout
            .instances
            .iter()
            .filter(|instance| instance.parent_instance.is_none())
            .any(|instance| (instance.bbox.width() - 80.0).abs() < 1e-6
                && (instance.bbox.height() - 200.0).abs() < 1e-6)
    );

    let profile = geometry::board_array_fabrication_profile(&parsed, &layout, &[]).unwrap();
    let panel_contours = profile
        .assembly_panel_outlines
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let footprints = ContourSet::from_filled_contours(&panel_contours, tol::REGION_MM);
    let usable = ContourSet::rectangle(BALANCE_SPEC.usable_bbox().unwrap(), tol::REGION_MM);
    let copper = composed_copper_image(&parsed, "TOP").unwrap();
    let gutter_copper = copper.difference(&footprints);

    assert!(
        copper.difference(&usable).area() <= 1e-6,
        "process margins must stay bare"
    );
    assert!(
        gutter_copper.area() > 3_000.0,
        "expected substantial generated gutter copper, got {:.1} mm²",
        gutter_copper.area()
    );
    assert!(
        gutter_copper
            .intersection(&footprints.disk_dilate(0.4))
            .area()
            <= 1e-6,
        "generated copper must keep clearance from the placed panels"
    );
    assert!(
        gutter_copper.difference(&usable.disk_erode(0.4)).area() <= 1e-6,
        "generated copper must keep clearance from the usable-area boundary"
    );
}

#[test]
fn single_panel_filling_the_usable_area_generates_no_fill() {
    let sources = vec![dense_assembly_panel_xml(120.0, 220.0)];
    let creation = create_fab_panel(&sources, &[0], BALANCE_SPEC).unwrap();

    let layer = &creation.copper_balance.layers[0];
    assert_eq!(layer.mode, CopperBalanceMode::None);
    assert_eq!(layer.generated_area_mm2, 0.0);
    assert_eq!(
        creation
            .xml
            .matches(r#"<LayerFeature layerRef="TOP">"#)
            .count(),
        1,
        "a fully covered usable area leaves no room for balance copper"
    );
}
