//! Tests for `pcb sync` dependency hydration.
//!
//! These tests verify that `pcb sync` reconciles a workspace's source imports and
//! hydrates its `pcb.toml` manifests with both direct `[dependencies]` and the
//! lane-qualified `[dependencies.indirect]` closure. Branch dependencies are pinned
//! to a pseudo-version in the manifest.
//!
//! Note: @stdlib remains implicit.

#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::{FixtureRepo, Sandbox};
use std::ffi::OsStr;
use std::process::Output;

const PCB_TOML: &str = r#"[workspace]
pcb-version = "0.4"
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

fn seed_simple_resistor_repo(
    sandbox: &mut Sandbox,
    commit_message: &str,
    tag: Option<&str>,
) -> String {
    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    write_simple_resistor_package(&mut fixture, SIMPLE_RESISTOR_ZEN);
    fixture.commit(commit_message);
    if let Some(tag) = tag {
        fixture.tag(tag, false);
    }
    fixture.push_mirror();
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

fn run_sync_check(sandbox: &mut Sandbox) -> Output {
    run_pcbc_unchecked(sandbox, ["sync", "--check"])
}

fn hydrated_version(manifest: &str, package_name: &str) -> String {
    let needle = format!("{package_name}\" = \"");
    manifest
        .split_once(&needle)
        .and_then(|(_, rest)| rest.split('"').next())
        .expect("hydrated manifest should pin package")
        .to_string()
}

fn write_local_lib_workspace(sandbox: &mut Sandbox) -> &mut Sandbox {
    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
repository = "github.com/example/demo"

[board]
name = "Main"
path = "board.zen"
"#,
        )
        .write(
            "board.zen",
            r#"
vcc = Net("VCC")
gnd = Net("GND")
"#,
        )
        .write("modules/Lib/pcb.toml", "[dependencies]\n")
        .write(
            "modules/Lib/Lib.zen",
            r#"
P1 = io(Net)
"#,
        )
}

fn add_unsynced_local_lib_import(sandbox: &mut Sandbox) {
    sandbox.write(
        "board.zen",
        r#"
Lib = Module("github.com/example/demo/modules/Lib/Lib.zen")

Lib(name = "X", P1 = Net("P1"))
"#,
    );
}

fn write_vendored_simple_resistor_workspace(sandbox: &mut Sandbox) -> &mut Sandbox {
    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
vendor = ["github.com/mycompany/components/**"]

[dependencies]
"github.com/mycompany/components/SimpleResistor" = "1.0.0"
"#,
        )
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
}

#[test]
fn test_sync_keeps_stdlib_implicit() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"load("@stdlib/units.zen", "kOhm")

x = kOhm(10)
"#;

    sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .sync();

    assert_eq!(read_root_manifest(&sandbox), PCB_TOML);
}

#[test]
fn test_sync_pins_branch_dep_to_rev_and_builds() {
    let mut sandbox = Sandbox::new();

    let head_rev = seed_simple_resistor_repo(&mut sandbox, "Add SimpleResistor package", None);

    let pcb_toml = r#"[workspace]
pcb-version = "0.4"

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
    let output = sandbox.snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );

    assert!(pinned_toml.contains(&format!(
        "\"github.com/mycompany/components/SimpleResistor\" = \"{pseudo_version}\""
    )));
}

#[test]
fn test_sync_local_only_workspace_builds() {
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
}

#[test]
fn test_sync_preserves_empty_package_manifest() {
    let mut sandbox = Sandbox::new();

    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
repository = "github.com/example/demo"
"#,
        )
        .write("modules/Empty/pcb.toml", "")
        .write("modules/Empty/Empty.zen", "P1 = io(Net)\n")
        .sync();

    assert_eq!(read_sandbox_file(&sandbox, "modules/Empty/pcb.toml"), "");
}

#[test]
fn test_sync_adds_workspace_dependency_for_cross_package_relative_load() {
    let mut sandbox = Sandbox::new();

    let workspace_toml = r#"[workspace]
pcb-version = "0.4"
"#;

    let lib_toml = "[dependencies]\n";
    let lib_zen = r#"
P1 = io(Net)
P2 = io(Net)
"#;

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

    let board_pcb_toml = read_sandbox_file(&sandbox, "boards/Main/pcb.toml");
    assert!(
        board_pcb_toml.contains("modules/Lib"),
        "expected board pcb.toml to contain dependency on modules/Lib package, got:\n{}",
        board_pcb_toml
    );
}

#[test]
fn test_build_rejects_unsynced_cross_package_relative_load() {
    let mut sandbox = Sandbox::new();

    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
repository = "github.com/example/demo"

[board]
name = "Main"
path = "board.zen"
"#,
        )
        .write(
            "board.zen",
            r#"
load("modules/Lib/Lib.zen", "Marker")

check(Marker == "ok", "loaded marker")
"#,
        )
        .write("modules/Lib/pcb.toml", "[dependencies]\n")
        .write(
            "modules/Lib/Lib.zen",
            r#"
Marker = "ok"
"#,
        );

    let build_before_sync = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen"]);
    let output_before_sync = command_output(&build_before_sync);
    assert!(
        !build_before_sync.status.success(),
        "expected unsynced build to fail:\n{output_before_sync}"
    );
    assert!(
        output_before_sync.contains("Run `pcb sync`"),
        "expected build to point at pcb sync:\n{output_before_sync}"
    );

    sandbox.run("pcbc", ["sync"]).run().unwrap();

    let build_after_sync = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen"]);
    let output_after_sync = command_output(&build_after_sync);
    assert!(
        build_after_sync.status.success(),
        "expected synced build to succeed:\n{output_after_sync}"
    );
}

#[test]
fn test_same_package_url_rejected() {
    let mut sandbox = Sandbox::new();

    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
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
fn test_sync_adds_root_workspace_package_dependency() {
    let mut sandbox = Sandbox::new();

    let output = sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
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

/// Workspace with a `libs/Helper` package tagged `v1.2.3`; returns the board
/// pin for Helper after syncing a board whose manifest ends with `board_deps`.
fn sync_tagged_helper_workspace(board_deps: &str) -> String {
    let mut sandbox = Sandbox::new();
    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
repository = "github.com/example/demo"
"#,
        )
        .write("libs/Helper/pcb.toml", "[dependencies]\n")
        .write("libs/Helper/Helper.zen", "P1 = io(Net)\n")
        .write(
            "boards/Main/pcb.toml",
            format!("[board]\nname = \"Main\"\npath = \"Main.zen\"\n{board_deps}"),
        )
        .write(
            "boards/Main/Main.zen",
            r#"
Helper = Module("github.com/example/demo/libs/Helper/Helper.zen")

Helper(name = "H", P1 = Net("P1"))
"#,
        )
        .init_git()
        .commit("init")
        .tag("libs/Helper/v1.2.3")
        .sync();

    let board_pcb_toml = read_sandbox_file(&sandbox, "boards/Main/pcb.toml");
    hydrated_version(&board_pcb_toml, "libs/Helper")
}

#[test]
fn test_sync_pins_workspace_dependency_to_latest_tag() {
    assert_eq!(sync_tagged_helper_workspace(""), "1.2.3");
}

#[test]
fn test_sync_never_downgrades_workspace_pin() {
    // A pin newer than the local tags means the tags simply haven't been
    // fetched; sync must keep the pin rather than downgrade it.
    let pin = sync_tagged_helper_workspace(
        "\n[dependencies]\n\"github.com/example/demo/libs/Helper\" = \"2.0.0\"\n",
    );
    assert_eq!(pin, "2.0.0");
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
pcb-version = "0.4"

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
fn test_root_package_url_to_package_read_only() {
    let mut sandbox = Sandbox::new();

    sandbox
        .write(
            "pcb.toml",
            r#"[workspace]
pcb-version = "0.4"
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

    let result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen"]);
    let output = command_output(&result);
    assert!(
        result.status.success(),
        "expected root package build to succeed:\n{output}"
    );
}

#[test]
fn test_branch_only_dep_hydrates_before_read_only_and_offline() {
    let mut sandbox = Sandbox::new();

    let head_rev = seed_simple_resistor_repo(&mut sandbox, "Add SimpleResistor package", None);

    let pcb_toml = r#"[workspace]
pcb-version = "0.4"

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
    let build_result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen"]);
    let build_output = command_output(&build_result);
    assert!(
        build_result.status.success(),
        "expected build to use hydrated pseudo-version:\n{build_output}"
    );

    let offline_result = run_pcbc_unchecked(&mut sandbox, ["build", "board.zen", "--offline"]);
    let offline_output = command_output(&offline_result);
    assert!(
        offline_result.status.success(),
        "expected offline build to use cached hydrated pseudo-version:\n{offline_output}"
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
pcb-version = "0.4"

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
pcb-version = "0.4"

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
}

#[test]
fn test_sync_check_clean_workspace() {
    let mut sandbox = Sandbox::new();

    write_local_lib_workspace(&mut sandbox).sync();

    let before = sandbox.snapshot_dir(".");
    let result = run_sync_check(&mut sandbox);
    let output = command_output(&result);

    assert!(
        result.status.success(),
        "expected sync --check to pass:\n{output}"
    );
    assert!(
        output.trim().is_empty(),
        "expected sync --check success to be quiet:\n{output}"
    );
    assert_eq!(
        before,
        sandbox.snapshot_dir("."),
        "sync --check must not change a clean workspace"
    );
}

#[test]
fn test_sync_check_detects_manifest_drift() {
    let mut sandbox = Sandbox::new();

    write_local_lib_workspace(&mut sandbox).sync();
    let manifest_before = read_root_manifest(&sandbox);
    add_unsynced_local_lib_import(&mut sandbox);

    let result = run_sync_check(&mut sandbox);
    let output = command_output(&result);

    assert!(
        !result.status.success(),
        "expected sync --check to fail on manifest drift:\n{output}"
    );
    assert!(
        output.contains("would update pcb.toml"),
        "expected drift report to name pcb.toml:\n{output}"
    );
    assert!(
        output.contains("workspace is not synced; run `pcb sync`"),
        "expected final sync guidance:\n{output}"
    );
    assert_eq!(
        manifest_before,
        read_root_manifest(&sandbox),
        "sync --check must not rewrite the manifest"
    );

    sandbox.sync();
    let clean_result = run_sync_check(&mut sandbox);
    let clean_output = command_output(&clean_result);
    assert!(
        clean_result.status.success(),
        "expected sync --check to pass after sync:\n{clean_output}"
    );
}

#[test]
fn test_sync_check_detects_vendor_drift() {
    let mut sandbox = Sandbox::new();

    seed_simple_resistor_repo(
        &mut sandbox,
        "Add SimpleResistor package",
        Some("SimpleResistor/v1.0.0"),
    );
    write_vendored_simple_resistor_workspace(&mut sandbox).sync();

    let vendored = sandbox
        .default_cwd()
        .join("vendor/github.com/mycompany/components/SimpleResistor/1.0.0");
    assert!(vendored.exists(), "expected sync to vendor SimpleResistor");
    std::fs::remove_dir_all(&vendored).expect("remove vendored package");

    let missing_result = run_sync_check(&mut sandbox);
    let missing_output = command_output(&missing_result);
    assert!(
        !missing_result.status.success(),
        "expected sync --check to fail on missing vendored package:\n{missing_output}"
    );
    assert!(
        missing_output
            .contains("would vendor vendor/github.com/mycompany/components/SimpleResistor/1.0.0"),
        "expected missing vendor report:\n{missing_output}"
    );
    assert!(
        !vendored.exists(),
        "sync --check must not re-vendor the missing package"
    );

    sandbox.sync();
    let stale = sandbox
        .default_cwd()
        .join("vendor/github.com/mycompany/components/SimpleResistor/9.9.9");
    std::fs::create_dir_all(&stale).expect("create stale vendored package");
    std::fs::write(stale.join("pcb.toml"), "[dependencies]\n")
        .expect("write stale vendored manifest");

    let stale_result = run_sync_check(&mut sandbox);
    let stale_output = command_output(&stale_result);
    assert!(
        !stale_result.status.success(),
        "expected sync --check to fail on stale vendored package:\n{stale_output}"
    );
    assert!(
        stale_output
            .contains("would prune vendor/github.com/mycompany/components/SimpleResistor/9.9.9"),
        "expected stale vendor report:\n{stale_output}"
    );
    assert!(
        stale.exists(),
        "sync --check must not prune the stale package"
    );

    sandbox.sync();
    let clean_result = run_sync_check(&mut sandbox);
    let clean_output = command_output(&clean_result);
    assert!(
        clean_result.status.success(),
        "expected sync --check to pass after vendor sync:\n{clean_output}"
    );
}

#[test]
fn test_sync_check_from_subdirectory_checks_whole_workspace() {
    let mut sandbox = Sandbox::new();

    seed_simple_resistor_repo(
        &mut sandbox,
        "Add SimpleResistor package",
        Some("SimpleResistor/v1.0.0"),
    );
    write_vendored_simple_resistor_workspace(&mut sandbox).sync();

    let stale = sandbox
        .default_cwd()
        .join("vendor/github.com/mycompany/components/SimpleResistor/9.9.9");
    std::fs::create_dir_all(&stale).expect("create stale vendored package");
    std::fs::write(stale.join("pcb.toml"), "[dependencies]\n")
        .expect("write stale vendored manifest");

    sandbox.cwd("sub");
    let result = run_sync_check(&mut sandbox);
    let output = command_output(&result);
    assert!(
        !result.status.success(),
        "expected sync --check from a subdirectory to detect workspace drift:\n{output}"
    );
    assert!(
        output.contains("would prune vendor/github.com/mycompany/components/SimpleResistor/9.9.9"),
        "expected stale vendor report from a subdirectory:\n{output}"
    );
}

#[test]
fn test_sync_check_writes_nothing_on_drift() {
    let mut sandbox = Sandbox::new();

    write_local_lib_workspace(&mut sandbox).sync();
    add_unsynced_local_lib_import(&mut sandbox);
    let before = sandbox.snapshot_dir(".");

    let result = run_sync_check(&mut sandbox);
    let output = command_output(&result);

    assert!(
        !result.status.success(),
        "expected sync --check to fail on drift:\n{output}"
    );
    assert_eq!(
        before,
        sandbox.snapshot_dir("."),
        "sync --check must not write workspace files when reporting drift"
    );
}

/// `pcb update` is disabled; use `pcb add -u` instead.
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
pcb-version = "0.4"

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
        output.contains("`pcb update` is no longer supported"),
        "expected unsupported-command rejection message:\n{output}"
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
pcb-version = "0.4"

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
