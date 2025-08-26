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

const UNSTABLE_REF_BOARD_ZEN: &str = r#"
SimpleResistor = Module("@github/mycompany/components:main/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

const LOAD_AND_MODULE_UNSTABLE_ZEN: &str = r#"
load("@github/mycompany/components:main/common.zen", "helper")
SimpleResistor = Module("@github/mycompany/components:main/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

const WARNING_AND_ERROR_ZEN: &str = r#"
SimpleResistor = Module("@github/mycompany/components:main/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
# This will cause an error - missing required parameter
SimpleResistor(name = "R1", P1 = vcc)
"#;

const METHOD_CHAINING_UNSTABLE_ZEN: &str = r#"
# Test Module() with method chaining - should still show span highlighting
R = Module("@github/mycompany/components:main/SimpleResistor.zen").Resistance
"#;

const COMMON_ZEN: &str = r#"
def helper():
    return "helper function"
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

#[test]
fn test_pcb_build_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a simple component
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board that uses the component from the fake git repository with unstable ref (:main)
    let output = sandbox
        .write("board.zen", UNSTABLE_REF_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("module_unstable_ref_warning", output);
}

#[test]
fn test_load_and_module_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with components and common utilities
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("common.zen", COMMON_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add components and utilities")
        .push_mirror();

    // Create a board that uses both load() and Module() with unstable refs
    let output = sandbox
        .write("board.zen", LOAD_AND_MODULE_UNSTABLE_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("load_and_module_unstable_ref_warning", output);
}

#[test]
fn test_unstable_ref_no_explicit_branch() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board that uses unstable ref without explicit ":main" (defaults to HEAD)
    let unstable_default_zen = r#"
SimpleResistor = Module("@github/mycompany/components/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("board.zen", unstable_default_zen)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("unstable_ref_no_explicit_branch", output);
}

#[test]
fn test_warning_and_error_mixed() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a simple component
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board that has both a warning (unstable ref) and an error (missing param)
    let output = sandbox
        .write("board.zen", WARNING_AND_ERROR_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("warning_and_error_mixed", output);
}

#[test]
fn test_deny_warnings_flag() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board with unstable ref and test -Dwarnings flag
    let output = sandbox
        .write("board.zen", UNSTABLE_REF_BOARD_ZEN)
        .snapshot_run("pcb", ["build", "board.zen", "-Dwarnings"]);
    assert_snapshot!("deny_warnings_flag", output);
}

#[test]
fn test_cross_repo_unstable_warning() {
    let mut sandbox = Sandbox::new();

    // Create a third-party repo (unstable)
    sandbox
        .git_fixture("https://github.com/thirdparty/tools.git")
        .write("Tool.zen", r#"def tool_function(): return "utility""#)
        .commit("Add tool utility")
        .push_mirror();

    // Create main components repo (stable) that depends on unstable third-party repo
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write(
            "StableComponent.zen",
            r#"
# This stable component depends on an unstable third-party repo  
ThirdPartyTool = Module("@github/thirdparty/tools:main/Tool.zen")

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
"#,
        )
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add stable component")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create a board that uses the stable component (which internally uses unstable dep)
    let output = sandbox
        .write(
            "board.zen",
            r#"
# Load from stable tagged repo
StableComponent = Module("@github/mycompany/components:v1.0.0/StableComponent.zen")

vcc = Net("VCC")
gnd = Net("GND")
StableComponent(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#,
        )
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("cross_repo_unstable_warning", output);
}

#[test]
fn test_method_chaining_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with a component that exports a sub-interface
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board that uses Module() with method chaining (.Resistance)
    // This should show proper span highlighting on the string argument
    let output = sandbox
        .write("board.zen", METHOD_CHAINING_UNSTABLE_ZEN)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("method_chaining_unstable_ref_warning", output);
}

#[test]
fn test_alias_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with components
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a pcb.toml with an alias that points to an unstable ref
    let pcb_toml_content = r#"
[packages]
mycomps = "@github/mycompany/components:main"
"#;

    // Create a board that uses the component via an alias
    let board_zen_content = r#"
SimpleResistor = Module("@mycomps/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("pcb.toml", pcb_toml_content)
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("alias_unstable_ref_warning", output);
}

#[test]
fn test_default_alias_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake stdlib repository that matches the default stdlib alias
    sandbox
        .git_fixture("https://github.com/diodeinc/stdlib.git")
        .write("TestModule.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add test module")
        .push_mirror();

    // Create a board that uses the default stdlib alias with HEAD (unstable)
    let board_zen_content = r#"
TestModule = Module("@stdlib/TestModule.zen")

vcc = Net("VCC")
gnd = Net("GND")
TestModule(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("default_alias_unstable_ref_warning", output);
}

#[test]
fn test_gitlab_unstable_ref_warning() {
    let mut sandbox = Sandbox::new();

    // Create a fake GitLab repository with components
    sandbox
        .git_fixture("https://gitlab.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create a board that uses GitLab with unstable ref
    let board_zen_content = r#"
SimpleResistor = Module("@gitlab/mycompany/components:main/SimpleResistor.zen")

vcc = Net("VCC")
gnd = Net("GND")
SimpleResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("gitlab_unstable_ref_warning", output);
}

#[test]
fn test_transitive_unstable_dependencies() {
    let mut sandbox = Sandbox::new();

    // Create a third-party utility repo (unstable)
    sandbox
        .git_fixture("https://github.com/thirdparty/utils.git")
        .write(
            "Utility.zen",
            r#"
def utility_function(): 
    return "helper"
"#,
        )
        .commit("Add utility functions")
        .push_mirror();

    // Create intermediate components repo (stable) that depends on unstable third-party via PCB.toml
    sandbox
        .git_fixture("https://github.com/mycompany/intermediate.git")
        .write(
            "pcb.toml",
            r#"
[packages]
utils = "@github/thirdparty/utils:main"
"#,
        )
        .write(
            "IntermediateComponent.zen",
            r#"
# This intermediate component depends on unstable third-party via alias
load("@utils/Utility.zen", "utility_function")

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
"#,
        )
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add intermediate component")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create a PCB.toml with multiple aliases creating a chain
    let pcb_toml_content = r#"
[packages]
intermediate = "@github/mycompany/intermediate:v1.0.0"
utils = "@github/thirdparty/utils:main"
"#;

    // Create a board that uses the intermediate alias (which internally uses the utils alias)
    let board_zen_content = r#"
IntermediateComponent = Module("@intermediate/IntermediateComponent.zen")

vcc = Net("VCC")
gnd = Net("GND")
IntermediateComponent(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("pcb.toml", pcb_toml_content)
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("transitive_unstable_dependencies", output);
}

#[test]
fn test_mixed_stable_unstable_refs() {
    let mut sandbox = Sandbox::new();

    // Create a stable component repository
    sandbox
        .git_fixture("https://github.com/stable/components.git")
        .write("StableResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add stable resistor")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create an unstable component repository
    sandbox
        .git_fixture("https://github.com/unstable/components.git")
        .write(
            "UnstableCapacitor.zen",
            r#"
value = config("value", str, default = "1uF")

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "C",
    prefix = "C",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value, "type": "capacitor"},
)
"#,
        )
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add unstable capacitor")
        .push_mirror();

    // Create PCB.toml with mixed stable/unstable aliases
    let pcb_toml_content = r#"
[packages]
stable = "@github/stable/components:v1.0.0"
unstable = "@github/unstable/components:main"
"#;

    // Create a board that uses both stable and unstable components
    let board_zen_content = r#"
StableResistor = Module("@stable/StableResistor.zen")
UnstableCapacitor = Module("@unstable/UnstableCapacitor.zen")

vcc = Net("VCC")
gnd = Net("GND")
StableResistor(name = "R1", value = "1kOhm", P1 = vcc, P2 = gnd)
UnstableCapacitor(name = "C1", value = "1uF", P1 = vcc, P2 = gnd)
"#;

    let output = sandbox
        .write("pcb.toml", pcb_toml_content)
        .write("board.zen", board_zen_content)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("mixed_stable_unstable_refs", output);
}

#[test]
fn test_aggregated_warnings() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with components
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror();

    // Create pcb.toml with a package alias that points to an unstable ref
    let pcb_toml_content = r#"
[packages]
mycomponents = "@github/mycompany/components:main"
"#;

    // Create a board that uses the alias multiple times - should aggregate warnings
    // because all warnings will trace back to the same PCB.toml line
    let board_zen_content = r#"
SimpleResistor1 = Module("@mycomponents/SimpleResistor.zen")
SimpleResistor2 = Module("@mycomponents/SimpleResistor.zen") 
SimpleResistor3 = Module("@mycomponents/SimpleResistor.zen")

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
        .write("Component1.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add component1")
        .push_mirror();

    sandbox
        .git_fixture("https://github.com/company2/components.git")
        .write("Component2.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add component2")
        .push_mirror();

    // Create pcb.toml with a package alias for one of the repos (will create aggregated warnings)
    let pcb_toml_content = r#"
[packages]
comp1 = "@github/company1/components:main"
"#;

    // Create a board with both aggregated and unique warnings
    let board_zen_content = r#"
# These should aggregate (same alias used multiple times -> same PCB.toml span)
Comp1a = Module("@comp1/Component1.zen")
Comp1b = Module("@comp1/Component1.zen")
# This should be separate (direct reference -> different span in .zen file)
Comp2 = Module("@github/company2/components:main/Component2.zen")

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
fn test_unstable_ref_wrong_tag() {
    let mut sandbox = Sandbox::new();

    // Create a fake git repository with HEAD tagged
    sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .tag("v1.0.0", false)
        .push_mirror();

    // Create a board that uses branch unstabe ref
    let unstable_default_zen = r#"
SimpleResistor = Module("@github/mycompany/components:main/SimpleResistor.zen")
"#;

    let output = sandbox
        .write("board.zen", unstable_default_zen)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("unstable_ref_wrong_tag", output);
}

#[test]
fn test_commit_stable_ref() {
    let mut sandbox = Sandbox::new();

    let short_hash = &sandbox
        .git_fixture("https://github.com/mycompany/components.git")
        .branch("foo")
        .write("SimpleResistor.zen", SIMPLE_RESISTOR_ZEN)
        .write("test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add simple resistor component")
        .push_mirror()
        .rev_parse_head()[0..7];

    // Create a board that uses branch unstabe ref
    let unstable_default_zen = format!(
        r#"
SimpleResistor = Module("@github/mycompany/components:{}/SimpleResistor.zen")
"#,
        short_hash
    );

    let output = sandbox
        .write("board.zen", unstable_default_zen)
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("commit_stable_ref", output);
}
