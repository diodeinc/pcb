#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*", "special/custom-board"]
"#;

const WORKSPACE_PCB_TOML_WITH_PREFERRED: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*", "special/custom-board"]
preferred = ["boards/test-board"]
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
load("@stdlib/interfaces.zen", "Gpio")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_pcb_info_empty_workspace() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("empty_workspace", output);
}

#[test]
fn test_pcb_info_single_board() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
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
fn test_pcb_info_json_includes_preferred() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML_WITH_PREFERRED)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/main-board/pcb.toml", MAIN_BOARD_PCB_TOML)
        .write("boards/main-board/main_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info", "-f", "json"]);
    assert_snapshot!("json_format_with_preferred", output);
}

#[test]
fn test_pcb_info_json_includes_published_at() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("initial publishable board");

    sandbox
        .cmd(
            "git",
            [
                "tag",
                "-a",
                "boards/test-board/v0.1.0",
                "-m",
                "Release 0.1.0",
            ],
        )
        .env("GIT_COMMITTER_DATE", "2024-06-01T12:00:00+00:00")
        .run()
        .expect("create first annotated tag");

    sandbox
        .cmd(
            "git",
            [
                "tag",
                "-a",
                "boards/test-board/v0.2.0",
                "-m",
                "Release 0.2.0",
            ],
        )
        .env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05+00:00")
        .run()
        .expect("create second annotated tag");

    let expected_published_at = sandbox
        .cmd(
            "git",
            [
                "for-each-ref",
                "--format=%(taggerdate:iso8601-strict)",
                "refs/tags/boards/test-board/v0.2.0",
            ],
        )
        .read()
        .expect("read latest tag timestamp")
        .trim()
        .to_string();

    let output = sandbox.snapshot_run("pcb", ["info", "-f", "json"]);
    let json = output
        .split("--- STDOUT ---\n")
        .nth(1)
        .and_then(|stdout| stdout.split("\n--- STDERR ---").next())
        .expect("extract JSON output");
    let parsed: serde_json::Value = serde_json::from_str(json).expect("parse JSON output");
    let pkg = &parsed["packages"]["boards/test-board"];

    assert_eq!(pkg["version"], "0.2.0");
    assert_eq!(pkg["published_at"], expected_published_at);

    let normalized = output.replace(&expected_published_at, "<PUBLISHED_AT>");
    assert_snapshot!("json_format_with_published_at", normalized);
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
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/discovered/pcb.toml", BOARD_NO_PATH_PCB_TOML)
        .write("boards/discovered/discovered.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("zen_discovery", output);
}

#[test]
fn test_pcb_info_zen_discovery_json() {
    // Test JSON output includes discovered path
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
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

#[test]
fn test_pcb_info_multiple_zen_files() {
    // When multiple .zen files exist, discovery should fail gracefully
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/ambiguous/pcb.toml", BOARD_MULTI_ZEN_PCB_TOML)
        .write("boards/ambiguous/board1.zen", TEST_BOARD_ZEN)
        .write("boards/ambiguous/board2.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcb", ["info"]);
    assert_snapshot!("multiple_zen_files", output);
}
