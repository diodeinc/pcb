//! Snapshot tests for V2 auto dependency detection
//!
//! These tests verify that auto-dependency detection properly modifies pcb.toml to add:
//! - @kicad-symbols imports -> gitlab.com/kicad/libraries/kicad-symbols asset
//! - @kicad-footprints imports -> gitlab.com/kicad/libraries/kicad-footprints asset
//!
//! Note: @stdlib is provided implicitly by the toolchain and does NOT get added to [dependencies].
//!
//! Note: These tests run offline and verify pcb.toml modification only.
//! The build itself will fail (missing deps) but that's expected - we're testing auto-dep detection.

#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const V2_PCB_TOML: &str = r#"[workspace]
pcb-version = "0.3"
"#;

/// Test that @stdlib does NOT add a dependency to pcb.toml (toolchain provides it implicitly)
#[test]
fn test_auto_deps_stdlib() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"load("@stdlib/units.zen", "kOhm")

x = kOhm(10)
"#;

    // Run build (will fail due to missing dep, but pcb.toml should be modified)
    let _output = sandbox
        .write("pcb.toml", V2_PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml was updated with the dependency
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_stdlib_pcb_toml", pcb_toml_content);
}

/// Test that @kicad-symbols auto-adds the gitlab asset dependency to pcb.toml
#[test]
fn test_auto_deps_kicad_symbols() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"# Reference a KiCad symbol (this triggers auto-dep detection)
symbol_path = "@kicad-symbols/Device.kicad_sym:R"
"#;

    let _output = sandbox
        .write("pcb.toml", V2_PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml was updated with the asset
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_symbols_pcb_toml", pcb_toml_content);
}

/// Test that @kicad-footprints auto-adds the gitlab asset dependency to pcb.toml
#[test]
fn test_auto_deps_kicad_footprints() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"# Reference a KiCad footprint (this triggers auto-dep detection)
footprint_path = "@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
"#;

    let _output = sandbox
        .write("pcb.toml", V2_PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml was updated with the asset
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_footprints_pcb_toml", pcb_toml_content);
}

/// Test that multiple auto-deps are added together correctly to pcb.toml
#[test]
fn test_auto_deps_multiple() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"load("@stdlib/units.zen", "kOhm", "pF")

# Use both stdlib and kicad assets
resistance = kOhm(10)
capacitance = pF(100)
symbol_path = "@kicad-symbols/Device.kicad_sym:R"
footprint_path = "@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
"#;

    let _output = sandbox
        .write("pcb.toml", V2_PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml contains all dependencies
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_multiple_pcb_toml", pcb_toml_content);
}

/// Test that auto-deps don't duplicate existing dependencies in pcb.toml
#[test]
fn test_auto_deps_no_duplicate() {
    let mut sandbox = Sandbox::new();

    // pcb.toml that already has the stdlib dependency
    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/diodeinc/stdlib" = "0.4.0"
"#;

    let zen_content = r#"load("@stdlib/units.zen", "kOhm")

x = kOhm(10)
"#;

    let _output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml wasn't duplicated - should remain unchanged
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_no_duplicate_pcb_toml", pcb_toml_content);
}

/// Test that auto-deps work with dynamic kicad paths (directory-only dep)
#[test]
fn test_auto_deps_kicad_dynamic_path() {
    let mut sandbox = Sandbox::new();

    // Dynamic footprint path - should add Resistor_SMD.pretty as the asset
    let zen_content = r#"footprint_template = "@kicad-footprints/Resistor_SMD.pretty/R_{size}.kicad_mod"
"#;

    let _output = sandbox
        .write("pcb.toml", V2_PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen", "--offline"]);

    // Verify pcb.toml has the directory-level asset
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_dynamic_path_pcb_toml", pcb_toml_content);
}
