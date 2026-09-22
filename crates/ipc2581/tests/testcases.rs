use ipc2581::{Ipc2581, LayerFeature, LayerFunction, Mode, PlatingStatus};
use std::io::Read;

/// Parses `tests/data/<name>.xml.zst`; the test data is stored compressed.
fn parse(name: &str) -> Ipc2581 {
    let path = format!("tests/data/{name}.xml.zst");
    let file = std::fs::File::open(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let mut xml = String::new();
    zstd::Decoder::new(file)
        .and_then(|mut decoder| decoder.read_to_string(&mut xml))
        .unwrap_or_else(|error| panic!("{path}: {error}"));
    let doc = Ipc2581::parse(&xml).unwrap_or_else(|error| panic!("{path}: {error}"));
    assert_eq!(doc.revision(), "C", "{path}");
    doc
}

/// Parses a full-mode file and checks that every section came out populated.
fn parse_populated(name: &str) -> Ipc2581 {
    let doc = parse(name);
    let ecad = doc.ecad().expect(name);
    let step = ecad.cad_data.steps.first().expect(name);

    assert!(!step.padstack_defs.is_empty(), "{name}: padstacks");
    assert!(!step.packages.is_empty(), "{name}: packages");
    assert!(!step.components.is_empty(), "{name}: components");
    assert!(!step.logical_nets.is_empty(), "{name}: nets");
    assert!(
        ecad.cad_data.layers.iter().any(|layer| matches!(
            layer.layer_function,
            LayerFunction::Plane | LayerFunction::Conductor
        )),
        "{name}: copper layers"
    );
    assert!(drill_holes(&doc).next().is_some(), "{name}: drills");
    doc
}

/// The holes of the first step's drill layers.
fn drill_holes(doc: &Ipc2581) -> impl Iterator<Item = &ipc2581::Hole> {
    let cad_data = &doc.ecad().unwrap().cad_data;
    let is_drill_layer = |feature: &&LayerFeature| {
        cad_data.layers.iter().any(|layer| {
            layer.name == feature.layer_ref && layer.layer_function == LayerFunction::Drill
        })
    };
    cad_data.steps[0]
        .layer_features
        .iter()
        .filter(is_drill_layer)
        .flat_map(LayerFeature::holes)
}

// Test Case 1: Network Card
#[test]
fn test_testcase1_full() {
    let full = parse("testcase1-revc/testcase1-revc-full");
    let assembly = parse("testcase1-revc/testcase1-revc-assembly");
    let bom = parse("testcase1-revc/testcase1-revc-bom");

    validate_testcase1_metadata(&full);
    validate_testcase1_cross_file_consistency(&full, &assembly, &bom);
}

#[test]
fn test_testcase1_fabrication() {
    parse("testcase1-revc/testcase1-revc-fabrication");
}

#[test]
fn test_testcase1_test() {
    parse("testcase1-revc/testcase1-revc-test");
}

#[test]
fn test_testcase1_stencil() {
    parse("testcase1-revc/testcase1-revc-stencil");
}

// Test Case 3: Round Test Card
#[test]
fn test_testcase3_all_modes() {
    for mode in ["assembly", "bom", "fabrication", "stackup", "test"] {
        parse(&format!("testcase3-revc/testcase3-revc-{mode}"));
    }
    parse_populated("testcase3-revc/testcase3-revc-full");
}

// Test Case 5: Cadence Allegro
#[test]
fn test_testcase5_full() {
    parse_populated("testcase5-revc/testcase5-revc-full");
}

#[test]
fn test_testcase5_bom() {
    parse("testcase5-revc/testcase5-revc-bom");
}

#[test]
fn test_testcase5_stackup() {
    parse("testcase5-revc/testcase5-revc-stackup");
}

// Test Case 6: Cadence Allegro
#[test]
fn test_testcase6_full() {
    parse_populated("testcase6-revc/testcase6-revc-full");
}

// Test Case 9: LED Display Card
#[test]
fn test_testcase9_full() {
    parse_populated("testcase9-revc/testcase9-revc-full");
}

// Test Case 10: Demo Board
#[test]
fn test_testcase10_full() {
    parse_populated("testcase10-revc/testcase10-revc-full");
}

// Test Case 11: Rigid Flex Display Card
#[test]
fn test_testcase11_full() {
    let doc = parse_populated("testcase11-revc/testcase11-rdgflx-revc-full");

    // Rigid-flex layers carry one Profile per zone.
    let layers = &doc.ecad().unwrap().cad_data.layers;
    let profiles = layers.iter().map(|layer| layer.profiles.len()).max();
    assert_eq!(profiles, Some(3));
}

// Test Case 12: Display board w/controller
#[test]
fn test_testcase12_full() {
    parse_populated("testcase12-revc/testcase12-rdgflx-full");
}

// KiCad generated file
#[test]
fn test_kicad_dm0002() {
    parse("DM0002-IPC-2518");
}

#[test]
fn test_function_modes() {
    for (suffix, mode) in [
        ("assembly", Mode::Assembly),
        ("fabrication", Mode::Fabrication),
        ("stackup", Mode::Stackup),
        ("bom", Mode::Bom),
        ("test", Mode::Test),
        ("stencil", Mode::Stencil),
    ] {
        let doc = parse(&format!("testcase11-revc/testcase11-rdgflx-revc-{suffix}"));
        assert_eq!(doc.content().function_mode.mode, mode, "{suffix}");
    }
}

/// Reference data from the IPC-2581 consortium website:
/// 10.5"x8.5"; 52 mils thick; 1640 package symbols, 27 mechanical symbols;
/// 90 padstack definitions; 12 layers, 4 plane and 8 signal;
/// 5819 drills, 5782 plated and 37 non plated, 5516 through hole vias.
/// The file has 1656 `Component` elements, 11 short of 1640 + 27.
fn validate_testcase1_metadata(doc: &Ipc2581) {
    let cad_data = &doc.ecad().unwrap().cad_data;
    let step = &cad_data.steps[0];
    let layers = |function| {
        let layers = cad_data.layers.iter();
        layers.filter(|l| l.layer_function == function).count()
    };
    let drills = |statuses: &[PlatingStatus]| {
        drill_holes(doc)
            .filter(|hole| statuses.contains(&hole.plating_status))
            .count()
    };

    assert_eq!(step.padstack_defs.len(), 90);
    assert_eq!(step.packages.len(), 105);
    assert_eq!(step.components.len(), 1656);
    assert_eq!(step.logical_nets.len(), 2436);
    assert_eq!(layers(LayerFunction::Plane), 4);
    assert_eq!(layers(LayerFunction::Conductor), 8);
    assert_eq!(drill_holes(doc).count(), 5819);
    assert_eq!(
        drills(&[PlatingStatus::Via, PlatingStatus::ViaCapped]),
        5516
    );
    assert_eq!(drills(&[PlatingStatus::Plated]), 266);
    assert_eq!(drills(&[PlatingStatus::NonPlated]), 37);

    let points = step.profile.as_ref().unwrap().polygon.points();
    let span_inches = |coordinate: fn(&ipc2581::Point) -> f64| {
        let values = points.iter().map(coordinate);
        let (min, max) = values.fold((f64::MAX, f64::MIN), |(min, max), value| {
            (min.min(value), max.max(value))
        });
        (max - min) / 25.4
    };
    assert!((span_inches(|point| point.x) - 10.5).abs() < 0.01);
    assert!((span_inches(|point| point.y) - 8.5).abs() < 0.1);
    let thickness = cad_data.stackups[0].overall_thickness.unwrap() / 25.4;
    assert!((thickness - 0.053).abs() < 0.001);
}

fn validate_testcase1_cross_file_consistency(
    full: &Ipc2581,
    assembly: &Ipc2581,
    bom_doc: &Ipc2581,
) {
    let full_step = &full.ecad().unwrap().cad_data.steps[0];
    let assembly_step = &assembly.ecad().unwrap().cad_data.steps[0];
    assert_eq!(full_step.components.len(), assembly_step.components.len());
    assert_eq!(full_step.packages.len(), assembly_step.packages.len());

    let placed_quantity: u32 = bom_doc
        .bom()
        .expect("BOM data")
        .items
        .iter()
        .filter(|item| item.reference_designators().next().is_some())
        .map(|item| item.quantity.unwrap_or(0))
        .sum();
    assert!(
        full_step
            .components
            .len()
            .abs_diff(placed_quantity as usize)
            <= 1,
        "BOM placed quantity should match the component count"
    );
}
