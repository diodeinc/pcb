#![cfg(not(target_os = "windows"))]

use pcb_test_utils::{assert_snapshot, sandbox::Sandbox};

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
name = "test_workspace"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio", "Ground", "Power")

add_property("layout_path", "build/TB0001")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_pcb_tag_simple_workspace() {
    let mut sb = Sandbox::new().allow_network();
    sb.write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit");

    // Generate layout files before release (full releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/Test/TB0001.zen"])
        .run()
        .expect("layout generation failed");

    let output = sb.snapshot_run(
        "pcb",
        [
            "tag",
            "-v",
            "1.0.0",
            "-b",
            "TB0001",
            "-S",
            "layout.drc.invalid_outline",
        ],
    );
    assert_snapshot!("tag_simple_workspace", output);
}

#[test]
fn test_pcb_tag_invalid_version() {
    let output = Sandbox::new()
        .allow_network()
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run("pcb", ["tag", "-v", "not-a-version", "-b", "TB0001"]);
    assert_snapshot!("tag_invalid_version", output);
}

#[test]
fn test_pcb_tag_duplicate_tag() {
    let output = Sandbox::new()
        .allow_network()
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .tag("boards/Test/v1.0.0") // Pre-existing tag (using package path, not board name)
        .snapshot_run("pcb", ["tag", "-v", "1.0.0", "-b", "TB0001"]);
    assert_snapshot!("tag_duplicate_tag", output);
}

#[test]
fn test_pcb_tag_older_version_allowed() {
    let mut sb = Sandbox::new().allow_network();
    sb.write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .tag("boards/Test/v1.5.0"); // Existing higher version (using package path)

    // Generate layout files before release (full releases require layout)
    sb.run("pcb", ["layout", "--no-open", "boards/Test/TB0001.zen"])
        .run()
        .expect("layout generation failed");

    let output = sb.snapshot_run(
        "pcb",
        [
            "tag",
            "-v",
            "1.2.0",
            "-b",
            "TB0001",
            "-S",
            "layout.drc.invalid_outline",
        ],
    );
    assert_snapshot!("tag_older_version_allowed", output);
}

#[test]
fn test_pcb_tag_invalid_board() {
    let output = Sandbox::new()
        .allow_network()
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", "[board]\nname = \"TB0001\"\n")
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run("pcb", ["tag", "-b", "NonExistentBoard", "-v", "1.0.0"]);
    assert_snapshot!("tag_invalid_board", output);
}
