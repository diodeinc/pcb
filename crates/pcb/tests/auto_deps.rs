//! Snapshot tests for auto dependency detection
//!
//! These tests verify that auto-dependency detection properly modifies pcb.toml to add:
//! - regular package dependencies from URL imports
//!
//! Note: @stdlib remains implicit; other aliases require explicit dependencies.
//!
//! Most tests verify pcb.toml modification only.
//! Some tests also cover branch/rev pinning behavior in resolver Phase 1.

#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::{FixtureRepo, Sandbox};

const PCB_TOML: &str = r#"[workspace]
pcb-version = "0.3"
"#;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config("value", str, default = "10kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value, "type": "resistor"},
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

fn lock_dep_lines<'a>(pcb_sum: &'a str, module_path: &str) -> Vec<&'a str> {
    let prefix = format!("{module_path} ");
    pcb_sum
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .collect()
}

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
    let _output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);

    // Verify pcb.toml was updated with the dependency
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_stdlib_pcb_toml", pcb_toml_content);
}

/// Test that unresolved @kicad-symbols alias does not mutate pcb.toml
#[test]
fn test_auto_deps_kicad_symbols() {
    let mut sandbox = Sandbox::new();

    let zen_content = r#"# Reference a KiCad symbol (this triggers auto-dep detection)
symbol_path = "@kicad-symbols/Device.kicad_sym:R"
"#;

    let _output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_eq!(pcb_toml_content.trim(), PCB_TOML.trim());
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

    let _output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_eq!(pcb_toml_content.trim(), PCB_TOML.trim());
}

/// Test that auto-deps don't duplicate existing dependencies in pcb.toml
#[test]
fn test_auto_deps_no_duplicate() {
    let mut sandbox = Sandbox::new();

    // pcb.toml that already has the stdlib dependency
    let pcb_toml = r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/diodeinc/stdlib" = "0.4.0"
"#;

    let zen_content = r#"load("@stdlib/units.zen", "kOhm")

x = kOhm(10)
"#;

    let _output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);

    // Verify pcb.toml wasn't duplicated - should remain unchanged
    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_snapshot!("auto_deps_no_duplicate_pcb_toml", pcb_toml_content);
}

/// Test that dynamic kicad paths are ignored by auto-deps
#[test]
fn test_auto_deps_kicad_dynamic_path() {
    let mut sandbox = Sandbox::new();

    // Dynamic footprint path with unresolved alias should not mutate manifest.
    let zen_content = r#"footprint_template = "@kicad-footprints/Resistor_SMD.pretty/R_{size}.kicad_mod"
"#;

    let _output = sandbox
        .write("pcb.toml", PCB_TOML)
        .write("board.zen", zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);

    let pcb_toml_content =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert_eq!(pcb_toml_content.trim(), PCB_TOML.trim());
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

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .snapshot_run("pcb", ["build", "board.zen"]);

    let pinned_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(
        pinned_toml.contains("branch = \"main\""),
        "expected branch to remain in pcb.toml"
    );
    assert!(
        pinned_toml.contains(&format!("rev = \"{}\"", head_rev)),
        "expected rev to be pinned to fixture HEAD"
    );

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    assert!(!pcb_sum.is_empty(), "expected pcb.sum to be written");

    let dep_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components/SimpleResistor");
    assert_eq!(
        dep_lines.len(),
        2,
        "expected content and pcb.toml hashes for the pinned dependency"
    );

    let dep_version = dep_lines[0]
        .split_whitespace()
        .nth(1)
        .expect("dependency content line must include version");
    assert!(
        dep_version.ends_with(&head_rev),
        "expected pseudo-version to be derived from pinned rev"
    );
    assert!(
        dep_lines[1].contains(&format!("{}/pcb.toml", dep_version)),
        "expected second lockfile line to be the dependency manifest hash"
    );

    let snapshot = sandbox.sanitize_output(&format!(
        "{}\n\n--- pcb.toml ---\n{}\n\n--- pcb.sum (dep lines) ---\n{}\n",
        output,
        pinned_toml.replace(&head_rev, "<HEAD_REV>"),
        dep_lines
            .join("\n")
            .replace(dep_version, "<PSEUDO_VERSION>")
    ));
    assert_snapshot!("auto_deps_branch_dep_pins_rev_and_builds", snapshot);
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
        .run("pcb", ["build", "board.zen", "--locked"])
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
        .run("pcb", ["build", "board.zen", "--offline"])
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
        .run("pcb", ["build", "board.zen", "--locked"])
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

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    let dep_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components/SimpleResistor");
    assert_eq!(
        dep_lines.len(),
        2,
        "expected content and pcb.toml hashes for the pinned dependency"
    );
    let dep_version = dep_lines[0]
        .split_whitespace()
        .nth(1)
        .expect("dependency content line must include version");
    assert!(
        dep_version.ends_with(&rev1),
        "expected pseudo-version to use pinned rev"
    );
    assert!(
        !dep_version.ends_with(&rev2),
        "expected pseudo-version to ignore moved branch head"
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

    let first_output = sandbox
        .write("pcb.toml", pcb_toml)
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert!(
        first_output.contains("Exit Code: 0"),
        "expected first build to succeed:\n{first_output}"
    );

    let first_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    let first_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();

    let second_output = sandbox.snapshot_run("pcb", ["build", "board.zen"]);
    assert!(
        second_output.contains("Exit Code: 0"),
        "expected second build to succeed:\n{second_output}"
    );

    let second_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    let second_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();

    assert_eq!(first_toml, second_toml, "expected pcb.toml to be stable");
    assert_eq!(first_sum, second_sum, "expected pcb.sum to be stable");
}

#[test]
fn test_update_refreshes_branch_rev_and_lockfile() {
    let mut sandbox = Sandbox::new();

    let mut fixture = sandbox.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("v1")
        .push_mirror();
    let rev1 = fixture.rev_parse_head();

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"#,
        rev1
    );

    // Seed a lockfile entry for rev1 first so update must replace stale pseudo-version.
    let build_output = sandbox
        .write("pcb.toml", pcb_toml.clone())
        .write("board.zen", BOARD_USING_SIMPLE_RESISTOR)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert!(
        build_output.contains("Exit Code: 0"),
        "expected initial build to succeed:\n{build_output}"
    );

    fixture
        .write(
            "SimpleResistor/SimpleResistor.zen",
            SIMPLE_RESISTOR_ZEN.replace("default = \"10kOhm\"", "default = \"22kOhm\""),
        )
        .commit("v2")
        .push_mirror();
    let rev2 = fixture.rev_parse_head();
    assert_ne!(rev1, rev2);

    let output = sandbox.snapshot_run("pcb", ["update"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected update to succeed:\n{output}"
    );

    let updated_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(
        updated_toml.contains(&format!("rev = \"{}\"", rev2)),
        "expected pcb update to refresh rev to latest branch head"
    );
    assert!(
        !updated_toml.contains(&format!("rev = \"{}\"", rev1)),
        "expected old rev to be replaced"
    );

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    let dep_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components/SimpleResistor");
    assert_eq!(
        dep_lines.len(),
        2,
        "expected content and pcb.toml hashes for refreshed dependency"
    );
    let dep_version = dep_lines[0]
        .split_whitespace()
        .nth(1)
        .expect("dependency content line must include version");
    assert!(
        dep_version.ends_with(&rev2),
        "expected lockfile pseudo-version to use refreshed rev"
    );
    assert!(
        !dep_version.ends_with(&rev1),
        "expected lockfile pseudo-version to drop old rev"
    );
}

#[test]
fn test_update_filter_refreshes_only_matching_branch_dep() {
    let mut sandbox = Sandbox::new();

    let mut fixture_a = sandbox.git_fixture("https://github.com/mycompany/components-a.git");
    write_simple_resistor_package(&mut fixture_a, SIMPLE_RESISTOR_ZEN);
    fixture_a.commit("a-v1").push_mirror();
    let a_rev1 = fixture_a.rev_parse_head();
    write_simple_resistor_package(
        &mut fixture_a,
        &SIMPLE_RESISTOR_ZEN.replace("default = \"10kOhm\"", "default = \"11kOhm\""),
    );
    fixture_a.commit("a-v2").push_mirror();
    let a_rev2 = fixture_a.rev_parse_head();

    let mut fixture_b = sandbox.git_fixture("https://github.com/mycompany/components-b.git");
    write_simple_resistor_package(&mut fixture_b, SIMPLE_RESISTOR_ZEN);
    fixture_b.commit("b-v1").push_mirror();
    let b_rev1 = fixture_b.rev_parse_head();
    write_simple_resistor_package(
        &mut fixture_b,
        &SIMPLE_RESISTOR_ZEN.replace("default = \"10kOhm\"", "default = \"12kOhm\""),
    );
    fixture_b.commit("b-v2").push_mirror();
    let b_rev2 = fixture_b.rev_parse_head();

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components-a/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"github.com/mycompany/components-b/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"#,
        a_rev1, b_rev1
    );

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .snapshot_run("pcb", ["update", "-p", "components-a"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected update -p to succeed:\n{output}"
    );

    let updated_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(updated_toml.contains(&format!("rev = \"{}\"", a_rev2)));
    assert!(!updated_toml.contains(&format!("rev = \"{}\"", a_rev1)));
    assert!(updated_toml.contains(&format!("rev = \"{}\"", b_rev1)));
    assert!(!updated_toml.contains(&format!("rev = \"{}\"", b_rev2)));

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    let a_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components-a/SimpleResistor");
    let b_lines = lock_dep_lines(&pcb_sum, "github.com/mycompany/components-b/SimpleResistor");
    assert!(
        !a_lines.is_empty(),
        "expected lockfile entries for components-a"
    );
    assert!(
        !b_lines.is_empty(),
        "expected lockfile entries for components-b"
    );

    let a_ver = a_lines[0].split_whitespace().nth(1).unwrap_or_default();
    let b_ver = b_lines[0].split_whitespace().nth(1).unwrap_or_default();
    assert!(a_ver.ends_with(&a_rev2));
    assert!(!a_ver.ends_with(&a_rev1));
    assert!(b_ver.ends_with(&b_rev1));
    assert!(!b_ver.ends_with(&b_rev2));
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
        .snapshot_run("pcb", ["build", "board.zen"]);
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

#[test]
fn test_update_branch_refresh_failure_warns_but_succeeds() {
    let mut sandbox = Sandbox::new();

    let mut good = sandbox.git_fixture("https://github.com/mycompany/components-good.git");
    write_simple_resistor_package(&mut good, SIMPLE_RESISTOR_ZEN);
    good.commit("good-v1").push_mirror();
    let good_rev1 = good.rev_parse_head();
    write_simple_resistor_package(
        &mut good,
        &SIMPLE_RESISTOR_ZEN.replace("default = \"10kOhm\"", "default = \"15kOhm\""),
    );
    good.commit("good-v2").push_mirror();
    let good_rev2 = good.rev_parse_head();

    let mut bad = sandbox.git_fixture("https://github.com/mycompany/components-bad.git");
    bad.set_default_branch("dev");
    write_simple_resistor_package(&mut bad, SIMPLE_RESISTOR_ZEN);
    bad.commit("bad-dev-v1").push_mirror();
    let bad_rev = bad.rev_parse_head();

    let pcb_toml = format!(
        r#"[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components-good/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"github.com/mycompany/components-bad/SimpleResistor" = {{ branch = "main", rev = "{}" }}
"#,
        good_rev1, bad_rev
    );

    let output = sandbox
        .write("pcb.toml", pcb_toml)
        .snapshot_run("pcb", ["update"]);
    assert!(
        output.contains("Exit Code: 0"),
        "expected update to succeed despite one refresh failure:\n{output}"
    );
    assert!(
        output.contains("Warning: Failed to refresh branch 'main'"),
        "expected warning for failed branch refresh:\n{output}"
    );
    assert!(
        output.contains("components-bad/SimpleResistor"),
        "expected warning to identify the bad dependency:\n{output}"
    );

    let updated_toml =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.toml")).unwrap_or_default();
    assert!(updated_toml.contains(&format!("rev = \"{}\"", good_rev2)));
    assert!(!updated_toml.contains(&format!("rev = \"{}\"", good_rev1)));
    assert!(updated_toml.contains(&format!("rev = \"{}\"", bad_rev)));

    let pcb_sum =
        std::fs::read_to_string(sandbox.default_cwd().join("pcb.sum")).unwrap_or_default();
    let good_lines = lock_dep_lines(
        &pcb_sum,
        "github.com/mycompany/components-good/SimpleResistor",
    );
    let bad_lines = lock_dep_lines(
        &pcb_sum,
        "github.com/mycompany/components-bad/SimpleResistor",
    );
    assert!(
        !good_lines.is_empty(),
        "expected lockfile entries for components-good"
    );
    assert!(
        !bad_lines.is_empty(),
        "expected lockfile entries for components-bad"
    );

    let good_ver = good_lines[0].split_whitespace().nth(1).unwrap_or_default();
    let bad_ver = bad_lines[0].split_whitespace().nth(1).unwrap_or_default();
    assert!(good_ver.ends_with(&good_rev2));
    assert!(bad_ver.ends_with(&bad_rev));
}
