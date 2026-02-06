#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config("value", str, default = "10kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)

Resistance = "foobar"

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value, "type": "resistor"},
)
"#;

const WARNING_AND_ERROR_ZEN: &str = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# [dependencies]
# "github.com/mycompany/components" = "1.0.0"
# ```

SimpleResistor = Module("github.com/mycompany/components/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
# This will cause an error - missing required parameter
SimpleResistor(name = "R1", P1 = vcc)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const SUPPRESSED_WARNINGS_ZEN: &str = r#"
warn("Regular warning")
warn("Suppressed warning 1", suppress=True)
warn("Suppressed warning 2", suppress=True)
"#;

const SUPPRESSED_ERRORS_ZEN: &str = r#"
error("Suppressed error 1", suppress=True)
error("Suppressed error 2", suppress=True)
"#;

const MIXED_SUPPRESSED_ZEN: &str = r#"
warn("Regular warning 1")
warn("Suppressed warning 1", suppress=True)
warn("Suppressed warning 2", suppress=True)
error("Suppressed error 1", suppress=True)
warn("Regular warning 2")
"#;

const CATEGORIZED_DIAGNOSTICS_ZEN: &str = r#"
warn("Voltage mismatch detected", kind="electrical.voltage_mismatch")
warn("Spacing violation", kind="layout.spacing")
warn("BOM missing part", kind="bom.missing_part")
warn("Regular warning without kind")
"#;

const MULTIPLE_ELECTRICAL_WARNINGS_ZEN: &str = r#"
warn("Overvoltage detected", kind="electrical.voltage.overvoltage")
warn("Undervoltage detected", kind="electrical.voltage.undervoltage")
warn("Current too high", kind="electrical.current.overcurrent")
warn("Layout issue", kind="layout.spacing")
"#;

const MIXED_CATEGORIZED_ZEN: &str = r#"
warn("Regular warning")
warn("Voltage issue", kind="electrical.voltage")
warn("Another regular warning")
error("Layout error", suppress=True, kind="layout.error")
warn("BOM warning", kind="bom.missing")
"#;

// Tests for inline comment suppression
const INLINE_SUPPRESS_BASIC_ZEN: &str = r#"
warn("This should be suppressed", kind="bom.match_generic")  # suppress: bom.match_generic
warn("This should not be suppressed", kind="bom.match_generic")
"#;

const INLINE_SUPPRESS_HIERARCHICAL_ZEN: &str = r#"
warn("Voltage warning", kind="electrical.voltage.overvoltage")  # suppress: electrical
warn("Current warning", kind="electrical.current.overcurrent")  # suppress: electrical
warn("Layout warning", kind="layout.spacing")
"#;

const INLINE_SUPPRESS_SEVERITY_ZEN: &str = r#"
warn("Warning 1")  # suppress: warnings
warn("Warning 2")
error("Error 1", suppress=True)  # suppress: errors
"#;

const INLINE_SUPPRESS_MULTIPLE_ZEN: &str = r#"
warn("Should be suppressed", kind="bom.match_generic")  # suppress: bom.match_generic, electrical
warn("Should not be suppressed", kind="layout.spacing")
"#;

const INLINE_SUPPRESS_ALL_ZEN: &str = r#"
warn("Suppressed by all", kind="bom.match_generic")  # suppress: all
error("Also suppressed", suppress=True, kind="electrical.voltage")  # suppress: all
warn("Not suppressed", kind="layout.spacing")
"#;

const INLINE_SUPPRESS_CASE_INSENSITIVE_ZEN: &str = r#"
warn("Suppressed", kind="bom.match_generic")  # SUPPRESS: bom.match_generic
warn("Also suppressed")  # suppress: WARNINGS
"#;

const INLINE_SUPPRESS_NO_SPACE_ZEN: &str = r#"
warn("Suppressed without space", kind="bom.match_generic")  #suppress: bom.match_generic
warn("Suppressed with space", kind="electrical.voltage")  # suppress: electrical
"#;

// Tests for previous-line suppression
const PREVIOUS_LINE_SUPPRESS_BASIC_ZEN: &str = r#"
# suppress: bom.match_generic
warn("This should be suppressed", kind="bom.match_generic")
warn("This should not be suppressed", kind="bom.match_generic")
"#;

const PREVIOUS_LINE_SUPPRESS_HIERARCHICAL_ZEN: &str = r#"
# suppress: electrical
warn("Voltage warning", kind="electrical.voltage.overvoltage")
# suppress: electrical
warn("Current warning", kind="electrical.current.overcurrent")
warn("Layout warning not suppressed", kind="layout.spacing")
"#;

const PREVIOUS_LINE_SUPPRESS_MULTIPLE_ZEN: &str = r#"
# suppress: bom.match_generic, electrical.voltage
warn("Should be suppressed by first pattern", kind="bom.match_generic")
# suppress: layout, warnings
warn("Should be suppressed by warnings pattern")
"#;

const PREVIOUS_LINE_MIXED_WITH_INLINE_ZEN: &str = r#"
# suppress: bom.match_generic
warn("Suppressed by previous line", kind="bom.match_generic")
warn("Suppressed by inline", kind="electrical.voltage")  # suppress: electrical
warn("Not suppressed", kind="layout.spacing")
"#;

const PREVIOUS_LINE_WITH_COMMENT_ZEN: &str = r#"
# This is a regular comment explaining the code
# suppress: bom.match_generic
warn("Should be suppressed", kind="bom.match_generic")
"#;

const PREVIOUS_LINE_MULTILINE_STATEMENT_ZEN: &str = r#"
# suppress: bom.match_generic
warn(
    "Should be suppressed",
    kind="bom.match_generic"
)
"#;

const INLINE_NO_CROSS_LINE_CONTAMINATION_ZEN: &str = r#"
warn("Warning 1")  # suppress: warnings
warn("Warning 2 should NOT be suppressed")
warn("Warning 3")  # suppress: warnings
warn("Warning 4 should NOT be suppressed")
"#;

#[test]
fn test_warning_and_error_mixed() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a simple component
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("pcb.toml", "[dependencies]")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create a board that has both a warning (unstable ref) and an error (missing param)
    let output = sandbox
        .write("board.zen", WARNING_AND_ERROR_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("warning_and_error_mixed", output);
}

#[test]
fn test_suppressed_warnings() {
    let output = Sandbox::new()
        .write("test.zen", SUPPRESSED_WARNINGS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("suppressed_warnings", output);
}

#[test]
fn test_suppressed_errors() {
    let output = Sandbox::new()
        .write("test.zen", SUPPRESSED_ERRORS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("suppressed_errors", output);
}

#[test]
fn test_mixed_suppressed_diagnostics() {
    let output = Sandbox::new()
        .write("test.zen", MIXED_SUPPRESSED_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("mixed_suppressed_diagnostics", output);
}

#[test]
fn test_suppressed_warnings_with_deny_flag() {
    // Suppressed warnings should not cause build failure even with -Dwarnings
    let output = Sandbox::new()
        .write("test.zen", SUPPRESSED_ERRORS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-Dwarnings"]);
    assert_snapshot!("suppressed_with_deny_flag", output);
}

#[test]
fn test_mixed_suppressed_with_deny_flag() {
    // Regular warnings should still fail with -Dwarnings, but suppressed should not
    let output = Sandbox::new()
        .write("test.zen", MIXED_SUPPRESSED_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-Dwarnings"]);
    assert_snapshot!("mixed_suppressed_with_deny_flag", output);
}

#[test]
fn test_aggregated_warnings() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with components
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write(
            "SimpleResistor/pcb.toml",
            r#"
[dependencies]
"#,
        )
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("SimpleResistor/v1.0.0", false)
        .push_mirror();

    // Create pcb.toml with a package alias that points to an unstable ref
    let pcb_toml_content = r#"
[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = { branch = "main" }
"#;

    // Create a board that uses the alias multiple times - should aggregate warnings
    // because all warnings will trace back to the same PCB.toml line
    let board_zen_content = r#"
SimpleResistor1 = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen")
SimpleResistor2 = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen") 
SimpleResistor3 = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor1(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
SimpleResistor2(name = "R2", value = "2kOhm", P1 = vcc, P2 = gnd)
SimpleResistor3(name = "R3", value = "3kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("pcb.toml", pcb_toml_content)
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("aggregated_warnings", output);
}

#[test]
fn test_mixed_aggregated_and_unique_warnings() {
    let mut sandbox = Sandbox::new();

    // Create multiple fake git repositories
    sandbox
        .git_fixture("https://github.com/company1/components.git")
        .write(
            "Component1/pcb.toml",
            r#"
[dependencies]
"#,
        )
        .write("Component1/Component1.zen", SIMPLE_RESISTOR_ZEN)
        .write("Component1/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add component1")
        .tag("Component1/v1.0.0", false)
        .push_mirror();

    sandbox
        .git_fixture("https://github.com/company2/components.git")
        .write(
            "Component2/pcb.toml",
            r#"
[dependencies]
"#,
        )
        .write("Component2/Component2.zen", SIMPLE_RESISTOR_ZEN)
        .write("Component2/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add component2")
        .tag("Component2/v1.0.0", false)
        .push_mirror();

    // Create pcb.toml with unstable refs (branch) for both deps.
    // The first dep is referenced twice and should aggregate.
    let pcb_toml_content = r#"
[workspace]
pcb-version = "0.3"

[dependencies]
"github.com/company1/components/Component1" = { branch = "main" }
"github.com/company2/components/Component2" = { branch = "main" }
"#;

    // Create a board with both aggregated and unique warnings
    let board_zen_content = r#"
# These should aggregate (same dependency line used multiple times -> same PCB.toml span)
Comp1a = Module("github.com/company1/components/Component1/Component1.zen")
Comp1b = Module("github.com/company1/components/Component1/Component1.zen")
# This should be unique (separate dependency line)
Comp2 = Module("github.com/company2/components/Component2/Component2.zen")

vcc = Net("VCC")
gnd = Net("GND")
Comp1a(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
Comp1b(name = "R2", value = "2kOhm", P1 = vcc, P2 = gnd) 
Comp2(name = "R3", value = "3kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("pcb.toml", pcb_toml_content)
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("mixed_aggregated_and_unique_warnings", output);
}

#[test]
fn test_commit_stable_ref() {
    let mut sandbox = Sandbox::new();

    let short_hash = &sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .branch("foo")
        .write(
            "pcb.toml",
            r#"
[dependencies]
"#,
        )
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror()
        .rev_parse_head()[0..7];

    // Create a board that uses branch unstabe ref
    let unstable_default_zen = format!(
        r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# 
# [dependencies]
# "github.com/mycompany/components" = {{ rev = "{}" }}
# ```

SimpleResistor = Module("github.com/mycompany/components/SimpleResistor.zen")
"#,
        short_hash
    );

    let output = sandbox
        .write("board.zen", unstable_default_zen)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("commit_stable_ref", output);
}

#[test]
fn test_inline_manifest_v2() {
    // Standalone .zen file with inline pcb.toml (V2 mode)
    // Uses minimal code that doesn't require dependencies
    let inline_manifest_zen = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

# Simple V2 standalone script - no dependencies needed
x = 1 + 2
"#;

    let output = Sandbox::new()
        .write("standalone.zen", inline_manifest_zen)
        .snapshot_run("pcb", ["build", "standalone.zen"]);
    assert_snapshot!("inline_manifest_v2", output);
}

#[test]
fn test_inline_manifest_v2_unnamed_net_warning() {
    let io_module = r#"
P1 = io("P1", Net, optional = True)

Component(
    name = "R1",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1"},
    pins = {"P1": P1},
)
"#;

    let inline_manifest_zen = r#"# ```pcb
# [workspace]
# pcb-version = "0.3"
# ```

I2s = interface(
    BCLK = Net(),
    LRCLK = Net(),
    SDATA = Net(),
    MCLK = Net(),
)

IoModule = Module("IoModule.zen")
IoModule(name = "IO")

unnamed = Net()

Component(
    name = "U1",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1"},
    pins = {"P1": unnamed},
)
"#;

    let output = Sandbox::new()
        .write("IoModule.zen", io_module)
        .write("standalone.zen", inline_manifest_zen)
        .snapshot_run("pcb", ["build", "standalone.zen"]);
    assert_snapshot!("inline_manifest_v2_unnamed_net_warning", output);
}

// Tests for -S flag with kind-based suppression

#[test]
fn test_suppress_by_exact_kind() {
    // Suppress only electrical.voltage_mismatch
    let output = Sandbox::new()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcb",
            ["build", "test.zen", "-S", "electrical.voltage_mismatch"],
        );
    assert_snapshot!("suppress_by_exact_kind", output);
}

#[test]
fn test_suppress_by_hierarchical_kind() {
    // -S electrical should suppress all electrical.* warnings
    let output = Sandbox::new()
        .write("test.zen", MULTIPLE_ELECTRICAL_WARNINGS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("suppress_by_hierarchical_kind", output);
}

#[test]
fn test_suppress_by_partial_hierarchy() {
    // -S electrical.voltage should suppress electrical.voltage.* but not electrical.current.*
    let output = Sandbox::new()
        .write("test.zen", MULTIPLE_ELECTRICAL_WARNINGS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "electrical.voltage"]);
    assert_snapshot!("suppress_by_partial_hierarchy", output);
}

#[test]
fn test_suppress_multiple_kinds() {
    // Suppress multiple different kinds
    let output = Sandbox::new()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcb",
            [
                "build",
                "test.zen",
                "-S",
                "electrical.voltage_mismatch",
                "-S",
                "layout.spacing",
            ],
        );
    assert_snapshot!("suppress_multiple_kinds", output);
}

#[test]
fn test_suppress_all_warnings_by_severity() {
    // -S warnings should suppress all warnings regardless of kind
    let output = Sandbox::new()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "warnings"]);
    assert_snapshot!("suppress_all_warnings_by_severity", output);
}

#[test]
fn test_suppress_all_errors_by_severity() {
    let errors_zen = r#"
error("Error 1", suppress=True, kind="validation.error1")
error("Error 2", suppress=True, kind="validation.error2")
"#;

    // -S errors should suppress all errors
    let output = Sandbox::new()
        .write("test.zen", errors_zen)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "errors"]);
    assert_snapshot!("suppress_all_errors_by_severity", output);
}

#[test]
fn test_suppress_kind_with_deny_warnings() {
    // Suppressed warnings should not cause build failure even with -Dwarnings
    let output = Sandbox::new()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcb",
            [
                "build",
                "test.zen",
                "-S",
                "electrical.voltage_mismatch",
                "-S",
                "layout.spacing",
                "-S",
                "bom.missing_part",
                "-Dwarnings",
            ],
        );
    assert_snapshot!("suppress_kind_with_deny_warnings", output);
}

// Tests for inline comment suppression

#[test]
fn test_inline_suppress_basic() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_BASIC_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_basic", output);
}

#[test]
fn test_inline_suppress_hierarchical() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_HIERARCHICAL_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_hierarchical", output);
}

#[test]
fn test_inline_suppress_severity() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_SEVERITY_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_severity", output);
}

#[test]
fn test_inline_suppress_multiple_patterns() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_MULTIPLE_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_multiple_patterns", output);
}

#[test]
fn test_inline_suppress_all() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_ALL_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_all", output);
}

#[test]
fn test_inline_suppress_case_insensitive() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_CASE_INSENSITIVE_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_case_insensitive", output);
}

#[test]
fn test_inline_suppress_no_space_after_hash() {
    let output = Sandbox::new()
        .write("test.zen", INLINE_SUPPRESS_NO_SPACE_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_no_space_after_hash", output);
}

#[test]
fn test_inline_suppress_combined_with_cli() {
    // Both inline and CLI suppression should work together
    let combined_zen = r#"
warn("Suppressed by inline", kind="bom.match_generic")  # suppress: bom.match_generic
warn("Suppressed by CLI", kind="electrical.voltage")
warn("Not suppressed", kind="layout.spacing")
"#;

    let output = Sandbox::new()
        .write("test.zen", combined_zen)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("inline_suppress_combined_with_cli", output);
}

// Tests for previous-line suppression

#[test]
fn test_previous_line_suppress_basic() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_BASIC_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_basic", output);
}

#[test]
fn test_previous_line_suppress_hierarchical() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_HIERARCHICAL_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_hierarchical", output);
}

#[test]
fn test_previous_line_suppress_multiple_patterns() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_MULTIPLE_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_multiple_patterns", output);
}

#[test]
fn test_previous_line_mixed_with_inline() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_MIXED_WITH_INLINE_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_mixed_with_inline", output);
}

#[test]
fn test_previous_line_with_regular_comment() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_WITH_COMMENT_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_with_regular_comment", output);
}

#[test]
fn test_previous_line_multiline_statement() {
    let output = Sandbox::new()
        .write("test.zen", PREVIOUS_LINE_MULTILINE_STATEMENT_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("previous_line_multiline_statement", output);
}

#[test]
fn test_inline_no_cross_line_contamination() {
    // End-of-line comments should NOT affect the next line
    let output = Sandbox::new()
        .write("test.zen", INLINE_NO_CROSS_LINE_CONTAMINATION_ZEN)
        .snapshot_run("pcb", ["build", "test.zen"]);
    assert_snapshot!("inline_no_cross_line_contamination", output);
}

#[test]
fn test_mixed_suppress_and_regular_diagnostics() {
    // Mix of suppressed (by -S) and regular warnings
    let output = Sandbox::new()
        .write("test.zen", MIXED_CATEGORIZED_ZEN)
        .snapshot_run("pcb", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("mixed_suppress_and_regular", output);
}
