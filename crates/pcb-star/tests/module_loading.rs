mod common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_component_via_module() {
    let env = TestProject::new();

    env.add_file(
        "pcb.toml",
        r#"
[module]
name = "test"
"#,
    );

    env.add_file(
        "nested/file/import.star",
        r#"
def DummyFunction():
    pass
"#,
    );

    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "sub.star",
        r#"
# Import the component factory from the current directory.
load(".", COMP = "C146731")
load("//nested/file/import.star", DummyFunction = "DummyFunction")

DummyFunction()

# Instantiate with required pin connections via `pins` dict.
COMP(
    name = "NB3N551DG",
    footprint = "SMD:0805",
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
)
"#,
    );

    env.add_file(
        "top.star",
        r#"
# Import the `sub` module from the current directory and alias it to `Sub`
load(".", Sub = "sub")

Sub(name = "sub")
"#,
    );

    star_snapshot!(env, "top.star");
}
