#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

#[test]
fn test_moved_old_component_still_exists_warning() {
    let output = Sandbox::new()
        .write(
            "board.zen",
            r#"
# Simple resistor component
R1_P1 = io("R1_P1", Net)
R1_P2 = io("R1_P2", Net)

R2_P1 = io("R2_P1", Net)
R2_P2 = io("R2_P2", Net)

# Create components
Component(
    name = "OLD_RESISTOR",
    prefix = "R",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": R1_P1, "P2": R1_P2},
    properties = {"value": "1kOhm", "type": "resistor"}
)

Component(
    name = "NEW_RESISTOR", 
    prefix = "R",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": R2_P1, "P2": R2_P2},
    properties = {"value": "1kOhm", "type": "resistor"}
)

# moved() directive claims OLD_RESISTOR was moved to NEW_RESISTOR
# But OLD_RESISTOR still exists - this should warn
moved("OLD_RESISTOR", "NEW_RESISTOR")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("old_component_still_exists_warning", output);
}

#[test]
fn test_moved_new_component_missing_warning() {
    let output = Sandbox::new()
        .write(
            "board.zen",
            r#"
R1_P1 = io("R1_P1", Net)
R1_P2 = io("R1_P2", Net)

# Create only one component
Component(
    name = "EXISTING_RESISTOR",
    prefix = "R", 
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": R1_P1, "P2": R1_P2},
    properties = {"value": "1kOhm", "type": "resistor"}
)

# moved() directive points to NEW_COMPONENT that doesn't exist - should warn
moved("OLD_COMPONENT", "NEW_COMPONENT")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("new_component_missing_warning", output);
}

#[test]
fn test_moved_both_issues_warnings() {
    let output = Sandbox::new()
        .write(
            "board.zen",
            r#"
R1_P1 = io("R1_P1", Net) 
R1_P2 = io("R1_P2", Net)

# Create OLD_POWER_SUPPLY component - this should trigger "old still exists" warning
Component(
    name = "OLD_POWER_SUPPLY",
    prefix = "PS",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"VIN": "1", "VOUT": "2"},
    pins = {"VIN": R1_P1, "VOUT": R1_P2},
    properties = {"value": "1kOhm", "type": "power_supply"}
)

# moved() directive with old path existing and new path missing - should warn twice
moved("OLD_POWER_SUPPLY", "NEW_POWER_SUPPLY")

# Another moved() directive with new path missing
moved("NONEXISTENT_OLD", "NONEXISTENT_NEW")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("both_issues_warnings", output);
}

#[test]
fn test_moved_valid_directive_no_warnings() {
    let output = Sandbox::new()
        .write(
            "board.zen",
            r#"
R1_P1 = io("R1_P1", Net)
R1_P2 = io("R1_P2", Net)

# Create only the NEW component (old doesn't exist, new exists)
Component(
    name = "NEW_COMPONENT",
    prefix = "R",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": R1_P1, "P2": R1_P2},
    properties = {"value": "1kOhm", "type": "resistor"}
)

# Valid moved() directive: old doesn't exist, new exists - should not warn
moved("OLD_COMPONENT", "NEW_COMPONENT")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcb", ["build", "board.zen"]);
    assert_snapshot!("valid_directive_no_warnings", output);
}
