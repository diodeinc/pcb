//! Tests for `pcb sync` dependency hydration (the v2 reconcile/auto-dep flow).
//!
//! These tests verify that `pcb sync` reconciles a workspace's source imports and
//! hydrates its `pcb.toml` manifests with both direct `[dependencies]` and the
//! lane-qualified `[dependencies.indirect]` closure. Branch dependencies are pinned
//! to a pseudo-version in the manifest; no `pcb.sum` lockfile is produced.
//!
//! Note: @stdlib remains implicit; other aliases require explicit dependencies.

#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::{FixtureRepo, Sandbox};

const PCB_TOML: &str = r#"[workspace]
pcb-version = "0.3"
"#;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config(str, default = "10kOhm")

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    type = "resistor",
    properties = {"value": value},
)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const BOARD_USING_SIMPLE_RESISTOR: &str = r#"
SimpleResistor = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

fn write_simple_resistor_package(repo: &mut FixtureRepo, module_source: &str) {
    repo.write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", module_source)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD);
}

/// Test that @stdlib does NOT add a dependency to pcb.toml (toolchain provides it implicitly)
#[test]
fn test_auto_deps_stdlib() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"load("@stdlib/units.zen", "kOhm")

x = kOhm(10)
"#;

    // Run build (will fail due to missing dep, but pcb.toml should be modified)
    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    // Verify pcb.toml was updated with the dependency
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_stdlib_pcb_toml", pcb_toml_content);
}

/// Test that @kicad-symbols alias auto-adds the configured KiCad symbols dependency.
#[test]
fn test_auto_deps_kicad_symbols() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"# Reference a KiCad symbol (this triggers auto-dep detection)
symbol_path = "@kicad-symbols/Device.kicad_sym:R"
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_symbols_pcb_toml", pcb_toml_content);
}

/// Test that @kicad-footprints alias auto-adds the configured KiCad footprints dependency.
#[test]
fn test_auto_deps_kicad_footprints() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"# Reference a KiCad footprint (this triggers auto-dep detection)
footprint_path = "@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_footprints_pcb_toml", pcb_toml_content);
}

/// Test that multiple auto-deps are added together correctly to pcb.toml
#[test]
fn test_auto_deps_multiple() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"load("@stdlib/units.zen", "kOhm", "pF")

# Use stdlib and kicad aliases
resistance = kOhm(10)
capacitance = pF(100)
symbol_path = "@kicad-symbols/Device.kicad_sym:R"
footprint_path = "@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_multiple_pcb_toml", pcb_toml_content);
}

/// Test that dynamic kicad alias paths still register the base dependency.
#[test]
fn test_auto_deps_kicad_dynamic_path() {
    let mut sandbox = Sandbox::new();

    // Dynamic footprint path still carries a resolvable alias.
    let zen_content = r#"footprint_template = "@kicad-footprints/Resistor_SMD.pretty/R_{size}.kicad_mod"
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_kicad_dynamic_path_pcb_toml", pcb_toml_content);
}

#[test]
fn test_branch_dep_gets_pinned_to_rev_and_builds() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add SimpleResistor package")
        .push_mirror();
    let head_rev = fixture.rev_parse_head();

    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = { branch = "main" }
"#;

    sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync();

    // `pcb sync` resolves the branch to its HEAD commit and pins the dependency to a
    // pseudo-version in the hydrated manifest — the v2 replacement for pcb.sum.
    let pinned_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    let pseudo_version = pinned_toml
        .split_once("SimpleResistor\" = \"")
        .and_then(|(_, rest)| rest.split('"').next())
        .expect("hydrated manifest should pin SimpleResistor")
        .to_string();
    assert!(
        pseudo_version.ends_with(&head_rev),
        "expected pseudo-version {pseudo_version} to embed the branch HEAD rev {head_rev}"
    );
    assert!(
        pseudo_version.starts_with("0.1.1-0."),
        "expected unpublished branch dep in the 0.1.1 pseudo-version family, got {pseudo_version}"
    );
    assert!(
        !sandbox.default_cwd().join("pcb.sum").exists(),
        "v2 hydration must not create a pcb.sum lockfile"
    );

    let output = sandbox.snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );

    let snapshot = sandbox.sanitize_output(&format!(
        "--- pcb.toml ---\n{}\n",
        pinned_toml.replace(&pseudo_version, "<PSEUDO_VERSION>")
    ));
    assert_snapshot!("auto_deps_branch_dep_pins_rev_and_builds", snapshot);
}

#[test]
fn test_local_only_workspace_needs_no_lockfile() {
    let mut sandbox = Sandbox::new();

    let output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write(
            "board.zen",
            r#"
Layout(name="LocalOnly", path="build/LocalOnly", bom_profile=None)

vcc = Net("VCC")
gnd = Net("GND")
"#,
        )
        .sync()
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );

    // A workspace with no external dependencies hydrates to nothing and, under MVS
    // v2, never produces a pcb.sum lockfile.
    assert!(
        !sandbox.default_cwd().join("pcb.sum").exists(),
        "local-only v2 workspace should not create a pcb.sum lockfile"
    );
}

#[test]
fn test_locked_build_without_pcb_sum() {
    let mut sandbox = Sandbox::new();

    let output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write(
            "board.zen",
            r#"
Layout(name="LocalOnly", path="build/LocalOnly", bom_profile=None)

vcc = Net("VCC")
gnd = Net("GND")
"#,
        )
        .snapshot_run("pcbc", ["build", "board.zen", "--locked"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected locked build to succeed without pcb.sum:\n{output}"
    );

    let pcb_sum_path = sandbox.default_cwd().join("pcb.sum");
    assert!(
        !pcb_sum_path.exists(),
        "expected locked build without pcb.sum to leave it absent"
    );
}

/// Test that a relative path load("../../modules/Lib/Lib.zen") that escapes a board's
/// package boundary into another workspace package triggers auto-dep for that package.
#[test]
fn test_auto_deps_relative_path_cross_package() {
    let mut sandbox = Sandbox::new();

    let workspace_toml = r#"[workspace]
pcb-version = "0.3"
"#;

    let lib_toml = "[dependencies]\n";
    let lib_zen = r#"
P1 = io(Net)
P2 = io(Net)
"#;

    // Board loads the library module via a relative path that escapes the board package
    let board_toml = r#"[board]
name = "Main"
path = "Main.zen"
"#;
    let board_zen = r#"
load("../../modules/Lib/Lib.zen", "P1", "P2")

vcc = Net("VCC")
gnd = Net("GND")
"#;

    sandbox
        .write("pcb.toml", workspace_toml)
        .write("modules/Lib/pcb.toml", lib_toml)
        .write("modules/Lib/Lib.zen", lib_zen)
        .write("boards/Main/pcb.toml", board_toml)
        .write("boards/Main/Main.zen", board_zen)
        .sync();

    // The board's pcb.toml should now contain a dependency on the Lib package.
    let board_pcb_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("boards/Main/pcb.toml"))
            .unwrap_or_default();
    assert!(
        board_pcb_toml.contains("modules/Lib"),
        "expected board pcb.toml to contain auto-dep on modules/Lib package, got:\n{}",
        board_pcb_toml
    );
}

#[test]
fn test_same_package_url_rejected() {
    for locked in [false, true] {
        let mut sandbox = Sandbox::new();

        let cmd = if locked {
            vec!["build", "boards/Main/Main.zen", "--locked"]
        } else {
            vec!["build", "boards/Main/Main.zen"]
        };

        let result = sandbox
            .write(
                "pcb.toml",
                r#"[workspace]
pcb-version = "0.3"
repository = "github.com/example/demo"
"#,
            )
            .write(
                "boards/Main/pcb.toml",
                r#"[board]
name = "Main"
path = "Main.zen"
"#,
            )
            .write(
                "boards/Main/Main.zen",
                r#"
Child = Module("github.com/example/demo/boards/Main/src/Child.zen")

Child(name = "X", P1 = Net("P1"))
"#,
            )
            .write(
                "boards/Main/src/Child.zen",
                r#"
P1 = io(Net)
"#,
            )
            .run("pcbc", cmd)
            .stderr_capture()
            .stdout_capture()
            .unchecked()
            .run()
            .expect("same-package URL run should execute");

        let output = format!(
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr),
        );
        assert!(
            !result.status.success(),
            "expected build to fail:\n{output}"
        );
        assert!(
            output.contains("use a relative path instead"),
            "expected relative-path guidance, got:\n{output}"
        );

        let board_pcb_toml =
            std::fs::read_to_string(sandbox.default_cwd().join("boards/Main/pcb.toml"))
                .unwrap_or_default();
        assert!(
            !board_pcb_toml.contains("\"github.com/example/demo/boards/Main\""),
            "expected build to avoid self-dependency, got:\n{}",
            board_pcb_toml
        );
    }
}

#[test]
fn test_root_package_url_to_package_auto_dep() {
    let mut sandbox = Sandbox::new();

    let output = sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.3"
repository = "github.com/example/demo"

[dependencies]
"github.com/example/demo/libs/Helper" = "0.1.0"
"#,
        )
        .write(
            "board.zen",
            r#"Child = Module("github.com/example/demo/boards/Child/Child.zen")

Child(name = "X", P1 = Net("P1"))
"#,
        )
        .write("boards/Child/pcb.toml", "[dependencies]\n")
        .write(
            "boards/Child/Child.zen",
            r#"
P1 = io(Net)
"#,
        )
        .write("libs/Helper/pcb.toml", "[dependencies]\n")
        .write("libs/Helper/Helper.zen", "P1 = io(\"P1\", Net)\n")
        .sync()
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected root package build to succeed:\n{output}"
    );

    let root_pcb_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(
        root_pcb_toml.contains("\"github.com/example/demo/boards/Child\""),
        "expected root pcb.toml to gain package dependency, got:\n{}",
        root_pcb_toml
    );
}

#[test]
fn test_sync_preserves_pinned_dependency_version() {
    let mut sandbox = Sandbox::new();

    // A remote package published at v1.2.3.
    sandbox
        .git_fixture("https://github.com/example/components.git")
        .write("Helper/pcb.toml", "[dependencies]\n")
        .write("Helper/Helper.zen", "P1 = io(\"P1\", Net)\n")
        .commit("Add Helper")
        .tag("Helper/v1.2.3", false)
        .push_mirror();

    let output = sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/example/components/Helper" = "1.2.3"
"#,
        )
        .write(
            "board.zen",
            r#"Helper = Module("github.com/example/components/Helper/Helper.zen")

Helper(name = "X", P1 = Net("P1"))
"#,
        )
        .sync()
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );

    // sync keeps the pinned version as-is rather than re-resolving it.
    let root_pcb_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(
        root_pcb_toml.contains("\"github.com/example/components/Helper\" = \"1.2.3\""),
        "expected pinned version to be preserved, got:\n{}",
        root_pcb_toml
    );
}

#[test]
fn test_root_package_url_to_package_locked() {
    let mut sandbox = Sandbox::new();

    let result = sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.3"
repository = "github.com/example/demo"

[dependencies]
"github.com/example/demo/boards/Child" = "0.1.0"
"#,
        )
        .write(
            "board.zen",
            r#"Child = Module("github.com/example/demo/boards/Child/Child.zen")

Child(name = "X", P1 = Net("P1"))
"#,
        )
        .write("boards/Child/pcb.toml", "[dependencies]\n")
        .write(
            "boards/Child/Child.zen",
            r#"
P1 = io(Net)
"#,
        )
        .sync()
        .run("pcbc", ["build", "board.zen", "--locked"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("locked root package run should execute");

    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert!(
        result.status.success(),
        "expected locked root package build to succeed:\n{output}"
    );
}

#[test]
fn test_branch_only_dep_rejected_in_locked_and_offline() {
    let mut sandbox = Sandbox::new();

    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = { branch = "main" }
"#;

    let locked_result = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", "x = 1\n")
        .run("pcbc", ["build", "board.zen", "--locked"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("locked run should execute");
    let locked_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&locked_result.stdout),
        String::from_utf8_lossy(&locked_result.stderr),
    );
    assert!(
        !locked_result.status.success(),
        "expected locked build to fail"
    );
    assert!(locked_output.contains("without rev, which is not reproducible in --locked mode."));

    let offline_result = sandbox
        .run("pcbc", ["build", "board.zen", "--offline"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("offline run should execute");
    let offline_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&offline_result.stdout),
        String::from_utf8_lossy(&offline_result.stderr),
    );
    assert!(
        !offline_result.status.success(),
        "expected offline build to fail"
    );
    assert!(offline_output.contains("without rev, which is not reproducible in --offline mode."));
}

#[test]
fn test_locked_ignores_kicad_entries_in_lockfile() {
    let mut sandbox = Sandbox::new();

    let pcb_sum = r#"gitlab.com/kicad/libraries/kicad-symbols 9.0.3 h1:legacy
"#;

    let result = sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", "x = 1\n")
        .write("pcb.sum", pcb_sum)
        .run("pcbc", ["build", "board.zen", "--locked"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("locked run should execute");

    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );
    assert!(
        result.status.success(),
        "expected locked build to succeed:\n{output}"
    );
}

#[test]
fn test_branch_plus_rev_uses_rev_when_branch_moves() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("v1")
        .push_mirror();
    let rev1 = fixture.rev_parse_head();

    fixture
        .write(
            "SimpleResistor/SimpleResistor.zen",
            "this is intentionally invalid starlark\n",
        )
        .commit("break main")
        .push_mirror();
    let rev2 = fixture.rev_parse_head();
    assert_ne!(rev1, rev2);

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"#,
        rev1
    );

    sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync();

    // The pinned rev is honoured even though `main` has since moved to a broken commit:
    // sync resolves to rev1's pseudo-version and the build succeeds against it.
    let pinned_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    let pseudo_version = pinned_toml
        .split_once("SimpleResistor\" = \"")
        .and_then(|(_, rest)| rest.split('"').next())
        .expect("hydrated manifest should pin SimpleResistor")
        .to_string();
    assert!(
        pseudo_version.ends_with(&rev1),
        "expected pseudo-version {pseudo_version} to use the pinned rev {rev1}"
    );
    assert!(
        !pseudo_version.ends_with(&rev2),
        "expected pseudo-version to ignore the moved branch head"
    );

    let output = sandbox.snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );
}

#[test]
fn test_branch_pinning_is_idempotent() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add SimpleResistor package")
        .push_mirror();

    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = { branch = "main" }
"#;

    sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync();
    let first_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();

    // A second sync must leave the hydrated manifest byte-for-byte identical.
    sandbox.sync();
    let second_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();

    assert_eq!(
        first_toml, second_toml,
        "expected hydrated pcb.toml to be stable across syncs"
    );
    assert!(
        !sandbox.default_cwd().join("pcb.sum").exists(),
        "v2 hydration must not create a pcb.sum lockfile"
    );
}

/// `pcb update` is a legacy (pcb.sum) command. Once a workspace is hydrated to MVS
/// v2 it is rejected and points the user at `pcb add -u`.
#[test]
fn test_update_rejected_on_hydrated_v2_workspace() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("v1")
        .push_mirror();
    let rev = fixture.rev_parse_head();

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{rev}" }}
"#
    );

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync()
        .snapshot_run("pcbc", ["update"]);

    assert!(
        !output.contains("Exit Code: 0"),
        "expected `pcb update` to be rejected on a hydrated workspace:\n{output}"
    );
    assert!(
        output.contains("`pcb update` is for legacy dependency manifests"),
        "expected legacy-manifest rejection message:\n{output}"
    );
    assert!(
        output.contains("Use `pcb add -u`"),
        "expected the rejection to point at `pcb add -u`:\n{output}"
    );
}

#[test]
fn test_covered_import_skips_unknown_remote_url_warning() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    write_simple_resistor_package(&mut fixture, SIMPLE_RESISTOR_ZEN);
    fixture.commit("v1").push_mirror();
    let rev = fixture.rev_parse_head();

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"#,
        rev
    );

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync()
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );
    assert!(
        !output.contains("unknown remote URLs"),
        "expected covered import to skip unknown-url warning:\n{output}"
    );
    assert!(
        !output.contains("Failed to discover package"),
        "expected covered import to skip remote discovery warning:\n{output}"
    );
}
