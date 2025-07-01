#[macro_use]
mod common;

snapshot_eval!(io_config, {
    "Module.star" => r#"
        pwr = io("pwr", Net)
        baud = config("baud", int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.star" => r#"
        load(".", "Module")

        Module(
            name = "U1",
            pwr = Net("VCC"),
            baud = 9600,
        )
    "#
});

snapshot_eval!(missing_required_io_config, {
    "Module.star" => r#"
        pwr = io("pwr", Net)
        baud = config("baud", int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.star" => r#"
        load(".", "Module")

        Module(
            name = "U1",
            # intentionally omit `pwr` and `baud` - should trigger an error
        )
    "#
});

snapshot_eval!(optional_io_config, {
    "Module.star" => r#"
        pwr = io("pwr", Net, optional = True)
        baud = config("baud", int, optional = True)

        # The io() should be default-initialized, and the config() should be None.
        check(pwr != None, "pwr should not be None when omitted")
        check(baud == None, "baud should be None when omitted")

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": Net("")},
        )
    "#,
    "top.star" => r#"
        load(".", "Module")

        Module(
            name = "U1",
            # omit both inputs - allowed because they are optional
        )
    "#
});

snapshot_eval!(interface_io, {
    "Module.star" => r#"
        Power = interface(vcc = Net)
        PdmMic = interface(power = Power, data = Net, select = Net, clock = Net)

        pdm = io("pdm", PdmMic)
    "#,
    "top.star" => r#"
        load(".", "Module")

        pdm = Module.PdmMic("PDM")
        Module(name = "U1", pdm = pdm)
    "#
});

snapshot_eval!(io_interface_incompatible, {
    "Module.star" => r#"
        signal = io("signal", Net)
    "#,
    "parent.star" => r#"
        load(".", "Module")

        SingleNet = interface(signal = Net)
        sig_if = SingleNet("SIG")

        Module(name="U1", signal=sig_if)  # Should fail - interface not accepted for Net io
    "#
});

snapshot_eval!(config_str, {
    "test.star" => r#"
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
    "test.star" => r#"
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
    "Module.star" => r#"
        Direction = enum("NORTH", "SOUTH")

        heading = config("heading", Direction)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = { "V": "1" },
            pins = { "V": Net("VCC") },
        )
    "#,
    "top.star" => r#"
        load(".", "Module")

        Module(
            name = "child",
            heading = "NORTH",
        )
    "#
});

snapshot_eval!(interface_net_incompatible, {
    "Module.star" => r#"
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
    "Module.star" => r#"
        MyInterface = interface(test = Net("MYTEST", prop=True, value="123"))
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
    "test.star" => r#"
        Power = interface(
            vcc = Net("3V3", voltage="3.3V", type="power"),
            gnd = Net("GND", type="ground"),
            enable = Net("EN", active="high")
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
    "test.star" => r#"
        # Nested interface templates
        PowerNets = interface(
            vcc = Net("VCC", type="power"),
            gnd = Net("GND", type="ground")
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

snapshot_eval!(interface_template_property_inheritance, {
    "test.star" => r#"
        # Test that properties are properly copied from templates
        SignalInterface = interface(
            clk = Net("CLK", frequency="100MHz", type="clock"),
            data = Net("DATA", width="8", direction="bidirectional"),
            valid = Net("VALID", active="high")
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
    "#
});

snapshot_eval!(interface_mixed_templates_and_types, {
    "test.star" => r#"
        # Mix of templates and regular types
        MixedInterface = interface(
            # Template nets with properties
            power = Net("VDD", voltage="1.8V"),
            ground = Net("VSS", type="ground"),
            # Regular net type
            signal = Net,
            # Nested interface template
            control = interface(
                enable = Net("EN"),
                reset = Net("RST", active="low")
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

snapshot_eval!(config_with_convert_function, {
    "Module.star" => r#"
        # Define a record type for units
        UnitType = record(
            value = field(float),
            unit = field(str),
        )

        # Define a converter function that parses strings like "5V" into the record
        def parse_unit(s):
            if type(s) == "string":
                # Simple parser: extract number and unit
                import_value = ""
                import_unit = ""
                for c in s.elems():
                    if c.isdigit() or c == ".":
                        import_value += c
                    else:
                        import_unit += c

                if import_value and import_unit:
                    return UnitType(value = float(import_value), unit = import_unit)
            return s

        # Test 1: config with converter should accept string and convert to record
        # Provide a default since records require defaults
        voltage = config("voltage", UnitType, default = UnitType(value = 0.0, unit = "V"), convert = parse_unit)

        # Test 2: config with converter and default value that needs conversion
        # The default string should be converted when no value is provided
        current = config("current", UnitType, default = "2.5A", convert = parse_unit)

        # Test 3: optional config with converter
        optional_power = config("power", UnitType, convert = parse_unit, optional = True)

        # Add properties to verify the values
        add_property("voltage_value", voltage.value)
        add_property("voltage_unit", voltage.unit)
        add_property("current_value", current.value)
        add_property("current_unit", current.unit)
        add_property("optional_power_is_none", optional_power == None)
    "#,
    "top.star" => r#"
        load(".", "Module")

        # Provide string input that should be converted
        m = Module(
            name = "test",
            voltage = "5V",
            # current uses default "2.5A" which should be converted
            # power is optional and not provided
        )
    "#
});

snapshot_eval!(config_without_convert_fails_type_check, {
    "Module.star" => r#"
        UnitType = record(
            value = field(float),
            unit = field(str),
        )

        # This should fail because "5V" is not a record and no converter is provided
        # Provide a default since records require defaults
        voltage = config("voltage", UnitType, default = UnitType(value = 0.0, unit = "V"))
    "#,
    "top.star" => r#"
        load(".", "Module")

        # This should fail - string cannot be used for record type without converter
        m = Module(
            name = "test",
            voltage = "5V",
        )
    "#
});

snapshot_eval!(config_convert_with_default, {
    "Module.star" => r#"
        def int_to_string(x):
            # Convert int to string with prefix
            return "value_" + str(x)

        # Config with default that needs conversion - int to string
        name = config("name", str, default = 42, convert = int_to_string)

        # Verify the default was converted by adding it as a property
        add_property("name_value", name)
    "#,
    "top.star" => r#"
        load(".", "Module")

        # Don't provide input, so default is used and converted
        m = Module(name = "test")
    "#
});

snapshot_eval!(config_convert_preserves_correct_types, {
    "Module.star" => r#"
        UnitType = record(
            value = field(float),
            unit = field(str),
        )

        converter_called = [False]  # Use list to allow mutation in nested function

        def tracking_converter(x):
            # This converter tracks if it was called
            converter_called[0] = True
            return x

        # If we pass a proper record, the converter should not be invoked
        # Provide a default since records require defaults
        voltage = config("voltage", UnitType, default = UnitType(value = 0.0, unit = "V"), convert = tracking_converter)

        # Add properties to verify behavior
        add_property("converter_called", converter_called[0])
        add_property("voltage_value", voltage.value)
        add_property("voltage_unit", voltage.unit)
    "#,
    "top.star" => r#"
        load(".", "Module")

        # Create a proper record value
        unit_value = Module.UnitType(value = 5.0, unit = "V")

        # Pass the correct type - converter should not be called
        m = Module(
            name = "test",
            voltage = unit_value,
        )
    "#
});

snapshot_eval!(config_convert_chain, {
    "Module.star" => r#"
        def parse_number(s):
            if type(s) == "string":
                return float(s)
            return s

        def multiply_by_two(x):
            return x * 2

        def composed_converter(s):
            return multiply_by_two(parse_number(s))

        # String "5" -> 5.0 -> 10.0
        value = config("value", float, convert = composed_converter)

        # Add property to verify the conversion
        add_property("converted_value", value)
    "#,
    "top.star" => r#"
        load(".", "Module")

        # Provide string that will be converted through the chain
        m = Module(
            name = "test",
            value = "5",
        )
    "#
});

snapshot_eval!(config_convert_with_enum, {
    "Module.star" => r#"
        # Define an enum type
        Direction = enum("NORTH", "SOUTH", "EAST", "WEST")

        def direction_converter(s):
            # Convert string to enum variant
            if type(s) == "string":
                # Call the enum factory with the uppercase string
                return Direction(s.upper())
            return s

        # Config that converts string to enum
        heading = config("heading", Direction, convert = direction_converter)

        # Add property to verify conversion
        add_property("heading_is_north", heading == Direction("NORTH"))
    "#,
    "top.star" => r#"
        load(".", "Module")

        # Provide lowercase string that should be converted to enum
        m = Module(
            name = "test",
            heading = "north",
        )
    "#
});
