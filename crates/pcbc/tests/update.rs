#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;

#[test]
fn update_rejects_v2_package_scope() {
    let output = Sandbox::new()
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.3"
repository = "github.com/example/project"
members = ["boards/*"]
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
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
        )
        .write("boards/Board/Board.zen", "p1 = Net(\"P1\")\n")
        .snapshot_run("pcbc", ["update", "boards/Board"]);

    assert!(
        output.contains("Exit Code: 1"),
        "expected update to fail:\n{output}"
    );
    assert!(
        output.contains("`pcb update` is for legacy dependency manifests"),
        "expected legacy-only explanation:\n{output}"
    );
    assert!(
        output.contains("Use `pcb add -u` from the package directory"),
        "expected V2 replacement command:\n{output}"
    );
}

#[test]
fn update_rejects_v2_workspace_scope() {
    let output = Sandbox::new()
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.3"
repository = "github.com/example/project"
members = ["modules/*"]
"#,
        )
        .write(
            "modules/Driver/pcb.toml",
            r#"
[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"

[dependencies.indirect]
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
        )
        .write("modules/Driver/Driver.zen", "p1 = Net(\"P1\")\n")
        .snapshot_run("pcbc", ["update"]);

    assert!(
        output.contains("Exit Code: 1"),
        "expected update to fail:\n{output}"
    );
    assert!(
        output.contains("package github.com/example/project/modules/Driver")
            || output.contains("V2 packages"),
        "expected offending package in error:\n{output}"
    );
}
