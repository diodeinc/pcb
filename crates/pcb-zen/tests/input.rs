use crate::common;
use common::TestProject;

#[test]
fn snapshot_missing_required_inputs_should_error() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- my_sub.zen
# Declare a required power net - no default and not optional
pwr = io(Net)
baud = config(int)

# Tiny component referencing the power net so that the schematic/netlist is non-empty
Component(
    name = "comp0",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"V": "1"},
    pins = {"V": pwr},
)

# --- top.zen
# Load the `my_sub` module from the current directory.
Sub = Module("my_sub.zen")

Sub(
    name = "sub",
    # intentionally omit `pwr` and `baud` - should trigger an error
)
"#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
fn snapshot_optional_inputs_return_none() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- my_sub.zen
# Declare optional placeholders without explicit defaults
pwr = io(Net, optional = True)
baud = config(int, optional = True)

# Ensure the config placeholders indeed evaluate to `None` when not supplied.
check(pwr != None, "pwr should not be None when omitted")
check(baud == None, "baud should be None when omitted")

# Tiny component referencing the power net so that the schematic/netlist is non-empty
Component(
    name = "comp0",
    part = Part(mpn = "TEST", manufacturer = "TEST"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"V": "1"},
    pins = {"V": Net("INTERNAL_V")},
)

# --- top.zen
# Load the `my_sub` module from the current directory.
Sub = Module("my_sub.zen")

Sub(
    name = "sub",
    # omit both inputs - allowed because they are optional
)
"#,
    );

    star_snapshot!(env, "top.zen");
}

#[test]
fn test_interface_input() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- sub.zen
Power = builtin.net_type("Power")
PdmMic = interface(power = Power, data = Net, select = Net, clock = Net)

pdm = io(PdmMic)

# --- top.zen
# Load the `sub` module from the current directory.
Sub = Module("sub.zen")

print(Sub.PdmMic)
pdm = Sub.PdmMic("PDM")
print(pdm)
Sub(name = "sub", pdm = pdm)
"#,
    );

    star_snapshot!(env, "top.zen");
}
