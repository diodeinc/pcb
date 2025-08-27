#[macro_use]
mod common;

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
        load(".", "Module")

        Module(
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
        load(".", "Module")

        Module(
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
            pins = {"V": Net("")},
        )
    "#,
    "top.zen" => r#"
        load(".", "Module")

        Module(
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
        load(".", "Module")

        pdm = Module.PdmMic("PDM")
        Module(name = "U1", pdm = pdm)
    "#
});

snapshot_eval!(io_interface_incompatible, {
    "Module.zen" => r#"
        signal = io("signal", Net)
    "#,
    "parent.zen" => r#"
        load(".", "Module")

        SingleNet = interface(signal = Net)
        sig_if = SingleNet("SIG")

        Module(name="U1", signal=sig_if)  # Should fail - interface not accepted for Net io
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
        load(".", "Module")

        Module(
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

snapshot_eval!(config_with_convert_function, {
    "Module.zen" => r#"
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
    "top.zen" => r#"
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
        load(".", "Module")

        # This should fail - string cannot be used for record type without converter
        m = Module(
            name = "test",
            voltage = "5V",
        )
    "#
});

snapshot_eval!(config_convert_with_default, {
    "Module.zen" => r#"
        def int_to_string(x):
            # Convert int to string with prefix
            return "value_" + str(x)

        # Config with default that needs conversion - int to string
        name = config("name", str, default = 42, convert = int_to_string)

        # Verify the default was converted by adding it as a property
        add_property("name_value", name)
    "#,
    "top.zen" => r#"
        load(".", "Module")

        # Don't provide input, so default is used and converted
        m = Module(name = "test")
    "#
});

snapshot_eval!(config_convert_preserves_correct_types, {
    "Module.zen" => r#"
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
    "top.zen" => r#"
        MyModule = Module("./Module.zen")

        # Create a proper record value
        unit_value = MyModule.UnitType(value = 5.0, unit = "V")

        # Pass the correct type - converter should not be called
        MyModule(
            name = "test",
            voltage = unit_value,
        )
    "#
});

snapshot_eval!(config_convert_chain, {
    "Module.zen" => r#"
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
    "top.zen" => r#"
        load(".", "Module")

        # Provide string that will be converted through the chain
        m = Module(
            name = "test",
            value = "5",
        )
    "#
});

snapshot_eval!(config_convert_with_enum, {
    "Module.zen" => r#"
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
    "top.zen" => r#"
        load(".", "Module")

        # Provide lowercase string that should be converted to enum
        m = Module(
            name = "test",
            heading = "north",
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
        
        # Config with converter and help
        def parse_voltage(s):
            if type(s) == "string" and s.endswith("V"):
                return float(s[:-1])
            return s
        
        voltage = config("voltage", float, default = 3.3, convert = parse_voltage, help = "Operating voltage in volts")
        
        # Add a component to make the module valid
        Component(
            name = "test",
            footprint = "TEST:0402",
            pin_defs = {"PWR": "1", "GND": "2"},
            pins = {"PWR": power, "GND": Net("GND")},
        )
    "#,
    "top.zen" => r#"
        load(".", "Module")
        
        # Create module instance with some parameters
        Module(
            name = "U1",
            power = Net("VCC"),
            baud_rate = 115200,
            device_name = "TestDevice",
            voltage = "5V",  # This will be converted to 5.0
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

snapshot_eval!(record_enum_deserialization, {
    "test.zen" => r#"
        # Test record with enum field serialization/deserialization
        
        # Define enum and record types
        UnitEnum = enum("V", "A", "K")
        UnitRecord = record(
            value = field(float),
            unit = field(UnitEnum, UnitEnum("V")),
            tolerance = field(float, 0.0)
        )
        
        # Create a record instance
        voltage = UnitRecord(value=3.3, unit=UnitEnum("V"), tolerance=0.05)
        print("Original voltage record:", voltage)
        
        # Test serialize/deserialize round-trip
        serialized = serialize(voltage)
        print("Serialized JSON:", serialized)
        deserialized = deserialize(UnitRecord, serialized)
        print("Deserialized voltage:", deserialized)
        
        # Verify the deserialized values match
        check(deserialized.value == voltage.value, "Value should match")
        check(str(deserialized.unit) == str(voltage.unit), "Unit should match")
        check(deserialized.tolerance == voltage.tolerance, "Tolerance should match")
        
        # Test with different enum variant
        current = UnitRecord(value=0.1, unit=UnitEnum("A"))
        print("Original current record:", current)
        curr_serialized = serialize(current)
        print("Current serialized JSON:", curr_serialized)
        curr_deserialized = deserialize(UnitRecord, curr_serialized)
        print("Deserialized current:", curr_deserialized)
        
        check(curr_deserialized.value == 0.1, "Current value should be 0.1")
        check(str(curr_deserialized.unit) == "UnitEnum(\"A\")", "Current unit should be A")
        check(curr_deserialized.tolerance == 0.0, "Default tolerance should be 0.0")
    "#
});

snapshot_eval!(record_enum_metadata_integration, {
    "test.zen" => r#"
        # Test record with enum fields in metadata system
        
        # Define types
        TempEnum = enum("K", "C", "F")  
        TempRecord = record(
            value = field(float),
            unit = field(TempEnum, TempEnum("K")),
            tolerance = field(float, 0.0)
        )
        
        # Create metadata container
        temp_metadata = metadata(TempRecord)
        
        # Store multiple temperature records
        temp1 = TempRecord(value=25.0, unit=TempEnum("K"), tolerance=0.1)
        temp2 = TempRecord(value=100.0, unit=TempEnum("C"))  # Use default tolerance
        
        push_metadata(temp_metadata, temp1)
        push_metadata(temp_metadata, temp2)
        
        # Retrieve and verify - values are already deserialized
        latest_temp = get_metadata(temp_metadata)
        all_temps = list_metadata(temp_metadata)
        
        print("Latest temperature (auto-deserialized):", latest_temp)
        print("Latest temp value:", latest_temp.value)
        print("Latest temp unit:", latest_temp.unit)
        print("All temperatures:", all_temps)
        print("Number of temperatures:", len(all_temps))
        
        # Verify latest temperature (already deserialized)
        check(latest_temp.value == 100.0, "Latest temp value should be 100.0")
        check(str(latest_temp.unit) == "TempEnum(\"C\")", "Latest temp unit should be C")
        
        # Verify all temperatures (already deserialized)
        check(len(all_temps) == 2, "Should have 2 temperature records")
        check(all_temps[0].value == 25.0, "First temp should be 25.0")
        check(str(all_temps[0].unit) == "TempEnum(\"K\")", "First temp unit should be K")
        check(all_temps[1].value == 100.0, "Second temp should be 100.0")
        check(str(all_temps[1].unit) == "TempEnum(\"C\")", "Second temp unit should be C")
    "#
});

snapshot_eval!(comprehensive_serialization_test, {
    "test.zen" => r#"
        # Test comprehensive serialize/deserialize functionality
        print("=== Comprehensive Serialization Test ===")
        
        # Test 1: Simple types serialization
        print("\n1. Testing simple types...")
        
        # String
        str_val = "hello world"
        str_json = serialize(str_val)
        str_deser = deserialize(str, str_json)
        check(str_val == str_deser, "String serialize/deserialize should work")
        print("String:", str_val, "->", str_json, "->", str_deser)
        
        # Int
        int_val = 42
        int_json = serialize(int_val)
        int_deser = deserialize(int, int_json)
        check(int_val == int_deser, "Int serialize/deserialize should work")
        print("Int:", int_val, "->", int_json, "->", int_deser)
        
        # Float
        float_val = 3.14159
        float_json = serialize(float_val)
        float_deser = deserialize(float, float_json)
        check(float_val == float_deser, "Float serialize/deserialize should work")
        print("Float:", float_val, "->", float_json, "->", float_deser)
        
        # Bool
        bool_val = True
        bool_json = serialize(bool_val)
        bool_deser = deserialize(bool, bool_json)
        check(bool_val == bool_deser, "Bool serialize/deserialize should work")
        print("Bool:", bool_val, "->", bool_json, "->", bool_deser)
        
        # List
        list_val = [1, 2, "test", True]
        list_json = serialize(list_val)
        list_deser = deserialize(list, list_json)
        check(list_val == list_deser, "List serialize/deserialize should work")
        print("List length:", len(list_val), "==", len(list_deser))
        
        # Dict
        dict_val = {"key1": "value1", "key2": 42, "key3": False}
        dict_json = serialize(dict_val)
        dict_deser = deserialize(dict, dict_json)
        check(dict_val == dict_deser, "Dict serialize/deserialize should work")
        print("Dict keys:", len(dict_val), "==", len(dict_deser))
        
        print("✓ Simple types serialization works")
        
        # Test 2: Record with enum serialization
        print("\n2. Testing record with enum fields...")
        
        ColorEnum = enum("RED", "GREEN", "BLUE")
        ItemRecord = record(
            name = field(str),
            color = field(ColorEnum, ColorEnum("RED")),
            count = field(int, 1),
            active = field(bool, True)
        )
        
        item = ItemRecord(name="Widget", color=ColorEnum("BLUE"), count=5, active=False)
        item_json = serialize(item)
        item_deser = deserialize(ItemRecord, item_json)
        
        print("Original item:", item)
        print("Serialized:", item_json)
        print("Deserialized:", item_deser)
        
        check(item.name == item_deser.name, "Item name should match")
        check(str(item.color) == str(item_deser.color), "Item color should match")
        check(item.count == item_deser.count, "Item count should match")
        check(item.active == item_deser.active, "Item active should match")
        
        print("✓ Record with enum serialization works")
        
        # Test 3: Metadata with mixed types
        print("\n3. Testing metadata with mixed types...")
        
        # Simple type metadata
        str_meta = metadata(str)
        int_meta = metadata(int)
        
        # Record type metadata
        item_meta = metadata(ItemRecord)
        
        # Store values
        push_metadata(str_meta, "metadata test")
        push_metadata(int_meta, 999)
        push_metadata(item_meta, item)
        
        # Retrieve (already deserialized)
        latest_str = get_metadata(str_meta)
        latest_int = get_metadata(int_meta)
        latest_item = get_metadata(item_meta)
        
        print("Retrieved string:", latest_str)
        print("Retrieved int:", latest_int)
        print("Retrieved item:", latest_item)
        print("Retrieved item name:", latest_item.name)
        print("Retrieved item color:", latest_item.color)
        
        check(latest_str == "metadata test", "String metadata should work")
        check(latest_int == 999, "Int metadata should work")
        check(latest_item.name == item.name, "Item metadata should preserve fields")
        check(str(latest_item.color) == str(item.color), "Item metadata should preserve enum fields")
        
        print("✓ Mixed type metadata works")
        
        print("\n=== All Serialization Tests Passed! ===")
    "#
});

snapshot_eval!(complex_record_enum_fields, {
    "test.zen" => r#"
        # Test record with multiple enum fields
        
        DirectionEnum = enum("NORTH", "SOUTH", "EAST", "WEST")
        StatusEnum = enum("ACTIVE", "INACTIVE", "ERROR")
        
        DeviceRecord = record(
            name = field(str),
            direction = field(DirectionEnum, DirectionEnum("NORTH")),
            status = field(StatusEnum, StatusEnum("ACTIVE")),
            value = field(float, 0.0)
        )
        
        # Create and test device record
        device = DeviceRecord(
            name="Sensor1",
            direction=DirectionEnum("EAST"),
            status=StatusEnum("ACTIVE"),
            value=42.5
        )
        
        # Test serialization round-trip
        serialized = serialize(device)
        deserialized = deserialize(DeviceRecord, serialized)
        
        # Verify all fields
        check(deserialized.name == "Sensor1", "Name should match")
        check(str(deserialized.direction) == "DirectionEnum(\"EAST\")", "Direction should be EAST")
        check(str(deserialized.status) == "StatusEnum(\"ACTIVE\")", "Status should be ACTIVE")
        check(deserialized.value == 42.5, "Value should be 42.5")
        
        # Test metadata storage - values are already deserialized
        device_metadata = metadata(DeviceRecord)
        push_metadata(device_metadata, device)
        
        retrieved = get_metadata(device_metadata)  # Already returns deserialized record
        print("Retrieved device from metadata:", retrieved)
        print("Retrieved device name:", retrieved.name)
        print("Retrieved device direction:", retrieved.direction)
        print("Retrieved device status:", retrieved.status)
        print("Retrieved device value:", retrieved.value)
        
        check(retrieved.name == device.name, "Retrieved name should match")
        check(str(retrieved.direction) == str(device.direction), "Retrieved direction should match")
        check(str(retrieved.status) == str(device.status), "Retrieved status should match")
    "#
});

snapshot_eval!(serialize_deserialize_simple_types, {
    "test.zen" => r#"
        # Dedicated serialize/deserialize tests for simple types (no metadata)
        print("=== Simple Types Serialize/Deserialize Test ===")
        
        # Test 1: String types
        print("\n1. Testing string serialization...")
        test_strings = ["hello", "world with spaces", "unicode: ñ", "", "123"]
        for s in test_strings:
            json_str = serialize(s)
            restored = deserialize(str, json_str)
            check(s == restored, "String " + s + " should round-trip correctly")
            print("String:", repr(s), "->", json_str, "->", repr(restored))
        
        # Test 2: Numeric types
        print("\n2. Testing numeric types...")
        test_ints = [0, 1, -1, 42, -999, 2147483647]
        for i in test_ints:
            json_str = serialize(i)
            restored = deserialize(int, json_str)
            check(i == restored, "Int " + str(i) + " should round-trip correctly")
            print("Int:", i, "->", json_str, "->", restored)
        
        test_floats = [0.0, 1.0, -1.0, 3.14159, -2.71828, 1e-10, 1e10]
        for f in test_floats:
            json_str = serialize(f)
            restored = deserialize(float, json_str)
            check(f == restored, "Float " + str(f) + " should round-trip correctly")
            print("Float:", f, "->", json_str, "->", restored)
        
        # Test 3: Boolean types
        print("\n3. Testing boolean types...")
        for b in [True, False]:
            json_str = serialize(b)
            restored = deserialize(bool, json_str)
            check(b == restored, "Bool " + str(b) + " should round-trip correctly")
            print("Bool:", b, "->", json_str, "->", restored)
        
        print("✓ Simple types serialize/deserialize correctly")
    "#
});

snapshot_eval!(serialize_deserialize_collections, {
    "test.zen" => r#"
        # Test serialize/deserialize for collections (no metadata)
        print("=== Collections Serialize/Deserialize Test ===")
        
        # Test 1: Lists
        print("\n1. Testing list serialization...")
        test_lists = [
            [],
            [1, 2, 3],
            ["a", "b", "c"],
            [True, False],
            [1, "mixed", 3.14, True],
            [[1, 2], [3, 4]],  # Nested lists
        ]
        
        for lst in test_lists:
            json_str = serialize(lst)
            restored = deserialize(list, json_str)
            check(lst == restored, "List should round-trip correctly")
            print("List length:", len(lst), "->", len(restored), "equal:", lst == restored)
        
        # Test 2: Dictionaries
        print("\n2. Testing dict serialization...")
        test_dicts = [
            {},
            {"key": "value"},
            {"a": 1, "b": 2, "c": 3},
            {"mixed": True, "types": 42, "here": 3.14},
            {"nested": {"inner": "value"}},
        ]
        
        for d in test_dicts:
            json_str = serialize(d)
            restored = deserialize(dict, json_str)
            check(d == restored, "Dict should round-trip correctly")
            print("Dict keys:", len(d), "->", len(restored), "equal:", d == restored)
        
        print("✓ Collections serialize/deserialize correctly")
    "#
});

snapshot_eval!(serialize_deserialize_enums, {
    "test.zen" => r#"
        # Test serialize/deserialize for enum types (no metadata)
        print("=== Enum Serialize/Deserialize Test ===")
        
        # Test 1: Basic enum
        print("\n1. Testing basic enum...")
        ColorEnum = enum("RED", "GREEN", "BLUE")
        
        for color_name in ["RED", "GREEN", "BLUE"]:
            color = ColorEnum(color_name)
            json_str = serialize(color)
            restored = deserialize(ColorEnum, json_str)
            
            check(str(color) == str(restored), "Enum " + color_name + " should round-trip correctly")
            print("Enum:", str(color), "->", json_str, "->", str(restored))
        
        # Test 2: Enum with numbers
        print("\n2. Testing enum with numbers...")
        StatusEnum = enum("STATUS_0", "STATUS_1", "STATUS_2")
        
        for status_name in ["STATUS_0", "STATUS_1", "STATUS_2"]:
            status = StatusEnum(status_name)
            json_str = serialize(status)
            restored = deserialize(StatusEnum, json_str)
            
            check(str(status) == str(restored), "Status enum should round-trip correctly")
            print("Status:", str(status), "->", json_str, "->", str(restored))
        
        print("✓ Enums serialize/deserialize correctly")
    "#
});

snapshot_eval!(serialize_deserialize_records, {
    "test.zen" => r#"
        # Test serialize/deserialize for record types (no metadata)
        print("=== Record Serialize/Deserialize Test ===")
        
        # Test 1: Simple record
        print("\n1. Testing simple record...")
        PersonRecord = record(
            name = field(str),
            age = field(int, 0),
            active = field(bool, True)
        )
        
        person = PersonRecord(name="Alice", age=30, active=True)
        json_str = serialize(person)
        restored = deserialize(PersonRecord, json_str)
        
        check(person.name == restored.name, "Person name should match")
        check(person.age == restored.age, "Person age should match")
        check(person.active == restored.active, "Person active should match")
        print("Person:", person, "->", str(len(json_str)), "chars ->", restored)
        
        # Test 2: Record with enum fields
        print("\n2. Testing record with enum fields...")
        TypeEnum = enum("TYPE_A", "TYPE_B", "TYPE_C")
        ItemRecord = record(
            id = field(int),
            name = field(str, "default"),
            type = field(TypeEnum, TypeEnum("TYPE_A")),
            value = field(float, 0.0)
        )
        
        item = ItemRecord(id=42, name="Test Item", type=TypeEnum("TYPE_B"), value=99.5)
        json_str = serialize(item)
        restored = deserialize(ItemRecord, json_str)
        
        check(item.id == restored.id, "Item ID should match")
        check(item.name == restored.name, "Item name should match") 
        check(str(item.type) == str(restored.type), "Item type should match")
        check(item.value == restored.value, "Item value should match")
        print("Item:", item)
        print("Restored:", restored)
        
        # Test 3: Record with defaults
        print("\n3. Testing record with defaults...")
        ConfigRecord = record(
            enabled = field(bool, False),
            count = field(int, 1),
            name = field(str, "unnamed")
        )
        
        config = ConfigRecord()  # Use all defaults
        json_str = serialize(config)
        restored = deserialize(ConfigRecord, json_str)
        
        check(config.enabled == restored.enabled, "Config enabled should match")
        check(config.count == restored.count, "Config count should match")
        check(config.name == restored.name, "Config name should match")
        print("Default config:", config, "->", restored)
        
        print("✓ Records serialize/deserialize correctly")
    "#
});

snapshot_eval!(serialize_deserialize_complex_types, {
    "test.zen" => r#"
        # Test serialize/deserialize for complex type combinations (no metadata)
        print("=== Complex Type Serialize/Deserialize Test ===")
        
        # Test 1: Record with list and dict fields
        print("\n1. Testing record with collection fields...")
        
        DataRecord = record(
            name = field(str, ""),
            tags = field(list, []),
            metadata = field(dict, {}),
            active = field(bool, True)
        )
        
        data = DataRecord(
            name="TestData",
            tags=["tag1", "tag2", "tag3"],
            metadata={"version": 1, "created": "2024-01-01"},
            active=True
        )
        
        json_str = serialize(data)
        restored = deserialize(DataRecord, json_str)
        
        check(data.name == restored.name, "Name should match")
        check(len(data.tags) == len(restored.tags), "Tags length should match")
        check(len(data.metadata) == len(restored.metadata), "Metadata length should match")
        check(data.active == restored.active, "Active should match")
        
        print("Original data:", data)
        print("Restored data:", restored)
        print("Tags match:", data.tags == restored.tags)
        print("Metadata match:", data.metadata == restored.metadata)
        
        # Test 2: Mixed type list serialization
        print("\n2. Testing mixed type lists...")
        
        mixed_list = [1, "hello", 3.14, True, [1, 2], {"key": "value"}]
        json_str = serialize(mixed_list)
        restored = deserialize(list, json_str)
        
        check(len(mixed_list) == len(restored), "Mixed list length should match")
        check(mixed_list == restored, "Mixed list content should match")
        
        print("Original mixed list:", mixed_list)
        print("Restored mixed list:", restored)
        print("Lists equal:", mixed_list == restored)
        
        # Test 3: Complex nested dictionaries
        print("\n3. Testing nested dictionaries...")
        
        complex_dict = {
            "level1": {
                "level2": {
                    "values": [1, 2, 3],
                    "flags": {"a": True, "b": False}
                },
                "count": 42
            },
            "simple": "value"
        }
        
        json_str = serialize(complex_dict)
        restored = deserialize(dict, json_str)
        
        check(complex_dict == restored, "Complex dict should match")
        
        print("Original dict keys:", len(complex_dict))
        print("Restored dict keys:", len(restored))
        print("Dicts equal:", complex_dict == restored)
        print("Nested level2 equal:", complex_dict["level1"]["level2"] == restored["level1"]["level2"])
        
        print("✓ Complex types serialize/deserialize correctly")
    "#
});

snapshot_eval!(enhanced_metadata_coverage, {
    "test.zen" => r#"
        # Enhanced metadata test coverage with comprehensive scenarios
        print("=== Enhanced Metadata Coverage Test ===")
        
        # Test 1: Multiple containers of same type
        print("\n1. Testing multiple containers of same type...")
        
        str_meta1 = metadata(str)
        str_meta2 = metadata(str)
        int_meta1 = metadata(int)
        int_meta2 = metadata(int)
        
        push_metadata(str_meta1, "container1_value1")
        push_metadata(str_meta1, "container1_value2")
        push_metadata(str_meta2, "container2_value1")
        
        push_metadata(int_meta1, 100)
        push_metadata(int_meta1, 200)
        push_metadata(int_meta2, 999)
        
        # Verify isolation between containers
        str1_latest = get_metadata(str_meta1)
        str2_latest = get_metadata(str_meta2)
        int1_all = list_metadata(int_meta1)
        int2_all = list_metadata(int_meta2)
        
        check(str1_latest == "container1_value2", "String container 1 should have correct latest")
        check(str2_latest == "container2_value1", "String container 2 should have correct latest")
        check(len(int1_all) == 2, "Int container 1 should have 2 values")
        check(len(int2_all) == 1, "Int container 2 should have 1 value")
        
        print("Container 1 latest string:", str1_latest)
        print("Container 2 latest string:", str2_latest)
        print("Container 1 int count:", len(int1_all))
        print("Container 2 int count:", len(int2_all))
        
        # Test 2: Mixed record and enum metadata
        print("\n2. Testing mixed record and enum metadata...")
        
        PriorityEnum = enum("LOW", "MEDIUM", "HIGH", "CRITICAL")
        TaskRecord = record(
            title = field(str, ""),
            priority = field(PriorityEnum, PriorityEnum("MEDIUM")),
            completed = field(bool, False),
            points = field(int, 1)
        )
        
        # Create containers
        task_meta = metadata(TaskRecord)
        
        # Store different tasks
        task1 = TaskRecord(title="Fix bug", priority=PriorityEnum("HIGH"), completed=False, points=3)
        task2 = TaskRecord(title="Write docs", priority=PriorityEnum("LOW"), completed=True, points=1)
        task3 = TaskRecord(title="Release", priority=PriorityEnum("CRITICAL"), points=5)
        
        push_metadata(task_meta, task1)
        push_metadata(task_meta, task2)
        push_metadata(task_meta, task3)
        
        # Retrieve and verify
        all_tasks = list_metadata(task_meta)
        latest_task = get_metadata(task_meta)
        
        print("Task count:", len(all_tasks))
        print("Latest task title:", latest_task.title)
        print("Latest task priority:", latest_task.priority)
        print("Latest task completed:", latest_task.completed)
        print("Latest task points:", latest_task.points)
        
        check(len(all_tasks) == 3, "Should have 3 tasks")
        check(latest_task.title == "Release", "Latest task should be Release")
        check(str(latest_task.priority) == "PriorityEnum(\"CRITICAL\")", "Latest task priority should be CRITICAL")
        
        # Test 3: Complex record with multiple enum fields
        print("\n3. Testing complex record with multiple enum fields...")
        
        StatusEnum = enum("PENDING", "PROCESSING", "COMPLETED", "FAILED")
        TypeEnum = enum("ORDER", "RETURN", "EXCHANGE")
        
        RequestRecord = record(
            id = field(int, 0),
            type = field(TypeEnum, TypeEnum("ORDER")),
            status = field(StatusEnum, StatusEnum("PENDING")),
            amount = field(float, 0.0),
            notes = field(str, "")
        )
        
        request_meta = metadata(RequestRecord)
        
        # Create various request types
        req1 = RequestRecord(id=1001, type=TypeEnum("ORDER"), status=StatusEnum("COMPLETED"), amount=150.0, notes="First order")
        req2 = RequestRecord(id=1002, type=TypeEnum("RETURN"), status=StatusEnum("PROCESSING"), amount=75.0, notes="Return item")
        req3 = RequestRecord(id=1003, type=TypeEnum("EXCHANGE"), amount=200.0)  # Uses default PENDING status
        
        push_metadata(request_meta, req1)
        push_metadata(request_meta, req2)
        push_metadata(request_meta, req3)
        
        all_requests = list_metadata(request_meta)
        latest_request = get_metadata(request_meta)
        
        print("Total requests:", len(all_requests))
        print("Latest request ID:", latest_request.id)
        print("Latest request type:", latest_request.type)
        print("Latest request status:", latest_request.status)
        
        # Verify each request preserved correctly
        check(all_requests[0].id == 1001, "First request ID should be 1001")
        check(str(all_requests[0].type) == "TypeEnum(\"ORDER\")", "First request should be ORDER")
        check(str(all_requests[0].status) == "StatusEnum(\"COMPLETED\")", "First request should be COMPLETED")
        
        check(all_requests[1].id == 1002, "Second request ID should be 1002")
        check(str(all_requests[1].type) == "TypeEnum(\"RETURN\")", "Second request should be RETURN")
        check(str(all_requests[1].status) == "StatusEnum(\"PROCESSING\")", "Second request should be PROCESSING")
        
        check(all_requests[2].id == 1003, "Third request ID should be 1003")
        check(str(all_requests[2].type) == "TypeEnum(\"EXCHANGE\")", "Third request should be EXCHANGE")
        check(str(all_requests[2].status) == "StatusEnum(\"PENDING\")", "Third request should use default PENDING")
        
        # Test 4: Empty metadata containers
        print("\n4. Testing empty metadata containers...")
        
        empty_str_meta = metadata(str)
        empty_task_meta = metadata(TaskRecord)
        
        empty_str_latest = get_metadata(empty_str_meta)
        empty_str_list = list_metadata(empty_str_meta)
        empty_task_latest = get_metadata(empty_task_meta)
        empty_task_list = list_metadata(empty_task_meta)
        
        check(empty_str_latest == None, "Empty string metadata should return None")
        check(len(empty_str_list) == 0, "Empty string metadata list should be empty")
        check(empty_task_latest == None, "Empty task metadata should return None")
        check(len(empty_task_list) == 0, "Empty task metadata list should be empty")
        
        print("Empty string latest:", empty_str_latest)
        print("Empty string list length:", len(empty_str_list))
        print("Empty task latest:", empty_task_latest)
        print("Empty task list length:", len(empty_task_list))
        
        print("\n✓ Enhanced metadata coverage complete!")
        print("✓ Multiple containers work independently")
        print("✓ Complex records with multiple enums supported")
        print("✓ Empty containers handle None correctly")
        print("✓ All metadata operations preserve type information")
    "#
});

snapshot_eval!(interface_metadata_integration, {
    "test.zen" => r#"
        # Test interface system integration with metadata containers
        print("=== Interface Metadata Integration Test ===")
        
        # Test 1: Interface with metadata container fields
        print("\n1. Creating interface with metadata containers...")
        
        PowerInterface = interface(
            # Regular fields
            label = field(str, ""),
            voltage = field(float, 0.0),
            
            # Metadata container fields
            voltage_history = metadata(float),
            status_log = metadata(str),
            measurements = metadata(dict)
        )
        
        print("✓ PowerInterface created with metadata containers")
        
        # Test 2: Create interface instance and use metadata
        print("\n2. Creating interface instance and using metadata...")
        
        power = PowerInterface(
            "POWER_RAIL",
            label="Main Power Rail",
            voltage=3.3
        )
        
        # Test metadata operations on interface fields
        push_metadata(power.voltage_history, 3.3)
        push_metadata(power.voltage_history, 5.0)
        push_metadata(power.voltage_history, 3.3)
        
        push_metadata(power.status_log, "Power rail initialized")
        push_metadata(power.status_log, "Voltage adjusted to 5.0V")
        push_metadata(power.status_log, "Voltage restored to 3.3V")
        
        push_metadata(power.measurements, {"timestamp": 1000, "current": 0.5})
        push_metadata(power.measurements, {"timestamp": 2000, "current": 0.7})
        
        print("✓ Metadata operations on interface fields work")
        
        # Test 3: Verify metadata access and retrieval
        print("\n3. Testing metadata retrieval...")
        
        latest_voltage = get_metadata(power.voltage_history)
        all_voltages = list_metadata(power.voltage_history)
        
        latest_status = get_metadata(power.status_log)
        all_status = list_metadata(power.status_log)
        
        latest_measurement = get_metadata(power.measurements)
        all_measurements = list_metadata(power.measurements)
        
        print("Latest voltage:", latest_voltage)
        print("All voltages:", all_voltages)
        print("Latest status:", latest_status)
        print("Status count:", len(all_status))
        print("Latest measurement:", latest_measurement)
        print("Measurement count:", len(all_measurements))
        
        check(latest_voltage == 3.3, "Latest voltage should be 3.3")
        check(len(all_voltages) == 3, "Should have 3 voltage entries")
        check(latest_status == "Voltage restored to 3.3V", "Latest status should be restore message")
        check(len(all_status) == 3, "Should have 3 status entries")
        check(len(all_measurements) == 2, "Should have 2 measurement entries")
        
        print("✓ Metadata retrieval from interface fields works")
        
        # Test 4: Mixed interface with nets and metadata
        print("\n4. Testing mixed interface with nets and metadata...")
        
        NetworkInterface = interface(
            # Net fields
            data_net = Net("DATA"),
            control_net = Net("CTRL"),
            
            # Metadata fields
            traffic_log = metadata(str),
            bandwidth_history = metadata(float),
            
            # Regular fields
            protocol = field(str, "TCP")
        )
        
        network = NetworkInterface(
            "NETWORK", 
            protocol="UDP"
        )
        
        # Test metadata on mixed interface
        push_metadata(network.traffic_log, "Connection established")
        push_metadata(network.bandwidth_history, 100.0)
        push_metadata(network.bandwidth_history, 150.0)
        
        net_traffic = get_metadata(network.traffic_log)
        bandwidth_list = list_metadata(network.bandwidth_history)
        
        print("Network traffic:", net_traffic)
        print("Bandwidth history:", bandwidth_list)
        print("Network protocol:", network.protocol)
        print("Data net name:", network.data_net.name)
        
        check(net_traffic == "Connection established", "Network traffic should match")
        check(len(bandwidth_list) == 2, "Should have 2 bandwidth entries")
        check(network.protocol == "UDP", "Protocol should be UDP")
        
        print("✓ Mixed interface with nets, metadata, and fields works")
        
        # Test 5: Interface inheritance with metadata
        print("\n5. Testing interface usage patterns...")
        
        # Test metadata containers as standalone fields work correctly
        standalone_meta = metadata(int)
        push_metadata(standalone_meta, 42)
        push_metadata(standalone_meta, 100)
        
        standalone_latest = get_metadata(standalone_meta)
        standalone_all = list_metadata(standalone_meta)
        
        print("Standalone metadata latest:", standalone_latest)
        print("Standalone metadata all:", standalone_all)
        
        check(standalone_latest == 100, "Standalone metadata should work")
        check(len(standalone_all) == 2, "Standalone metadata should have 2 entries")
        
        print("✓ Standalone metadata containers work alongside interfaces")
        
        print("\n=== Interface Metadata Integration Complete! ===")
        print("✓ Interfaces can contain metadata container fields")
        print("✓ Metadata operations work on interface fields")
        print("✓ Mixed interfaces with nets, metadata, and regular fields work")
        print("✓ Interface instances preserve metadata container functionality")
        print("✓ Metadata containers integrate seamlessly with existing interface system")
    "#
});
