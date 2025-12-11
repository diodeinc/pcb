#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
name = "BoardDiscoveryTest"
members = ["boards/*", "special/custom-board"]
default_board = "TestBoard"

[packages]
stdlib = "@github/diodeinc/stdlib:v1.0.0"
"#;

const TEST_BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "Main test board for validation"
"#;

const MAIN_BOARD_PCB_TOML: &str = r#"
[board]
name = "MainBoard"
path = "main_board.zen"
"#;

const BROKEN_BOARD_PCB_TOML: &str = r#"
[board]
name = "BrokenBoard"
path = "broken.zen"
"#;

const CUSTOM_BOARD_PCB_TOML: &str = r#"
[board]
name = "CustomBoard"
path = "custom.zen"
description = "Special custom board with unique features"
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib:v0.2.10/interfaces.zen", "Gpio", "Ground", "Power")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_pcb_info_empty_workspace() {
    let output = Sandbox::new().snapshot_run("pcb", ["info"]);
    assert_snapshot!("empty_workspace", output);
}

#[test]
fn test_pcb_info_single_board() {
    let output = Sandbox::new()
        .write("boards/TestBoard/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/TestBoard/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("single_board", output);
}

#[test]
fn test_pcb_info_multiple_boards() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/main-board/pcb.toml", MAIN_BOARD_PCB_TOML)
        .write("boards/main-board/main_board.zen", TEST_BOARD_ZEN)
        .write("boards/broken-board/pcb.toml", BROKEN_BOARD_PCB_TOML)
        .write("special/custom-board/pcb.toml", CUSTOM_BOARD_PCB_TOML)
        .write("special/custom-board/custom.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("multiple_boards", output);
}

#[test]
fn test_pcb_info_json_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/main-board/pcb.toml", MAIN_BOARD_PCB_TOML)
        .write("boards/main-board/main_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info", "-f", "json"]);
    assert_snapshot!("json_format", output);
}

#[test]
fn test_pcb_info_with_path() {
    let output = Sandbox::new()
        .write("subdir/pcb.toml", WORKSPACE_PCB_TOML)
        .write("subdir/boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("subdir/boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info", "subdir"]);
    assert_snapshot!("with_path", output);
}

#[test]
fn test_pcb_info_no_workspace_config() {
    let output = Sandbox::new()
        .write("boards/TestBoard/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/TestBoard/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("no_workspace_config", output);
}

#[test]
fn test_pcb_info_board_without_pcb_toml() {
    let output = Sandbox::new()
        .write("boards/BoardWithoutToml/board.zen", TEST_BOARD_ZEN)
        .write("boards/BoardWithToml/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/BoardWithToml/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("board_without_pcb_toml", output);
}

// Board config without explicit path - should discover the single .zen file
const BOARD_NO_PATH_PCB_TOML: &str = r#"
[board]
name = "DiscoveredBoard"
description = "Board with auto-discovered zen file"
"#;

#[test]
fn test_pcb_info_zen_discovery() {
    // Test that a single .zen file is auto-discovered when path is not specified
    let output = Sandbox::new()
        .write("boards/discovered/pcb.toml", BOARD_NO_PATH_PCB_TOML)
        .write("boards/discovered/discovered.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("zen_discovery", output);
}

#[test]
fn test_pcb_info_zen_discovery_json() {
    // Test JSON output includes discovered path
    let output = Sandbox::new()
        .write("boards/discovered/pcb.toml", BOARD_NO_PATH_PCB_TOML)
        .write("boards/discovered/discovered.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info", "-f", "json"]);
    assert_snapshot!("zen_discovery_json", output);
}

// Board with multiple .zen files - discovery should fail
const BOARD_MULTI_ZEN_PCB_TOML: &str = r#"
[board]
name = "AmbiguousBoard"
description = "Board with multiple zen files"
"#;

const V2_WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]
"#;

#[test]
fn test_pcb_info_multiple_zen_files() {
    // When multiple .zen files exist, discovery should fail gracefully
    // Using V2 workspace to test the V2 display path
    let output = Sandbox::new()
        .write("pcb.toml", V2_WORKSPACE_PCB_TOML)
        .write("boards/ambiguous/pcb.toml", BOARD_MULTI_ZEN_PCB_TOML)
        .write("boards/ambiguous/board1.zen", TEST_BOARD_ZEN)
        .write("boards/ambiguous/board2.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("multiple_zen_files", output);
}
