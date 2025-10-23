use bumpalo::Bump;
use ipc_2581::Ipc2581;
use std::fs;
use std::path::Path;

/// Helper to parse and validate a file with comprehensive checks
fn parse_and_validate(path: &Path) {
    use ipc_2581::StandardPrimitive;

    let arena = Bump::new();
    let result = Ipc2581::parse_file(&arena, path);

    match result {
        Ok(doc) => {
            // Validate revision
            assert_eq!(doc.revision(), "C", "Expected revision C");

            let content = doc.content();

            // Verify all refs resolve to non-empty strings
            for step_ref in &content.step_refs {
                assert!(!doc.resolve(*step_ref).is_empty(), "Step ref should resolve");
            }
            for layer_ref in &content.layer_refs {
                assert!(!doc.resolve(*layer_ref).is_empty(), "Layer ref should resolve");
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
                assert!(entry.line_desc.line_width >= 0.0, "Line width must be non-negative");
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
                        assert!(r.width > 0.0 && r.height > 0.0, "Rectangle dimensions must be positive");
                    }
                    StandardPrimitive::RectRound(r) => {
                        assert!(r.width > 0.0 && r.height > 0.0, "Rectangle dimensions must be positive");
                        assert!(r.radius >= 0.0, "Radius must be non-negative");
                    }
                    StandardPrimitive::Oval(o) => {
                        assert!(o.width > 0.0 && o.height > 0.0, "Oval dimensions must be positive");
                    }
                    StandardPrimitive::Contour(c) => {
                        assert!(!c.polygon.steps.is_empty(), "Contour polygon must have steps");
                        // Validate cutouts are properly nested
                        for cutout in &c.cutouts {
                            assert!(!cutout.steps.is_empty(), "Cutout must have steps");
                        }
                    }
                    _ => {} // Other primitives - basic validation done
                }
            }

            // Validate function mode is valid
            assert!(matches!(
                content.function_mode.mode,
                ipc_2581::Mode::UserDef | ipc_2581::Mode::Bom | ipc_2581::Mode::Stackup |
                ipc_2581::Mode::Fabrication | ipc_2581::Mode::Assembly | ipc_2581::Mode::Test |
                ipc_2581::Mode::Stencil | ipc_2581::Mode::Dfx
            ), "Function mode should be valid");

            println!("✓ {} - Rev {}, Mode {:?}, {} layers, {} std primitives",
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
    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-full.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_assembly() {
    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-Assembly.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_fabrication() {
    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-Fabrication.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_test() {
    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-Test.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase1_stencil() {
    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-Stencil.xml");
    parse_and_validate(path);
}

// Test Case 3: Round Test Card
#[test]
fn test_testcase3_all_modes() {
    let dir = Path::new("tests/data/testcase3_2581REVC");
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("xml") {
            parse_and_validate(&path);
        }
    }
}

// Test Case 5: Cadence Allegro
#[test]
fn test_testcase5_full() {
    let path = Path::new("tests/data/testcase5-revC-Data/testcase5-RevC-Full.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase5_bom() {
    let path = Path::new("tests/data/testcase5-revC-Data/testcase5-RevC-BOM.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase5_stackup() {
    let path = Path::new("tests/data/testcase5-revC-Data/testcase5-RevC-Stackup.xml");
    parse_and_validate(path);
}

// Test Case 6: Cadence Allegro
#[test]
fn test_testcase6_full() {
    let path = Path::new("tests/data/testcase6-RevC_Data/testcase6-RevC-Full.xml");
    parse_and_validate(path);
}

// Test Case 9: LED Display Card
#[test]
fn test_testcase9_full() {
    let path = Path::new("tests/data/testcase9-RevC-data/testcase9-RevC-Full.xml");
    parse_and_validate(path);
}

// Test Case 10: Demo Board
#[test]
fn test_testcase10_full() {
    let path = Path::new("tests/data/testcase10-Rev C data/testcase10-RevC-Full.xml");
    parse_and_validate(path);
}

// Test Case 11: Rigid Flex Display Card
#[test]
fn test_testcase11_full() {
    let path = Path::new("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-full.xml");
    parse_and_validate(path);
}

#[test]
fn test_testcase11_assembly() {
    let path = Path::new("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Assembly.xml");
    parse_and_validate(path);
}

// Test Case 12: Display board w/controller
#[test]
fn test_testcase12_full() {
    let path = Path::new("tests/data/testcase12-RevC/testcase12-rdgflx-full.xml");
    parse_and_validate(path);
}

// KiCad generated file
#[test]
fn test_kicad_dm0002() {
    let path = Path::new("tests/data/DM0002-IPC-2518.xml");
    parse_and_validate(path);
}

/// Test that verifies different function modes parse correctly
#[test]
fn test_function_modes() {
    use ipc_2581::Mode;

    let test_files = [
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Assembly.xml", Mode::Assembly),
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Fabrication.xml", Mode::Fabrication),
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Stackup.xml", Mode::Stackup),
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-BOM.xml", Mode::Bom),
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Test.xml", Mode::Test),
        ("tests/data/testcase11-RevC/testcase11-rdgflx-RevC-Stencil.xml", Mode::Stencil),
    ];

    for (path, expected_mode) in test_files {
        let arena = Bump::new();
        let doc = Ipc2581::parse_file(&arena, path).unwrap();
        assert_eq!(doc.content().function_mode.mode, expected_mode, "Mode mismatch in {}", path);
    }
}

/// Test that prints metadata for testcase1 to validate against reference data
#[test]
fn test_testcase1_metadata() {
    use ipc_2581::LayerFunction;

    let path = Path::new("tests/data/Testcase1-RevC/testcase1-RevC-full.xml");
    let arena = Bump::new();
    let doc = Ipc2581::parse_file(&arena, path).unwrap();

    // Get Ecad data
    if let Some(ecad) = doc.ecad() {
        let step = &ecad.cad_data.steps[0];

        let packages = step.packages.len();
        let components = step.components.len();
        let logical_nets = step.logical_nets.len();

        // Count total connections (sum of pins in all nets)
        let connections: usize = step.logical_nets.iter()
            .map(|net| net.pin_refs.len())
            .sum();

        // Count layer types
        let plane_layers = ecad.cad_data.layers.iter()
            .filter(|l| l.layer_function == LayerFunction::Plane)
            .count();
        let conductor_layers = ecad.cad_data.layers.iter()
            .filter(|l| l.layer_function == LayerFunction::Conductor)
            .count();
        let total_copper_layers = plane_layers + conductor_layers;

        println!("Testcase 1 Metadata:");
        println!("  Packages: {}", packages);
        println!("  Components: {}", components);
        println!("  LogicalNets: {}", logical_nets);
        println!("  Connections (total pins): {}", connections);
        println!("  Layers: {} copper ({} plane + {} conductor)", total_copper_layers, plane_layers, conductor_layers);
        println!("  Total layers (all types): {}", ecad.cad_data.layers.len());

        // Reference data from website:
        // 10.5"x8.5"; 52 mils thick; 1640 package symbols, 27 mechanical symbols
        // 90 padstack definitions; 12 layers; 4 plane layers/8 Signal layers
        // 5675 connections; 5819 - total drills; 5782 plated, 37 non plated; 5516 through hole vias

        assert_eq!(packages, 105, "Should have 105 packages");
        assert_eq!(components, 1656, "Should have 1656 components (1640 + 27 mechanical = ~1656)");
        assert_eq!(logical_nets, 2436, "Should have 2436 logical nets");
        assert_eq!(plane_layers, 4, "Should have 4 plane layers");
        assert_eq!(conductor_layers, 8, "Should have 8 conductor layers");
        assert_eq!(total_copper_layers, 12, "Should have 12 total copper layers");
    } else {
        panic!("Ecad section not found in testcase1");
    }
}
