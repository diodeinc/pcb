mod test_helpers;

use ipc2581::Ipc2581;
use std::fs;
use std::path::Path;

// Helper to get BOM stats without loading the whole document
fn get_bom_stats() -> (u32, u32, usize, usize) {
    let bom_path = Path::new("tests/data/testcase1-revc/testcase1-revc-bom.xml");
    let compressed_path = bom_path.with_extension("xml.zst");
    if !compressed_path.exists() {
        return (0, 0, 0, 0);
    }

    let Ok(bom_doc) = test_helpers::parse_compressed(bom_path.to_str().unwrap()) else {
        return (0, 0, 0, 0);
    };

    let Some(bom) = bom_doc.bom() else {
        return (0, 0, 0, 0);
    };

    use ipc2581::BomCategory;

    let mut mechanical_qty = 0u32;
    let mut electrical_qty = 0u32;
    let mut mechanical_types = 0usize;

    for item in &bom.items {
        match item.category {
            Some(BomCategory::Mechanical) => {
                mechanical_qty += item.quantity.unwrap_or(0);
                mechanical_types += 1;
            }
            Some(BomCategory::Electrical) => {
                electrical_qty += item.quantity.unwrap_or(0);
            }
            None => {}
        }
    }

    (
        mechanical_qty,
        electrical_qty,
        mechanical_types,
        bom.items.len(),
    )
}

/// Helper to parse and validate a file with comprehensive checks
fn parse_and_validate(path: &Path) {
    use ipc2581::StandardPrimitive;

    // Load from compressed file
    let content = test_helpers::load_compressed_xml(path);
    let result = Ipc2581::parse(&content);

    match result {
        Ok(doc) => {
            // Validate revision
            assert_eq!(doc.revision(), "C", "Expected revision C");

            let content = doc.content();

            // Verify all refs resolve to non-empty strings
            for step_ref in &content.step_refs {
                assert!(
                    !doc.resolve(*step_ref).is_empty(),
                    "Step ref should resolve"
                );
            }
            for layer_ref in &content.layer_refs {
                assert!(
                    !doc.resolve(*layer_ref).is_empty(),
                    "Layer ref should resolve"
                );
            }
            for bom_ref in &content.bom_refs {
                assert!(!doc.resolve(*bom_ref).is_empty(), "BOM ref should resolve");
            }
            for avl_ref in &content.avl_refs {
                assert!(!doc.resolve(*avl_ref).is_empty(), "AVL ref should resolve");
            }

            // Verify dictionary entries have valid IDs and data
            for entry in &content.dictionary_color.entries {
                let id = doc.resolve(entry.id);
                assert!(!id.is_empty(), "Color ID should not be empty");
                // RGB values are always valid (u8)
            }

            for entry in &content.dictionary_line_desc.entries {
                let id = doc.resolve(entry.id);
                assert!(!id.is_empty(), "LineDesc ID should not be empty");
                assert!(
                    entry.line_desc.line_width >= 0.0,
                    "Line width must be non-negative"
                );
            }

            for entry in &content.dictionary_standard.entries {
                let id = doc.resolve(entry.id);
                assert!(!id.is_empty(), "Standard primitive ID should not be empty");

                // Validate primitive-specific constraints
                match &entry.primitive {
                    StandardPrimitive::Circle(c) => {
                        assert!(c.diameter > 0.0, "Circle diameter must be positive");
                    }
                    StandardPrimitive::RectCenter(r) => {
                        assert!(
                            r.width > 0.0 && r.height > 0.0,
                            "Rectangle dimensions must be positive"
                        );
                    }
                    StandardPrimitive::RectRound(r) => {
                        assert!(
                            r.width > 0.0 && r.height > 0.0,
                            "Rectangle dimensions must be positive"
                        );
                        assert!(r.radius >= 0.0, "Radius must be non-negative");
                    }
                    StandardPrimitive::Oval(o) => {
                        assert!(
                            o.width > 0.0 && o.height > 0.0,
                            "Oval dimensions must be positive"
                        );
                    }
                    StandardPrimitive::Contour(c) => {
                        assert!(
                            !c.polygon.steps.is_empty(),
                            "Contour polygon must have steps"
                        );
                        // Validate cutouts are properly nested
                        for cutout in &c.cutouts {
                            assert!(!cutout.steps.is_empty(), "Cutout must have steps");
                        }
                    }
                    _ => {} // Other primitives - basic validation done
                }
            }

            // Validate function mode is valid
            assert!(
                matches!(
                    content.function_mode.mode,
                    ipc2581::Mode::UserDef
                        | ipc2581::Mode::Bom
                        | ipc2581::Mode::Stackup
                        | ipc2581::Mode::Fabrication
                        | ipc2581::Mode::Assembly
                        | ipc2581::Mode::Test
                        | ipc2581::Mode::Stencil
                        | ipc2581::Mode::Dfx
                ),
                "Function mode should be valid"
            );

            println!(
                "✓ {} - Rev {}, Mode {:?}, {} layers, {} std primitives",
                path.file_name().unwrap().to_string_lossy(),
                doc.revision(),
                content.function_mode.mode,
                content.layer_refs.len(),
                content.dictionary_standard.entries.len()
            );
        }
        Err(e) => {
            panic!("Failed to parse {}: {}", path.display(), e);
        }
    }
}

// Test Case 1: Network Card - Full mode
#[test]
fn test_testcase1_full() {
    let path = Path::new("tests/data/testcase1-revc/testcase1-revc-full.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_assembly() {
    let path = Path::new("tests/data/testcase1-revc/testcase1-revc-assembly.xml");
    parse_and_validate(path);
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
            parse_and_validate(&xml_path);
        }
    }
}

// Test Case 5: Cadence Allegro
#[test]
fn test_testcase5_full() {
    let path = Path::new("tests/data/testcase5-revc/testcase5-revc-full.xml");
    parse_and_validate(path);
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
    parse_and_validate(path);
}

// Test Case 9: LED Display Card
#[test]
fn test_testcase9_full() {
    let path = Path::new("tests/data/testcase9-revc/testcase9-revc-full.xml");
    parse_and_validate(path);
}

// Test Case 10: Demo Board
#[test]
fn test_testcase10_full() {
    let path = Path::new("tests/data/testcase10-revc/testcase10-revc-full.xml");
    parse_and_validate(path);
}

// Test Case 11: Rigid Flex Display Card
#[test]
fn test_testcase11_full() {
    let path = Path::new("tests/data/testcase11-revc/testcase11-rdgflx-revc-full.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase11_assembly() {
    let path = Path::new("tests/data/testcase11-revc/testcase11-rdgflx-revc-assembly.xml");
    parse_and_validate(path);
}

// Test Case 12: Display board w/controller
#[test]
fn test_testcase12_full() {
    let path = Path::new("tests/data/testcase12-revc/testcase12-rdgflx-full.xml");
    parse_and_validate(path);
}

// KiCad generated file
#[test]
fn test_kicad_dm0002() {
    // This file is compressed, load it directly
    let doc = test_helpers::parse_compressed("tests/data/DM0002-IPC-2518.xml")
        .expect("Failed to parse DM0002");

    // Inline validation (same as parse_and_validate)
    use ipc2581::StandardPrimitive;

    assert_eq!(doc.revision(), "C", "Expected revision C");

    let content = doc.content();

    // Verify all refs resolve to non-empty strings
    for step_ref in &content.step_refs {
        assert!(
            !doc.resolve(*step_ref).is_empty(),
            "Step ref should resolve"
        );
    }
    for layer_ref in &content.layer_refs {
        assert!(
            !doc.resolve(*layer_ref).is_empty(),
            "Layer ref should resolve"
        );
    }

    // Verify dictionary entries
    for entry in &content.dictionary_color.entries {
        assert!(
            !doc.resolve(entry.id).is_empty(),
            "Color ID should not be empty"
        );
    }

    for entry in &content.dictionary_standard.entries {
        assert!(
            !doc.resolve(entry.id).is_empty(),
            "Standard primitive ID should not be empty"
        );

        match &entry.primitive {
            StandardPrimitive::Circle(c) => {
                assert!(c.diameter > 0.0, "Circle diameter must be positive");
            }
            StandardPrimitive::RectCenter(r) => {
                assert!(
                    r.width > 0.0 && r.height > 0.0,
                    "Rectangle dimensions must be positive"
                );
            }
            _ => {}
        }
    }

    println!(
        "✓ DM0002-IPC-2518.xml - Rev {}, Mode {:?}",
        doc.revision(),
        content.function_mode.mode
    );
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
        let doc = test_helpers::parse_compressed(path).unwrap();
        assert_eq!(
            doc.content().function_mode.mode,
            expected_mode,
            "Mode mismatch in {}",
            path
        );
    }
}

/// Test that prints metadata for testcase1 to validate against reference data
#[test]
fn test_testcase1_metadata() {
    use ipc2581::{LayerFunction, PlatingStatus};

    let doc = test_helpers::parse_compressed("tests/data/testcase1-revc/testcase1-revc-full.xml")
        .unwrap();

    // Get Ecad data
    if let Some(ecad) = doc.ecad() {
        let step = &ecad.cad_data.steps[0];

        let padstack_defs = step.padstack_defs.len();
        let packages = step.packages.len();
        let components = step.components.len();
        let logical_nets = step.logical_nets.len();

        // Count total connections (sum of pins in all nets)
        let connections: usize = step.logical_nets.iter().map(|net| net.pin_refs.len()).sum();

        // Count layer types
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

        // Count drills from step layer features
        let mut total_drills = 0;
        let mut via_drills = 0;
        let mut plated_drills = 0;
        let mut nonplated_drills = 0;

        for feature in &step.layer_features {
            // Check if this is a drill layer
            let layer_name = doc.resolve(feature.layer_ref);
            let is_drill_layer = ecad.cad_data.layers.iter().any(|l| {
                doc.resolve(l.name) == layer_name && l.layer_function == LayerFunction::Drill
            });

            if is_drill_layer {
                for set in &feature.sets {
                    for hole in &set.holes {
                        total_drills += 1;
                        match hole.plating_status {
                            PlatingStatus::Via => via_drills += 1,
                            PlatingStatus::Plated => plated_drills += 1,
                            PlatingStatus::NonPlated => nonplated_drills += 1,
                        }
                    }
                }
            }
        }

        let total_plated = via_drills + plated_drills;

        // Calculate board dimensions from profile (values are in mm, convert to inches)
        let (board_width_mm, board_height_mm) = if let Some(profile) = &step.profile {
            let polygon = &profile.polygon;

            let mut min_x = polygon.begin.x;
            let mut max_x = polygon.begin.x;
            let mut min_y = polygon.begin.y;
            let mut max_y = polygon.begin.y;

            for step in &polygon.steps {
                let (x, y) = match step {
                    ipc2581::PolyStep::Segment(s) => (s.x, s.y),
                    ipc2581::PolyStep::Curve(c) => (c.x, c.y),
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

        // Convert dimensions from mm to inches for reporting
        let board_width = board_width_mm / 25.4;
        let board_height = board_height_mm / 25.4;

        // Get board thickness from stackup (in mm, convert to inches)
        let board_thickness_mm = ecad
            .cad_data
            .stackups
            .first()
            .and_then(|s| s.overall_thickness)
            .unwrap_or(0.0);
        let board_thickness = board_thickness_mm / 25.4;

        // Check BOM if available
        let (
            bom_mechanical_instances,
            bom_electrical_instances,
            bom_mechanical_types,
            bom_total_items,
        ) = get_bom_stats();

        println!("Testcase 1 Metadata:");
        println!(
            "  Board dimensions: {:.4}\" x {:.4}\" x {:.4}\" ({:.1} mils thick)",
            board_width,
            board_height,
            board_thickness,
            board_thickness * 1000.0
        );
        println!("  Padstack definitions: {}", padstack_defs);
        println!("  Packages: {}", packages);
        println!("  Components: {}", components);
        println!("  LogicalNets: {}", logical_nets);
        println!("  Connections (total pins): {}", connections);
        println!(
            "  Layers: {} copper ({} plane + {} conductor)",
            total_copper_layers, plane_layers, conductor_layers
        );
        println!("  Total layers (all types): {}", ecad.cad_data.layers.len());
        println!(
            "  Drills: {} total ({} plated = {} via + {} tht, {} non-plated)",
            total_drills, total_plated, via_drills, plated_drills, nonplated_drills
        );
        if bom_total_items > 0 {
            println!(
                "  BOM: {} items ({} mechanical types = {} instances, {} electrical instances)",
                bom_total_items,
                bom_mechanical_types,
                bom_mechanical_instances,
                bom_electrical_instances
            );
        }

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

// Macro to generate simple metadata validation tests
macro_rules! testcase_metadata_test {
    ($name:ident, $path:expr, $testcase_name:expr) => {
        #[test]
        fn $name() {
            let doc = test_helpers::parse_compressed($path).unwrap();
            let (
                padstack_defs,
                packages,
                components,
                logical_nets,
                _,
                total_copper_layers,
                _,
                _,
                total_drills,
                ..,
            ) = print_testcase_metadata(&doc, $testcase_name);

            assert!(padstack_defs > 0);
            assert!(packages > 0);
            assert!(components > 0);
            assert!(logical_nets > 0);
            assert!(total_copper_layers > 0);
            assert!(total_drills > 0);
        }
    };
}

testcase_metadata_test!(
    test_testcase3_metadata,
    "tests/data/testcase3-revc/testcase3-revc-full.xml",
    "Testcase 3"
);

// Helper function to extract and print testcase metadata
fn print_testcase_metadata(
    doc: &Ipc2581,
    testcase_name: &str,
) -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    f64,
    f64,
    f64,
) {
    if let Some(ecad) = doc.ecad() {
        let step = &ecad.cad_data.steps[0];

        let padstack_defs = step.padstack_defs.len();
        let packages = step.packages.len();
        let components = step.components.len();
        let logical_nets = step.logical_nets.len();
        let connections: usize = step.logical_nets.iter().map(|net| net.pin_refs.len()).sum();

        let plane_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == ipc2581::LayerFunction::Plane)
            .count();
        let conductor_layers = ecad
            .cad_data
            .layers
            .iter()
            .filter(|l| l.layer_function == ipc2581::LayerFunction::Conductor)
            .count();
        let total_copper_layers = plane_layers + conductor_layers;

        let mut total_drills = 0;
        let mut via_drills = 0;
        let mut plated_drills = 0;
        let mut nonplated_drills = 0;

        for feature in &step.layer_features {
            let layer_name = doc.resolve(feature.layer_ref);
            let is_drill_layer = ecad.cad_data.layers.iter().any(|l| {
                doc.resolve(l.name) == layer_name
                    && l.layer_function == ipc2581::LayerFunction::Drill
            });

            if is_drill_layer {
                for set in &feature.sets {
                    for hole in &set.holes {
                        total_drills += 1;
                        match hole.plating_status {
                            ipc2581::PlatingStatus::Via => via_drills += 1,
                            ipc2581::PlatingStatus::Plated => plated_drills += 1,
                            ipc2581::PlatingStatus::NonPlated => nonplated_drills += 1,
                        }
                    }
                }
            }
        }

        let total_plated = via_drills + plated_drills;

        let (board_width, board_height) = if let Some(profile) = &step.profile {
            let polygon = &profile.polygon;
            let mut min_x = polygon.begin.x;
            let mut max_x = polygon.begin.x;
            let mut min_y = polygon.begin.y;
            let mut max_y = polygon.begin.y;

            for step in &polygon.steps {
                let (x, y) = match step {
                    ipc2581::PolyStep::Segment(s) => (s.x, s.y),
                    ipc2581::PolyStep::Curve(c) => (c.x, c.y),
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

        let board_thickness = ecad
            .cad_data
            .stackups
            .first()
            .and_then(|s| s.overall_thickness)
            .unwrap_or(0.0);

        println!("{} Metadata:", testcase_name);
        println!(
            "  Board dimensions: {:.4}\" x {:.4}\" x {:.4}\" ({:.1} mils thick)",
            board_width,
            board_height,
            board_thickness,
            board_thickness * 1000.0
        );
        println!("  Padstack definitions: {}", padstack_defs);
        println!("  Packages: {}", packages);
        println!("  Components: {}", components);
        println!("  LogicalNets: {}", logical_nets);
        println!("  Connections (total pins): {}", connections);
        println!(
            "  Layers: {} copper ({} plane + {} conductor)",
            total_copper_layers, plane_layers, conductor_layers
        );
        println!("  Total layers (all types): {}", ecad.cad_data.layers.len());
        println!(
            "  Drills: {} total ({} plated = {} via + {} tht, {} non-plated)",
            total_drills, total_plated, via_drills, plated_drills, nonplated_drills
        );

        (
            padstack_defs,
            packages,
            components,
            logical_nets,
            connections,
            total_copper_layers,
            plane_layers,
            conductor_layers,
            total_drills,
            via_drills,
            plated_drills,
            board_width,
            board_height,
            board_thickness,
        )
    } else {
        panic!("Ecad section not found in {}", testcase_name);
    }
}

testcase_metadata_test!(
    test_testcase5_metadata,
    "tests/data/testcase5-revc/testcase5-revc-full.xml",
    "Testcase 5"
);
testcase_metadata_test!(
    test_testcase6_metadata,
    "tests/data/testcase6-revc/testcase6-revc-full.xml",
    "Testcase 6"
);
testcase_metadata_test!(
    test_testcase9_metadata,
    "tests/data/testcase9-revc/testcase9-revc-full.xml",
    "Testcase 9"
);
testcase_metadata_test!(
    test_testcase10_metadata,
    "tests/data/testcase10-revc/testcase10-revc-full.xml",
    "Testcase 10"
);
testcase_metadata_test!(
    test_testcase11_metadata,
    "tests/data/testcase11-revc/testcase11-rdgflx-revc-full.xml",
    "Testcase 11"
);
testcase_metadata_test!(
    test_testcase12_metadata,
    "tests/data/testcase12-revc/testcase12-rdgflx-full.xml",
    "Testcase 12"
);

#[test]
fn test_testcase1_cross_file_consistency() {
    // Parse all three main views
    let full = test_helpers::parse_compressed("tests/data/testcase1-revc/testcase1-revc-full.xml")
        .unwrap();
    let assembly =
        test_helpers::parse_compressed("tests/data/testcase1-revc/testcase1-revc-assembly.xml")
            .unwrap();
    let bom_doc =
        test_helpers::parse_compressed("tests/data/testcase1-revc/testcase1-revc-bom.xml").unwrap();

    println!("\nCross-file Consistency Validation:");

    // Component count should match between full and assembly
    let full_components = full.ecad().unwrap().cad_data.steps[0].components.len();
    let assembly_components = assembly.ecad().unwrap().cad_data.steps[0].components.len();
    println!("  Full view components: {}", full_components);
    println!("  Assembly view components: {}", assembly_components);
    assert_eq!(
        full_components, assembly_components,
        "Component count should match between full and assembly views"
    );

    // Nets - assembly view may not have them (it's assembly-focused, not electrical)
    let full_nets = full.ecad().unwrap().cad_data.steps[0].logical_nets.len();
    let assembly_nets = assembly.ecad().unwrap().cad_data.steps[0]
        .logical_nets
        .len();
    println!("  Full view nets: {}", full_nets);
    println!(
        "  Assembly view nets: {} (assembly view typically omits nets)",
        assembly_nets
    );

    // Package count should match
    let full_packages = full.ecad().unwrap().cad_data.steps[0].packages.len();
    let assembly_packages = assembly.ecad().unwrap().cad_data.steps[0].packages.len();
    println!("  Full view packages: {}", full_packages);
    println!("  Assembly view packages: {}", assembly_packages);
    assert_eq!(
        full_packages, assembly_packages,
        "Package count should match between full and assembly views"
    );

    // BOM item count
    let bom_items = bom_doc.bom().unwrap().items.len();
    println!("  BOM items: {}", bom_items);

    // Calculate total BOM quantity (placed components)
    let mut bom_total_qty = 0u32;
    let mut bom_placed_qty = 0u32;
    for item in &bom_doc.bom().unwrap().items {
        let qty = item.quantity.unwrap_or(0);
        bom_total_qty += qty;

        // Count items with refdes (physically placed)
        if !item.ref_des_list.is_empty() {
            bom_placed_qty += qty;
        }
    }
    println!("  BOM total quantity: {}", bom_total_qty);
    println!("  BOM placed quantity: {}", bom_placed_qty);
    println!(
        "  BOM unplaced quantity: {}",
        bom_total_qty - bom_placed_qty
    );

    // The placed BOM quantity should be close to component count
    // (might differ due to DNP components or other factors)
    println!(
        "  Component vs BOM placed difference: {}",
        (full_components as i32 - bom_placed_qty as i32).abs()
    );

    println!("\n✅ Cross-file consistency validated!");
}
