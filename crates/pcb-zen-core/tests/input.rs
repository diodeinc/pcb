#[macro_use]
mod common;

use crate::common::eval_zen;
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcb_zen_core::lang::io_direction::IoDirection;

snapshot_eval!(config_default_implies_optional_in_signature, {
    "test.zen" => r#"
        # No explicit optional, but default is provided.
        led_color = config("led_color", str, default = "green")
    "#
});

snapshot_eval!(config_optional_false_missing_emits_error_diagnostic, {
    "Module.zen" => r#"
        led_color = config("led_color", str, default = "green", optional = False)

        Component(
            name = "D1",
            footprint = "TEST:0402",
            pin_defs = {"A": "1", "K": "2"},
            pins = {"A": Net("VCC"), "K": Net("GND")},
            properties = {"color": led_color},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")
        Mod(name = "U1")
    "#
});

snapshot_eval!(io_config, {
    "Module.zen" => r#"
        pwr = io("pwr", Net)
        baud = config("baud", int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            pwr = Net("VCC"),
            baud = 9600,
        )
    "#
});

snapshot_eval!(missing_required_io_config, {
    "Module.zen" => r#"
        pwr = io("pwr", Net)
        baud = config("baud", int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            # intentionally omit `pwr` and `baud` - should trigger an error
        )
    "#
});

snapshot_eval!(optional_io_config, {
    "Module.zen" => r#"
        pwr = io("pwr", Net, optional = True)
        baud = config("baud", int, optional = True)

        # The io() should be default-initialized, and the config() should be None.
        check(pwr != None, "pwr should not be None when omitted")
        check(baud == None, "baud should be None when omitted")

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": Net("INTERNAL_V")},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            # omit both inputs - allowed because they are optional
        )
    "#
});

snapshot_eval!(interface_io, {
    "Module.zen" => r#"
        Power = interface(vcc = Net)
        PdmMic = interface(power = Power, data = Net, select = Net, clock = Net)

        pdm = io("pdm", PdmMic)
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        pdm = Mod.PdmMic("PDM")
        Mod(name = "U1", pdm = pdm)
    "#
});

#[test]
fn unused_io_warns_only_for_unconnected_ports() {
    let eval_result = eval_zen(vec![
        (
            "Leaf.zen".to_string(),
            r#"
                VIN = io("VIN", Net)

                Component(
                    name = "LOAD",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": VIN},
                )
            "#
            .to_string(),
        ),
        (
            "Wrapper.zen".to_string(),
            r#"
                Leaf = Module("Leaf.zen")

                Bus = interface(DATA = Net, CTRL = Net)

                VIN = io("VIN", Net)
                SPARE = io("SPARE", Net)
                BUS = io("BUS", Bus)
                UNUSED_BUS = io("UNUSED_BUS", Bus)

                Component(
                    name = "TAP",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": BUS.DATA},
                )

                Leaf(name = "LEAF", VIN = VIN)
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Wrapper = Module("Wrapper.zen")

                bus = Wrapper.Bus("BUS")
                unused_bus = Wrapper.Bus("UNUSED")

                Wrapper(
                    name = "WRAP",
                    VIN = Net("VIN"),
                    SPARE = Net("SPARE"),
                    BUS = bus,
                    UNUSED_BUS = unused_bus,
                )
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let sch_result = eval_output.to_schematic_with_diagnostics();

    assert!(
        !sch_result.diagnostics.has_errors(),
        "schematic conversion produced unexpected errors: {:?}",
        sch_result.diagnostics
    );

    let unused_io_bodies: Vec<String> = sch_result
        .diagnostics
        .iter()
        .filter(|diag| {
            diag.downcast_error_ref::<CategorizedDiagnostic>()
                .map(|categorized| categorized.kind == "module.io.unused")
                .unwrap_or(false)
        })
        .map(|diag| diag.body.clone())
        .collect();

    assert_eq!(
        unused_io_bodies.len(),
        2,
        "unexpected warnings: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .any(|body| body.contains("SPARE") && body.contains("WRAP")),
        "missing SPARE warning: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .any(|body| body.contains("UNUSED_BUS") && body.contains("WRAP")),
        "missing UNUSED_BUS warning: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies.iter().all(|body| !body.contains("VIN")),
        "forwarded VIN should not warn: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .all(|body| !body.starts_with("io() 'BUS'")),
        "partially used BUS interface should not warn: {unused_io_bodies:?}"
    );
}

snapshot_eval!(io_interface_incompatible, {
    "Module.zen" => r#"
        signal = io("signal", Net)
    "#,
    "parent.zen" => r#"
        Mod = Module("Module.zen")

        SingleNet = interface(signal = Net)
        sig_if = SingleNet("SIG")

        Mod(name="U1", signal=sig_if)  # Should fail - interface not accepted for Net io
    "#
});

snapshot_eval!(config_str, {
    "test.zen" => r#"
        value = config("value", str)

        # Use the string config
        Component(
            name = "test_comp",
            footprint = "test_footprint",
            pin_defs = {"in": "1", "out": "2"},
            pins = {
                "in": Net("1"),
                "out": Net("2")
            },
            properties = {
                "value": value
            }
        )
    "#
});

snapshot_eval!(config_types, {
    "test.zen" => r#"
        # Test various config() and io() declarations for signature generation

        # Basic types
        str_config = config("str_config", str)
        int_config = config("int_config", int)
        float_config = config("float_config", float)
        bool_config = config("bool_config", bool)

        # Optional configs with defaults
        opt_str = config("opt_str", str, optional=True, default="default_value")
        opt_int = config("opt_int", int, optional=True, default=42)
        opt_float = config("opt_float", float, optional=True, default=3.14)
        opt_bool = config("opt_bool", bool, optional=True, default=True)

        # Optional without defaults
        opt_no_default = config("opt_no_default", str, optional=True)

        # IO declarations
        net_io = io("net_io", Net)
        opt_net_io = io("opt_net_io", Net, optional=True)

        # Interface types
        Power = interface(vcc = Net, gnd = Net)
        power_io = io("power_io", Power)
        opt_power_io = io("opt_power_io", Power, optional=True)

        # Nested interface
        DataBus = interface(
            data = Net,
            clock = Net,
            enable = Net
        )
        bus_io = io("bus_io", DataBus)

        # Complex nested interface
        System = interface(
            power = Power,
            bus = DataBus,
            reset = Net
        )
        system_io = io("system_io", System)

        # Add a simple component to make the module valid
        Component(
            name = "test",
            type = "test_component",
            pin_defs = {"1": "1"},
            footprint = "TEST:FP",
            pins = {"1": Net("TEST")},
        )
    "#
});

snapshot_eval!(implicit_enum_conversion, {
    "Module.zen" => r#"
        Direction = enum("NORTH", "SOUTH")

        heading = config("heading", Direction)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = { "V": "1" },
            pins = { "V": Net("VCC") },
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "child",
            heading = "NORTH",
        )
    "#
});

snapshot_eval!(interface_net_incompatible, {
    "Module.zen" => r#"
        SingleNet = interface(signal = Net)

        signal_if = SingleNet(name="sig")

        Component(
            name = "test_comp",
            footprint = "test_footprint",
            pin_defs = {"in": "1", "out": "2"},
            pins = {
                "in": signal_if,  # This should fail - interfaces not accepted for pins
                "out": Net()
            }
        )
    "#
});

snapshot_eval!(interface_net_template_basic, {
    "Module.zen" => r#"
        MyInterface = interface(test = Net("MYTEST"))
        instance = MyInterface("PREFIX")

        Component(
            name = "R1",
            type = "resistor",
            pin_defs = {"1": "1", "2": "2"},
            footprint = "SMD:0805",
            pins = {"1": instance.test, "2": Net("GND")},
        )
    "#
});

snapshot_eval!(interface_multiple_net_templates, {
    "test.zen" => r#"
        Power = interface(
            vcc = Net("3V3"),
            gnd = Net("GND"),
            enable = Net("EN")
        )

        pwr1 = Power("MCU")
        pwr2 = Power("SENSOR")

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
    "#
});

snapshot_eval!(interface_nested_template, {
    "test.zen" => r#"
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
    "#
});

snapshot_eval!(interface_mixed_templates_and_types, {
    "test.zen" => r#"
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
    "#
});

snapshot_eval!(config_without_convert_fails_type_check, {
    "Module.zen" => r#"
        UnitType = record(
            value = field(float),
            unit = field(str),
        )

        # This should fail because "5V" is not a record and no converter is provided
        # Provide a default since records require defaults
        voltage = config("voltage", UnitType, default = UnitType(value = 0.0, unit = "V"))
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        # This should fail - string cannot be used for record type without converter
        Mod(
            name = "test",
            voltage = "5V",
        )
    "#
});

snapshot_eval!(io_config_with_help_text, {
    "Module.zen" => r#"
        # Test io() and config() with help parameter
        
        # IO with help text
        power = io("power", Net, help = "Main power supply net")
        data = io("data", Net, optional = True, help = "Optional data line")
        
        # Config with help text
        baud_rate = config("baud_rate", int, default = 9600, help = "Serial communication baud rate")
        device_name = config("device_name", str, help = "Human-readable device identifier")
        
        # Optional config with help
        debug_mode = config("debug_mode", bool, optional = True, help = "Enable debug logging")
        
        voltage = config("voltage", float, default = 3.3, help = "Operating voltage in volts")
        
        # Add a component to make the module valid
        Component(
            name = "test",
            footprint = "TEST:0402",
            pin_defs = {"PWR": "1", "GND": "2"},
            pins = {"PWR": power, "GND": Net("GND")},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")
        
        # Create module instance with some parameters
        Mod(
            name = "U1",
            power = Net("VCC"),
            baud_rate = 115200,
            device_name = "TestDevice",
            voltage = 5.0,
        )
    "#
});

snapshot_eval!(cfg_enum_value, {
    "Module.zen" => r#"
        # Test io() with enum value

        EnumType = enum("A", "B", "C")
        
        cfg = config("cfg", EnumType, default = "A")
        print(cfg)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")

        # Create module instance with some parameters
        MyModule(
            name = "U1",
            cfg = MyModule.EnumType("B"),
        )
    "#
});

snapshot_eval!(config_int_to_float_conversion, {
    "Module.zen" => r#"
        # Test automatic int to float conversion
        voltage = config("voltage", float)
        current = config("current", float, default = 1)  # int default should convert to float
        power = config("power", float, optional = True)
        
        # Verify the values are floats
        add_property("voltage_value", voltage)
        add_property("voltage_type", type(voltage))
        add_property("current_value", current) 
        add_property("current_type", type(current))
        
        # Test arithmetic to ensure they behave as floats
        add_property("voltage_divided", voltage / 2)
        add_property("current_multiplied", current * 1.5)
        
        # Optional power should be None when not provided
        add_property("power_is_none", power == None)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")
        
        # Provide integer values that should be converted to floats
        m = MyModule(
            name = "test",
            voltage = 5,      # int 5 should become float 5.0
            current = 2,      # int 2 should become float 2.0
            # power is not provided, should be None
        )
    "#
});

snapshot_eval!(config_mixed_numeric_types, {
    "Module.zen" => r#"
        # Test that float values remain floats and int values convert to float
        voltage1 = config("voltage1", float)
        voltage2 = config("voltage2", float) 
        voltage3 = config("voltage3", float, default = 0)  # int default
        
        # Verify all are floats
        add_property("v1_value", voltage1)
        add_property("v1_type", type(voltage1))
        add_property("v2_value", voltage2)
        add_property("v2_type", type(voltage2))
        add_property("v3_value", voltage3)
        add_property("v3_type", type(voltage3))
        
        # Test that float arithmetic works correctly
        add_property("sum", voltage1 + voltage2 + voltage3)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")
        
        m = MyModule(
            name = "test",
            voltage1 = 3.14,   # Already a float
            voltage2 = 10,     # Int that should convert to float
            # voltage3 uses default int 0 that should convert to float
        )
    "#
});

snapshot_eval!(io_invalid_type, {
    "test.zen" => r#"
        # io() should only accept NetType or InterfaceFactory, not primitive types
        value = io("value", int)
    "#
});

snapshot_eval!(config_string_to_physical_value, {
    "types.zen" => r#"
        Voltage = builtin.physical_value("V")
        Resistance = builtin.physical_value("Ω")
        Current = builtin.physical_value("A")
    "#,
    "child.zen" => r#"
        load("types.zen", "Voltage", "Resistance", "Current")

        voltage = config("voltage", Voltage)
        resistance = config("resistance", Resistance)
        current = config("current", Current)

        print("voltage:", voltage)
        print("resistance:", resistance)
        print("current:", current)
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        # Provide mixed scalar/string values that should be converted
        # through the PhysicalValue constructor path.
        Child(name = "test", voltage = "3.3V", resistance = 10000, current = 0.02)

        print("String to PhysicalValue conversion: success")
    "#
});

snapshot_eval!(config_string_to_physical_value_with_bounds, {
    "types.zen" => r#"
        Voltage = builtin.physical_value("V")
    "#,
    "child.zen" => r#"
        load("types.zen", "Voltage")

        voltage = config("voltage", Voltage)

        print("voltage:", voltage)
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        Child(name = "test", voltage = "3.0V to 3.6V")

        print("String to PhysicalValue (bounds) conversion: success")
    "#
});

#[test]
fn io_direction_appears_in_signature() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = io("VIN", Net, direction = "input")
            VOUT = io("VOUT", Net, direction = "output")
            BIDIR = io("BIDIR", Net)

            Component(
                name = "test",
                footprint = "TEST:0402",
                pin_defs = {"IN": "1", "OUT": "2", "IO": "3"},
                pins = {"IN": VIN, "OUT": VOUT, "IO": BIDIR},
            )
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let signature = eval_output.signature;

    let vin = signature
        .iter()
        .find(|param| param.name == "VIN")
        .expect("expected VIN in signature");
    assert_eq!(vin.direction, Some(IoDirection::Input));

    let vout = signature
        .iter()
        .find(|param| param.name == "VOUT")
        .expect("expected VOUT in signature");
    assert_eq!(vout.direction, Some(IoDirection::Output));

    let bidir = signature
        .iter()
        .find(|param| param.name == "BIDIR")
        .expect("expected BIDIR in signature");
    assert_eq!(bidir.direction, None);
}

#[test]
fn io_direction_rejects_invalid_values() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = io("VIN", Net, direction = "in")
        "#
        .to_string(),
    )]);

    assert!(
        eval_result.output.is_none(),
        "expected evaluation to fail for invalid direction"
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("io() direction must be \"input\" or \"output\"")),
        "expected invalid direction diagnostic, got: {:?}",
        eval_result.diagnostics
    );
}
