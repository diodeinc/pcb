#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

const WORKSPACE_PCB_TOML_WITH_PREFERRED: &str = r#"
[workspace]
pcb-version = "0.4"
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
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("empty_workspace", output);
}

#[test]
fn test_pcb_info_single_board() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/TestBoard/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/TestBoard/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info"]);
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
        .snapshot_run("pcbc", ["info"]);
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
        .snapshot_run("pcbc", ["info", "-f", "json"]);
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
        .snapshot_run("pcbc", ["info", "-f", "json"]);
    assert_snapshot!("json_format_with_preferred", output);
}

#[test]
fn test_pcb_info_json_includes_external_dependency_closure() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/vendor/components.git")
        .write(
            "Thing/pcb.toml",
            r#"
[dependencies]
"github.com/vendor/components/Leaf" = "1.0.0"
"#,
        )
        .write("Thing/Thing.zen", "P1 = io(Net)\n")
        .write("Leaf/pcb.toml", "")
        .write("Leaf/Leaf.zen", "P1 = io(Net)\n")
        .commit("Add component packages")
        .tag("Thing/v1.0.0", true)
        .tag("Leaf/v1.0.0", true)
        .push_mirror();

    sandbox.write(
        "pcb.toml",
        r#"
[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"

[dependencies.indirect]
"github.com/vendor/components/Thing@1" = "1.0.0"
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
    );

    let json_output = sandbox.snapshot_run("pcbc", ["info", "-f", "json"]);
    assert_snapshot!("json_with_external_dependencies", json_output);

    let human_output = sandbox.snapshot_run("pcbc", ["info"]);
    assert_snapshot!("human_with_external_dependencies", human_output);
}

#[test]
fn test_pcb_info_json_includes_sum_free_external_dependency_closure() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/vendor/components.git")
        .write(
            "Thing/pcb.toml",
            r#"
[dependencies]
"github.com/vendor/components/Leaf" = "1.0.0"
"#,
        )
        .write("Thing/Thing.zen", "P1 = io(Net)\n")
        .write("Leaf/pcb.toml", "")
        .write("Leaf/Leaf.zen", "P1 = io(Net)\n")
        .commit("Add component packages")
        .tag("Thing/v1.0.0", true)
        .tag("Leaf/v1.0.0", true)
        .push_mirror();

    let output = sandbox
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.4"
"#,
        )
        .write(
            "boards/Board/pcb.toml",
            r#"
[board]
name = "Board"
path = "Board.zen"

[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"

[dependencies.indirect]
"github.com/vendor/components/Thing@1" = "1.0.0"
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
        )
        .write("boards/Board/Board.zen", "p1 = Net(\"P1\")\n")
        .run("pcbc", ["info", "-f", "json", "boards/Board"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("run pcb info");

    assert!(output.status.success(), "pcb info failed: {output:?}");

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse pcb info JSON");
    let deps = json["external_dependencies"]
        .as_object()
        .expect("external_dependencies is an object");

    assert!(deps.contains_key("github.com/vendor/components/Thing@1.0.0"));
    assert!(deps.contains_key("github.com/vendor/components/Leaf@1.0.0"));
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

    sandbox.env("GIT_COMMITTER_DATE", "2024-06-01T12:00:00+00:00");
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
        .run()
        .expect("create first annotated tag");

    sandbox.env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05+00:00");
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
        .run()
        .expect("create second annotated tag");

    let expected_published_at = "2024-01-02T03:04:05Z";

    let output = sandbox.snapshot_run("pcbc", ["info", "-f", "json"]);
    let json = output
        .split("--- STDOUT ---\n")
        .nth(1)
        .and_then(|stdout| stdout.split("\n--- STDERR ---").next())
        .expect("extract JSON output");
    let parsed: serde_json::Value = serde_json::from_str(json).expect("parse JSON output");
    let pkg = &parsed["packages"]["boards/test-board"];

    assert_eq!(pkg["version"], "0.2.0");
    assert_eq!(pkg["published_at"], expected_published_at);

    let normalized = output.replace(expected_published_at, "<PUBLISHED_AT>");
    assert_snapshot!("json_format_with_published_at", normalized);
}

#[test]
fn test_pcb_info_with_path() {
    let output = Sandbox::new()
        .write("subdir/pcb.toml", WORKSPACE_PCB_TOML)
        .write("subdir/boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("subdir/boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info", "subdir"]);
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
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("zen_discovery", output);
}

#[test]
fn test_pcb_info_zen_discovery_json() {
    // Test JSON output includes discovered path
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/discovered/pcb.toml", BOARD_NO_PATH_PCB_TOML)
        .write("boards/discovered/discovered.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info", "-f", "json"]);
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
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("multiple_zen_files", output);
}
