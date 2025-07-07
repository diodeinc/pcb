#[macro_use]
mod common;

snapshot_eval!(interface_net_symbol_copy, {
    "test.zen" => r#"
        # Create a symbol
        power_symbol = Symbol(
            name = "PowerSymbol",
            definition = [
                ("VCC", ["1"]),
                ("GND", ["2"])
            ]
        )

        # Create a net template with a symbol
        power_net_template = Net("POWER", symbol = power_symbol)

        # Create an interface using the net template
        PowerInterface = interface(
            power = power_net_template,
            ground = Net("GND")  # Net without symbol
        )

        # Instantiate the interface
        power_instance = PowerInterface("PWR")

        # Print everything
        print("Template net:", power_net_template)
        print("Instance power net:", power_instance.power)
        print("Instance ground net:", power_instance.ground)
    "#
});

snapshot_eval!(interface_nested_symbol_copy, {
    "test.zen" => r#"
        # Create symbols
        data_symbol = Symbol(
            name = "DataSymbol",
            definition = [("DATA", ["1", "2"])]
        )
        
        power_symbol = Symbol(
            name = "PowerSymbol",
            definition = [("VCC", ["1"]), ("GND", ["2"])]
        )

        # Create net templates
        data_net = Net("DATA", symbol = data_symbol)
        power_net = Net("POWER", symbol = power_symbol)

        # Create nested interfaces
        DataInterface = interface(
            data = data_net
        )
        
        SystemInterface = interface(
            data = DataInterface,
            power = power_net
        )

        # Instantiate
        system = SystemInterface("SYS")

        # Print the nets
        print("Data net:", system.data.data)
        print("Power net:", system.power)
    "#
});

snapshot_eval!(interface_multiple_instances_independent_symbols, {
    "test.zen" => r#"
        # Create a symbol
        io_symbol = Symbol(
            name = "IOSymbol",
            definition = [("IO", ["1"])]
        )

        # Create interface with net template
        IOInterface = interface(
            io = Net("IO", symbol = io_symbol)
        )

        # Create multiple instances
        io1 = IOInterface("IO1")
        io2 = IOInterface("IO2")

        # Print both instances
        print("IO1 net:", io1.io)
        print("IO2 net:", io2.io)
    "#
});

snapshot_eval!(interface_invoke_with_net_override, {
    "test.zen" => r#"
        # Create symbols
        default_symbol = Symbol(
            name = "DefaultSymbol",
            definition = [("A", ["1"])]
        )
        
        override_symbol = Symbol(
            name = "OverrideSymbol", 
            definition = [("B", ["2"])]
        )

        # Create interface with default net
        TestInterface = interface(
            signal = Net("DEFAULT", symbol = default_symbol)
        )

        # Instance with default
        default_instance = TestInterface("INST1")
        
        # Instance with override
        override_net = Net("OVERRIDE", symbol = override_symbol)
        override_instance = TestInterface("INST2", signal = override_net)

        # Print results
        print("Default instance net:", default_instance.signal)
        print("Override instance net:", override_instance.signal)
    "#
});
