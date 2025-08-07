mod common;
use common::TestProject;

#[test]
fn test_interface_with_net_template() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test interface with net template
MyIf = interface(test = Net("MYTEST"))
instance = MyIf("PREFIX")

# Create component to use the net
Component(
    name = "component",
    type = "test_component",
    pin_defs = {"P1": "1"},
    footprint = "TEST:FOOTPRINT",
    pins = {"P1": instance.test},
)
"#,
    );

    let result = env.eval_netlist("test.zen");

    // Check that evaluation succeeded
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");

    // The netlist output should contain our net with the proper name
    let netlist = result.output.unwrap();
    assert!(
        netlist.contains("PREFIX_MYTEST"),
        "Should contain PREFIX_MYTEST net"
    );
}

#[test]
fn test_interface_with_multiple_net_templates() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test interface with multiple net templates
Power = interface(
    vcc = Net("3V3"),
    gnd = Net("GND"),
    enable = Net()  # Regular net type, not template
)

# Create instance with prefix
pwr = Power("MCU")

# Create components to use the nets
Component(
    name = "resistor",
    type = "resistor",
    pin_defs = {"P1": "1", "P2": "2"},
    footprint = "SMD:0805",
    pins = {
        "P1": pwr.vcc,
        "P2": pwr.gnd,
    },
)

Component(
    name = "transistor",
    type = "transistor",
    pin_defs = {"G": "1", "D": "2", "S": "3"},
    footprint = "SOT:23",
    pins = {
        "G": pwr.enable,
        "D": pwr.vcc,
        "S": pwr.gnd,
    },
)
"#,
    );

    let result = env.eval_netlist("test.zen");
    if !result.diagnostics.is_empty() {
        eprintln!("Diagnostics: {:?}", result.diagnostics);
    }
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");

    let netlist = result.output.unwrap();
    println!("netlist:\n{}", netlist);
    assert!(netlist.contains("MCU_3V3"), "Should contain MCU_3V3 net");
    assert!(netlist.contains("MCU_GND"), "Should contain MCU_GND net");
    assert!(
        netlist.contains("MCU_ENABLE"),
        "Should contain MCU_ENABLE net"
    );
}

#[test]
fn test_interface_with_nested_interface_template() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test nested interface templates
PowerNets = interface(
    vcc = Net("VCC"),
    gnd = Net("GND")
)

# Create a template instance
template_pwr = PowerNets()

# Use the template instance in another interface
System = interface(
    power = template_pwr,
    data = Net("DATA")
)

# Create system instance
sys = System("MAIN")

# Use the nets
Component(
    name = "chip",
    type = "ic",
    pin_defs = {"VCC": "1", "GND": "2", "DATA": "3"},
    footprint = "QFN:16",
    pins = {
        "VCC": sys.power.vcc,
        "GND": sys.power.gnd,
        "DATA": sys.data,
    },
)
"#,
    );

    let result = env.eval_netlist("test.zen");
    if !result.diagnostics.is_empty() {
        eprintln!("Diagnostics: {:?}", result.diagnostics);
    }
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");

    let netlist = result.output.unwrap();
    println!("netlist:\n{}", netlist);
    assert!(
        netlist.contains("MAIN_POWER_VCC"),
        "Should contain MAIN_POWER_VCC net"
    );
    assert!(
        netlist.contains("MAIN_POWER_GND"),
        "Should contain MAIN_POWER_GND net"
    );
    assert!(
        netlist.contains("MAIN_DATA"),
        "Should contain MAIN_DATA net"
    );
}

#[test]
fn test_interface_template_without_name() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test interface with unnamed net template
MyIf = interface(
    test = Net()  # No name specified
)

# Create instance without prefix
instance = MyIf()

# Use the net
Component(
    name = "component",
    type = "test",
    pin_defs = {"P1": "1"},
    footprint = "TEST:FP",
    pins = {"P1": instance.test},
)
"#,
    );

    let result = env.eval_netlist("test.zen");
    if !result.diagnostics.is_empty() {
        eprintln!("Diagnostics: {:?}", result.diagnostics);
    }
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");
    let netlist = result.output.unwrap();
    println!("netlist:\n{}", netlist);
}

#[test]
fn test_interface_preserves_unique_net_ids() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test that templates create new nets with unique IDs
MyIf = interface(test = Net("SHARED"))

# Create two instances - should have different net IDs
inst1 = MyIf("A")
inst2 = MyIf("B")

# Use both nets
Component(
    name = "comp1",
    type = "test",
    pin_defs = {"P1": "1"},
    footprint = "TEST:FP",
    pins = {"P1": inst1.test},
)

Component(
    name = "comp2",
    type = "test",
    pin_defs = {"P1": "1"},
    footprint = "TEST:FP",
    pins = {"P1": inst2.test},
)
"#,
    );

    let result = env.eval_netlist("test.zen");
    assert!(result.output.is_some(), "Should produce output");
    assert!(result.diagnostics.is_empty(), "Should have no errors");

    let netlist = result.output.unwrap();
    assert!(netlist.contains("A_SHARED"), "Should contain A_SHARED net");
    assert!(netlist.contains("B_SHARED"), "Should contain B_SHARED net");
}
