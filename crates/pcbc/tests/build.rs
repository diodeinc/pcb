#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const SIMPLE_RESISTOR_ZEN: &str = r#"
value = config(str, default = "10kOhm")

P1 = io(Net)
P2 = io(Net)

Resistance = "foobar"

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

const WARNING_AND_ERROR_ZEN: &str = r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
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

const TEST_NO_CONNECT_SYMBOL: &str = r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "NcPin"
    (property "Reference" "U")
    (symbol "NcPin_0_1"
      (pin no_connect line
        (at 0 0 0)
        (length 2.54)
        (name "NC")
        (number "1")
      )
    )
  )
)
"#;

const DIODES_ZEN: &str = r#"
Rectifier = Module("@stdlib/generics/Rectifier.zen")
Zener = Module("@stdlib/generics/Zener.zen")

vin = Power("VIN")
gnd = Ground("GND")
protected = Net("PROTECTED")

Rectifier(
    name = "D1",
    package = "DO-214AC",
    reverse_voltage = "40V",
    forward_current = "1A",
    A = vin,
    K = protected,
)

Zener(
    name = "D2",
    package = "SOD-123",
    zener_voltage = "5.1V",
    power = "500mW",
    A = gnd,
    K = protected,
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
warn("This should be suppressed", kind="test.match")  # suppress: test.match
warn("This should not be suppressed", kind="test.match")
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
warn("Should be suppressed", kind="test.match")  # suppress: test.match, electrical
warn("Should not be suppressed", kind="layout.spacing")
"#;

const INLINE_SUPPRESS_ALL_ZEN: &str = r#"
warn("Suppressed by all", kind="test.match")  # suppress: all
error("Also suppressed", suppress=True, kind="electrical.voltage")  # suppress: all
warn("Not suppressed", kind="layout.spacing")
"#;

const INLINE_SUPPRESS_CASE_INSENSITIVE_ZEN: &str = r#"
warn("Suppressed", kind="test.match")  # SUPPRESS: test.match
warn("Also suppressed")  # suppress: WARNINGS
"#;

const INLINE_SUPPRESS_NO_SPACE_ZEN: &str = r#"
warn("Suppressed without space", kind="test.match")  #suppress: test.match
warn("Suppressed with space", kind="electrical.voltage")  # suppress: electrical
"#;

// Tests for previous-line suppression
const PREVIOUS_LINE_SUPPRESS_BASIC_ZEN: &str = r#"
# suppress: test.match
warn("This should be suppressed", kind="test.match")
warn("This should not be suppressed", kind="test.match")
"#;

const PREVIOUS_LINE_SUPPRESS_HIERARCHICAL_ZEN: &str = r#"
# suppress: electrical
warn("Voltage warning", kind="electrical.voltage.overvoltage")
# suppress: electrical
warn("Current warning", kind="electrical.current.overcurrent")
warn("Layout warning not suppressed", kind="layout.spacing")
"#;

const PREVIOUS_LINE_SUPPRESS_MULTIPLE_ZEN: &str = r#"
# suppress: test.match, electrical.voltage
warn("Should be suppressed by first pattern", kind="test.match")
# suppress: layout, warnings
warn("Should be suppressed by warnings pattern")
"#;

const PREVIOUS_LINE_MIXED_WITH_INLINE_ZEN: &str = r#"
# suppress: test.match
warn("Suppressed by previous line", kind="test.match")
warn("Suppressed by inline", kind="electrical.voltage")  # suppress: electrical
warn("Not suppressed", kind="layout.spacing")
"#;

const PREVIOUS_LINE_WITH_COMMENT_ZEN: &str = r#"
# This is a regular comment explaining the code
# suppress: test.match
warn("Should be suppressed", kind="test.match")
"#;

const PREVIOUS_LINE_MULTILINE_STATEMENT_ZEN: &str = r#"
# suppress: test.match
warn(
    "Should be suppressed",
    kind="test.match"
)
"#;

const INLINE_NO_CROSS_LINE_CONTAMINATION_ZEN: &str = r#"
warn("Warning 1")  # suppress: warnings
warn("Warning 2 should NOT be suppressed")
warn("Warning 3")  # suppress: warnings
warn("Warning 4 should NOT be suppressed")
"#;

const INVALID_INHERITED_SYMBOL_DATASHEET_COMPONENT_ZEN: &str = r#"
P1 = io(Net)
P2 = io(Net)

Component(
    name = "U",
    symbol = Symbol(library = "Part.kicad_sym"),
    pins = {"P1": P1, "P2": P2},
    part = Part(mpn = "TEST", manufacturer = "TEST"),
)
"#;

const INVALID_INHERITED_SYMBOL_DATASHEET_BOARD_ZEN: &str = r#"
Part = Module("components/TestPart/Part.zen")

Part(name = "U1", P1 = Net("A"), P2 = Net("B"))
"#;

const CONFIGURABLE_BUILD_ZEN: &str = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")
Mode = enum("ONE", "TWO")

enable_extra = config(bool, default=False)
count = config(int, default=1)
mode = config(Mode, default=Mode("ONE"))
package = config(str, default="0603")

vcc = Power("VCC")
gnd = Ground("GND")

for i in range(count):
    Resistor(
        name = "R{}".format(i + 1),
        value = "1kohm",
        package = package,
        P1 = vcc,
        P2 = gnd,
    )

if enable_extra:
    Resistor(name = "R_EXTRA", value = "2kohm", package = package, P1 = vcc, P2 = gnd)

if mode == Mode("TWO"):
    Resistor(name = "R_MODE", value = "3kohm", package = package, P1 = vcc, P2 = gnd)
"#;

const PIN_NO_CONNECT_REPORTS_AT_NET_ZEN: &str = r#"
sig = Net("SIG")

Component(
    name = "U1",
    footprint = File("test.kicad_mod"),
    symbol = Symbol(library = "nc_pin.kicad_sym"),
    part = Part(mpn = "TEST", manufacturer = "TEST"),
    pins = {
        "NC": sig,
    },
)
"#;

const PIN_NO_CONNECT_SUPPRESSES_AT_NET_ZEN: &str = r#"
sig = Net("SIG")  # suppress: pin.no_connect

Component(
    name = "U1",
    footprint = File("test.kicad_mod"),
    symbol = Symbol(library = "nc_pin.kicad_sym"),
    part = Part(mpn = "TEST", manufacturer = "TEST"),
    pins = {
        "NC": sig,
    },
)
"#;

const PIN_NO_CONNECT_NESTED_MODULE_DEDUPS_ZEN: &str = r#"
Child = Module("child.zen")

Child(name = "X1")
"#;

const PIN_NO_CONNECT_NESTED_CHILD_ZEN: &str = r#"
sig = Net("SIG")

Component(
    name = "U1",
    footprint = File("test.kicad_mod"),
    symbol = Symbol(library = "nc_pin.kicad_sym"),
    part = Part(mpn = "TEST", manufacturer = "TEST"),
    pins = {
        "NC": sig,
    },
)
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
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("warning_and_error_mixed", output);
}

#[test]
fn test_pin_no_connect_reports_at_net_site() {
    let mut sandbox = Sandbox::new().with_workspace();
    let output = sandbox
        .write("board.zen", PIN_NO_CONNECT_REPORTS_AT_NET_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL)
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("pin_no_connect_reports_at_net_site", output);
}

#[test]
fn test_pin_no_connect_suppresses_at_net_site() {
    let mut sandbox = Sandbox::new().with_workspace();
    let output = sandbox
        .write("board.zen", PIN_NO_CONNECT_SUPPRESSES_AT_NET_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL)
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("pin_no_connect_suppresses_at_net_site", output);
}

#[test]
fn test_pin_no_connect_dedups_in_nested_modules() {
    let mut sandbox = Sandbox::new().with_workspace();
    let output = sandbox
        .write("board.zen", PIN_NO_CONNECT_NESTED_MODULE_DEDUPS_ZEN)
        .write("child.zen", PIN_NO_CONNECT_NESTED_CHILD_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL)
        .snapshot_run("pcbc", ["build", "board.zen"]);

    assert_eq!(output.matches("Warning:").count(), 1, "{output}");
    assert!(output.contains("net 'SIG'"), "{output}");
}

#[test]
fn test_build_with_config_overrides() {
    let output = Sandbox::new()
        .with_workspace()
        .write("board.zen", CONFIGURABLE_BUILD_ZEN)
        .snapshot_run(
            "pcbc",
            [
                "build",
                "--config",
                "enable_extra=true",
                "--config",
                "count=2",
                "--config",
                "mode=TWO",
                "--config",
                "package=0402",
                "board.zen",
            ],
        );

    assert!(output.contains("Exit Code: 0"), "{output}");
    assert!(output.contains("(4 components)"), "{output}");
}

#[test]
fn test_build_rejects_unknown_config_override() {
    let output = Sandbox::new()
        .with_workspace()
        .write("board.zen", CONFIGURABLE_BUILD_ZEN)
        .snapshot_run(
            "pcbc",
            ["build", "--config", "frequency=100MHz", "board.zen"],
        );

    assert!(output.contains("Exit Code: 1"), "{output}");
    assert!(
        output.contains("Unknown root input(s): frequency"),
        "{output}"
    );
}

#[test]
fn test_diodes_build() {
    let output = Sandbox::new()
        .with_workspace()
        .write("diodes.zen", DIODES_ZEN)
        .snapshot_run("pcbc", ["build", "diodes.zen"]);

    assert!(output.contains("Exit Code: 0"), "{output}");
}

#[test]
fn test_invalid_inherited_symbol_datasheet_is_silent() {
    let output = Sandbox::new().with_workspace()
        .write(
            "components/TestPart/Part.kicad_sym",
            r#"(kicad_symbol_lib
  (version 20241209)
  (symbol "Part"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "Part" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "Part" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Datasheet" "missing-datasheet.pdf" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "Part_0_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#,
        )
        .write("components/TestPart/Part.kicad_mod", TEST_KICAD_MOD)
        .write(
            "components/TestPart/Part.zen",
            INVALID_INHERITED_SYMBOL_DATASHEET_COMPONENT_ZEN,
        )
        .write("board.zen", INVALID_INHERITED_SYMBOL_DATASHEET_BOARD_ZEN)
        .snapshot_run("pcbc", ["build", "board.zen"]);

    assert!(output.contains("Exit Code: 0"), "{output}");
    assert!(!output.contains("Warning:"), "{output}");
    assert!(!output.contains("Error:"), "{output}");
}

#[test]
fn test_suppressed_warnings() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", SUPPRESSED_WARNINGS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("suppressed_warnings", output);
}

#[test]
fn test_suppressed_errors() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", SUPPRESSED_ERRORS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("suppressed_errors", output);
}

#[test]
fn test_mixed_suppressed_diagnostics() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", MIXED_SUPPRESSED_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("mixed_suppressed_diagnostics", output);
}

#[test]
fn test_suppressed_warnings_with_deny_flag() {
    // Suppressed warnings should not cause build failure even with -Dwarnings
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", SUPPRESSED_ERRORS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-Dwarnings"]);
    assert_snapshot!("suppressed_with_deny_flag", output);
}

#[test]
fn test_mixed_suppressed_with_deny_flag() {
    // Regular warnings should still fail with -Dwarnings, but suppressed should not
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", MIXED_SUPPRESSED_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-Dwarnings"]);
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

    // Create pcb.toml with a package alias.
    let pcb_toml_content = r#"
[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/mycompany/components/SimpleResistor" = "1.0.0"
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
        .snapshot_run("pcbc", ["build", "board.zen"]);
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

    // Create pcb.toml with dependencies for both deps.
    // The first dep is referenced twice and should aggregate.
    let pcb_toml_content = r#"
[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/company1/components/Component1" = "1.0.0"
"github.com/company2/components/Component2" = "1.0.0"
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
        .snapshot_run("pcbc", ["build", "board.zen"]);
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

    // Read-only build rejects non-exact refs; `pcb sync` owns dependency hydration.
    let unstable_default_zen = format!(
        r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
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
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(
        !output.contains("Exit Code: 0"),
        "expected build to reject non-exact dependency ref:\n{output}"
    );
    assert!(
        output.contains("must specify an exact version"),
        "expected exact-version rejection:\n{output}"
    );
}

#[test]
fn test_inline_manifest() {
    // Standalone .zen file with inline pcb.toml
    // Uses minimal code that doesn't require dependencies
    let inline_manifest_zen = r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

# Simple standalone script - no dependencies needed
x = 1 + 2
"#;

    let output = Sandbox::new()
        .write("standalone.zen", inline_manifest_zen)
        .snapshot_run("pcbc", ["build", "standalone.zen"]);
    assert_snapshot!("inline_manifest", output);
}

#[test]
fn test_inline_manifest_dependency() {
    let mut sandbox = Sandbox::new();
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor/pcb.toml", "[dependencies]\n")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("SimpleResistor/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add SimpleResistor package")
        .tag("SimpleResistor/v1.0.0", false)
        .push_mirror();

    let inline_manifest_zen = r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
#
# [dependencies]
# "github.com/mycompany/components/SimpleResistor" = "1.0.0"
# ```

SimpleResistor = Module("github.com/mycompany/components/SimpleResistor/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")

SimpleResistor(name = "R1", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("standalone.zen", inline_manifest_zen)
        .snapshot_run("pcbc", ["build", "standalone.zen"]);
    assert_snapshot!("inline_manifest_dependency", output);
}

#[test]
fn test_inline_manifest_unnamed_net_error() {
    let inline_manifest_zen = r#"# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P1": "1"},
    pins = {"P1": Net()},
    part = Part(mpn = "TEST", manufacturer = "TEST"),
)
"#;

    let output = Sandbox::new()
        .write("standalone.zen", inline_manifest_zen)
        .snapshot_run("pcbc", ["build", "standalone.zen"]);
    assert_snapshot!("inline_manifest_unnamed_net_error", output);
}

#[test]
fn test_unused_module_io_warning() {
    let leaf_module = r#"
VIN = io(Net)

Component(
    name = "LOAD",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P": "1"},
    pins = {"P": VIN},
    part = Part(mpn = "TEST", manufacturer = "TEST"),
)
"#;

    let wrapper_module = r#"
Leaf = Module("Leaf.zen")

VIN = io(Net)
SPARE = io(Net)

Leaf(name = "LEAF", VIN = VIN)
"#;

    let board = r#"
Wrapper = Module("Wrapper.zen")

Wrapper(
    name = "WRAP",
    VIN = Net("VIN"),
    SPARE = Net("SPARE"),
)
"#;

    let output = Sandbox::new()
        .with_workspace()
        .write("Leaf.zen", leaf_module)
        .write("Wrapper.zen", wrapper_module)
        .write("board.zen", board)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("unused_module_io_warning", output);
}

// Tests for -S flag with kind-based suppression

#[test]
fn test_suppress_by_exact_kind() {
    // Suppress only electrical.voltage_mismatch
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcbc",
            ["build", "test.zen", "-S", "electrical.voltage_mismatch"],
        );
    assert_snapshot!("suppress_by_exact_kind", output);
}

#[test]
fn test_build_writes_diagnostics_json() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN);

    sandbox
        .run(
            "pcbc",
            ["build", "test.zen", "--diagnostics", "diagnostics.json"],
        )
        .run()
        .expect("build should succeed");

    let report_path = sandbox.root_path().join("diagnostics.json");
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report_path).expect("diagnostics report should be written"),
    )
    .expect("diagnostics report should be valid JSON");

    let diagnostics = report
        .get("test.zen")
        .and_then(|value| value.as_array())
        .expect("report should include diagnostics for the evaluated root file");
    assert_eq!(diagnostics.len(), 4);
    assert_eq!(diagnostics[0]["severity"], "warning");
    assert_eq!(diagnostics[0]["kind"], "electrical.voltage_mismatch");
    assert_eq!(diagnostics[0]["body"], "Voltage mismatch detected");
    assert_eq!(diagnostics[0]["suppressed"], false);
    assert_eq!(diagnostics[3]["kind"], serde_json::Value::Null);
}

#[test]
fn test_build_writes_diagnostics_json_on_failure() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("test.zen", r#"error("Build failed")"#);

    let output = sandbox
        .run(
            "pcbc",
            ["build", "test.zen", "--diagnostics", "diagnostics.json"],
        )
        .unchecked()
        .run()
        .expect("build command should run");
    assert!(!output.status.success());

    let report_path = sandbox.root_path().join("diagnostics.json");
    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(report_path).expect("diagnostics report should be written"),
    )
    .expect("diagnostics report should be valid JSON");

    let diagnostics = report
        .get("test.zen")
        .and_then(|value| value.as_array())
        .expect("report should include diagnostics for the evaluated root file");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], "error");
    assert_eq!(diagnostics[0]["body"], "Build failed");
}

#[test]
fn test_build_netlist_preserves_output_on_failed_checks() {
    let electrical_check = r#"
def check_critical(module):
    error("Critical check failed")

builtin.add_electrical_check(name = "critical", check_fn = check_critical)
"#;

    for (extra_source, extra_args, severity, message) in [
        (electrical_check, vec![], "error", "Critical check failed"),
        ("", vec!["-D", "warnings"], "warning", "marked no_connect"),
    ] {
        let mut sandbox = Sandbox::new().with_workspace();
        sandbox
            .write(
                "board.zen",
                format!("{PIN_NO_CONNECT_REPORTS_AT_NET_ZEN}\n{extra_source}"),
            )
            .write("test.kicad_mod", TEST_KICAD_MOD)
            .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL);

        for mode in ["human", "netlist", "json"] {
            let mut args = vec!["build", "board.zen"];
            args.extend(&extra_args);
            if mode == "json" {
                args.push("--json");
            } else {
                args.extend(["--diagnostics", "diagnostics.json"]);
            }
            if mode == "netlist" {
                args.push("--netlist");
            }
            let output = sandbox
                .run("pcbc", args)
                .stdout_capture()
                .stderr_capture()
                .unchecked()
                .run()
                .expect("build command should run");
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(stderr.contains(message), mode != "json", "{stderr}");
            assert!(!stderr.contains("(1 components)"), "{stderr}");

            let stdout_json: serde_json::Value = if mode == "human" {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&output.stdout).expect("stdout should be one JSON value")
            };
            let diagnostics = if mode == "json" {
                assert_eq!(stdout_json["success"], false);
                assert_eq!(stdout_json.as_object().unwrap().len(), 3);
                stdout_json["diagnostics"].clone()
            } else {
                let report: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(sandbox.root_path().join("diagnostics.json"))
                        .expect("diagnostics report should be written"),
                )
                .expect("diagnostics report should be valid JSON");
                report["board.zen"].clone()
            };
            assert!(diagnostics.as_array().unwrap().iter().any(|d| {
                d["severity"] == severity
                    && d["suppressed"] == false
                    && d["body"].as_str().unwrap().contains(message)
            }));

            if mode != "human" {
                let schematic: pcb_sch::Schematic = serde_json::from_value(if mode == "json" {
                    stdout_json["schematic"].clone()
                } else {
                    stdout_json
                })
                .expect("failed checks should still produce a usable netlist");
                let components: Vec<_> = schematic
                    .instances
                    .values()
                    .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
                    .collect();
                assert_eq!(components.len(), 1);
                assert_eq!(components[0].reference_designator.as_deref(), Some("U1"));
                let nets: Vec<_> = schematic.nets.values().collect();
                assert_eq!(nets.len(), 1);
                assert_eq!(nets[0].name, "SIG");
                assert_eq!(nets[0].ports.len(), 1);
            } else {
                assert!(output.stdout.is_empty());
            }
        }
    }
}

#[test]
fn test_build_netlist_omits_output_on_evaluation_or_conversion_failure() {
    for (source, message) in [
        (
            format!("{PIN_NO_CONNECT_REPORTS_AT_NET_ZEN}\nerror(\"Evaluation failed\")"),
            "Evaluation failed",
        ),
        (
            PIN_NO_CONNECT_REPORTS_AT_NET_ZEN.replace("\"NC\": sig", "\"NC\": Net()"),
            "Net is unnamed",
        ),
    ] {
        let mut sandbox = Sandbox::new().with_workspace();
        sandbox
            .write("board.zen", source)
            .write("test.kicad_mod", TEST_KICAD_MOD)
            .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL);
        for mode in ["--netlist", "--json"] {
            let output = sandbox
                .run("pcbc", ["build", "board.zen", mode])
                .stdout_capture()
                .stderr_capture()
                .unchecked()
                .run()
                .expect("build command should run");
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            if mode == "--json" {
                let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert!(response["schematic"].is_null());
                assert_eq!(response["success"], false);
                assert!(response["diagnostics"].as_array().unwrap().iter().any(|d| {
                    d["severity"] == "error" && d["body"].as_str().unwrap().contains(message)
                }));
                assert!(!stderr.contains(message), "{stderr}");
            } else {
                assert!(output.stdout.is_empty());
                assert!(stderr.contains(message), "{stderr}");
            }
        }
    }
}

#[test]
fn test_build_netlist_requires_complete_module_tree() {
    for child_path in ["child.zen", "wrapper.zen"] {
        for complete in [false, true] {
            let mut sandbox = Sandbox::new().with_workspace();
            sandbox
                .write(
                    "board.zen",
                    format!(
                        "{PIN_NO_CONNECT_REPORTS_AT_NET_ZEN}\nChild = Module(\"{child_path}\")\nChild(name = \"child\", fail_child = True)"
                    ),
                )
                .write(
                    "wrapper.zen",
                    "fail_child = config(bool, default = False)\nChild = Module(\"child.zen\")\nChild(name = \"grandchild\", fail_child = fail_child)",
                )
                .write(
                    "child.zen",
                    format!(
                        "{PIN_NO_CONNECT_REPORTS_AT_NET_ZEN}\nfail_child = config(bool, default = False)\nif fail_child:\n    error(\"Child error\", suppress = {})",
                        if complete { "True" } else { "False" },
                    ),
                )
                .write("test.kicad_mod", TEST_KICAD_MOD)
                .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL);

            // Suppressing diagnostics must not turn an incomplete tree into a
            // usable netlist. A nonfatal error in a complete child is different.
            for (suppress, mode) in [
                (false, "--netlist"),
                (true, "--netlist"),
                (false, "--json"),
                (true, "--json"),
            ] {
                let mut args = vec!["build", "board.zen", mode];
                if suppress {
                    args.extend(["-S", "errors"]);
                }
                let output = sandbox
                    .run("pcbc", args)
                    .stdout_capture()
                    .stderr_capture()
                    .unchecked()
                    .run()
                    .expect("build command should run");
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert_eq!(output.status.success(), suppress, "{stderr}");
                if !suppress && mode == "--netlist" {
                    assert!(stderr.contains("Child error"), "{stderr}");
                }
                let schematic = if mode == "--json" {
                    let response: serde_json::Value =
                        serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(response["success"], suppress);
                    let diagnostic = response["diagnostics"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|d| d["body"] == "Child error")
                        .expect("nested errors should be flattened");
                    assert_eq!(diagnostic["suppressed"], suppress);
                    assert!(
                        diagnostic["location"]
                            .as_str()
                            .unwrap()
                            .contains("child.zen:")
                    );
                    assert!(!diagnostic["stack"].as_array().unwrap().is_empty());
                    assert!(!stderr.contains("Child error"), "{stderr}");
                    response["schematic"].clone()
                } else if complete {
                    serde_json::from_slice(&output.stdout).unwrap()
                } else {
                    assert!(output.stdout.is_empty(), "incomplete tree for {child_path}");
                    serde_json::Value::Null
                };
                if complete {
                    let schematic: pcb_sch::Schematic = serde_json::from_value(schematic)
                        .unwrap_or_else(|e| {
                            panic!("complete tree for {child_path}: {e}\n{stderr}")
                        });
                    assert_eq!(
                        schematic
                            .instances
                            .values()
                            .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
                            .count(),
                        2,
                    );
                } else {
                    assert!(schematic.is_null(), "incomplete tree for {child_path}");
                }
            }
        }
    }
}

#[test]
fn test_build_json_success_and_suppression() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox
        .write("board.zen", PIN_NO_CONNECT_REPORTS_AT_NET_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .write("nc_pin.kicad_sym", TEST_NO_CONNECT_SYMBOL);
    let netlist = sandbox
        .run("pcbc", ["build", "board.zen", "--netlist"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .unwrap();
    let expected_schematic = String::from_utf8(netlist.stdout).unwrap();

    for suppress in [false, true] {
        let mut args = vec!["build", "board.zen", "--json"];
        if suppress {
            args.extend(["-S", "warnings", "-D", "warnings"]);
        }
        let output = sandbox
            .run("pcbc", args)
            .stdout_capture()
            .stderr_capture()
            .run()
            .expect("warning-only or suppressed build should succeed");
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["success"], true);
        // Compare canonical schematic data, not JSON number spelling (1 vs 1.0).
        let schematic: pcb_sch::Schematic =
            serde_json::from_value(response["schematic"].clone()).unwrap();
        assert_eq!(schematic.to_json().unwrap(), expected_schematic.trim());
        let diagnostics = response["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["kind"], "pin.no_connect");
        assert_eq!(diagnostics[0]["severity"], "warning");
        assert_eq!(diagnostics[0]["suppressed"], suppress);
        assert_eq!(diagnostics[0]["occurrences"], 1);
        assert!(
            diagnostics[0]["location"]
                .as_str()
                .unwrap()
                .contains("board.zen:2:")
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
}

#[test]
fn test_build_json_rejects_incompatible_outputs_and_targets() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", "").write("other.zen", "");
    for extra_args in [
        vec![],
        vec!["."],
        vec!["board.zen", "other.zen"],
        vec!["board.zen", "--netlist"],
        vec!["board.zen", "--diagnostics", "-"],
        vec!["board.zen", "--diagnostics", "diagnostics.json"],
    ] {
        let mut args = vec!["build", "--json"];
        args.extend(extra_args);
        let output = sandbox
            .run("pcbc", args)
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "must reject rather than drop target outputs"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--json"), "{stderr}");
        assert!(
            stderr.contains("single .zen file") || stderr.contains("cannot be used with"),
            "{stderr}"
        );
    }
    assert!(!sandbox.root_path().join("diagnostics.json").exists());
}

#[test]
fn test_suppress_by_hierarchical_kind() {
    // -S electrical should suppress all electrical.* warnings
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", MULTIPLE_ELECTRICAL_WARNINGS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("suppress_by_hierarchical_kind", output);
}

#[test]
fn test_suppress_by_partial_hierarchy() {
    // -S electrical.voltage should suppress electrical.voltage.* but not electrical.current.*
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", MULTIPLE_ELECTRICAL_WARNINGS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "electrical.voltage"]);
    assert_snapshot!("suppress_by_partial_hierarchy", output);
}

#[test]
fn test_suppress_multiple_kinds() {
    // Suppress multiple different kinds
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcbc",
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
        .with_workspace()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "warnings"]);
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
        .with_workspace()
        .write("test.zen", errors_zen)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "errors"]);
    assert_snapshot!("suppress_all_errors_by_severity", output);
}

#[test]
fn test_suppress_kind_with_deny_warnings() {
    // Suppressed warnings should not cause build failure even with -Dwarnings
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", CATEGORIZED_DIAGNOSTICS_ZEN)
        .snapshot_run(
            "pcbc",
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
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_BASIC_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_basic", output);
}

#[test]
fn test_inline_suppress_hierarchical() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_HIERARCHICAL_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_hierarchical", output);
}

#[test]
fn test_inline_suppress_severity() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_SEVERITY_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_severity", output);
}

#[test]
fn test_inline_suppress_multiple_patterns() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_MULTIPLE_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_multiple_patterns", output);
}

#[test]
fn test_inline_suppress_all() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_ALL_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_all", output);
}

#[test]
fn test_inline_suppress_case_insensitive() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_CASE_INSENSITIVE_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_case_insensitive", output);
}

#[test]
fn test_inline_suppress_no_space_after_hash() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_SUPPRESS_NO_SPACE_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_suppress_no_space_after_hash", output);
}

#[test]
fn test_inline_suppress_combined_with_cli() {
    // Both inline and CLI suppression should work together
    let combined_zen = r#"
warn("Suppressed by inline", kind="test.match")  # suppress: test.match
warn("Suppressed by CLI", kind="electrical.voltage")
warn("Not suppressed", kind="layout.spacing")
"#;

    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", combined_zen)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("inline_suppress_combined_with_cli", output);
}

// Tests for previous-line suppression

#[test]
fn test_previous_line_suppress_basic() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_BASIC_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_basic", output);
}

#[test]
fn test_previous_line_suppress_hierarchical() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_HIERARCHICAL_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_hierarchical", output);
}

#[test]
fn test_previous_line_suppress_multiple_patterns() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_SUPPRESS_MULTIPLE_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_suppress_multiple_patterns", output);
}

#[test]
fn test_previous_line_mixed_with_inline() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_MIXED_WITH_INLINE_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_mixed_with_inline", output);
}

#[test]
fn test_previous_line_with_regular_comment() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_WITH_COMMENT_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_with_regular_comment", output);
}

#[test]
fn test_previous_line_multiline_statement() {
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", PREVIOUS_LINE_MULTILINE_STATEMENT_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("previous_line_multiline_statement", output);
}

#[test]
fn test_inline_no_cross_line_contamination() {
    // End-of-line comments should NOT affect the next line
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", INLINE_NO_CROSS_LINE_CONTAMINATION_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen"]);
    assert_snapshot!("inline_no_cross_line_contamination", output);
}

#[test]
fn test_mixed_suppress_and_regular_diagnostics() {
    // Mix of suppressed (by -S) and regular warnings
    let output = Sandbox::new()
        .with_workspace()
        .write("test.zen", MIXED_CATEGORIZED_ZEN)
        .snapshot_run("pcbc", ["build", "test.zen", "-S", "electrical"]);
    assert_snapshot!("mixed_suppress_and_regular", output);
}
