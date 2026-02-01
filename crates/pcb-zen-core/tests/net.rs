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
        print("Data net symbol:", data_net.symbol)
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
        print("Net1 symbol:", net1.symbol)
        print("Net2:", net2)
        print("Net2 symbol:", net2.symbol)
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

snapshot_eval!(net_multiple_collisions, {
    "test.zen" => r#"
        # Multiple nets with the same name should all get unique suffixes
        a1 = Net("CLK")
        a2 = Net("CLK")
        a3 = Net("CLK")
        a4 = Net("CLK")

        print("a1:", a1.name)
        print("a2:", a2.name)
        print("a3:", a3.name)
        print("a4:", a4.name)
    "#,
});

snapshot_eval!(net_no_collision_different_names, {
    "test.zen" => r#"
        # Nets with different names should not trigger warnings
        clk = Net("CLK")
        rst = Net("RST")
        en = Net("EN")

        print("clk:", clk.name)
        print("rst:", rst.name)
        print("en:", en.name)
    "#,
});

snapshot_eval!(net_collision_in_child_module, {
    "child.zen" => r#"
        # Child module with duplicate nets
        sig1 = Net("SIG")
        sig2 = Net("SIG")

        print("child sig1:", sig1.name)
        print("child sig2:", sig2.name)
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")
        Child(name = "child1")
    "#,
});

snapshot_eval!(net_field_with_field_spec, {
    "test.zen" => r#"
        # Create a net type with field() specs
        Power = builtin.net_type("Power", voltage=field(str, "3.3V"))
        
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
        Signal = builtin.net_type("Signal", frequency=int)
        
        # Create instance
        clk = Signal("CLK", frequency=8000000)
        
        print("clk.frequency:", clk.frequency)
        check(clk.frequency == 8000000, "clk.frequency should be 8000000")
    "#
});

snapshot_eval!(net_field_type_mismatch, {
    "test.zen" => r#"
        # Create a net type with string field
        Power = builtin.net_type("Power", voltage=str)
        
        # This should fail - providing int instead of str
        vcc = Power("VCC", voltage=123)
    "#
});

snapshot_eval!(net_field_default_applied, {
    "test.zen" => r#"
        # Create a net type with defaulted field
        Power = builtin.net_type("Power", voltage=field(str, "3.3V"))
        
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
        Signal = builtin.net_type("Signal", level=Level)
        
        # Create instances
        sig1 = Signal("SIG1", level=Level("HIGH"))
        sig2 = Signal("SIG2", level=Level("LOW"))
        
        print("sig1.level:", sig1.level)
        check(sig1.level == Level("HIGH"), "sig1.level should be HIGH")
    "#
});

snapshot_eval!(net_field_with_physical_value, {
    "test.zen" => r#"
        Voltage = builtin.physical_value("V")
        
        # Create net type with physical value field
        Power = builtin.net_type("Power", voltage=Voltage)
        
        # Create instance
        vcc = Power("VCC", voltage=Voltage("5V"))
        
        print("vcc.voltage:", vcc.voltage)
    "#
});

snapshot_eval!(net_field_multiple_fields, {
    "test.zen" => r#"
        # Create net type with multiple fields of different types
        Power = builtin.net_type("Power", 
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
        # Test interface net naming - always includes field name with prefix
        
        # Create a regular net
        net = Net("VCC")
        
        # Define single-net interface
        Power = interface(
            NET = Net("VCC"),
        )
        
        # Create power interface instance - always suffixes field name
        power = Power("POWER")
        
        print("regular net:", net.name)
        print("interface net:", power.NET.name)
        
        # Check that interface includes field name with prefix
        check(power.NET.name == "POWER_VCC", "Interface net should include field name suffix")
    "#,
});

snapshot_eval!(net_type_cast_preserves_name_across_modules, {
    "interfaces.zen" => r#"
        # Typed net definitions for testing net type promotion
        Power = builtin.net_type("Power")
        Ground = builtin.net_type("Ground")
    "#,
    "component.zen" => r#"
        # Component expecting plain Net (not Power/Ground typed net)
        P1 = io("P1", Net)
        P2 = io("P2", Net)

        Component(
            name = "R",
            prefix = "R",
            footprint = "TEST:0402",
            pin_defs = {"P1": "1", "P2": "2"},
            pins = {"P1": P1, "P2": P2},
            properties = {"value": "10kOhm", "type": "resistor"},
        )
    "#,
    "child.zen" => r#"
        # Child module that receives Power/Ground typed nets and passes to component
        load("interfaces.zen", "Power", "Ground")
        
        io_V3V3 = io("io_V3V3", Power)
        io_GND = io("io_GND", Ground)
        
        Resistor = Module("component.zen")
        
        # This should trigger net type cast: Power/Ground -> Net
        # The net names should be preserved
        R1 = Resistor(name = "R1", P1 = io_V3V3, P2 = io_GND)
    "#,
    "test.zen" => r#"
        # Parent module that creates typed nets with specific names
        load("interfaces.zen", "Power", "Ground")
        
        Child = Module("child.zen")
        
        V3V3 = Power("3V3")
        GND = Ground("GND")
        
        print("Created Power:", V3V3.name)
        print("Created Ground:", GND.name)
        
        Child(name = "child", io_V3V3 = V3V3, io_GND = GND)
        
        # Verify net names are preserved (typed nets don't have field suffix)
        check(V3V3.name == "3V3", "Power net should be '3V3'")
        check(GND.name == "GND", "Ground net should be 'GND'")
    "#
});

snapshot_eval!(power_ground_have_default_symbols, {
    "interfaces.zen" => r#"
        Power = builtin.net_type(
            "Power",
            symbol=field(Symbol, default=Symbol(name="VCC", definition=[("VCC", ["1"])])),
            voltage=str,
        )

        Ground = builtin.net_type(
            "Ground",
            symbol=field(Symbol, default=Symbol(name="GND", definition=[("GND", ["1"])])),
        )

        Analog = builtin.net_type("Analog")
        Gpio = builtin.net_type("Gpio")
        Pwm = builtin.net_type("Pwm")
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "Power", "Ground", "Analog", "Gpio", "Pwm")

        # Test that Power has default symbol
        vcc = Power("VCC")
        print("Power net:", vcc)
        print("Power symbol:", vcc.symbol)
        check(vcc.symbol != None, "Power should have default symbol")

        # Test that Ground has default symbol
        gnd = Ground("GND")
        print("Ground net:", gnd)
        print("Ground symbol:", gnd.symbol)
        check(gnd.symbol != None, "Ground should have default symbol")

        # Test that Analog/Gpio/Pwm work (no default symbols expected)
        analog = Analog("ANALOG")
        gpio = Gpio("GPIO")
        pwm = Pwm("PWM")

        print("Analog net:", analog.name)
        print("Gpio net:", gpio.name)
        print("Pwm net:", pwm.name)
    "#
});

// =============================================================================
// NotConnected net type promotion tests
// =============================================================================
// NotConnected is the "universal donor" - it can promote to any other net type.
// Nothing can promote to NotConnected.

snapshot_eval!(not_connected_promotes_to_power, {
    "interfaces.zen" => r#"
        Power = builtin.net_type("Power")
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "Power")

        vcc = io("vcc", Power)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": vcc},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "NotConnected")

        Child = Module("child.zen")

        # NotConnected should promote to Power
        nc = NotConnected("NC")
        Child(name = "child", vcc = nc)

        print("NotConnected promotes to Power: success")
    "#
});

snapshot_eval!(not_connected_promotes_to_ground, {
    "interfaces.zen" => r#"
        Ground = builtin.net_type("Ground")
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "Ground")

        gnd = io("gnd", Ground)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": gnd},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "NotConnected")

        Child = Module("child.zen")

        # NotConnected should promote to Ground
        nc = NotConnected("NC")
        Child(name = "child", gnd = nc)

        print("NotConnected promotes to Ground: success")
    "#
});

snapshot_eval!(not_connected_promotes_to_net, {
    "interfaces.zen" => r#"
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        sig = io("sig", Net)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": sig},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "NotConnected")

        Child = Module("child.zen")

        # NotConnected should promote to Net
        nc = NotConnected("NC")
        Child(name = "child", sig = nc)

        print("NotConnected promotes to Net: success")
    "#
});

snapshot_eval!(not_connected_promotes_to_custom_type, {
    "interfaces.zen" => r#"
        Gpio = builtin.net_type("Gpio")
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "Gpio")

        gpio = io("gpio", Gpio)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": gpio},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "NotConnected")

        Child = Module("child.zen")

        # NotConnected should promote to any custom net type
        nc = NotConnected("NC")
        Child(name = "child", gpio = nc)

        print("NotConnected promotes to Gpio: success")
    "#
});

snapshot_eval!(net_cannot_promote_to_not_connected, {
    "interfaces.zen" => r#"
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "NotConnected")

        nc = io("nc", NotConnected)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": nc},
        )
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        # Net should NOT promote to NotConnected - this should fail
        sig = Net("SIG")
        Child(name = "child", nc = sig)
    "#
});

snapshot_eval!(power_cannot_promote_to_not_connected, {
    "interfaces.zen" => r#"
        Power = builtin.net_type("Power")
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "NotConnected")

        nc = io("nc", NotConnected)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": nc},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "Power")

        Child = Module("child.zen")

        # Power should NOT promote to NotConnected - this should fail
        vcc = Power("VCC")
        Child(name = "child", nc = vcc)
    "#
});

snapshot_eval!(ground_cannot_promote_to_not_connected, {
    "interfaces.zen" => r#"
        Ground = builtin.net_type("Ground")
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "NotConnected")

        nc = io("nc", NotConnected)

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": nc},
        )
    "#,
    "test.zen" => r#"
        load("interfaces.zen", "Ground")

        Child = Module("child.zen")

        # Ground should NOT promote to NotConnected - this should fail
        gnd = Ground("GND")
        Child(name = "child", nc = gnd)
    "#
});

snapshot_eval!(io_default_not_connected_promotes_to_net, {
    "interfaces.zen" => r#"
        NotConnected = builtin.net_type("NotConnected")
    "#,
    "child.zen" => r#"
        load("interfaces.zen", "NotConnected")

        # io() with optional=True and default=NotConnected() should promote to Net
        MH = io("MH", Net, optional = True, default = NotConnected("MH_NC"))

        Component(
            name = "R1",
            footprint = "TEST:0402",
            pin_defs = {"1": "1"},
            pins = {"1": MH},
        )
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        # Instantiate without providing MH - should use default NotConnected promoted to Net
        Child(name = "child")

        print("io() default NotConnected promotes to Net: success")
    "#
});
