mod common;
use common::TestProject;

#[test]
fn snapshot_io_and_config_with_values() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- my_sub.star
# Declare placeholders for a power net and a configurable baud rate
pwr = io("pwr", Net)
baud = config("baud", int)

# Tiny component referencing the power net so that the schematic/netlist is non-empty
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {"V": "1"},
    pins = {"V": pwr},
)

# --- top.star
# Load the `my_sub` module from the current directory.
load(".", Sub = "my_sub")

Sub(
    name = "sub",
    pwr = Net("VCC"),
    baud = 9600,
)
"#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
fn snapshot_missing_required_inputs_should_error() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- my_sub.star
# Declare a required power net - no default and not optional
pwr = io("pwr", Net)
baud = config("baud", int)

# Tiny component referencing the power net so that the schematic/netlist is non-empty
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {"V": "1"},
    pins = {"V": pwr},
)

# --- top.star
# Load the `my_sub` module from the current directory.
load(".", Sub = "my_sub")

Sub(
    name = "sub",
    # intentionally omit `pwr` and `baud` - should trigger an error
)
"#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
fn snapshot_optional_inputs_return_none() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- my_sub.star
# Declare optional placeholders without explicit defaults
pwr = io("pwr", Net, optional = True)
baud = config("baud", int, optional = True)

# Ensure the config placeholders indeed evaluate to `None` when not supplied.
check(pwr != None, "pwr should not be None when omitted")
check(baud == None, "baud should be None when omitted")

# Tiny component referencing the power net so that the schematic/netlist is non-empty
Component(
    name = "comp0",
    footprint = "TEST:0402",
    pin_defs = {"V": "1"},
    pins = {"V": Net("")},
)

# --- top.star
# Load the `my_sub` module from the current directory.
load(".", Sub = "my_sub")

Sub(
    name = "sub",
    # omit both inputs - allowed because they are optional
)
"#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
fn test_interface_input() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- sub.star
Power = interface(vcc = Net)
PdmMic = interface(power = Power, data = Net, select = Net, clock = Net)

pdm = io("pdm", PdmMic)

# --- top.star
# Load the `sub` module from the current directory.
load(".", Sub = "sub")

pdm = Sub.PdmMic("PDM")
Sub(name = "sub", pdm = pdm)
"#,
    );

    star_snapshot!(env, "top.star");
}

#[test]
fn test_component_rejects_interface_even_with_single_net() {
    let env = TestProject::new();

    env.add_file(
        "test.star",
        r#"
# Define an interface with a single net
SingleNet = interface(signal = Net)

# Create an instance of the interface
signal_if = SingleNet(name="sig")

# Use the interface in a component - should fail
Component(
    name = "test_comp",
    footprint = "test_footprint",
    pin_defs = {"in": "1", "out": "2"},
    pins = {
        "in": signal_if,  # This should fail - interfaces not accepted for pins
        "out": Net()
    }
)
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn test_module_io_rejects_interface_when_net_expected() {
    let env = TestProject::new();

    env.add_file(
        "child.star",
        r#"
signal = io("signal", Net)

# Add a component to use the signal
Component(
    name = "test_comp",
    footprint = "test_footprint",
    pin_defs = {"in": "1"},
    pins = {
        "in": signal
    }
)
"#,
    );

    env.add_file(
        "parent.star",
        r#"
load(".", Child = "child")

SingleNet = interface(signal = Net)
sig_if = SingleNet("SIG")

# Load the child module with an interface instead of a net
Child(name="child1", signal=sig_if)  # Should fail - interface not accepted for Net io
"#,
    );

    star_snapshot!(env, "parent.star");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_component_factory_rejects_interface_even_with_single_net() {
    let env = TestProject::new();

    env.add_file(
        "C146731.kicad_sym",
        include_str!("resources/C146731.kicad_sym"),
    );

    env.add_file(
        "test.star",
        r#"
# Load a component factory
load(".", COMP = "C146731")

# Define an interface with a single net
SingleNet = interface(signal = Net)

# Create instances
signal_if = SingleNet(name="sig")

# Use the interface in a component instance - should fail
COMP(
    name = "C1",
    footprint = "SMD:0805",
    pins = {
        "ICLK": signal_if,  # This should fail - interface not accepted
        "Q1": Net("Q1"),
        "Q2": Net("Q2"),
        "Q3": Net("Q3"),
        "Q4": Net("Q4"),
        "GND": Net("GND"),
        "VDD": Net("VDD"),
        "OE": Net("OE"),
    }
)
"#,
    );

    star_snapshot!(env, "test.star");
}

#[test]
fn test_correct_usage_with_explicit_net_access() {
    let env = TestProject::new();

    env.add_file(
        "test.star",
        r#"
# Define an interface with a single net
SingleNet = interface(signal = Net)

# Create an instance of the interface
signal_if = SingleNet(name="sig")

# Use the interface correctly by accessing the net field
Component(
    name = "test_comp",
    footprint = "test_footprint",
    pin_defs = {"in": "1", "out": "2"},
    pins = {
        "in": signal_if.signal,  # Correct - explicitly access the net field
        "out": Net()
    }
)
"#,
    );

    star_snapshot!(env, "test.star");
}
