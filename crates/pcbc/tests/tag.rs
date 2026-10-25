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
    sb.write(
        "pcb.toml",
        "[workspace]\npcb-version = \"0.4\"\nrepository = \"github.com/acme/registry\"\n",
    )
    .write("modules/Foo/pcb.toml", "")
    .write("modules/Foo/Foo.zen", "P1 = io(Net)\n")
    .init_git()
    .commit("feat: add Foo")
    .tag("modules/Foo/v0.2.0")
    .write("modules/Foo/Foo.zen", "P1 = io(Net)\nP2 = io(Net)\n")
    .write(
        "modules/Bar/pcb.toml",
        "[dependencies]\n\"github.com/acme/registry/modules/Foo\" = \"0.2.0\"\n",
    )
    .write(
        "modules/Bar/Bar.zen",
        "Foo = Module(\"github.com/acme/registry/modules/Foo/Foo.zen\")\nP1 = io(Net)\n",
    )
    .commit("feat: add Bar, extend Foo");
    let output = sb
        .run("pcbc", ["publish", "--check"])
        .stdout_capture()
        .stderr_null()
        .run()
        .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // Bar's bump commit pins the Foo version that the first wave tags.
    assert_eq!(
        report,
        serde_json::json!({
            "packages": [
                {
                    "path": "modules/Foo", "current": "0.2.0", "version": "0.2.1", "tag": "modules/Foo/v0.2.1",
                    "message": "github.com/acme/registry/modules/Foo v0.2.1",
                },
                {
                    "path": "modules/Bar", "current": null, "version": "0.1.0", "tag": "modules/Bar/v0.1.0",
                    "message": "github.com/acme/registry/modules/Bar v0.1.0",
                },
            ],
            "waves": [
                { "manifests": {}, "tags": ["modules/Foo/v0.2.1"] },
                {
                    "manifests": {
                        "modules/Bar/pcb.toml": "[dependencies]\n\"github.com/acme/registry/modules/Foo\" = \"0.2.1\"\n",
                    },
                    "tags": ["modules/Bar/v0.1.0"],
                },
            ],
        })
    );
    assert_eq!(sb.cmd("git", ["tag"]).read().unwrap(), "modules/Foo/v0.2.0");
}

#[test]
fn test_publish_check_netlist_dir() {
    let mut sb = Sandbox::new();
    sb.write(
        "pcb.toml",
        "[workspace]\npcb-version = \"0.4\"\nrepository = \"github.com/acme/registry\"\n",
    )
    .write("modules/Foo/pcb.toml", "")
    .write("modules/Foo/Foo.zen", "P1 = io(Net)\n")
    .write(
        "modules/Bar/pcb.toml",
        "[dependencies]\n\"github.com/acme/registry/modules/Foo\" = \"0.2.0\"\n",
    )
    .write(
        "modules/Bar/Bar.zen",
        "Foo = Module(\"github.com/acme/registry/modules/Foo/Foo.zen\")\nFoo(name = \"U1\", P1 = Net(\"N1\"))\n",
    )
    .write("modules/Baz/pcb.toml", "")
    .write("modules/Baz/Baz.zen", "P1 = io(Net)\n")
    .init_git()
    .commit("feat: add packages")
    .tag("modules/Foo/v0.2.0")
    .tag("modules/Bar/v0.1.0")
    .tag("modules/Baz/v0.1.0")
    .write("modules/Foo/Foo.zen", "P1 = io(Net)\nInternal = Net(\"INT\")\n")
    .commit("feat: extend Foo");

    let plan = sb
        .run("pcbc", ["publish", "--check"])
        .stdout_capture()
        .stderr_null()
        .run()
        .unwrap()
        .stdout;
    let with_netlists = sb
        .run("pcbc", ["publish", "--check", "--netlist-dir", "out"])
        .stdout_capture()
        .stderr_null()
        .run()
        .unwrap()
        .stdout;
    assert_eq!(
        String::from_utf8(with_netlists).unwrap(),
        String::from_utf8(plan).unwrap()
    );

    let out = sb.root_path().join("out");
    for zen in ["modules/Foo/Foo.zen", "modules/Bar/Bar.zen"] {
        let path = out.join(format!("{zen}.json"));
        let netlist: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(netlist["instances"].is_object(), "{}", path.display());
    }
    let bar = std::fs::read_to_string(out.join("modules/Bar/Bar.zen.json")).unwrap();
    assert!(bar.contains("N1"));
    assert!(!out.join("modules/Baz").exists());

    // Reusing a non-empty output directory is rejected.
    let rerun = sb
        .run("pcbc", ["publish", "--check", "--netlist-dir", "out"])
        .stdout_null()
        .stderr_null()
        .unchecked()
        .run()
        .unwrap();
    assert!(!rerun.status.success());
}
