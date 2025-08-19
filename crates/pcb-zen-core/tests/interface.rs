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

snapshot_eval!(interface_field_specifications, {
    "test.zen" => r#"
        # Interface with field() specifications (basic types only)
        ConfigInterface = interface(
            enable = field(bool, True),
            count = field(int, 42),
            label = field(str, "Default Config"),
            ratio = field(float, 3.14),
        )
        
        # Test defaults
        config1 = ConfigInterface("CFG1")
        print("Config1 enable:", config1.enable)
        print("Config1 count:", config1.count)
        print("Config1 label:", config1.label)
        print("Config1 ratio:", config1.ratio)
        
        # Test overrides
        config2 = ConfigInterface("CFG2", enable=False, count=100, ratio=2.71)
        print("Config2 enable:", config2.enable)
        print("Config2 count:", config2.count)
        print("Config2 label:", config2.label)
        print("Config2 ratio:", config2.ratio)
        
        # Test serialization
        print("--- Serialized JSON ---")
        print(serialize(config1))
    "#
});

snapshot_eval!(interface_post_init_callback, {
    "test.zen" => r#"
        # Interface with __post_init__ callback
        def validate_power(self):
            print("Validating power interface:", self.net.name)
            if self.net.name.endswith("_VCC"):
                print("Power validation: PASS")
            else:
                print("Power validation: FAIL - name should end with _VCC")
        
        PowerInterface = interface(
            net = Net("VCC"),
            __post_init__ = validate_power,
        )
        
        # Test post_init execution
        power1 = PowerInterface("MAIN")
        power2 = PowerInterface("CPU")
        
        # Test serialization of interface with __post_init__
        print("--- Serialized JSON ---")
        print(serialize(power1))
    "#
});

snapshot_eval!(interface_mixed_field_types, {
    "test.zen" => r#"
        # Interface mixing regular nets and field() specifications
        MixedInterface = interface(
            power = Net("VCC"),
            ground = Net("GND"), 
            enable_pin = Net(),  # Auto-generated name
            debug_mode = field(bool, False),
            voltage_level = field(str, "3.3V"),
        )
        
        # Test mixed instantiation
        mixed1 = MixedInterface("CTRL")
        print("Mixed power:", mixed1.power.name)
        print("Mixed ground:", mixed1.ground.name)
        print("Mixed enable_pin:", mixed1.enable_pin.name)
        print("Mixed debug_mode:", mixed1.debug_mode)
        print("Mixed voltage_level:", mixed1.voltage_level)
        
        # Test with overrides
        custom_power = Net("CUSTOM_VCC")
        mixed2 = MixedInterface("ALT", power=custom_power, debug_mode=True)
        print("Alt power:", mixed2.power.name)
        print("Alt debug_mode:", mixed2.debug_mode)
    "#
});

snapshot_eval!(interface_nested_composition, {
    "test.zen" => r#"
        # Test interface composition with UART/USART example
        
        # Basic UART interface
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        # USART interface that embeds UART
        Usart = interface(
            uart = Uart(TX=Net("USART_TX"), RX=Net("USART_RX")),  # Embedded UART instance
            CK = Net("USART_CK"),
            RTS = Net("USART_RTS"),
            CTS = Net("USART_CTS"),
        )
        
        # Test basic UART
        uart1 = Uart("MCU_UART")
        print("UART TX:", uart1.TX.name)
        print("UART RX:", uart1.RX.name)
        
        # Test USART composition
        usart1 = Usart("MCU_USART")
        print("USART embedded UART TX:", usart1.uart.TX.name)
        print("USART embedded UART RX:", usart1.uart.RX.name)
        print("USART CK:", usart1.CK.name)
        print("USART RTS:", usart1.RTS.name)
        print("USART CTS:", usart1.CTS.name)
        
        # Test with field() in composition
        EnhancedUsart = interface(
            uart = Uart(),  # Use default UART
            clock_source = field(str, "internal"),
            baud_rate = field(int, 115200),
            flow_control = field(bool, True),
        )
        
        eusart1 = EnhancedUsart("ENHANCED")
        print("Enhanced UART TX:", eusart1.uart.TX.name)
        print("Enhanced clock_source:", eusart1.clock_source)
        print("Enhanced baud_rate:", eusart1.baud_rate)
        print("Enhanced flow_control:", eusart1.flow_control)
        
        # Test serialization of nested composition
        print("--- USART Serialized JSON ---")
        print(serialize(usart1))
        
        print("--- Enhanced USART Serialized JSON ---")
        print(serialize(eusart1))
    "#
});

snapshot_eval!(interface_serialization_formats, {
    "test.zen" => r#"
        # Test serialization formats for different interface types
        
        # Simple Net
        simple_net = Net("SIMPLE_NET")
        print("=== Simple Net ===")
        print(serialize(simple_net))
        
        # Simple Interface
        Power = interface(NET = Net("VCC"))
        power = Power("PWR")
        print("=== Simple Interface ===")
        print(serialize(power))
        
        # Complex Interface with field() specs
        Config = interface(
            enable = field(bool, True),
            mode = field(str, "auto"),
            count = field(int, 10),
        )
        config = Config("TEST", enable=False)
        print("=== Complex Interface ===")
        print(serialize(config))
        
        # Nested Interface (just Power for simplicity)
        System = interface(
            power = Power(),
            debug = field(bool, False),
        )
        system = System("SYS")
        print("=== Nested Interface ===")
        print(serialize(system))
    "#
});
