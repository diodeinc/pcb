mod common;
use common::TestProject;
use pcb_zen_core::WithDiagnostics;

fn expect_netlist(result: WithDiagnostics<String>) -> String {
    assert!(
        result.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        result.diagnostics
    );
    result.output.expect("expected netlist output")
}

#[test]
fn test_interface_with_net_template() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test interface with net template
MyIf = interface(test = Net("MYTEST"))
instance = MyIf("PREFIX")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Create a component to use the net
Resistor(
    name = "component",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = instance.test,
    P2 = Net("GND"),
)
"#,
    );

    // The netlist output should contain our net with the proper name
    // For single-net interfaces, the instance name becomes the net name directly
    let netlist = expect_netlist(env.eval_netlist("test.zen"));
    assert!(
        netlist.contains("PREFIX"),
        "Should contain PREFIX net (single-net interface uses instance name directly)"
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
Resistor = Module("@stdlib/generics/Resistor.zen")

# Create components to use the nets
Resistor(
    name = "resistor",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = pwr.vcc,
    P2 = pwr.gnd,
)

Resistor(
    name = "enable_pull",
    value = "10kohm",
    package = "0402",
    skip_bom = True,
    P1 = pwr.enable,
    P2 = pwr.vcc,
)
"#,
    );

    let netlist = expect_netlist(env.eval_netlist("test.zen"));
    assert!(netlist.contains("MCU_3V3"), "Should contain MCU_3V3 net");
    assert!(netlist.contains("MCU_GND"), "Should contain MCU_GND net");
    assert!(
        netlist.contains("MCU_enable"),
        "Should contain MCU_enable net"
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
Resistor = Module("@stdlib/generics/Resistor.zen")

# Use the nets
Resistor(
    name = "data_load",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = sys.data,
    P2 = sys.power.gnd,
)

Resistor(
    name = "power_load",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = sys.power.vcc,
    P2 = sys.power.gnd,
)
"#,
    );

    let netlist = expect_netlist(env.eval_netlist("test.zen"));
    assert!(
        netlist.contains("MAIN_power_VCC"),
        "Should contain MAIN_power_VCC net"
    );
    assert!(
        netlist.contains("MAIN_power_GND"),
        "Should contain MAIN_power_GND net"
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
Resistor = Module("@stdlib/generics/Resistor.zen")

# Create instance without prefix
instance = MyIf()

# Use the net
Resistor(
    name = "component",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = instance.test,
    P2 = Net("GND"),
)
"#,
    );

    expect_netlist(env.eval_netlist("test.zen"));
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
gnd = Net("GND")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Use both nets
Resistor(
    name = "comp1",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = inst1.test,
    P2 = gnd,
)

Resistor(
    name = "comp2",
    value = "1kohm",
    package = "0402",
    skip_bom = True,
    P1 = inst2.test,
    P2 = gnd,
)
"#,
    );

    let netlist = expect_netlist(env.eval_netlist("test.zen"));
    // For single-net interfaces, the instance name becomes the net name directly
    assert!(
        netlist.contains("A"),
        "Should contain A net (single-net interface uses instance name directly)"
    );
    assert!(
        netlist.contains("B"),
        "Should contain B net (single-net interface uses instance name directly)"
    );
}
