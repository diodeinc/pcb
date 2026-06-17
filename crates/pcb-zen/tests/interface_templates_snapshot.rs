mod common;
use common::TestProject;

#[test]
fn interface_net_template_basic() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Basic interface with net template
MyInterface = interface(test = Net("MYTEST"))
instance = MyInterface("PREFIX")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Create component to generate netlist
Resistor(
    name = "R1",
    value = "1kohm",
    package = "0402",
    P1 = instance.test,
    P2 = Net("GND"),
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn interface_multiple_net_templates() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Interface with multiple net templates
Power = interface(
    vcc = Net("3V3"),
    gnd = Net("GND"),
    enable = Net("EN")
)

# Create instances
pwr1 = Power("MCU")
pwr2 = Power("SENSOR")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Create components
Resistor(
    name = "MCU_PWR",
    value = "1kohm",
    package = "0402",
    P1 = pwr1.vcc,
    P2 = pwr1.gnd,
)

Resistor(
    name = "MCU_EN",
    value = "10kohm",
    package = "0402",
    P1 = pwr1.enable,
    P2 = pwr1.gnd,
)

Resistor(
    name = "SENSOR_PWR",
    value = "1kohm",
    package = "0402",
    P1 = pwr2.vcc,
    P2 = pwr2.gnd,
)

Resistor(
    name = "SENSOR_EN",
    value = "10kohm",
    package = "0402",
    P1 = pwr2.enable,
    P2 = pwr2.gnd,
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn interface_nested_template() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Nested interface templates
PowerNets = interface(
    vcc = Net("VCC"),
    gnd = Net("GND")
)

# Create a pre-configured power instance
usb_power = PowerNets("USB")

# Use as template in another interface
Device = interface(
    power = usb_power,
    data_p = Net("D+"),
    data_n = Net("D-")
)

# Create device instance
dev = Device("PORT1")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Wire up components
Resistor(
    name = "VBUS_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = dev.power.vcc,
    P2 = dev.power.gnd,
)

Resistor(
    name = "DP_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = dev.data_p,
    P2 = dev.power.gnd,
)

Resistor(
    name = "DN_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = dev.data_n,
    P2 = dev.power.gnd,
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn interface_template_property_inheritance() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test that net names are properly copied from templates
SignalInterface = interface(
    clk = Net("CLK"),
    data = Net("DATA"),
    valid = Net("VALID")
)

# Create multiple instances
bus1 = SignalInterface("CPU")
bus2 = SignalInterface("MEM")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Connect them
Resistor(
    name = "CPU_CLK_DATA",
    value = "1kohm",
    package = "0402",
    P1 = bus1.clk,
    P2 = bus1.data,
)

Resistor(
    name = "CPU_VALID_DATA",
    value = "1kohm",
    package = "0402",
    P1 = bus1.valid,
    P2 = bus1.data,
)

Resistor(
    name = "MEM_CLK_DATA",
    value = "1kohm",
    package = "0402",
    P1 = bus2.clk,
    P2 = bus2.data,
)

Resistor(
    name = "MEM_VALID_DATA",
    value = "1kohm",
    package = "0402",
    P1 = bus2.valid,
    P2 = bus2.data,
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn interface_mixed_templates_and_types() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Mix of templates and regular types
MixedInterface = interface(
    # Template nets without properties
    power = Net("VDD"),
    ground = Net("VSS"),
    # Regular net type
    signal = Net,
    # Nested interface template
    control = interface(
        enable = Net("EN"),
        reset = Net("RST")
    )()
)

# Create instance
mixed = MixedInterface("CHIP")
Resistor = Module("@stdlib/generics/Resistor.zen")

# Use all the nets
Resistor(
    name = "POWER_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = mixed.power,
    P2 = mixed.ground,
)

Resistor(
    name = "SIGNAL_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = mixed.signal,
    P2 = mixed.ground,
)

Resistor(
    name = "CONTROL_LOAD",
    value = "1kohm",
    package = "0402",
    P1 = mixed.control.enable,
    P2 = mixed.control.reset,
)
"#,
    );

    star_snapshot!(env, "test.zen");
}
