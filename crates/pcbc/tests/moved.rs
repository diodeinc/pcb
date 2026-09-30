#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

#[test]
fn test_moved_both_issues_warnings() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
R1_P1 = io(Net) 
R1_P2 = io(Net)

# Create OLD_POWER_SUPPLY component - this should trigger "old still exists" warning
Component(
    name = "OLD_POWER_SUPPLY",
    prefix = "PS",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"VIN": "1", "VOUT": "2"},
    pins = {"VIN": R1_P1, "VOUT": R1_P2},
    type = "power_supply", part = Part(mpn = "power_supply", manufacturer = "TEST"),
    properties = {"value": "1kOhm"}
)

# moved() directive with old path existing and new path missing - should warn twice
moved("OLD_POWER_SUPPLY", "NEW_POWER_SUPPLY")

# Another moved() directive with new path missing
moved("NONEXISTENT_OLD", "NONEXISTENT_NEW")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("both_issues_warnings", output);
}

#[test]
fn test_moved_valid_directive_no_warnings() {
    let output = Sandbox::new()
        .with_workspace()
        .write(
            "board.zen",
            r#"
R1_P1 = io(Net)
R1_P2 = io(Net)

# Create only the NEW component (old doesn't exist, new exists)
Component(
    name = "NEW_COMPONENT",
    prefix = "R",
    footprint = File("dummy.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": R1_P1, "P2": R1_P2},
    type = "resistor",
    properties = {"value": "1kOhm"}
)

# Valid moved() directive: old doesn't exist, new exists - should not warn
moved("OLD_COMPONENT", "NEW_COMPONENT")
"#,
        )
        .write("dummy.kicad_mod", "(footprint \"dummy\" )")
        .snapshot_run("pcbc", ["build", "board.zen"]);
    assert_snapshot!("valid_directive_no_warnings", output);
}
