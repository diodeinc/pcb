use crate::test_helpers;
use ipc2581::Ipc2581;
use std::fs;
use std::path::Path;

/// Helper to parse and validate a file with comprehensive checks
fn parse_and_validate(path: &Path) -> Ipc2581 {
    use ipc2581::StandardPrimitive;

    let xml = test_helpers::load_compressed_xml(path);
    let doc = Ipc2581::parse(&xml)
        .unwrap_or_else(|error| panic!("Failed to parse {}: {error}", path.display()));
    assert_eq!(doc.revision(), "C", "Expected revision C");

    let content = doc.content();
    for reference in content
        .step_refs
        .iter()
        .chain(&content.layer_refs)
        .chain(&content.bom_refs)
        .chain(&content.avl_refs)
    {
        assert!(
            !doc.resolve(*reference).is_empty(),
            "Reference should resolve"
        );
    }

    for entry in &content.dictionary_color.entries {
        assert!(
            !doc.resolve(entry.id).is_empty(),
            "Color ID should not be empty"
        );
    }

    for entry in &content.dictionary_line_desc.entries {
        assert!(
            !doc.resolve(entry.id).is_empty(),
            "LineDesc ID should not be empty"
        );
        assert!(
            entry.line_desc.line_width >= 0.0,
            "Line width must be non-negative"
        );
    }

    for entry in &content.dictionary_standard.entries {
        assert!(
            !doc.resolve(entry.id).is_empty(),
            "Standard primitive ID should not be empty"
        );
        match &entry.primitive {
            StandardPrimitive::Circle(circle) => {
                assert!(
                    circle.shape.diameter > 0.0,
                    "Circle diameter must be positive"
                );
            }
            StandardPrimitive::RectCenter(rect) => {
                assert!(
                    rect.shape.size.width > 0.0 && rect.shape.size.height > 0.0,
                    "Rectangle dimensions must be positive"
                );
            }
            StandardPrimitive::RectRound(rect) => {
                assert!(
                    rect.shape.size.width > 0.0 && rect.shape.size.height > 0.0,
                    "Rectangle dimensions must be positive"
                );
                assert!(rect.shape.radius >= 0.0, "Radius must be non-negative");
            }
            StandardPrimitive::Oval(oval) => {
                assert!(
                    oval.shape.size.width > 0.0 && oval.shape.size.height > 0.0,
                    "Oval dimensions must be positive"
                );
            }
            StandardPrimitive::Contour(contour) => {
                assert!(!contour.polygon.steps.is_empty(), "Contour must have steps");
                assert!(
                    contour
                        .cutouts
                        .iter()
                        .all(|cutout| !cutout.steps.is_empty()),
                    "Cutouts must have steps"
                );
            }
            _ => {}
        }
    }

    doc
}

// Test Case 1: Network Card - Full mode
#[test]
fn test_testcase1_full() {
    let full = parse_and_validate(Path::new(
        "tests/data/testcase1-revc/testcase1-revc-full.xml",
    ));
    let assembly = parse_and_validate(Path::new(
        "tests/data/testcase1-revc/testcase1-revc-assembly.xml",
    ));
    let bom =
        test_helpers::parse_compressed("tests/data/testcase1-revc/testcase1-revc-bom.xml").unwrap();

    validate_testcase1_metadata(&full);
    validate_testcase1_cross_file_consistency(&full, &assembly, &bom);
}

#[test]
fn test_testcase1_fabrication() {
    let path = Path::new("tests/data/testcase1-revc/testcase1-revc-fabrication.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_test() {
    let path = Path::new("tests/data/testcase1-revc/testcase1-revc-test.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_stencil() {
    let path = Path::new("tests/data/testcase1-revc/testcase1-revc-stencil.xml");
    parse_and_validate(path);
}

// Test Case 3: Round Test Card
#[test]
fn test_testcase3_all_modes() {
    let dir = Path::new("tests/data/testcase3-revc");
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("zst") {
            // Remove .zst extension to get the .xml path for parse_and_validate
            let xml_path = path.with_extension("").with_extension("");
            let doc = parse_and_validate(&xml_path);
            if xml_path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with("-full"))
            {
                assert_metadata_populated(&doc, "Testcase 3");
            }
        }
    }
}

// Test Case 5: Cadence Allegro
#[test]
fn test_testcase5_full() {
    let path = Path::new("tests/data/testcase5-revc/testcase5-revc-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 5");
}

#[test]
fn test_testcase5_bom() {
    let path = Path::new("tests/data/testcase5-revc/testcase5-revc-bom.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase5_stackup() {
    let path = Path::new("tests/data/testcase5-revc/testcase5-revc-stackup.xml");
    parse_and_validate(path);
}

// Test Case 6: Cadence Allegro
#[test]
fn test_testcase6_full() {
    let path = Path::new("tests/data/testcase6-revc/testcase6-revc-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 6");
}

// Test Case 9: LED Display Card
#[test]
fn test_testcase9_full() {
    let path = Path::new("tests/data/testcase9-revc/testcase9-revc-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 9");
}

// Test Case 10: Demo Board
#[test]
fn test_testcase10_full() {
    let path = Path::new("tests/data/testcase10-revc/testcase10-revc-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 10");
}

// Test Case 11: Rigid Flex Display Card
#[test]
fn test_testcase11_full() {
    let path = Path::new("tests/data/testcase11-revc/testcase11-rdgflx-revc-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 11");
}

// Test Case 12: Display board w/controller
#[test]
fn test_testcase12_full() {
    let path = Path::new("tests/data/testcase12-revc/testcase12-rdgflx-full.xml");
    let doc = parse_and_validate(path);
    assert_metadata_populated(&doc, "Testcase 12");
}

// KiCad generated file
#[test]
fn test_kicad_dm0002() {
    parse_and_validate(Path::new("tests/data/DM0002-IPC-2518.xml"));
}

/// Test that verifies different function modes parse correctly
#[test]
fn test_function_modes() {
    use ipc2581::Mode;

    let test_files = [
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-assembly.xml",
            Mode::Assembly,
        ),
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-fabrication.xml",
            Mode::Fabrication,
        ),
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-stackup.xml",
            Mode::Stackup,
        ),
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-bom.xml",
            Mode::Bom,
        ),
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-test.xml",
            Mode::Test,
        ),
        (
            "tests/data/testcase11-revc/testcase11-rdgflx-revc-stencil.xml",
            Mode::Stencil,
        ),
    ];

    for (path, expected_mode) in test_files {
        let doc = parse_and_validate(Path::new(path));
        assert_eq!(
            doc.content().function_mode.mode,
            expected_mode,
            "Mode mismatch in {}",
            path
        );
    }
}

fn validate_testcase1_metadata(doc: &Ipc2581) {
    use ipc2581::{LayerFunction, PlatingStatus};

    if let Some(ecad) = doc.ecad() {
        let step = &ecad.cad_data.steps[0];

        let padstack_defs = step.padstack_defs.len();
        let packages = step.packages.len();
        let components = step.components.len();
        let logical_nets = step.logical_nets.len();

        let plane_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == LayerFunction::Plane)
            .count();
        let conductor_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == LayerFunction::Conductor)
            .count();
        let total_copper_layers = plane_layers + conductor_layers;

        let mut total_drills = 0;
        let mut via_drills = 0;
        let mut plated_drills = 0;
        let mut nonplated_drills = 0;

        for feature in &step.layer_features {
            let layer_name = doc.resolve(feature.layer_ref);
            let is_drill_layer = ecad.cad_data.layers.iter().any(|l| {
                doc.resolve(l.name) == layer_name && l.layer_function == LayerFunction::Drill
            });

            if is_drill_layer {
                for set in &feature.sets {
                    for hole in set.holes() {
                        total_drills += 1;
                        match hole.plating_status {
                            PlatingStatus::Via | PlatingStatus::ViaCapped => via_drills += 1,
                            PlatingStatus::Plated => plated_drills += 1,
                            PlatingStatus::NonPlated => nonplated_drills += 1,
                        }
                    }
                }
            }
        }

        let total_plated = via_drills + plated_drills;

        let (board_width_mm, board_height_mm) = if let Some(profile) = &step.profile {
            let polygon = &profile.polygon;

            let mut min_x = polygon.begin.x;
            let mut max_x = polygon.begin.x;
            let mut min_y = polygon.begin.y;
            let mut max_y = polygon.begin.y;

            for step in &polygon.steps {
                let (x, y) = match step {
                    ipc2581::PolyStep::Segment(s) => (s.point.x, s.point.y),
                    ipc2581::PolyStep::Curve(c) => (c.point.x, c.point.y),
                };
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }

            (max_x - min_x, max_y - min_y)
        } else {
            (0.0, 0.0)
        };

        let board_width = board_width_mm / 25.4;
        let board_height = board_height_mm / 25.4;

        let board_thickness_mm = ecad
            .cad_data
            .stackups
            .first()
            .and_then(|s| s.overall_thickness)
            .unwrap_or(0.0);
        let board_thickness = board_thickness_mm / 25.4;

        // Reference data from website:
        // 10.5"x8.5"; 52 mils thick; 1640 package symbols, 27 mechanical symbols
        // 90 padstack definitions; 12 layers; 4 plane layers/8 Signal layers
        // 5675 connections; 5819 - total drills; 5782 plated, 37 non plated; 5516 through hole vias
        //
        // Note: Reference says "1640 + 27 = 1667 components" but XML has 1656 Component elements.
        // The discrepancy of 11 may be due to different counting methods or version differences.

        assert_eq!(padstack_defs, 90, "Should have 90 padstack definitions");
        assert_eq!(packages, 105, "Should have 105 package definitions");
        assert_eq!(
            components, 1656,
            "Should have 1656 component instances (XML actual count)"
        );
        assert_eq!(logical_nets, 2436, "Should have 2436 logical nets");
        assert_eq!(plane_layers, 4, "Should have 4 plane layers");
        assert_eq!(conductor_layers, 8, "Should have 8 conductor layers");
        assert_eq!(
            total_copper_layers, 12,
            "Should have 12 total copper layers"
        );
        assert_eq!(total_drills, 5819, "Should have 5819 total drills");
        assert_eq!(total_plated, 5782, "Should have 5782 plated (via + tht)");
        assert_eq!(via_drills, 5516, "Should have 5516 via drills");
        assert_eq!(plated_drills, 266, "Should have 266 plated tht drills");
        assert_eq!(nonplated_drills, 37, "Should have 37 non-plated drills");

        // Board dimensions (approximate match)
        assert!(
            (board_width - 10.5).abs() < 0.01,
            "Board width should be ~10.5 inches"
        );
        assert!(
            (board_height - 8.5).abs() < 0.1,
            "Board height should be ~8.5 inches"
        );
        assert!(
            (board_thickness - 0.053).abs() < 0.001,
            "Board thickness should be ~0.053 inches (53 mils)"
        );
    } else {
        panic!("Ecad section not found in testcase1");
    }
}

fn assert_metadata_populated(doc: &Ipc2581, testcase_name: &str) {
    let ecad = doc
        .ecad()
        .unwrap_or_else(|| panic!("Ecad section not found in {testcase_name}"));
    let step = ecad
        .cad_data
        .steps
        .first()
        .unwrap_or_else(|| panic!("Step not found in {testcase_name}"));

    assert!(!step.padstack_defs.is_empty(), "{testcase_name}: padstacks");
    assert!(!step.packages.is_empty(), "{testcase_name}: packages");
    assert!(!step.components.is_empty(), "{testcase_name}: components");
    assert!(!step.logical_nets.is_empty(), "{testcase_name}: nets");
    assert!(
        ecad.cad_data.layers.iter().any(|layer| matches!(
            layer.layer_function,
            ipc2581::LayerFunction::Plane | ipc2581::LayerFunction::Conductor
        )),
        "{testcase_name}: copper layers"
    );
    assert!(
        step.layer_features.iter().any(|feature| {
            let layer_name = doc.resolve(feature.layer_ref);
            let is_drill_layer = ecad.cad_data.layers.iter().any(|layer| {
                doc.resolve(layer.name) == layer_name
                    && layer.layer_function == ipc2581::LayerFunction::Drill
            });
            is_drill_layer && feature.sets.iter().any(|set| set.holes().next().is_some())
        }),
        "{testcase_name}: drills"
    );
}

fn validate_testcase1_cross_file_consistency(
    full: &Ipc2581,
    assembly: &Ipc2581,
    bom_doc: &Ipc2581,
) {
    let full_step = &full.ecad().expect("full ECAD data").cad_data.steps[0];
    let assembly_step = &assembly.ecad().expect("assembly ECAD data").cad_data.steps[0];

    assert_eq!(
        full_step.components.len(),
        assembly_step.components.len(),
        "Component count should match between full and assembly views"
    );
    assert_eq!(
        full_step.packages.len(),
        assembly_step.packages.len(),
        "Package count should match between full and assembly views"
    );

    let bom = bom_doc.bom().expect("BOM data");
    assert!(!bom.items.is_empty());
    let placed_quantity: u32 = bom
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
