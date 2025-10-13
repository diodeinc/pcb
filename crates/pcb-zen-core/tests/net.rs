#[macro_use]
mod common;

snapshot_eval!(net_with_symbol, {
    "test.zen" => r#"
        # Create a symbol
        my_symbol = Symbol(
            name = "TestSymbol",
            definition = [
                ("VCC", ["1"]),
            ]
        )

        # Create a net with a symbol
        power_net = Net("POWER", symbol = my_symbol)

        # Print the net directly
        print("Power net:", power_net)
    "#
});

snapshot_eval!(net_without_symbol, {
    "test.zen" => r#"
        # Create a net without a symbol
        ground_net = Net("GND")

        # Print the net directly
        print("Ground net:", ground_net)
    "#
});

snapshot_eval!(net_symbol_from_library, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test.zen" => r#"
        # Load a symbol from a library file
        lib_symbol = Symbol(library = "C146731.kicad_sym")

        # Create a net with the library symbol
        data_net = Net("DATA", symbol = lib_symbol)

        # Print the net directly
        print("Data net:", data_net)
    "#
});

snapshot_eval!(net_rejects_invalid_symbol, {
    "test.zen" => r#"
        # Try to create a net with an invalid symbol (should fail)
        Net("TEST", symbol = "not a symbol")
    "#
});

snapshot_eval!(net_symbol_deep_copy, {
    "test.zen" => r#"
        # Create a symbol and a net
        sym1 = Symbol(name = "Symbol1", definition = [("A", ["1"])])
        
        net1 = Net("NET1", symbol = sym1)
        
        # Create another net with the same symbol
        net2 = Net("NET2", symbol = sym1)
        
        # Print both nets
        print("Net1:", net1)
        print("Net2:", net2)
    "#
});

snapshot_eval!(net_name_property_access, {
    "test.zen" => r#"
        # Test accessing the name property on Net instances
        
        # Create nets with different names
        net1 = Net("POWER_3V3")
        net2 = Net("GND")
        
        # Access and print the name property
        print("net1.name:", net1.name)
        print("net2.name:", net2.name)
        
        # Verify the name property matches what was passed to Net()
        check(net1.name == "POWER_3V3", "net1.name should be 'POWER_3V3'")
        check(net2.name == "GND", "net2.name should be 'GND'")
    "#
});

snapshot_eval!(net_duplicate_names_uniq, {
    "test.zen" => r#"
        # Two same-named nets at the same level should uniquify to EN and EN_2
        en1 = Net("EN")
        en2 = Net("EN")

        Component(
            name = "U1",
            footprint = "TEST:0402",
            pin_defs = {"EN": "1"},
            pins = {"EN": en1},
        )

        Component(
            name = "U2",
            footprint = "TEST:0402",
            pin_defs = {"EN": "1"},
            pins = {"EN": en2},
        )

        print("en1:", en1.name)
        print("en2:", en2.name)
    "#,
});

snapshot_eval!(net_field_with_field_spec, {
    "test.zen" => r#"
        # Create a net type with field() specs
        Power = builtin.net("Power", voltage=field(str, "3.3V"))
        
        # Create instances with different voltages
        vcc = Power("VCC", voltage="5V")
        vdd = Power("VDD", voltage="3.3V")
        
        # Access field values
        print("vcc.voltage:", vcc.voltage)
        print("vdd.voltage:", vdd.voltage)
        
        check(vcc.voltage == "5V", "vcc.voltage should be '5V'")
        check(vdd.voltage == "3.3V", "vdd.voltage should be '3.3V'")
    "#
});

snapshot_eval!(net_field_with_direct_type, {
    "test.zen" => r#"
        # Create a net type with direct type constructor
        Signal = builtin.net("Signal", frequency=int)
        
        # Create instance
        clk = Signal("CLK", frequency=8000000)
        
        print("clk.frequency:", clk.frequency)
        check(clk.frequency == 8000000, "clk.frequency should be 8000000")
    "#
});

snapshot_eval!(net_field_type_mismatch, {
    "test.zen" => r#"
        # Create a net type with string field
        Power = builtin.net("Power", voltage=str)
        
        # This should fail - providing int instead of str
        vcc = Power("VCC", voltage=123)
    "#
});

snapshot_eval!(net_field_default_applied, {
    "test.zen" => r#"
        # Create a net type with defaulted field
        Power = builtin.net("Power", voltage=field(str, "3.3V"))
        
        # Create instance without providing voltage - should get default
        vcc = Power("VCC")
        
        print("vcc.voltage:", vcc.voltage)
        check(vcc.voltage == "3.3V", "vcc.voltage should use default '3.3V'")
    "#
});

snapshot_eval!(net_field_with_enum, {
    "test.zen" => r#"
        # Create enum and net type with enum field
        Level = enum("LOW", "HIGH")
        Signal = builtin.net("Signal", level=Level)
        
        # Create instances
        sig1 = Signal("SIG1", level=Level("HIGH"))
        sig2 = Signal("SIG2", level=Level("LOW"))
        
        print("sig1.level:", sig1.level)
        check(sig1.level == Level("HIGH"), "sig1.level should be HIGH")
    "#
});

snapshot_eval!(net_field_with_physical_value, {
    "test.zen" => r#"
        load("@stdlib/units.zen", "Voltage", "unit")
        
        # Create net type with physical value field
        Power = builtin.net("Power", voltage=Voltage)
        
        # Create instance
        vcc = Power("VCC", voltage=unit("5V", Voltage))
        
        print("vcc.voltage:", vcc.voltage)
    "#
});

snapshot_eval!(net_field_multiple_fields, {
    "test.zen" => r#"
        # Create net type with multiple fields of different types
        Power = builtin.net("Power", 
            voltage=field(str, "3.3V"),
            max_current=field(int, 1000),
            regulated=field(bool, True)
        )
        
        # Create instance overriding some defaults
        vcc = Power("VCC", voltage="5V", max_current=2000)
        
        print("vcc.voltage:", vcc.voltage)
        print("vcc.max_current:", vcc.max_current)
        print("vcc.regulated:", vcc.regulated)
        
        check(vcc.voltage == "5V", "voltage override should work")
        check(vcc.max_current == 2000, "max_current override should work")
        check(vcc.regulated == True, "regulated default should apply")
    "#
});

snapshot_eval!(interface_net_template_naming, {
    "test.zen" => r#"
        # Test single-net interface naming behavior with name conflicts
        
        # Create a regular net with same name as interface instance
        net = Net("VCC")
        
        # Define single-net interface
        Power = interface(
            NET = using(Net("VCC")),
        )
        
        # Create power interface instance - with single-net naming, uses instance name directly
        power = Power("POWER")
        
        print("regular net:", net.name)
        print("interface net:", power.NET.name)
        
        # Check that single-net interface uses instance name directly
        check(power.NET.name == "POWER", "Single-net interface should use instance name directly")
    "#,
});
