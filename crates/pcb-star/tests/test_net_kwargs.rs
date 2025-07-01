mod common;
use common::TestProject;

#[test]
fn test_net_with_kwargs() {
    let env = TestProject::new();

    env.add_file(
        "test.star",
        r#"
# Test nets with keyword arguments
net_a = Net("VCC", voltage="3.3V", type="power")
net_b = Net("GND", type="ground", protection="esd")
net_c = Net("DATA", speed="high", impedance=50)
net_d = Net()  # No name or properties

# Test that kwargs work with various types
net_with_types = Net(
    "SIGNAL",
    voltage="5.0",
    current_limit=100,
    protected=True,
    notes="Main signal line",
    list_prop=[1, 2, 3],
    dict_prop={"nested": "value"}
)

# Test component connections with nets created using kwargs
Component(
    name = "resistor",
    type = "resistor",
    pin_defs = {"P1": "1", "P2": "2"},
    footprint = "SMD:0805",
    pins = {
        "P1": net_a,
        "P2": net_b,
    },
)

# Additional components to ensure all nets are connected
Component(
    name = "data_buffer",
    type = "buffer",
    pin_defs = {"IN": "1", "OUT": "2"},
    footprint = "SOT:23",
    pins = {
        "IN": net_c,
        "OUT": net_with_types,
    },
)
"#,
    );

    let result = env.eval_netlist("test.star");

    // Check that evaluation succeeded
    assert!(result.output.is_some(), "Should produce output");
    assert!(
        result.diagnostics.is_empty(),
        "Should have no errors: {:?}",
        result.diagnostics
    );

    // The netlist output should contain our nets
    let netlist = result.output.unwrap();
    assert!(netlist.contains("VCC"), "Should contain VCC net");
    assert!(netlist.contains("GND"), "Should contain GND net");
    assert!(netlist.contains("DATA"), "Should contain DATA net");
    assert!(netlist.contains("SIGNAL"), "Should contain SIGNAL net");
}

#[test]
fn test_net_kwargs_backwards_compatibility() {
    let env = TestProject::new();

    env.add_file(
        "test.star",
        r#"
# Test that the old properties dict syntax still works
# (since properties is now just a regular kwarg)
net_old_style = Net("OLD", properties={"key": "value"})

# Test mixing positional name with kwargs
net_mixed = Net("MIXED", foo="bar", baz=42)

# Test only kwargs, no positional args
net_kwargs_only = Net(name="KWARGS_ONLY", type="signal")

Component(
    name = "test_comp",
    type = "generic",
    pin_defs = {"P1": "1", "P2": "2", "P3": "3"},
    footprint = "TEST:FOOTPRINT",
    pins = {
        "P1": net_old_style,
        "P2": net_mixed,
        "P3": net_kwargs_only,
    },
)
"#,
    );

    let result = env.eval_netlist("test.star");
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");

    let netlist = result.output.unwrap();
    assert!(netlist.contains("OLD"), "Should contain OLD net");
    assert!(netlist.contains("MIXED"), "Should contain MIXED net");
    assert!(
        netlist.contains("KWARGS_ONLY"),
        "Should contain KWARGS_ONLY net"
    );
}

#[test]
fn test_net_kwargs_edge_cases() {
    let env = TestProject::new();

    // Test error case: too many positional arguments
    env.add_file(
        "test_error.star",
        r#"
# This should fail - too many positional args
net_error = Net("NAME1", "NAME2", type="signal")
"#,
    );

    let result = env.eval_netlist("test_error.star");
    assert!(
        !result.diagnostics.is_empty(),
        "Should have errors for too many positional args"
    );

    // Test empty net creation
    env.add_file(
        "test_empty.star",
        r#"
# Test creating nets with no arguments
net_empty = Net()

# Test creating net with only kwargs
net_kwargs = Net(voltage="12V", current="2A")

Component(
    name = "test",
    type = "generic",
    pin_defs = {"P1": "1", "P2": "2"},
    footprint = "TEST:FOOTPRINT",
    pins = {
        "P1": net_empty,
        "P2": net_kwargs,
    },
)
"#,
    );

    let result2 = env.eval_netlist("test_empty.star");
    assert!(
        result2.output.is_some(),
        "Should produce output for empty nets"
    );
    assert!(
        result2.diagnostics.is_empty(),
        "Should have no errors for empty nets"
    );
}
