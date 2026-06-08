//! Tests for `pcb sync` dependency hydration.
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
use std::ffi::OsStr;
use std::process::Output;

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

fn seed_simple_resistor_repo(sandbox: &mut Sandbox, commit_message: &str) -> String {
    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    write_simple_resistor_package(&mut fixture, SIMPLE_RESISTOR_ZEN);
    fixture.commit(commit_message).push_mirror();
    fixture.rev_parse_head()
}

fn read_sandbox_file(sandbox: &Sandbox, rel: &str) -> String {
    std::fs::read_to_string(sandbox.default_cwd().join(rel)).unwrap_or_default()
}

fn read_root_manifest(sandbox: &Sandbox) -> String {
    read_sandbox_file(sandbox, "pcb.toml")
}

fn command_output(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn run_pcbc_unchecked<I>(sandbox: &mut Sandbox, args: I) -> Output
where
    I: IntoIterator,
    I::Item: AsRef<OsStr>,
{
    sandbox
        .run("pcbc", args)
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("pcbc command should execute")
}

fn hydrated_version(manifest: &str, package_name: &str) -> String {
    let needle = format!("{package_name}\" = \"");
    manifest
        .split_once(&needle)
        .and_then(|(_, rest)| rest.split('"').next())
        .expect("hydrated manifest should pin package")
        .to_string()
}

fn assert_no_pcb_sum(sandbox: &Sandbox) {
    assert!(
        !sandbox.default_cwd().join("pcb.sum").exists(),
        "dependency hydration must not create a pcb.sum lockfile"
    );
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
    assert_snapshot!("auto_deps_stdlib_pcb_toml", read_root_manifest(&sandbox));
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

    assert_snapshot!(
        "auto_deps_kicad_symbols_pcb_toml",
        read_root_manifest(&sandbox)
    );
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

    assert_snapshot!(
        "auto_deps_kicad_footprints_pcb_toml",
        read_root_manifest(&sandbox)
    );
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

    assert_snapshot!("auto_deps_multiple_pcb_toml", read_root_manifest(&sandbox));
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

    assert_snapshot!(
        "auto_deps_kicad_dynamic_path_pcb_toml",
        read_root_manifest(&sandbox)
    );
}

#[test]
fn test_branch_dep_gets_pinned_to_rev_and_builds() {
    let mut sandbox = Sandbox::new();

    let head_rev = seed_simple_resistor_repo(&mut sandbox, "Add SimpleResistor package");

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
    // pseudo-version in the hydrated manifest.
    let pinned_toml = read_root_manifest(&sandbox);
    let pseudo_version = hydrated_version(&pinned_toml, "SimpleResistor");
    assert!(
        pseudo_version.ends_with(&head_rev),
        "expected pseudo-version {pseudo_version} to embed the branch HEAD rev {head_rev}"
    );
    assert!(
        pseudo_version.starts_with("0.1.1-0."),
        "expected unpublished branch dep in the 0.1.1 pseudo-version family, got {pseudo_version}"
    );
    assert_no_pcb_sum(&sandbox);

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

    // A workspace with no external dependencies hydrates to nothing and never
    // produces a pcb.sum lockfile.
    assert_no_pcb_sum(&sandbox);
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

    assert_no_pcb_sum(&sandbox);
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
    let board_pcb_toml = read_sandbox_file(&sandbox, "boards/Main/pcb.toml");
    assert!(
        board_pcb_toml.contains("modules/Lib"),
        "expected board pcb.toml to contain auto-dep on modules/Lib package, got:\n{}",
        board_pcb_toml
    );
}

#[test]
fn test_same_package_url_rejected() {
    let mut sandbox = Sandbox::new();

    sandbox
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
        );

    let result = run_pcbc_unchecked(&mut sandbox, ["sync"]);
    let output = command_output(&result);
    assert!(!result.status.success(), "expected sync to fail:\n{output}");
    assert!(
        output.contains("use a relative path instead"),
        "expected relative-path guidance, got:\n{output}"
    );

    let board_pcb_toml = read_sandbox_file(&sandbox, "boards/Main/pcb.toml");
    assert!(
        !board_pcb_toml.contains("\"github.com/example/demo/boards/Main\""),
        "expected sync to avoid self-dependency, got:\n{}",
        board_pcb_toml
    );
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

    let root_pcb_toml = read_root_manifest(&sandbox);
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
    let root_pcb_toml = read_root_manifest(&sandbox);
    assert!(
        root_pcb_toml.contains("\"github.com/example/components/Helper\" = \"1.2.3\""),
        "expected pinned version to be preserved, got:\n{}",
        root_pcb_toml
    );
}

#[test]
fn test_root_package_url_to_package_locked() {
    let mut sandbox = Sandbox::new();

    sandbox
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
        .sync();

    let result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen", "--locked"]);
    let output = command_output(&result);
    assert!(
        result.status.success(),
        "expected locked root package build to succeed:\n{output}"
    );
}

#[test]
fn test_branch_only_dep_hydrates_before_locked_and_offline() {
    let mut sandbox = Sandbox::new();

    let head_rev = seed_simple_resistor_repo(&mut sandbox, "Add SimpleResistor package");

    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = { branch = "main" }
"#;

    sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .sync();

    let hydrated_toml = read_root_manifest(&sandbox);
    let pseudo_version = hydrated_version(&hydrated_toml, "SimpleResistor");
    assert!(
        pseudo_version.ends_with(&head_rev),
        "expected pseudo-version {pseudo_version} to embed branch HEAD {head_rev}"
    );
    assert_no_pcb_sum(&sandbox);

    let locked_result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen", "--locked"]);
    let locked_output = command_output(&locked_result);
    assert!(
        locked_result.status.success(),
        "expected locked build to use hydrated pseudo-version:\n{locked_output}"
    );

    let offline_result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen", "--offline"]);
    let offline_output = command_output(&offline_result);
    assert!(
        offline_result.status.success(),
        "expected offline build to use cached hydrated pseudo-version:\n{offline_output}"
    );
}

#[test]
fn test_locked_ignores_kicad_entries_in_lockfile() {
    let mut sandbox = Sandbox::new();

    let pcb_sum = r#"gitlab.com/kicad/libraries/kicad-symbols 9.0.3 h1:legacy
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", "x = 1\n")
        .write("pcb.sum", pcb_sum);
    let result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen", "--locked"]);
    let output = command_output(&result);
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
    let pinned_toml = read_root_manifest(&sandbox);
    let pseudo_version = hydrated_version(&pinned_toml, "SimpleResistor");
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
    let first_toml = read_root_manifest(&sandbox);

    // A second sync must leave the hydrated manifest byte-for-byte identical.
    sandbox.sync();
    let second_toml = read_root_manifest(&sandbox);

    assert_eq!(
        first_toml, second_toml,
        "expected hydrated pcb.toml to be stable across syncs"
    );
    assert_no_pcb_sum(&sandbox);
}

/// `pcb update` is a legacy (pcb.sum) command. Hydrated workspaces are rejected
/// and pointed at `pcb add -u`.
#[test]
fn test_update_rejected_on_hydrated_workspace() {
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
