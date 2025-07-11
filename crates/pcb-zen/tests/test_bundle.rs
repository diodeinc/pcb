mod common;

use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

use common::TestProject;
use pcb_zen::bundle::create_bundle;

/// Helper to verify bundle contents
fn verify_bundle_contents(bundle_path: &Path, expected_files: &[&str]) -> anyhow::Result<()> {
    let file = File::open(bundle_path)?;
    let mut archive = ZipArchive::new(file)?;

    // Collect all files in the archive for debugging
    let mut actual_files = Vec::new();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            actual_files.push(file.name().to_string());
        }
    }

    // Check that all expected files exist in the bundle
    for expected in expected_files {
        let found = (0..archive.len()).any(|i| {
            archive
                .by_index(i)
                .map(|f| f.name() == *expected)
                .unwrap_or(false)
        });

        if !found {
            anyhow::bail!(
                "Expected file '{}' not found in bundle. Bundle contains: {:?}",
                expected,
                actual_files
            );
        }
    }

    // Check bundle.toml exists
    let mut manifest = archive.by_name("bundle.toml")?;
    let mut contents = String::new();
    manifest.read_to_string(&mut contents)?;

    // Verify it's valid TOML
    toml::from_str::<toml::Value>(&contents)?;

    Ok(())
}

/// Helper to create a bundle and verify it
fn test_bundle_creation(env: &TestProject, entry_file: &str, expected_files: &[&str]) {
    let entry_path = env.root().join(entry_file);
    let output_path = env.root().join("output.bundle");

    // Create the bundle
    create_bundle(&entry_path, &output_path).expect("Failed to create bundle");

    // Verify the bundle was created
    assert!(output_path.exists(), "Bundle file should exist");

    // Verify contents
    verify_bundle_contents(&output_path, expected_files).expect("Bundle verification failed");
}

#[test]
fn bundle_no_dependencies() {
    let env = TestProject::new();

    env.add_file(
        "simple.zen",
        r#"
# A simple component with no external dependencies
Component(
    name = "R1",
    footprint = "SMD:0402",
    symbol = Symbol(
        definition = [
            ("1", ["1", "P1"]),
            ("2", ["2", "P2"]),
        ],
    ),
    pins = {
        "1": Net("IN"),
        "2": Net("OUT"),
    },
)
"#,
    );

    test_bundle_creation(&env, "simple.zen", &["bundle.toml", "simple.zen"]);
}

#[test]
fn bundle_local_dependencies() {
    let env = TestProject::new();

    env.add_file(
        "resistor.zen",
        r#"
# A reusable resistor module
P1 = io("P1", Net)
P2 = io("P2", Net)
value = config("value", str)
package = config("package", str, default = "0402")
"#,
    );

    env.add_file(
        "utils.zen",
        r#"
# Utility functions
def create_net_name(prefix, index):
    return prefix + str(index)
"#,
    );

    env.add_file(
        "main.zen",
        r#"
# Main file with local dependencies
load("./utils.zen", "create_net_name")
Resistor = Module("./resistor.zen")

# Create resistor instances
Resistor(
    name = "R1",
    value = "10k",
    P1 = Net("IN1"),
    P2 = Net("OUT1"),
)

Resistor(
    name = "R2",
    value = "10k",
    P1 = Net("IN2"),
    P2 = Net("OUT2"),
)

Resistor(
    name = "R3",
    value = "10k",
    P1 = Net("IN3"),
    P2 = Net("OUT3"),
)
"#,
    );

    test_bundle_creation(
        &env,
        "main.zen",
        &["bundle.toml", "main.zen", "resistor.zen", "utils.zen"],
    );
}

#[test]
fn bundle_symbol_dependencies() {
    let env = TestProject::new();

    // Add a KiCad symbol file
    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "circuit.zen",
        r#"

# Instantiate the component
Component(
    name = "U1",
    footprint = "SOIC:SOIC-8",
    symbol = Symbol(library = "./C146731.kicad_sym"),
    pins = {
        "ICLK": Net("CLK_IN"),
        "Q1": Net("Q1"),
        "Q2": Net("Q2"),
        "Q3": Net("Q3"),
        "Q4": Net("Q4"),
        "GND": Net("GND"),
        "VDD": Net("VCC"),
        "OE": Net("ENABLE"),
    },
)
"#,
    );

    test_bundle_creation(
        &env,
        "circuit.zen",
        &["bundle.toml", "circuit.zen", "C146731.kicad_sym"],
    );
}

#[test]
fn bundle_file_dependencies() {
    let env = TestProject::new();

    // Create some footprint files
    env.add_file(
        "footprints/custom.kicad_mod",
        r#"(module custom (layer F.Cu))"#,
    );

    env.add_file("resources/datasheet.pdf", "Mock PDF content");

    env.add_file(
        "board.zen",
        r#"
# Component using File() for paths
Component(
    name = "U1",
    footprint = File("./footprints/custom.kicad_mod"),
    symbol = Symbol(
        definition = [
            ("1", ["1", "VCC"]),
            ("2", ["2", "GND"]),
        ],
    ),
    pins = {
        "1": Net("VCC"),
        "2": Net("GND"),
    },
    # Note: datasheet is referenced but not loaded via File()
    # as it's just metadata
)
"#,
    );

    test_bundle_creation(
        &env,
        "board.zen",
        &["bundle.toml", "board.zen", "footprints/custom.kicad_mod"],
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn bundle_remote_dependencies() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[packages]
stdlib = "@github/diodeinc/stdlib:v0.0.6"
test_lib = "@github/hexdae/6d919d810f4a3a238688cfd59de8b7ea"
"#,
    );

    env.add_file(
        "remote_test.zen",
        r#"
# Test with remote dependencies
Resistor = Module("@stdlib/generics/Resistor.star")
load("@test_lib/Capacitor.star", "Package")

# Create a resistor from stdlib
Resistor(
    name = "R1",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
)

# Create package instance
pkg = Package("0603")
"#,
    );

    // For remote dependencies, we expect the bundle to contain
    // deps/ folder with downloaded files
    let output_path = env.root().join("remote.bundle");
    create_bundle(&env.root().join("remote_test.zen"), &output_path)
        .expect("Failed to create bundle with remote deps");

    // Verify bundle exists
    assert!(output_path.exists(), "Bundle should exist");

    // Check that it contains the entry file and manifest
    let file = File::open(&output_path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();

    let mut found_manifest = false;
    let mut found_entry = false;
    let mut found_pcb_toml = false;
    let mut has_deps = false;

    for i in 0..archive.len() {
        let file = archive.by_index(i).unwrap();
        let name = file.name();

        if name == "bundle.toml" {
            found_manifest = true;
        } else if name == "remote_test.zen" {
            found_entry = true;
        } else if name == "pcb.toml" {
            found_pcb_toml = true;
        } else if name.starts_with("deps/") {
            has_deps = true;
        }
    }

    assert!(found_manifest, "Should have bundle.toml");
    assert!(found_entry, "Should have entry file");
    assert!(found_pcb_toml, "Should have pcb.toml");
    assert!(has_deps, "Should have deps folder with remote dependencies");
}

#[test]
fn bundle_nested_local_modules() {
    let env = TestProject::new();

    env.add_file(
        "modules/base/component.zen",
        r#"
# Base component module

P1 = io("P1", Net)
P2 = io("P2", Net)
"#,
    );

    env.add_file(
        "modules/derived/resistor.zen",
        r#"
# Derived resistor module
BaseComponent = Module("../base/component.zen")

P1 = io("P1", Net)
P2 = io("P2", Net)

BaseComponent(
    name = "R1",
    P1 = P1,
    P2 = P2,
)
"#,
    );

    env.add_file(
        "complex.zen",
        r#"
# Main file using nested modules
Resistor = Module("./modules/derived/resistor.zen")

Resistor(
    name = "R1",
    P1 = Net("IN"),
    P2 = Net("OUT"),
)
"#,
    );

    test_bundle_creation(
        &env,
        "complex.zen",
        &[
            "bundle.toml",
            "complex.zen",
            "modules/base/component.zen",
            "modules/derived/resistor.zen",
        ],
    );
}

#[test]
fn bundle_workspace_root_references() {
    let env = TestProject::new();

    env.add_file("pcb.toml", "");

    env.add_file(
        "common/types.zen",
        r#"
# Common type definitions
NetType = Net
"#,
    );

    env.add_file(
        "src/modules/board.zen",
        r#"
# Board module using workspace root reference
load("//common/types.zen", "NetType")

input = io("input", NetType)
"#,
    );

    env.add_file(
        "src/main.zen",
        r#"
# Main using relative module
Board = Module("./modules/board.zen")

Board(
    name = "MainBoard",
    input = Net("IN"),
)
"#,
    );

    test_bundle_creation(
        &env,
        "src/main.zen",
        &[
            "bundle.toml",
            "main.zen", // src/main.zen becomes main.zen (relative to src/)
            "modules/board.zen", // src/modules/board.zen becomes modules/board.zen
                        // common/types.zen goes to deps/ with a hash prefix - we can't predict the exact name
        ],
    );

    // Additionally verify that the deps file exists
    let output_path = env.root().join("output.bundle");
    let file = File::open(&output_path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();

    let mut found_types_in_deps = false;
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name();
            if name.starts_with("deps/") && name.ends_with("_types.zen") {
                found_types_in_deps = true;
                break;
            }
        }
    }

    assert!(found_types_in_deps, "Should have types.zen in deps folder");
}

#[test]
fn bundle_with_local_aliases() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[packages]
components = "./lib/components"
utils = "./lib/utils"
"#,
    );

    env.add_file(
        "lib/components/led.zen",
        r#"
# LED component
A = io("A", Net)  # Anode
K = io("K", Net)  # Cathode

Component(
    name = "LED",
    footprint = "LED:LED_0603",
    symbol = Symbol(
        definition = [
            ("A", ["A", "P1"]),
            ("K", ["K", "P2"]),
        ],
    ),
    pins = {
        "A": A,
        "K": K,
    },
)
"#,
    );

    env.add_file(
        "lib/utils/helpers.zen",
        r#"
# Helper functions
def led_name(index):
    return "LED" + str(index)
"#,
    );

    env.add_file(
        "main.zen",
        r#"
# Main using package aliases
load("@utils/helpers.zen", "led_name")

LED = Module("@components/led.zen")

# Create LED array
LED(
    name = led_name(0),
    A = Net("VCC"),
    K = Net("LED_K_0"),
)

LED(
    name = led_name(1),
    A = Net("VCC"),
    K = Net("LED_K_1"),
)

LED(
    name = led_name(2),
    A = Net("VCC"),
    K = Net("LED_K_2"),
)
"#,
    );

    test_bundle_creation(
        &env,
        "main.zen",
        &[
            "bundle.toml",
            "main.zen",
            "lib/components/led.zen",
            "lib/utils/helpers.zen",
            "pcb.toml",
        ],
    );
}

// Test error cases
#[test]
fn bundle_missing_dependency_error() {
    let env = TestProject::new();

    env.add_file(
        "broken.zen",
        r#"
# This should fail - module doesn't exist
Missing = Module("./does_not_exist.zen")
"#,
    );

    let entry_path = env.root().join("broken.zen");
    let output_path = env.root().join("output.bundle");

    // This should fail
    let result = create_bundle(&entry_path, &output_path);
    assert!(result.is_err(), "Should fail with missing dependency");
    assert!(
        !output_path.exists(),
        "Bundle should not be created on error"
    );
}

#[test]
fn bundle_with_symbol_and_footprint_files() {
    let env = TestProject::new();

    // Create directory structure
    env.add_file(
        "symbols/custom.kicad_sym",
        r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "CustomIC" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "U" (id 0) (at 0 0 0))
    (symbol "CustomIC_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "CustomIC_1_1"
      (pin power_in line (at -12.7 7.62 0) (length 2.54)
        (name "VCC" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin power_in line (at -12.7 -7.62 0) (length 2.54)
        (name "GND" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    );

    env.add_file(
        "footprints/custom.kicad_mod",
        r#"(module CustomIC (layer F.Cu)
  (fp_text reference REF** (at 0 0) (layer F.SilkS))
  (pad 1 smd rect (at -1 0) (size 1 1) (layers F.Cu F.Paste F.Mask))
  (pad 2 smd rect (at 1 0) (size 1 1) (layers F.Cu F.Paste F.Mask))
)"#,
    );

    env.add_file(
        "board.zen",
        r#"
# Board using custom symbol and footprint files
Component(
    name = "U1",
    symbol = Symbol(
        library = File("./symbols/custom.kicad_sym"),
        name = "CustomIC"
    ),
    footprint = File("./footprints/custom.kicad_mod"),
    pins = {
        "VCC": Net("VCC"),
        "GND": Net("GND"),
    },
)
"#,
    );

    test_bundle_creation(
        &env,
        "board.zen",
        &[
            "bundle.toml",
            "board.zen",
            "symbols/custom.kicad_sym",
            "footprints/custom.kicad_mod",
        ],
    );
}
