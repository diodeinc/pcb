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

# Create component to generate netlist
Component(
    name = "R1",
    type = "resistor",
    pin_defs = {"1": "1", "2": "2"},
    footprint = "SMD:0805",
    pins = {"1": instance.test, "2": Net("GND")},
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

# Create components
Component(
    name = "U1",
    type = "mcu",
    pin_defs = {"VCC": "1", "GND": "2", "EN": "3"},
    footprint = "QFN:32",
    pins = {
        "VCC": pwr1.vcc,
        "GND": pwr1.gnd,
        "EN": pwr1.enable,
    }
)

Component(
    name = "U2",
    type = "sensor",
    pin_defs = {"VDD": "1", "VSS": "2", "ENABLE": "3"},
    footprint = "SOT:23-6",
    pins = {
        "VDD": pwr2.vcc,
        "VSS": pwr2.gnd,
        "ENABLE": pwr2.enable,
    }
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

# Wire up components
Component(
    name = "J1",
    type = "usb_connector",
    pin_defs = {"VBUS": "1", "D+": "2", "D-": "3", "GND": "4"},
    footprint = "USB:TYPE-C",
    pins = {
        "VBUS": dev.power.vcc,
        "D+": dev.data_p,
        "D-": dev.data_n,
        "GND": dev.power.gnd,
    }
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

# Connect them
Component(
    name = "CPU",
    type = "processor",
    pin_defs = {"CLK": "1", "DATA": "2", "VALID": "3"},
    footprint = "BGA:256",
    pins = {
        "CLK": bus1.clk,
        "DATA": bus1.data,
        "VALID": bus1.valid,
    }
)

Component(
    name = "MEM",
    type = "memory",
    pin_defs = {"CLK": "1", "DATA": "2", "VALID": "3"},
    footprint = "TSOP:48",
    pins = {
        "CLK": bus2.clk,
        "DATA": bus2.data,
        "VALID": bus2.valid,
    }
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

# Use all the nets
Component(
    name = "IC1",
    type = "asic",
    pin_defs = {
        "VDD": "1",
        "VSS": "2",
        "SIG": "3",
        "EN": "4",
        "RST": "5"
    },
    footprint = "QFN:48",
    pins = {
        "VDD": mixed.power,
        "VSS": mixed.ground,
        "SIG": mixed.signal,
        "EN": mixed.control.enable,
        "RST": mixed.control.reset,
    }
)
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn interface_using_promotion_targets() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# Test using() promotion target serialization
Power = interface(
    NET = using(Net("VCC")),
    voltage = field(str, "3.3V"),
)

Uart = interface(
    TX = Net("UART_TX"),
    RX = Net("UART_RX"),
)

Usart = interface(
    uart = using(Uart()),
    CK = Net("USART_CK"),
    RTS = Net("USART_RTS"),
    CTS = Net("USART_CTS"),
)

# Create instances to trigger serialization
power = Power("MAIN")
uart = Uart("DEBUG") 
usart = Usart("COMM")

# Print serialized forms to capture promotion_by_type
print("=== Serialized Interface Data ===")
print("Power serialized:")
print(serialize(power))
print("UART serialized:")
print(serialize(uart))
print("USART serialized:")
print(serialize(usart))
print("=== End Serialized Data ===")

# Use in components to generate serialization
Component(
    name = "U1",
    type = "regulator", 
    pin_defs = {"VIN": "1", "VOUT": "2", "GND": "3"},
    footprint = "SOT:23-5",
    pins = {
        "VOUT": power.NET,
        "GND": Net("GND"),
        "VIN": Net("VIN"),
    }
)

Component(
    name = "U2", 
    type = "mcu",
    pin_defs = {"TX": "1", "RX": "2", "CK": "3", "RTS": "4", "CTS": "5"},
    footprint = "QFN:32",
    pins = {
        "TX": usart.uart.TX,
        "RX": usart.uart.RX, 
        "CK": usart.CK,
        "RTS": usart.RTS,
        "CTS": usart.CTS,
    }
)
"#,
    );

    star_snapshot!(env, "test.zen");
}
