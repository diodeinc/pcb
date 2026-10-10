#![cfg(not(target_os = "windows"))]

use pcb_test_utils::{assert_snapshot, sandbox::Sandbox};

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
name = "test_workspace"
"#;

const PCB_TOML_WITH_PATCH: &str = r#"
[workspace]
pcb-version = "0.4"
name = "test_workspace"

[dependencies]
"github.com/example/components/Qux" = { version = "1.2.3", branch = "main" }
"github.com/example/components/Quux" = { rev = "def5678" }

[patch]
"github.com/example/components/Foo" = { path = "forks/Foo" }
"github.com/example/components/Bar" = { rev = "abc1234" }
"github.com/example/components/Baz" = { branch = "main" }
"#;

const BOARD_TB0001_PCB_TOML: &str = r#"
[board]
name = "TB0001"
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Layout(name="TB0001", path="build/TB0001")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_publish_board_invalid_path() {
    let mut sb = Sandbox::new();
    let output = sb
        .write("pcb.toml", PCB_TOML)
        .write("boards/Test/pcb.toml", BOARD_TB0001_PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .snapshot_run(
            "pcbc",
            [
                "publish",
                "boards/NonExistent.zen",
                "--bump=minor",
                "--no-push",
                "--force",
            ],
        );
    assert_snapshot!("publish_board_invalid_path", output);
}

#[test]
fn test_publish_board_with_patches() {
    let mut sb = Sandbox::new();
    let output = sb
        .write("pcb.toml", PCB_TOML_WITH_PATCH)
        .write("boards/Test/pcb.toml", BOARD_TB0001_PCB_TOML)
        .write("boards/Test/TB0001.zen", SIMPLE_BOARD_ZEN)
        .snapshot_run(
            "pcbc",
            [
                "publish",
                "boards/Test/TB0001.zen",
                "--bump=minor",
                "--no-push",
                "--force",
            ],
        );
    assert_snapshot!("publish_board_with_patches", output);
}

#[test]
fn test_publish_check_reports_package_versions() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", PCB_TOML)
        .write("modules/Foo/pcb.toml", "")
        .write("modules/Foo/Foo.zen", "P1 = io(Net)\n")
        .init_git()
        .commit("feat: add Foo")
        .tag("modules/Foo/v0.2.0")
        .write("modules/Foo/Foo.zen", "P1 = io(Net)\nP2 = io(Net)\n")
        .write("modules/Bar/pcb.toml", "")
        .write("modules/Bar/Bar.zen", "P1 = io(Net)\n")
        .commit("feat: add Bar, extend Foo");

    let output = sb
        .run("pcbc", ["publish", "--check"])
        .stdout_capture()
        .stderr_null()
        .run()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({ "packages": [
            { "path": "modules/Bar", "current": null, "version": "0.1.0", "tag": "modules/Bar/v0.1.0" },
            { "path": "modules/Foo", "current": "0.2.0", "version": "0.2.1", "tag": "modules/Foo/v0.2.1" },
        ]})
    );
    assert_eq!(sb.cmd("git", ["tag"]).read().unwrap(), "modules/Foo/v0.2.0");
}
