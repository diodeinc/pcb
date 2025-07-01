mod common;
use common::TestProject;

// TODO: Debug why the path filtering doesn't work on Windows for this specific test.
#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_component_properties() {
    let env = TestProject::new();

    // Include symbol resource used for components.
    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "test_props.star",
        r#"
# Import component factory from current directory.
load(".", COMP = "C146731")

# Instantiate with pin connections and a custom property.
COMP(
    name = "NB3N551DG",
    pins = {
        "ICLK": Net("ICLK"),
        "Q1": Net("Q1"),
        "Q2": Net("Q2"),
        "Q3": Net("Q3"),
        "Q4": Net("Q4"),
        "GND": Net("GND"),
        "VDD": Net("VDD"),
        "OE": Net("OE"),
    },
    footprint = "SMD:0805",
    properties = {"CustomProp": "Value123"},
)
"#,
    );

    star_snapshot!(env, "test_props.star");
}
