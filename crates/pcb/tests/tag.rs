#![cfg(not(target_os = "windows"))]

use pcb_test_utils::{assert_snapshot, sandbox::Sandbox};

const PCB_TOML: &str = r#"
[workspace]
name = "test_workspace"

[packages]
stdlib = "@github/diodeinc/stdlib:v0.2.4"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib:v0.2.4/interfaces.zen", "Gpio", "Ground", "Power")

add_property("layout_path", "build/TB0001")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_pcb_tag_simple_workspace() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run("pcb", ["tag", "-v", "1.0.0"]);
    assert_snapshot!("tag_simple_workspace", output);
}

#[test]
fn test_pcb_tag_invalid_version() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run("pcb", ["tag", "-v", "not-a-version"]);
    assert_snapshot!("tag_invalid_version", output);
}

#[test]
fn test_pcb_tag_duplicate_tag() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .tag("TB0001/v1.0.0") // Pre-existing tag
        .snapshot_run("pcb", ["tag", "-v", "1.0.0"]);
    assert_snapshot!("tag_duplicate_tag", output);
}

#[test]
fn test_pcb_tag_older_version_allowed() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .tag("TB0001/v1.5.0") // Existing higher version
        .snapshot_run("pcb", ["tag", "-v", "1.2.0"]);
    assert_snapshot!("tag_older_version_allowed", output);
}

#[test]
fn test_pcb_tag_invalid_board() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.4"])
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run("pcb", ["tag", "-b", "NonExistentBoard", "-v", "1.0.0"]);
    assert_snapshot!("tag_invalid_board", output);
}
