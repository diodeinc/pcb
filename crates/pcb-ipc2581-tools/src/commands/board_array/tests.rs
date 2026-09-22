use crate::copper_balance::balance_features;
use pcb_ir::geom::Resolution;

use super::balance::{extract_array_support_layers, generate_automatic_board_array_copper_balance};
use super::*;
use crate::accessors::IpcAccessor;
use crate::ipc2581::types::LayerFunction;
use crate::manufacturing::{
    ManufacturingExportOptions, ManufacturingPackage, build_manufacturing_package,
};
use pcb_ir::dialects::ipc::{
    ArtworkScope, BalancingRegionOptions, BoardArraySupportDocument, FeatureBucket, FeatureDomain,
    FeatureIntent, FeatureKind, FeatureOperation, FeatureRole, FeatureSpan, FiducialKind,
    LayoutStepKind, PlatingKind, board_array_balancing_region, collect_board_array_balancing_input,
};
use pcb_ir::geom::copper_balance::{
    DenseCopperBalanceMode, DenseCopperBalanceProfile, SpatialCopperBalanceLayerRequest,
    SpatialCopperBalanceRequest, generate_spatial_dense_copper_balance,
};
use pcb_ir::geom::{BBox, ContourSet, Point};
use pcb_ir::import::ipc2581::ImportedDesign;

fn design(ipc: &Ipc2581) -> ImportedDesign {
    pcb_ir::import::ipc2581::import_design(ipc, Resolution::default()).unwrap()
}

fn manufacturing_package(
    ipc: &Ipc2581,
    view: ArtworkScope,
) -> anyhow::Result<ManufacturingPackage> {
    build_manufacturing_package(
        &design(ipc),
        &ManufacturingExportOptions {
            view,
            relief_debug_dir: None,
        },
        Resolution::default(),
    )
}

/// A package file's contents, if the package has it.
fn gerber<'a>(package: &'a ManufacturingPackage, filename: &str) -> Option<&'a str> {
    let file = package.files.iter().find(|file| file.filename == filename);
    file.map(|file| file.contents.as_str())
}

/// The generated array's size from the origin and its board count.
fn assert_array(xml: &str, width_mm: f64, height_mm: f64, boards: usize) {
    let layout = geometry::extract_layout(&Ipc2581::parse(xml).unwrap()).unwrap();
    let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
    assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
    assert_point_close(panel_step.bbox.max, Point::new(width_mm, height_mm));
    assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), boards);
}

fn options(
    columns: u32,
    rows: u32,
    board_margin_mm: BoardMarginMm,
    edge_rail_mm: BoardMarginMm,
) -> BoardArrayCreateOptions {
    BoardArrayCreateOptions {
        columns,
        rows,
        board_margin_mm,
        edge_rail_mm,
    }
}

/// Thirty-six boards 5 mm apart inside 5 mm rails.
fn six_by_six() -> BoardArrayCreateOptions {
    options(6, 6, board_margin(5.0, 5.0), BoardMarginMm::all(5.0))
}

/// Panelize without balancing copper, for the cases that are about the array
/// itself. Balancing costs the panel's whole area, so the cases that are about
/// it ask for it.
fn create_board_array_xml(xml: &str, options: &BoardArrayCreateOptions) -> Result<String> {
    Ok(create_board_array(
        xml,
        options,
        false,
        Separation::VScore,
        Resolution::default(),
    )?
    .xml)
}

fn create_auto_board_array_xml(xml: &str, sheet: Option<AutoSheetSize>) -> Result<String> {
    Ok(create_auto_board_array(xml, sheet, false, Separation::VScore, Resolution::default())?.xml)
}

fn manual_spec(
    ipc: &Ipc2581,
    options: &BoardArrayCreateOptions,
    separation: Separation,
) -> BoardArraySpec {
    build_board_array_spec(
        ipc,
        primary_board_layout(ipc).unwrap(),
        options,
        BoardArrayPanelizationMetadata::MANUAL,
        separation,
        Resolution::default(),
    )
    .unwrap()
}

fn write_board_array_xml(xml: &str, spec: &BoardArraySpec) -> Result<String> {
    finished_board_array_xml(&ipc2581::edit::Doc::parse(xml)?, spec)
}

/// What balancing collects from an unbalanced array, and its copper layers.
fn balancing_collection(
    ipc: &Ipc2581,
) -> (
    pcb_ir::dialects::ipc::balancing_region::BoardArrayBalancingCollection,
    Vec<pcb_ir::dialects::ipc::BoardArrayCopperLayer>,
) {
    let resolution = Resolution::default();
    let imported = design(ipc);
    let layout = geometry::extract_layout(ipc).unwrap();
    let score_lines = geometry::board_array_vscore_lines(&imported).unwrap();
    let fabrication_profile =
        geometry::board_array_fabrication_profile(&imported, &layout, &score_lines, resolution)
            .unwrap();
    let support_layers = extract_array_support_layers(&imported).unwrap();
    let copper_layers = crate::layers::copper_layers(ipc.ecad().unwrap());
    let collection = collect_board_array_balancing_input(
        &layout,
        &fabrication_profile,
        &copper_layers,
        support_layers
            .iter()
            .map(|source| BoardArraySupportDocument::new(&source.document, source.policy)),
        resolution,
    )
    .unwrap();
    (collection, copper_layers)
}

#[test]
fn parses_board_margin_css_shorthand() {
    for (values, expected) in [
        (&[1.0][..], BoardMarginMm::all(1.0)),
        (&[1.0, 2.0], BoardMarginMm::new(1.0, 2.0, 1.0, 2.0)),
        (&[1.0, 2.0, 3.0], BoardMarginMm::new(1.0, 2.0, 3.0, 2.0)),
        (
            &[1.0, 2.0, 3.0, 4.0],
            BoardMarginMm::new(1.0, 2.0, 3.0, 4.0),
        ),
    ] {
        assert_eq!(BoardMarginMm::from_css_shorthand(values).unwrap(), expected);
    }
    assert!(BoardMarginMm::from_css_shorthand(&[]).is_err());
    assert!(BoardMarginMm::from_css_shorthand(&[1.0, 2.0, 3.0, 4.0, 5.0]).is_err());
}

#[test]
fn creates_rounded_panel_step_from_board_bbox() {
    let xml = create_board_array_xml(&board_fixture_mm(), &six_by_six()).unwrap();

    for expected in [
        r#"<StepRef name="array"/>"#,
        r#"<StepRef name="board_cell"/>"#,
        r#"<StepRef name="board"/>"#,
        r#"<LayerRef name="V-Score"/>"#,
        r#"<Layer name="V-Score" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>"#,
        r#"<Step name="array" type="PALLET">"#,
        r#"<NonstandardAttribute name="diode.panelize.schema_version" type="INTEGER" value="1"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="manual"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.columns" type="INTEGER" value="6"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.rows" type="INTEGER" value="6"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.board_margin_top_mm" type="DOUBLE" value="2.5"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.edge_rail_left_mm" type="DOUBLE" value="5"/>"#,
        r#"<Step name="board_cell" type="PALLET">"#,
        r#"<StepRepeat stepRef="board_cell" x="5" y="5" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#,
        r#"<StepRepeat stepRef="board" x="4.5" y="5.5" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#,
        r#"<LayerFeature layerRef="V-Score">"#,
        r#"<Spec name="Board_Array_VCut">"#,
        r#"<SpecRef id="Board_Array_VCut"/>"#,
        r#"<PolyStepCurve x="3" y="100" centerX="3" centerY="97" clockwise="true"/>"#,
        r#"<Line startX="7.5" startY="0" endX="7.5" endY="100">"#,
        r#"<Line startX="0" startY="7.5" endX="100" endY="7.5">"#,
    ] {
        assert!(xml.contains(expected), "{expected}");
    }

    assert_array(&xml, 100.0, 100.0, 36);
    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);

    let first_instance = layout
        .layout
        .instances
        .iter()
        .find(|instance| {
            layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Board
        })
        .unwrap();
    assert_point_close(first_instance.bbox.min, Point::new(7.5, 7.5));
    assert_point_close(first_instance.bbox.max, Point::new(17.5, 17.5));

    let vcut = geometry::extract_layer_for_view(
        &ipc,
        "V-Score",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert!(vcut.features.len() > 24);
    assert!(
        vcut.features
            .iter()
            .all(|feature| feature.intent.domain == FeatureDomain::VCut)
    );
    assert_eq!(
        geometry::board_array_vscore_lines(&design(&ipc))
            .unwrap()
            .len(),
        24
    );
}

#[test]
fn generated_board_array_has_a_certified_safe_balancing_region() {
    // Safe-region discovery runs on the completed but not-yet-balanced array;
    // otherwise the generated balance copper becomes its own obstacle.
    let xml =
        create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(13.0, 10.0), None).unwrap();
    let (collection, copper_layers) = balancing_collection(&Ipc2581::parse(&xml).unwrap());
    let input = collection.input_for_layer(copper_layers[0].name).unwrap();
    let result = board_array_balancing_region(&input, BalancingRegionOptions::default()).unwrap();

    assert!(collection.board_instance_count > 0);
    assert!(
        collection
            .support_layers
            .iter()
            .all(|layer| layer.unpainted_path_count == 0)
    );
    assert!(
        collection
            .support_layers
            .iter()
            .any(|layer| layer.excluded_documentation_path_count > 0),
        "V-cut callout geometry should be excluded from balancing obstacles"
    );
    assert!(!result.safe_region.is_empty());
    assert!(result.certificate.passes(1e-4));
}

/// A two-copper-layer board with copper only on TOP, on the smallest sheet:
/// balancing costs the panel's area, and nothing here asks how many boards fit.
fn two_layer_board_xml() -> String {
    board_fixture_with_top_line_mm().replace(r#"lineWidth="0.2""#, r#"lineWidth="4""#)
}

#[test]
fn board_array_balancing_solves_every_copper_layer() {
    let resolution = Resolution::default();

    let provisional_xml =
        create_auto_board_array_xml(&two_layer_board_xml(), Some(AutoSheetSize::A7)).unwrap();
    let provisional = Ipc2581::parse(&provisional_xml).unwrap();
    let balance =
        generate_automatic_board_array_copper_balance(&provisional, resolution.tolerance_mm)
            .unwrap();

    assert!(balance.panel_area_mm2 > 0.0);
    assert_eq!(balance.layers.len(), 2);
    assert!(
        balance
            .layers
            .iter()
            .all(|layer| !layer.result.usable.is_empty())
    );
    let top = balance
        .layers
        .iter()
        .find(|layer| layer.layer_name == "TOP")
        .unwrap();
    let bottom = balance
        .layers
        .iter()
        .find(|layer| layer.layer_name == "BOTTOM")
        .unwrap();
    assert!(top.target_density > 0.0);
    assert_eq!(bottom.target_density, 0.0);
    assert!(!top.features.is_empty());
    assert!(bottom.features.is_empty());

    for layer in &balance.layers {
        assert_eq!(
            layer.result.usable.resolution.accuracy,
            pcb_ir::geom::GeometryAccuracy::micrometres(50)
        );
        assert!(
            layer
                .result
                .usable
                .intersection(&layer.existing_copper)
                .unwrap()
                .is_empty()
        );
        assert_eq!(layer.result.solution.target_density, layer.target_density);
        assert!(
            layer.result.solution.residual_error
                <= (layer.result.solution.initial_density - layer.target_density).abs() + 1e-9
        );
    }
}

/// The same panel through creation itself: the report's accounting and the
/// generated geometry in the emitted document.
#[test]
fn board_array_creation_reports_and_emits_the_balance() {
    let resolution = Resolution::default();

    let input = two_layer_board_xml();
    let creation = create_auto_board_array(
        &input,
        Some(AutoSheetSize::A7),
        true,
        Separation::VScore,
        resolution,
    )
    .unwrap();
    let copper_balance = creation.copper_balance.as_ref().unwrap();
    let coarser = create_auto_board_array(
        &input,
        Some(AutoSheetSize::A7),
        true,
        Separation::VScore,
        resolution.with_accuracy(pcb_ir::geom::GeometryAccuracy::micrometres(30)),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&coarser.copper_balance).unwrap(),
        serde_json::to_value(&creation.copper_balance).unwrap()
    );
    assert_eq!(copper_balance.layers.len(), 2);
    for report in &copper_balance.layers {
        // Fixed copper, fillable region, and permanently bare area partition
        // that layer's density domain exactly.
        assert!(
            (report.existing_copper_area_mm2
                + report.usable_area_mm2
                + report.fixed_empty_area_mm2
                - report.density_domain_area_mm2)
                .abs()
                <= 1e-6
        );
        // Unfillable panel material stays out of the denominator entirely.
        assert!(report.density_domain_area_mm2 <= copper_balance.panel_area_mm2 + 1e-6);
        assert!(
            report.residual_error <= (report.initial_density - report.target_density).abs() + 1e-9
        );
    }
    let xml = creation.xml;
    // Perforated balance planes carry their voids as a negative instance set.
    assert!(xml.contains(r#"<Set polarity="NEGATIVE">"#));
    assert!(xml.matches("<Contour>").count() > 0);
}

#[test]
fn board_array_creation_accepts_no_source_copper_layers() {
    let resolution = Resolution::default();

    let input = board_fixture_mm().replace(
        r#"<Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>"#,
        r#"<Layer name="TOP" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>"#,
    );
    let creation = create_board_array(
        &input,
        &options(2, 2, board_margin(10.0, 10.0), BoardMarginMm::all(20.0)),
        true,
        Separation::VScore,
        resolution,
    )
    .unwrap();

    assert!(creation.copper_balance.unwrap().layers.is_empty());
}

#[test]
fn automatic_balancing_regions_scope_panel_fiducials_to_both_surface_copper_layers() {
    let resolution = Resolution::default();
    // The smallest sheet the board fits: fiducial scoping does not depend on
    // how much panel surrounds it.
    let xml = create_auto_board_array_xml(
        &board_fixture_with_mask_bbox_mm(60.0, 60.0),
        Some(AutoSheetSize::A6),
    )
    .unwrap();
    let provisional = Ipc2581::parse(&xml).unwrap();
    let (collection, copper_layers) = balancing_collection(&provisional);

    let support_area = |name: &str| {
        let layer = copper_layers
            .iter()
            .find(|layer| provisional.resolve(layer.name) == name)
            .unwrap();
        let input = collection.input_for_layer(layer.name).unwrap();
        input.support_features.area()
    };

    let top = support_area("TOP");
    let bottom = support_area("BOTTOM");
    assert!(top > 0.0);
    assert!(
        (bottom - top).abs() <= resolution.accuracy.max_error_mm().powi(2),
        "two-sided fiducials and mask openings should provide equal surface-copper obstacle area: top {top:.6} mm², bottom {bottom:.6} mm²",
    );
}

#[test]
fn generated_board_array_xml_validates_with_existing_history_and_callouts() {
    // A dotted history number counts up in its last component.
    for (number, next) in [("1", "2"), ("1.0", "1.1")] {
        let input = schema_valid_board_fixture_mm().replace(
            r#"<HistoryRecord number="1""#,
            &format!(r#"<HistoryRecord number="{number}""#),
        );
        let xml = create_board_array_xml(&input, &six_by_six()).unwrap();

        assert!(xml.contains(&format!(r#"<HistoryRecord number="{next}""#)));
        assert!(xml.contains("Created board array"));
        assert_eq!(xml.matches("<FileRevision").count(), 1);
        assert_eq!(xml.matches("<ChangeRec").count(), 1);
        assert!(xml.matches("<Line ").count() > 1);
        assert_eq!(
            xml.matches("<Features>").count(),
            xml.matches("<Line ").count() + xml.matches("<Contour>").count()
        );
        crate::ipc2581::validate(&xml).expect("generated board array XML should validate");
    }
}

#[test]
fn auto_create_projects_board_to_the_smallest_or_the_requested_sheet() {
    let attribute = |name: &str, kind: &str, value: &str| {
        format!(
            r#"<NonstandardAttribute name="diode.panelize.{name}" type="{kind}" value="{value}"/>"#
        )
    };
    for (sheet, mode, name, (width, height), repeat, rails, boards) in [
        (
            None,
            "auto",
            "A7",
            (105.0, 74.0),
            r#"x="6.5" y="7" nx="4" ny="3""#,
            ("6.5", "7"),
            12,
        ),
        (
            Some(AutoSheetSize::A5),
            "auto_sheet",
            "A5",
            (148.0, 210.0),
            r#"x="5" y="5" nx="6" ny="10""#,
            ("5", "5"),
            60,
        ),
    ] {
        let xml = create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(13.0, 10.0), sheet)
            .unwrap();

        assert!(xml.contains(&format!(
            r#"<StepRepeat stepRef="board_cell" {repeat} dx="23" dy="20" angle="0.00" mirror="false"/>"#
        )));
        for (key, kind, value) in [
            ("mode", "STRING", mode),
            ("sheet", "STRING", name),
            ("sheet_width_mm", "DOUBLE", &width.to_string()),
            ("sheet_height_mm", "DOUBLE", &height.to_string()),
            ("edge_rail_left_mm", "DOUBLE", rails.0),
            ("edge_rail_top_mm", "DOUBLE", rails.1),
        ] {
            assert!(xml.contains(&attribute(key, kind, value)), "{key}");
        }

        assert_array(&xml, width, height, boards);
    }
}

#[test]
fn auto_create_derives_board_margin_from_courtyard_overhang() {
    // The margin grows by the overhang on each side, however large.
    for (courtyard, expected, repeats, boards) in [
        (
            board_fixture_with_courtyard_mm(-2.0, -1.0, 14.0, 12.0),
            BoardMarginMm::new(7.0, 6.0, 6.0, 7.0),
            [
                r#"stepRef="board_cell" x="11" y="6.5" nx="2" ny="4" dx="26" dy="23""#,
                r#"stepRef="board" x="7" y="6" nx="1" ny="1" dx="0" dy="0""#,
            ],
            8,
        ),
        (
            board_fixture_with_courtyard_mm(0.0, 0.0, 32.0, 10.0),
            BoardMarginMm::new(5.0, 24.0, 5.0, 5.0),
            [r#"stepRef="board_cell""#, r#"stepRef="board""#],
            6,
        ),
    ] {
        let ipc = Ipc2581::parse(&courtyard).unwrap();
        let board = primary_board_layout(&ipc).unwrap();
        let margin = auto_board_margin(&ipc, board.bbox, Resolution::default()).unwrap();
        assert_eq!(margin, expected);

        let xml = create_auto_board_array_xml(&courtyard, None).unwrap();
        for repeat in repeats {
            assert!(xml.contains(&format!("<StepRepeat {repeat}")), "{repeat}");
        }
        let layout = geometry::extract_layout(&Ipc2581::parse(&xml).unwrap()).unwrap();
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), boards);
    }
}

#[test]
fn auto_create_allows_large_leftover_edge_rails() {
    let xml =
        create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(124.0, 110.0), None).unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="38" y="14" nx="1" ny="1" dx="134" dy="120" angle="0.00" mirror="false"/>"#
    ));

    assert_array(&xml, 210.0, 148.0, 1);
}

#[test]
fn auto_create_falls_back_to_minimum_single_board_panel_when_a4_does_not_fit() {
    let xml =
        create_auto_board_array_xml(&board_fixture_with_mask_bbox_mm(278.0, 10.0), None).unwrap();

    for expected in [
        r#"<StepRepeat stepRef="board_cell" x="5" y="5" nx="1" ny="1" dx="288" dy="20" angle="0.00" mirror="false"/>"#,
        r#"<StepRepeat stepRef="board" x="5" y="5" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#,
        r#"<NonstandardAttribute name="diode.panelize.mode" type="STRING" value="auto_minimum_panel"/>"#,
    ] {
        assert!(xml.contains(expected), "{expected}");
    }

    assert_array(&xml, 298.0, 30.0, 1);
}

#[test]
fn auto_create_requested_sheet_still_errors_when_sheet_does_not_fit() {
    let error = create_auto_board_array_xml(
        &board_fixture_with_mask_bbox_mm(278.0, 278.0),
        Some(AutoSheetSize::A4),
    )
    .unwrap_err();

    assert!(error.to_string().contains("cannot fit in A4"));
}

#[test]
fn creates_board_array_with_asymmetric_edge_rails() {
    let xml = create_board_array_xml(
        &board_fixture_mm(),
        &options(
            6,
            6,
            board_margin(5.0, 5.0),
            BoardMarginMm::new(8.0, 6.0, 5.0, 7.0),
        ),
    )
    .unwrap();

    assert!(xml.contains(
        r#"<StepRepeat stepRef="board_cell" x="7" y="5" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#
    ));

    assert_array(&xml, 103.0, 103.0, 36);
}

#[test]
fn created_board_array_vcuts_flow_to_svg_and_gerber() {
    let resolution = Resolution::default();

    let xml = create_board_array_xml(&board_fixture_mm(), &six_by_six()).unwrap();
    let ipc = Ipc2581::parse(&xml).unwrap();
    let accessor = IpcAccessor::new(&ipc);

    let svg =
        crate::board_array::render_board_array_overview_svg(&accessor, &design(&ipc), resolution)
            .unwrap()
            .unwrap();
    // Guides draw in their own layer at the score lines' own width.
    assert!(svg.contains("<g fill='#dc2626' stroke='#dc2626' opacity='1'>"));
    assert!(
        svg.matches("stroke-width='0.12' stroke-linecap='round'")
            .count()
            > 24
    );
    let viewbox = svg_viewbox(&svg);
    assert!(viewbox.0 + viewbox.2 > 100.0);
    // The overview draws world y up under one flip group, so the viewBox
    // starts at the negated top edge.
    assert!(-viewbox.1 > 100.0);
    assert_eq!(
        geometry::board_array_vscore_lines(&design(&ipc))
            .unwrap()
            .len(),
        24
    );

    let package = manufacturing_package(&ipc, ArtworkScope::ArrayFlattened).unwrap();

    let vcut = gerber(&package, "V_Cut.gbr").unwrap();
    assert!(vcut.contains("%TF.FileFunction,Vcut*%"));
    assert!(vcut.contains("%TF.Part,Array*%"));
    assert!(vcut.contains("%TA.AperFunction,Other,Vcut*%"));
    assert!(!vcut.contains("G36*"));
    assert!(vcut.matches("D01*").count() > 24);

    let board_package = manufacturing_package(&ipc, ArtworkScope::Board).unwrap();
    assert!(gerber(&board_package, "V_Cut.gbr").is_none());
}

#[test]
fn created_board_array_profile_gerber_derives_vscore_reliefs() {
    let resolution = Resolution::default();

    let rounded_corner = r#"<PolyBegin x="0" y="0"/>
        <PolyStepSegment x="10" y="0"/>
        <PolyStepSegment x="10" y="10"/>
        <PolyStepSegment x="4" y="10"/>
        <PolyStepCurve x="0" y="6" centerX="4" centerY="6" clockwise="false"/>
        <PolyStepSegment x="0" y="0"/>"#;
    let xml = create_board_array_xml(
        &board_fixture(TOP_LAYER, "", rounded_corner, ""),
        &six_by_six(),
    )
    .unwrap();

    assert!(!xml.contains("<SlotCavity"));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let layout = geometry::extract_layout(&ipc).unwrap();
    let fabrication_profile = geometry::board_array_fabrication_profile(
        &design(&ipc),
        &layout,
        &geometry::board_array_vscore_lines(&design(&ipc)).unwrap(),
        resolution,
    )
    .unwrap();
    assert_eq!(
        fabrication_profile.purpose,
        pcb_ir::dialects::ipc::LayoutPurpose::Product
    );
    assert!(fabrication_profile.assembly_panel_outlines.is_empty());

    let package = manufacturing_package(&ipc, ArtworkScope::ArrayFlattened).unwrap();
    let vcut = gerber(&package, "V_Cut.gbr").unwrap();
    assert!(!vcut.contains("G36*"));
    assert!(gerber(&package, "Edge_Cuts.gm1").is_none());
    let profile = gerber(&package, "Board_Array_Profile.gm1").unwrap();
    assert!(profile.contains("%TF.FileFunction,Profile,NP*%"));
    assert!(profile.contains("%TF.Part,Array*%"));
    assert!(profile.contains("%TA.AperFunction,Profile*%"));
    assert!(profile.contains("%ADD10C,0.05*%"));
    assert!(!profile.contains("%ADD11C,1*%"));
    assert!(!profile.contains("G36*"));
    assert!(
        profile.matches("D01*").count()
            > geometry::board_array_vscore_lines(&design(&ipc))
                .unwrap()
                .len(),
        "routed reliefs should emit closed contour strokes, not only the V-cut guide lines"
    );
    gerberx2::GerberX2::parse(profile).unwrap();
}

#[test]
fn board_array_creation_drops_source_board_outline_layer_features() {
    let layers = format!(
        r#"{SURFACE_LAYERS}
  <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>"#
    );
    let features = line_feature("TOP", (1.0, 1.0), (5.0, 1.0), 0.2)
        + &line_feature("Edge.Cuts", (0.0, 0.0), (40.0, 0.0), 0.05);
    let input = board_fixture(
        &layers,
        r#"<LayerRef name="Edge.Cuts"/>"#,
        &rectangle(0.0, 0.0, 40.0, 40.0),
        &features,
    );
    let xml = create_board_array_xml(
        &input,
        &options(2, 2, board_margin(5.0, 5.0), BoardMarginMm::all(5.0)),
    )
    .unwrap();

    assert!(xml.contains(r#"<LayerFeature layerRef="TOP">"#));
    assert!(!xml.contains(r#"<LayerRef name="Edge.Cuts""#));
    assert!(!xml.contains(r#"<Layer name="Edge.Cuts""#));
    assert!(!xml.contains(r#"<LayerFeature layerRef="Edge.Cuts">"#));

    let ipc = Ipc2581::parse(&xml).unwrap();
    let package = manufacturing_package(&ipc, ArtworkScope::ArrayFlattened).unwrap();
    assert!(gerber(&package, "Edge_Cuts.gm1").is_none());
    assert!(gerber(&package, "Board_Array_Profile.gm1").is_some());
}

#[test]
fn board_array_creation_preserves_board_target_geometry() {
    let input = board_fixture_with_top_line_mm();
    let before_ipc = Ipc2581::parse(&input).unwrap();
    let before = geometry::extract_layer_for_view(
        &before_ipc,
        "TOP",
        ArtworkScope::Board,
        Resolution::default(),
    )
    .unwrap();

    let xml = create_board_array_xml(&input, &six_by_six()).unwrap();
    let after_ipc = Ipc2581::parse(&xml).unwrap();
    let after = geometry::extract_layer_for_view(
        &after_ipc,
        "TOP",
        ArtworkScope::Board,
        Resolution::default(),
    )
    .unwrap();

    assert_eq!(before.features.len(), after.features.len());
    assert_eq!(before.arena.paths.len(), after.arena.paths.len());
    assert_eq!(before.arena.contours.len(), after.arena.contours.len());
    assert_eq!(before.arena.cmds, after.arena.cmds);

    for (before_feature, after_feature) in before.features.iter().zip(&after.features) {
        assert_eq!(before_feature.kind, after_feature.kind);
        assert_eq!(before_feature.bucket, after_feature.bucket);
        assert_eq!(before_feature.polarity, after_feature.polarity);
        assert_intent_eq(
            &before_ipc,
            &after_ipc,
            &before_feature.intent,
            &after_feature.intent,
        );
        assert_eq!(before_feature.fiducial_kind, after_feature.fiducial_kind);
        assert_eq!(before_feature.bbox, after_feature.bbox);
        assert_eq!(before_feature.paths.count, after_feature.paths.count);
    }
}

#[test]
fn generated_array_geometry_writes_fiducials_and_nonplated_holes() {
    // Boards this small carry no tooling of the array's own.
    let input = board_fixture(SURFACE_LAYERS, "", &rectangle(-2.0, -3.0, 8.0, 7.0), "");
    let ipc = Ipc2581::parse(&input).unwrap();
    let mut spec = manual_spec(&ipc, &six_by_six(), Separation::VScore);

    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "TOP",
        round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 1.0),
    );
    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "F.Mask",
        round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 2.0),
    );
    spec.generated_geometry.layers.push(GeneratedLayer {
        name: "Array_Drill".to_string(),
        layer_function: LayerFunction::Drill,
        side: Side::All,
        span: None,
    });
    spec.generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        "Array_Drill",
        round_nonplated_hole_features([(20.0, 20.0)], 2.0),
    );
    spec.content_layer_refs = content_layer_refs(
        &ipc,
        &spec.generated_geometry,
        &spec.board_outline_layer_names,
    );

    let xml = write_board_array_xml(&input, &spec).unwrap();

    for expected in [
        r#"<LayerRef name="F.Mask"/>"#,
        r#"<LayerRef name="Array_Drill"/>"#,
        r#"<Layer name="Array_Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>"#,
    ] {
        assert!(xml.contains(expected), "{expected}");
    }
    assert_eq!(xml.matches("<GlobalFiducial>").count(), 2);
    for expected in [
        r#"<Circle diameter="1"/>"#,
        r#"<Circle diameter="2"/>"#,
        r#"diameter="2" platingStatus="NONPLATED""#,
        r#"x="20" y="20""#,
    ] {
        assert!(xml.contains(expected), "{expected}");
    }

    let parsed = Ipc2581::parse(&xml).unwrap();
    let top = geometry::extract_layer_for_view(
        &parsed,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert!(top.features.iter().any(|feature| {
        feature.intent.role == FeatureRole::Fiducial
            && feature.fiducial_kind == FiducialKind::Global
    }));

    let drill = geometry::extract_layer_for_view(
        &parsed,
        "Array_Drill",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert_eq!(drill.features.len(), 1);
    assert_eq!(drill.features[0].kind, FeatureKind::Hole);
    assert_eq!(drill.features[0].bucket, FeatureBucket::Cutout);
    assert_eq!(drill.features[0].intent.domain, FeatureDomain::Drill);
    assert_eq!(drill.features[0].intent.role, FeatureRole::Hole);
    assert_eq!(drill.features[0].intent.operation, FeatureOperation::Drill);
    assert_eq!(drill.features[0].intent.plating, PlatingKind::NonPlated);

    let package = manufacturing_package(&parsed, ArtworkScope::ArrayFlattened).unwrap();
    let top = gerber(&package, "F_Cu.gtl").unwrap();
    let mask = gerber(&package, "F_Mask.gts").unwrap();
    let drill = gerber(&package, "NPTH.drl").unwrap();

    assert!(top.contains("%TA.AperFunction,FiducialPad,Global*%"));
    assert!(mask.contains("%TA.AperFunction,Material*%"));
    assert!(!mask.contains("%TA.AperFunction,FiducialPad"));
    assert!(drill.contains("; #@! TF.FileFunction,NonPlated"));
    assert!(drill.contains("; #@! TA.AperFunction,NonPlated,NPTH,ComponentDrill"));
    assert!(drill.contains("X20.0Y20.0"));
    assert!(!top.contains("%TA.AperFunction,Other,Drill*%"));
    assert!(!mask.contains("%TA.AperFunction,Other,Drill*%"));
}

#[test]
fn explicit_copper_balance_region_round_trips_as_panel_geometry() {
    let resolution = Resolution::default();

    let input = board_fixture_with_top_line_mm();
    let ipc = Ipc2581::parse(&input).unwrap();
    let mut spec = manual_spec(&ipc, &six_by_six(), Separation::VScore);
    let safe_region = ContourSet::rectangle(
        BBox::new(Point::new(0.0, 10.0), Point::new(5.0, 90.0)),
        resolution,
    );

    let existing = ContourSet::empty(Resolution::default());
    let layers = [SpatialCopperBalanceLayerRequest {
        safe_region: &safe_region,
        existing_copper: &existing,
        density_domain: &safe_region,
        target_density: 0.70,
        stack_weight_mm2: 0.0,
    }];
    let balance = generate_spatial_dense_copper_balance(
        DenseCopperBalanceProfile::V1,
        SpatialCopperBalanceRequest {
            panel_region: &safe_region,
            lattice_origin: Point::new(50.0, 50.0),
            layers: &layers,
        },
    )
    .unwrap()
    .layers
    .pop()
    .unwrap();
    let features = balance_features(&balance).unwrap();
    let void_count = features
        .void_sets
        .iter()
        .map(|set| set.sites.len())
        .sum::<usize>();
    let (templates, features) = features.into_layer_features("TOP");
    spec.generated_geometry.user_entries = templates;
    spec.generated_geometry.layer_features.extend(
        features
            .into_iter()
            .map(|feature| (GeneratedFeatureScope::Array, feature)),
    );
    assert!(matches!(
        balance.solution.mode,
        DenseCopperBalanceMode::Perforated { .. }
    ));
    assert!(void_count > 0);

    let xml = write_board_array_xml(&input, &spec).unwrap();
    assert!(xml.contains(r#"<Set polarity="NEGATIVE">"#));
    for kind in ["plane", "full_void", "edge_void", "boundary_web"] {
        assert!(xml.contains(&format!(
            r#"<NonstandardAttribute name="diode.copper_balance" type="STRING" value="{kind}"/>"#
        )));
    }

    let parsed = Ipc2581::parse(&xml).unwrap();
    assert!(xml.matches("<Contour>").count() > 0);
    assert!(xml.matches("<EntryUser").count() > 0);
    assert!(xml.matches("<Location ").count() >= void_count);
    assert!(xml.matches("<UserPrimitiveRef").count() < void_count);

    let mut top = geometry::extract_layer_for_view(
        &parsed,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert!(
        !top.feature_placement_groups.is_empty(),
        "shared IPC Locations should remain a placement group"
    );
    pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut top);
    let copper_balance = |feature: &pcb_ir::dialects::ipc::Feature| {
        top.feature_set(feature)
            .is_some_and(|set| set.copper_balance)
    };
    assert!(
        top.features
            .iter()
            .filter(|feature| feature.source_step_kind == LayoutStepKind::Panel
                && !feature.is_fiducial())
            .all(copper_balance)
    );
    // Paint the balance features in order: the plane, the voids that clear
    // it, then the boundary web.
    let round_trip = top
        .features
        .iter()
        .filter(|feature| {
            feature.source_step_kind == LayoutStepKind::Panel
                && feature.kind == FeatureKind::Primitive
                && copper_balance(feature)
        })
        .fold(ContourSet::empty(resolution), |image, feature| {
            let paint = ContourSet::from_painted_paths(
                &top.arena,
                feature.paths.slice(&top.arena.paths),
                resolution,
            )
            .unwrap();
            match feature.polarity {
                pcb_ir::geom::Polarity::Dark => image.union(&paint),
                pcb_ir::geom::Polarity::Clear => image.difference(&paint),
            }
            .unwrap()
        });

    assert!(!round_trip.is_empty());
    assert!(
        (round_trip.area() - balance.solution.generated_area_mm2).abs()
            <= balance.solution.generated_area_mm2 * 5e-4,
        "IPC area {}, source area {}",
        round_trip.area(),
        balance.solution.generated_area_mm2
    );

    let package = manufacturing_package(&parsed, ArtworkScope::ArrayFlattened).unwrap();
    let top_gerber = gerber(&package, "F_Cu.gtl").unwrap();
    assert!(top_gerber.contains("G36*"));
    assert!(top_gerber.contains("G37*"));
    assert!(top_gerber.contains("%SRX"));
    assert!(top_gerber.contains("%TA.AperFunction,CopperBalancing*%"));
    // Manufacturing Gerbers expand array hierarchy for broad CAM compatibility.
    assert!(!top_gerber.contains("%ABD"));
    // CAM importers composite every clear object, so the lattice ships
    // dark-only: shared cell-ring flashes where the plane is solid, regions
    // along its boundary.
    assert!(top_gerber.contains("%AMOUTLINE"));
    assert!(!top_gerber.contains("%LPC*%"));

    // The composed Gerber image must match the composed IPC image.
    let ipc_copper = {
        let imported =
            pcb_ir::import::ipc2581::import_design(&parsed, Resolution::default()).unwrap();
        imported.composed_layer_image(
            imported.layer_id("TOP").unwrap(),
            pcb_ir::dialects::ipc::ArtworkScope::ArrayFlattened,
            resolution,
        )
    }
    .unwrap();
    let parsed_gerber = gerberx2::GerberX2::parse(top_gerber).unwrap();
    let mask = pcb_ir::dialects::artwork::compose_to_mask(
        &gerberx2::geometry::extract_document(&parsed_gerber, resolution.accuracy).unwrap(),
        resolution,
    )
    .unwrap();
    let mut rings = Vec::new();
    for layer in &mask.layers {
        for shape in mask.shapes(layer) {
            rings.extend(
                pcb_ir::geom::ContourSet::from_contours(
                    &mask.arena.path_contours(shape),
                    pcb_ir::geom::FillRule::NonZero,
                    resolution.strict(),
                )
                .unwrap()
                .rings,
            );
        }
    }
    let gerber_copper =
        ContourSet::from_rings(rings, pcb_ir::geom::FillRule::NonZero, resolution).unwrap();
    assert!(
        (gerber_copper.area() - ipc_copper.area()).abs() <= ipc_copper.area() * 1e-3,
        "Gerber area {}, IPC area {}",
        gerber_copper.area(),
        ipc_copper.area()
    );
}

#[test]
fn board_array_creation_adds_default_tooling_at_single_column_min_width() {
    let input = board_fixture_with_mask_bbox_mm(28.0, 40.0);
    let xml = create_board_array_xml(
        &input,
        &options(1, 1, board_margin(5.0, 0.0), edge_rail(18.5, 15.0)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_two_sided_fiducials(
        &ipc,
        step,
        IpcFiducialKind::Global,
        &[(29.0, 66.15), (41.0, 66.15), (33.0, 3.85), (37.0, 3.85)],
        &[(30.0, 66.15), (40.0, 66.15), (32.0, 3.85), (38.0, 3.85)],
    );
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert!(
        tooling_holes
            .iter()
            .all(|hole| hole.plating_status == PlatingStatus::NonPlated)
    );
    assert_corner_holes(&corner_holes, 70.0, 70.0);
    assert_points_close(
        hole_points(&rail_holes),
        vec![(23.5, 67.5), (46.5, 67.5), (27.5, 2.5), (42.5, 2.5)],
    );

    let package = manufacturing_package(&ipc, ArtworkScope::ArrayFlattened).unwrap();
    assert_fiducial_gerbers(&package, "Global");
}

#[test]
fn board_array_creation_rejects_missing_bottom_soldermask_for_fiducials() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 30.0).replace(
        r#"  <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>
"#,
        "",
    );
    let error = create_board_array_xml(
        &input,
        &options(1, 1, board_margin(5.0, 5.0), BoardMarginMm::all(20.0)),
    )
    .unwrap_err();

    assert!(
        format!("{error:#}")
            .contains("missing bottom solder-mask layer required for two-sided surface features"),
        "{error:#}"
    );
}

#[test]
fn board_array_creation_uses_declared_surface_layers_regardless_of_name() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 30.0)
        .replace(r#"name="TOP""#, r#"name="front-signal""#)
        .replace(r#"name="F.Mask""#, r#"name="front-coating""#)
        .replace(r#"name="BOTTOM""#, r#"name="rear-signal""#)
        .replace(r#"name="B.Mask""#, r#"name="rear-coating""#);
    let xml = create_board_array_xml(
        &input,
        &options(1, 1, board_margin(5.0, 5.0), BoardMarginMm::all(20.0)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    for layer_name in [
        "front-signal",
        "front-coating",
        "rear-signal",
        "rear-coating",
    ] {
        assert_eq!(fiducials_on_layer(&ipc, step, layer_name).len(), 4);
    }
}

#[test]
fn board_array_creation_adds_default_tooling_at_multi_column_min_width() {
    let input = board_fixture_with_mask_bbox_mm(13.0, 40.0);
    let xml = create_board_array_xml(
        &input,
        &options(2, 1, board_margin(5.0, 0.0), edge_rail(18.0, 16.0)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let mask_fiducials = fiducials_on_layer(&ipc, step, "F.Mask");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(mask_fiducials.len(), 4);
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert_corner_holes(&corner_holes, 72.0, 72.0);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(28.5, 68.15), (43.5, 68.15), (32.5, 3.85), (39.5, 3.85)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(23.0, 69.5), (49.0, 69.5), (27.0, 2.5), (45.0, 2.5)],
    );
    // The array is scored along every board edge, through the rails. No
    // fiducial's mask opening may reach a score line, on either side.
    let score_x = [20.5, 33.5, 38.5, 51.5];
    for line in score_x {
        assert!(xml.contains(&format!(
            r#"<Line startX="{line}" startY="0" endX="{line}" endY="72">"#
        )));
    }
    for layer in ["TOP", "BOTTOM"] {
        for (x, _) in fiducial_points(&fiducials_on_layer(&ipc, step, layer)) {
            for line in score_x {
                assert!(
                    (x - line).abs() + 1e-9 >= FIDUCIAL_MASK_OPENING_DIAMETER_MM / 2.0,
                    "{layer} fiducial at x={x} reaches the score line at x={line}"
                );
            }
        }
    }
}

#[test]
fn rail_tooling_needs_room_between_the_score_lines_of_an_outer_board() {
    // 12 mm is the deepest fiducial inset: there it would sit on the score
    // line along the far edge of the outer board.
    let grid = |board_width_mm| ArrayGrid {
        columns: 2,
        rows: 1,
        board_width_mm,
        board_height_mm: 10.0,
        margin_x_mm: 20.5,
        margin_y_mm: 15.0,
        pitch_x_mm: board_width_mm + 5.0,
        pitch_y_mm: 10.0,
        array_width_mm: 2.0 * board_width_mm + 46.0,
        array_height_mm: 40.0,
    };
    assert_eq!(board_array_tooling_rails(&grid(12.0)), None);
    assert_eq!(board_array_tooling_rails(&grid(12.99)), None);
    assert_eq!(
        board_array_tooling_rails(&grid(13.0)),
        Some(RailPair::TopBottom)
    );
}

#[test]
fn board_array_creation_places_array_tooling_on_left_right_for_landscape_arrays() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 28.0);
    let xml = create_board_array_xml(
        &input,
        &options(1, 1, board_margin(5.0, 0.0), edge_rail(15.0, 21.0)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let corner_holes = holes_with_diameter(&tooling_holes, CORNER_TOOLING_HOLE_DIAMETER_MM);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_eq!(top_fiducials.len(), 4);
    assert_eq!(corner_holes.len(), 4);
    assert_eq!(rail_holes.len(), 4);
    assert_corner_holes(&corner_holes, 75.0, 70.0);
    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(3.85, 41.0), (3.85, 29.0), (71.15, 37.0), (71.15, 33.0)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(2.5, 46.5), (2.5, 23.5), (72.5, 42.5), (72.5, 27.5)],
    );
}

#[test]
fn board_array_tooling_falls_back_to_the_other_rail_pair() {
    let input = board_fixture_with_mask_bbox_mm(12.99, 40.0);
    let xml = create_board_array_xml(
        &input,
        &options(2, 1, board_margin(5.0, 0.0), edge_rail(18.5, 20.0)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);
    let rail_holes = holes_with_diameter(&tooling_holes, TOOLING_HOLE_DIAMETER_MM);

    assert_points_close(
        fiducial_points(&top_fiducials),
        vec![(3.85, 52.0), (3.85, 28.0), (69.13, 48.0), (69.13, 32.0)],
    );
    assert_points_close(
        hole_points(&rail_holes),
        vec![(2.5, 57.5), (2.5, 22.5), (70.48, 53.5), (70.48, 26.5)],
    );
}

#[test]
fn auto_create_errors_when_rail_tooling_cannot_fit() {
    let input = board_fixture_with_mask_bbox_mm(10.0, 10.0);
    let error = create_auto_board_array_xml(&input, None).unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot fit rail fiducials and tooling holes"),
        "{error:#}"
    );
}

#[test]
fn board_array_tooling_skips_when_no_rail_pair_fits() {
    let input = board_fixture_with_mask_bbox_mm(12.99, 27.99);
    let xml = create_board_array_xml(
        &input,
        &options(2, 1, board_margin(5.0, 0.0), edge_rail(18.5, 21.5)),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let step = array_step(&ipc);
    let fiducial_count = step
        .layer_features
        .iter()
        .flat_map(|layer_feature| layer_feature.fiducials())
        .count();
    let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);

    assert_eq!(fiducial_count, 0);
    assert!(
        tooling_holes
            .iter()
            .all(|hole| close(hole.diameter, CORNER_TOOLING_HOLE_DIAMETER_MM))
    );
    assert_corner_holes(&tooling_holes, 72.98, 70.99);
}

#[test]
fn board_array_creation_adds_board_cell_fiducials_on_top_bottom_margins() {
    let input = board_fixture_with_mask_bbox_mm(40.0, 30.0);
    let xml = create_board_array_xml(
        &input,
        &options(
            2,
            1,
            BoardMarginMm::new(5.0, 0.0, 5.0, 0.0),
            BoardMarginMm::all(15.0),
        ),
    )
    .unwrap();

    let ipc = Ipc2581::parse(&xml).unwrap();
    let cell = board_cell_step(&ipc);
    assert_two_sided_fiducials(
        &ipc,
        cell,
        IpcFiducialKind::Local,
        &[(3.0, 38.0), (37.0, 38.0), (7.0, 2.0), (33.0, 2.0)],
        &[(3.0, 38.0), (37.0, 38.0), (7.0, 2.0), (33.0, 2.0)],
    );

    let top = geometry::extract_layer_for_view(
        &ipc,
        "TOP",
        ArtworkScope::ArrayFlattened,
        Resolution::default(),
    )
    .unwrap();
    assert_eq!(
        top.features
            .iter()
            .filter(|feature| feature.fiducial_kind == FiducialKind::Local)
            .count(),
        8
    );

    let package = manufacturing_package(&ipc, ArtworkScope::ArrayFlattened).unwrap();
    assert_fiducial_gerbers(&package, "Local");
}

#[test]
fn board_cell_fiducials_follow_the_longer_board_side_with_room_for_them() {
    let none: &[(f64, f64)] = &[];
    for (board, (columns, rows), margin, rail, expected) in [
        // A tall board takes them in its left and right margins.
        (
            (30.0, 40.0),
            (1, 2),
            BoardMarginMm::new(0.0, 5.0, 0.0, 5.0),
            15.0,
            &[(2.0, 37.0), (2.0, 3.0), (38.0, 33.0), (38.0, 7.0)][..],
        ),
        // A single-board array carries them as well as its rail fiducials.
        (
            (40.0, 30.0),
            (1, 1),
            BoardMarginMm::all(5.0),
            15.0,
            &[(8.0, 38.0), (42.0, 38.0), (12.0, 2.0), (38.0, 2.0)],
        ),
        // A margin too narrow for them, then a board too short.
        (
            (40.0, 35.0),
            (2, 1),
            BoardMarginMm::new(4.99, 0.0, 4.99, 0.0),
            15.0,
            none,
        ),
        ((16.99, 16.99), (2, 1), BoardMarginMm::all(5.0), 30.0, none),
    ] {
        let xml = create_board_array_xml(
            &board_fixture_with_mask_bbox_mm(board.0, board.1),
            &options(columns, rows, margin, BoardMarginMm::all(rail)),
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        for layer in ["TOP", "F.Mask"] {
            let fiducials = fiducials_on_layer(&ipc, board_cell_step(&ipc), layer);
            assert_points_close(fiducial_points(&fiducials), expected.to_vec());
        }
        if (columns, rows) == (1, 1) {
            assert_eq!(fiducials_on_layer(&ipc, array_step(&ipc), "TOP").len(), 4);
        }
    }
}

#[test]
fn writes_generated_board_array_values_in_cad_header_units() {
    let xml = create_board_array_xml(
        &board_fixture_with_mask_bbox_mm(1.0, 1.0).replace("MILLIMETER", "INCH"),
        &options(1, 1, board_margin(0.0, 0.0), BoardMarginMm::all(25.4)),
    )
    .unwrap();

    for expected in [
        r#"<PolyStepSegment x="0" y="2.88188976"/>"#,
        r#"<PolyStepCurve x="0.11811024" y="3" centerX="0.11811024" centerY="2.88188976" clockwise="true"/>"#,
        r#"<StepRepeat stepRef="board_cell" x="1" y="1" nx="1" ny="1" dx="1" dy="1" angle="0.00" mirror="false"/>"#,
        r#"<StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#,
    ] {
        assert!(xml.contains(expected), "{expected}");
    }
}

#[test]
fn rejects_primary_panel_step() {
    let error = create_board_array_xml(
        &board_fixture_mm().replace(r#"type="BOARD""#, r#"type="PALLET""#),
        &options(1, 1, board_margin(0.0, 0.0), BoardMarginMm::all(5.0)),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("primary IPC-2581 step is already a board array")
    );
}

#[test]
fn rejects_options_outside_their_ranges() {
    let small = board_fixture_mm();
    let large = board_fixture_with_mask_bbox_mm(60.0, 60.0);
    let spaced = board_margin(5.0, 5.0);
    for (input, columns, rows, margin, rail, expected) in [
        (
            &small,
            11,
            1,
            board_margin(0.0, 0.0),
            5.0,
            "columns must be between 1 and 10; got 11",
        ),
        (
            &small,
            2,
            1,
            board_margin(4.99, 0.0),
            5.0,
            "horizontal board clearance must be 0 mm or at least 5 mm; got 4.99 mm",
        ),
        (
            &small,
            1,
            2,
            board_margin(0.0, 4.99),
            5.0,
            "vertical board clearance must be 0 mm or at least 5 mm; got 4.99 mm",
        ),
        (
            &small,
            1,
            1,
            board_margin(0.0, 0.0),
            0.0,
            "edge rail top must be between 5 and 30 mm; got 0 mm",
        ),
        (
            &small,
            3,
            2,
            spaced,
            5.0,
            "array width must be at least 70 mm; got 55 mm",
        ),
        (
            &small,
            4,
            2,
            spaced,
            5.0,
            "array height must be at least 70 mm; got 40 mm",
        ),
        (
            &large,
            6,
            1,
            spaced,
            5.0,
            "array width must be at most 297 mm; got 400 mm",
        ),
        (
            &large,
            1,
            6,
            spaced,
            5.0,
            "array height must be at most 297 mm; got 400 mm",
        ),
    ] {
        let options = options(columns, rows, margin, BoardMarginMm::all(rail));
        let error = create_board_array_xml(input, &options).unwrap_err();
        assert_eq!(error.to_string(), expected);
    }
}

fn board_margin(horizontal_gap_mm: f64, vertical_gap_mm: f64) -> BoardMarginMm {
    BoardMarginMm::new(
        vertical_gap_mm / 2.0,
        horizontal_gap_mm / 2.0,
        vertical_gap_mm / 2.0,
        horizontal_gap_mm / 2.0,
    )
}

fn edge_rail(horizontal_mm: f64, vertical_mm: f64) -> BoardMarginMm {
    BoardMarginMm::new(vertical_mm, horizontal_mm, vertical_mm, horizontal_mm)
}

fn svg_viewbox(svg: &str) -> (f64, f64, f64, f64) {
    let value = svg
        .split("viewBox='")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .expect("SVG should have a viewBox");
    let values = value
        .split_whitespace()
        .map(|part| part.parse::<f64>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 4);
    (values[0], values[1], values[2], values[3])
}

fn assert_point_close(actual: Point, expected: Point) {
    assert!(
        (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
        "expected {expected:?}, got {actual:?}"
    );
}

fn assert_intent_eq(
    before_ipc: &Ipc2581,
    after_ipc: &Ipc2581,
    before: &FeatureIntent,
    after: &FeatureIntent,
) {
    assert_eq!(before.domain, after.domain);
    assert_eq!(before.role, after.role);
    assert_eq!(before.operation, after.operation);
    assert_eq!(before.material, after.material);
    assert_eq!(before.plating, after.plating);
    assert_eq!(before.side, after.side);
    assert_eq!(
        resolved_feature_span(before_ipc, before.span),
        resolved_feature_span(after_ipc, after.span)
    );
}

fn resolved_feature_span(ipc: &Ipc2581, span: FeatureSpan) -> String {
    match span {
        FeatureSpan::Unknown => "Unknown".to_string(),
        FeatureSpan::ThroughBoard => "ThroughBoard".to_string(),
        FeatureSpan::Layer(layer) => format!("Layer({})", ipc.resolve(layer)),
        FeatureSpan::FromTo { from, to } => format!(
            "FromTo({},{})",
            from.map(|layer| ipc.resolve(layer)).unwrap_or(""),
            to.map(|layer| ipc.resolve(layer)).unwrap_or("")
        ),
    }
}

fn close(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() < 1e-9
}

fn array_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
    ipc.ecad()
        .unwrap()
        .cad_data
        .steps
        .iter()
        .find(|step| ipc.resolve(step.name) == "array")
        .unwrap()
}

fn board_cell_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
    ipc.ecad()
        .unwrap()
        .cad_data
        .steps
        .iter()
        .find(|step| ipc.resolve(step.name) == "board_cell")
        .unwrap()
}

fn fiducials_on_layer<'a>(
    ipc: &'a Ipc2581,
    step: &'a ipc2581::types::ecad::Step,
    layer_name: &str,
) -> Vec<&'a Fiducial> {
    step.layer_features
        .iter()
        .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
        .flat_map(|layer_feature| layer_feature.fiducials())
        .collect()
}

fn assert_two_sided_fiducials(
    ipc: &Ipc2581,
    step: &ipc2581::types::ecad::Step,
    kind: IpcFiducialKind,
    expected_top_points: &[(f64, f64)],
    expected_bottom_points: &[(f64, f64)],
) {
    for (layer_name, diameter_mm, expected_points) in [
        ("TOP", FIDUCIAL_COPPER_DIAMETER_MM, expected_top_points),
        (
            "F.Mask",
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
            expected_top_points,
        ),
        (
            "BOTTOM",
            FIDUCIAL_COPPER_DIAMETER_MM,
            expected_bottom_points,
        ),
        (
            "B.Mask",
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
            expected_bottom_points,
        ),
    ] {
        let fiducials = fiducials_on_layer(ipc, step, layer_name);
        assert_eq!(fiducials.len(), expected_points.len());
        assert!(fiducials.iter().all(|fiducial| fiducial.kind == kind));
        assert!(
            fiducials
                .iter()
                .all(|fiducial| close(fiducial_diameter(fiducial), diameter_mm))
        );
        assert_points_close(fiducial_points(&fiducials), expected_points.to_vec());
    }
}

fn assert_fiducial_gerbers(package: &ManufacturingPackage, kind: &str) {
    let attribute = format!("%TA.AperFunction,FiducialPad,{kind}*%");
    for filename in ["F_Cu.gtl", "B_Cu.gbl"] {
        assert!(
            gerber(package, filename).unwrap().contains(&attribute),
            "{filename} is missing {kind} fiducial metadata"
        );
    }
    for filename in ["F_Mask.gts", "B_Mask.gbs"] {
        let mask = gerber(package, filename).unwrap();
        assert!(mask.contains("%TA.AperFunction,Material*%"));
        assert!(!mask.contains("%TA.AperFunction,FiducialPad"));
    }
}

fn holes_on_layer<'a>(
    ipc: &'a Ipc2581,
    step: &'a ipc2581::types::ecad::Step,
    layer_name: &str,
) -> Vec<&'a Hole> {
    step.layer_features
        .iter()
        .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
        .flat_map(|layer_feature| layer_feature.holes())
        .collect()
}

fn fiducial_diameter(fiducial: &Fiducial) -> f64 {
    match &fiducial.shape {
        FiducialShape::Primitive(StandardPrimitive::Circle(circle)) => circle.shape.diameter,
        _ => panic!("expected round fiducial"),
    }
}

fn fiducial_points(fiducials: &[&Fiducial]) -> Vec<(f64, f64)> {
    fiducials
        .iter()
        .map(|fiducial| (fiducial.location.x, fiducial.location.y))
        .collect()
}

fn hole_points(holes: &[&Hole]) -> Vec<(f64, f64)> {
    holes.iter().map(|hole| (hole.x, hole.y)).collect()
}

fn holes_with_diameter<'a>(holes: &[&'a Hole], diameter_mm: f64) -> Vec<&'a Hole> {
    holes
        .iter()
        .copied()
        .filter(|hole| close(hole.diameter, diameter_mm))
        .collect()
}

fn assert_corner_holes(holes: &[&Hole], array_width_mm: f64, array_height_mm: f64) {
    let inset = ARRAY_CORNER_TOOLING_HOLE_INSET_MM;
    assert_points_close(
        hole_points(holes),
        vec![
            (inset, inset),
            (array_width_mm - inset, inset),
            (array_width_mm - inset, array_height_mm - inset),
            (inset, array_height_mm - inset),
        ],
    );
}

fn assert_points_close(actual: Vec<(f64, f64)>, expected: Vec<(f64, f64)>) {
    let actual = sorted_points(actual);
    let expected = sorted_points(expected);
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        assert!(
            close(actual.0, expected.0) && close(actual.1, expected.1),
            "expected {expected:?}, got {actual:?}"
        );
    }
}

fn sorted_points(mut points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    points.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.total_cmp(&right.0))
    });
    points
}

const TOP_LAYER: &str =
    r#"  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>"#;
const SURFACE_LAYERS: &str = r#"  <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
  <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
  <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
  <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>"#;

/// A millimeter board Step named "board" with `layers`, the steps of its
/// `profile` polygon and its `features`. Content references TOP and
/// `layer_ref`.
fn board_fixture(layers: &str, layer_ref: &str, profile: &str, features: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
<FunctionMode mode="FABRICATION"/>
<StepRef name="board"/>
<LayerRef name="TOP"/>
{layer_ref}
  </Content>
  <Ecad>
<CadHeader units="MILLIMETER"/>
<CadData>
{layers}
  <Step name="board" type="BOARD">
    <Datum x="0" y="0"/>
    <Profile>
      <Polygon>
{profile}
      </Polygon>
    </Profile>
{features}
  </Step>
</CadData>
  </Ecad>
</IPC-2581>"#
    )
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    format!(
        r#"<PolyBegin x="{x0}" y="{y0}"/>
        <PolyStepSegment x="{x1}" y="{y0}"/>
        <PolyStepSegment x="{x1}" y="{y1}"/>
        <PolyStepSegment x="{x0}" y="{y1}"/>
        <PolyStepSegment x="{x0}" y="{y0}"/>"#
    )
}

fn line_feature(layer: &str, start: (f64, f64), end: (f64, f64), width: f64) -> String {
    format!(
        r#"<LayerFeature layerRef="{layer}">
      <Set polarity="POSITIVE">
        <Features>
          <Line startX="{}" startY="{}" endX="{}" endY="{}">
            <LineDesc lineWidth="{width}" lineEnd="ROUND"/>
          </Line>
        </Features>
      </Set>
    </LayerFeature>"#,
        start.0, start.1, end.0, end.1
    )
}

/// One copper layer under a board from (-2, -3) to (8, 7).
fn board_fixture_mm() -> String {
    board_fixture(TOP_LAYER, "", &rectangle(-2.0, -3.0, 8.0, 7.0), "")
}

fn schema_valid_board_fixture_mm() -> String {
    board_fixture_mm().replace(
        "  <Ecad>",
        r#"  <LogisticHeader>
    <Role id="Owner" roleFunction="SENDER"/>
    <Enterprise id="UNKNOWN" code="NONE"/>
    <Person name="UNKNOWN" enterpriseRef="UNKNOWN" roleRef="Owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="KiCad EDA" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="Initial export">
      <SoftwarePackage name="KiCad" revision="10.0.4" vendor="KiCad EDA">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Ecad name="board">"#,
    )
}

/// Both surfaces' copper and mask under a board from the origin.
fn board_fixture_with_mask_bbox_mm(width_mm: f64, height_mm: f64) -> String {
    let profile = rectangle(0.0, 0.0, width_mm, height_mm);
    board_fixture(SURFACE_LAYERS, "", &profile, "")
}

/// A 13 x 10 mm board under one courtyard rectangle.
fn board_fixture_with_courtyard_mm(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let layers = format!(
        r#"{SURFACE_LAYERS}
  <Layer name="F.Courtyard" layerFunction="COURTYARD" side="TOP" polarity="POSITIVE"/>"#
    );
    let courtyard = format!(
        r#"<LayerFeature layerRef="F.Courtyard">
      <Set polarity="POSITIVE">
        <Features>
          <Polygon>
        {}
          </Polygon>
        </Features>
      </Set>
    </LayerFeature>"#,
        rectangle(x0, y0, x1, y1)
    );
    board_fixture(
        &layers,
        r#"<LayerRef name="F.Courtyard"/>"#,
        &rectangle(0.0, 0.0, 13.0, 10.0),
        &courtyard,
    )
}

/// A 13 x 10 mm board from (-2, -3) with one TOP trace.
fn board_fixture_with_top_line_mm() -> String {
    board_fixture(
        SURFACE_LAYERS,
        "",
        &rectangle(-2.0, -3.0, 11.0, 7.0),
        &line_feature("TOP", (0.0, 0.0), (5.0, 0.0), 0.2),
    )
}

#[test]
fn mouse_bite_array_routes_slots_bridged_by_perforated_tabs() {
    let resolution = Resolution::default();
    let creation = create_board_array(
        &board_fixture_with_top_line_mm(),
        &options(2, 2, BoardMarginMm::all(5.0), BoardMarginMm::all(20.0)),
        false,
        Separation::MouseBite,
        resolution,
    )
    .unwrap();
    let xml = creation.xml;
    assert!(!xml.contains("V-Score") && !xml.contains("V_Cut"));
    assert!(xml.contains(r#"name="diode.panelize.separation" type="STRING" value="mouse-bite""#));
    let tabs_per_board: usize = xml
        .split(r#"name="diode.panelize.tabs_per_board" type="INTEGER" value=""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .unwrap();
    assert!(tabs_per_board >= 1);
    // Every tab splits its board's slot once, so a board with k tabs leaves
    // k routed voids, and every tab carries five break holes.
    assert_eq!(xml.matches("<Cutout>").count(), 4 * tabs_per_board);
    let holes = xml.matches(r#"diameter="0.381""#).count();
    assert_eq!(holes, 4 * tabs_per_board * 5);
    let reparsed = Ipc2581::parse(&xml).unwrap();
    let array = reparsed
        .ecad()
        .unwrap()
        .cad_data
        .steps
        .iter()
        .find(|step| reparsed.resolve(step.name) == "array")
        .unwrap();
    assert_eq!(
        array.profile.as_ref().unwrap().cutouts.len(),
        4 * tabs_per_board
    );
}

#[test]
fn mouse_bite_margins_must_hold_the_routed_slot_and_tab_landing() {
    let create = |board_margin_mm| {
        create_board_array(
            &board_fixture_with_mask_bbox_mm(60.0, 60.0),
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm,
                edge_rail_mm: BoardMarginMm::all(20.0),
            },
            false,
            Separation::MouseBite,
            Resolution::default(),
        )
    };
    // Abutting boards are a V-score layout: a slot routed around one would be
    // cut out of its neighbour.
    let error = create(BoardMarginMm::all(0.0)).unwrap_err().to_string();
    assert!(
        error.contains("board margin top must be at least 2.4 mm for mouse-bite separation"),
        "{error}"
    );
    // One short side is enough: there the slot would run into the edge rail
    // and its tooling.
    let error = create(BoardMarginMm {
        left: 2.0,
        ..BoardMarginMm::all(5.0)
    })
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("board margin left must be at least 2.4 mm") && error.contains("got 2 mm"),
        "{error}"
    );
    // The scored-array clearance rule does not apply to routed arrays.
    create(BoardMarginMm::all(2.4)).unwrap();
}

#[test]
fn generated_drill_layer_spans_the_outer_copper_layers() {
    let xml = create_board_array_xml(
        &board_fixture_with_mask_bbox_mm(60.0, 60.0),
        &options(1, 1, BoardMarginMm::all(5.0), BoardMarginMm::all(5.0)),
    )
    .unwrap();
    assert!(xml.contains(r#"<Span fromLayer="TOP" toLayer="BOTTOM"/>"#));

    // The importer resolves tooling holes to that span, as it does for the
    // source's own drill layers, instead of assuming the whole stack.
    let ipc = Ipc2581::parse(&xml).unwrap();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap();
    let layer = imported.layer_id("Board_Array_Drill").unwrap();
    let doc = imported
        .materialize_layer(layer, ArtworkScope::ArrayFlattened)
        .unwrap();
    assert!(!doc.features.is_empty());
    for feature in &doc.features {
        assert_eq!(
            resolved_feature_span(&ipc, feature.intent.span),
            "FromTo(TOP,BOTTOM)"
        );
    }
}

#[test]
fn tabs_land_on_the_narrowest_rail_the_array_leaves() {
    let gap = placement::PRESET.routing_gap_mm;
    let options = |columns, rows, margin, rail| BoardArrayCreateOptions {
        columns,
        rows,
        board_margin_mm: BoardMarginMm::all(margin),
        edge_rail_mm: BoardMarginMm::all(rail),
    };
    // One board: only the strip to the array edge, less its one slot.
    assert!(close(narrowest_rail_mm(&options(1, 1, 5.0, 5.0), gap), 8.6));
    // Between boards a slot is routed on both sides of the shared strip.
    assert!(close(narrowest_rail_mm(&options(2, 1, 5.0, 5.0), gap), 7.2));
    assert!(close(
        narrowest_rail_mm(&options(1, 2, 2.4, 20.0), gap),
        2.0
    ));
    // A short side counts even where nothing is repeated.
    let lopsided = BoardArrayCreateOptions {
        board_margin_mm: BoardMarginMm {
            left: 2.4,
            ..BoardMarginMm::all(10.0)
        },
        ..options(1, 1, 10.0, 5.0)
    };
    assert!(close(narrowest_rail_mm(&lopsided, gap), 6.0));
}

#[test]
fn every_board_of_a_mouse_bite_array_gets_the_same_tabs_and_voids() {
    let ipc = Ipc2581::parse(&board_fixture_with_mask_bbox_mm(60.0, 60.0)).unwrap();
    let options = options(3, 2, BoardMarginMm::all(5.0), BoardMarginMm::all(10.0));
    let spec = manual_spec(&ipc, &options, Separation::MouseBite);
    let points = |polygon: &Polygon| {
        polygon
            .points()
            .iter()
            .map(|p| (p.x, p.y))
            .collect::<Vec<_>>()
    };
    // One void per tab per board, board by board along each row: every
    // board's are the first board's moved by whole pitches.
    let per_board = spec.tabs_per_board;
    assert_eq!(spec.profile_cutouts.len(), 6 * per_board);
    for (index, cutout) in spec.profile_cutouts.iter().enumerate() {
        let (board, void) = (index / per_board, index % per_board);
        let shift = (
            (board % 3) as f64 * spec.grid.pitch_x_mm,
            (board / 3) as f64 * spec.grid.pitch_y_mm,
        );
        let expected = points(&spec.profile_cutouts[void])
            .into_iter()
            .map(|(x, y)| (x + shift.0, y + shift.1))
            .collect::<Vec<_>>();
        assert_points_close(points(cutout), expected);
    }
}
