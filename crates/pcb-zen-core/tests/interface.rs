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

snapshot_eval!(interface_using_promotion_serialization, {
    "test.zen" => r#"
        # Test using() promotion target serialization
        
        # Power interface with using() Net field
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        # UART interface without using() (baseline)
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        # USART interface with using() Interface field
        Usart = interface(
            uart = using(Uart()),
            CK = Net("USART_CK"),
        )
        
        # Complex interface with multiple using() fields of different types (no conflicts)
        ComplexInterface = interface(
            uart_field = using(Uart()),     # Direct: Uart
            power_field = using(Power()),   # Direct: Power, Transitive: Net via power_field.NET
            regular_net = Net("REGULAR"),   # Regular field, no promotion
            config = field(str, "default"), # Regular field, no promotion
        )
        
        # Create instances
        power = Power("MAIN")
        uart = Uart("DEBUG")
        usart = Usart("COMM")
        complex = ComplexInterface("COMPLEX")
        
        # Test serialization output
        print("=== Power Interface (Net promotion) ===")
        print(serialize(power))
        
        print("=== UART Interface (no promotion) ===")
        print(serialize(uart))
        
        print("=== USART Interface (Interface promotion) ===")
        print(serialize(usart))
        
        print("=== Complex Interface (multiple promotions) ===")
        print(serialize(complex))
    "#
});

snapshot_eval!(interface_transitive_promotion, {
    "test.zen" => r#"
        # Test transitive promotion path discovery and conflict detection
        
        # Base interfaces
        Control = interface(
            reset = using(Net("RESET")),
        )
        
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        # Valid transitive promotion - no conflicts
        ValidSystem = interface(
            power = using(Power()),    # Direct: Power, Transitive: Net via power.NET
            uart = using(Uart()),      # Direct: Uart (no conflicts)
        )
        
        # Create instances and test serialization
        control = Control("CTRL")
        power = Power("PWR") 
        uart = Uart("UART")
        system = ValidSystem("SYS")
        
        print("=== Control Interface ===")
        print(serialize(control))
        
        print("=== Power Interface (with transitive to Control and Net) ===")
        print(serialize(power))
        
        print("=== UART Interface (no promotions) ===")
        print(serialize(uart))
        
        print("=== System Interface (multiple transitive paths) ===")
        print(serialize(system))
    "#
});

snapshot_eval!(interface_using_chain_validation, {
    "test.zen" => r#"
        # Test using() chain validation - only complete using() chains create promotion paths
        
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        # Define Uart interface first
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        # Test 1: Complete using() chain - should create transitive promotion
        CompleteChain = interface(
            power = using(Power()),     # using() → Power → NET using() → Net
            uart = using(Uart()),
        )
        
        # Test 2: Broken chain - regular field blocks transitive promotion
        BrokenChain = interface(
            power = Power(),            # Regular field - NO transitive promotion
            uart = using(Uart()),
        )
        
        # Create instances
        complete = CompleteChain("COMPLETE")
        broken = BrokenChain("BROKEN")
        
        print("=== Complete using() Chain ===")
        print(serialize(complete))
        
        print("=== Broken Chain (regular field) ===") 
        print(serialize(broken))
    "#
});

snapshot_eval!(interface_using_direct_conflict_error, {
    "test.zen" => r#"
        # Test direct conflict detection - should error
        BadInterface = interface(
            net1 = using(Net("NET1")),
            net2 = using(Net("NET2")),  # CONFLICT: Two using() fields for same type
        )
    "#
});

snapshot_eval!(interface_using_transitive_conflict_error, {
"test.zen" => r#"
# Test transitive conflict detection - should error
Power = interface(
NET = using(Net("VCC")),
)

ConflictInterface = interface(
power = using(Power()),         # Transitive Net via power.NET
direct_net = using(Net("GND")), # CONFLICT: Direct Net path
)
"#
});

snapshot_eval!(interface_using_deep_recursion, {
    "test.zen" => r#"
        # Test deep recursion in promotion path discovery
        
        # Level 1: Basic interface with using()
        Level1 = interface(
            net = using(Net("LEVEL1")),
        )
        
        # Level 2: Interface that uses Level1
        Level2 = interface(
            level1 = using(Level1()),
        )
        
        # Level 3: Interface that uses Level2  
        Level3 = interface(
            level2 = using(Level2()),
        )
        
        # This should create transitive promotion: Level3 → Net via "level2.level1.net"
        l3_factory = Level3
        l3_instance = Level3("L3")
        
        print("=== Level3 Factory ===")
        print(serialize(l3_factory))
        
        print("=== Level3 Instance ===")
        print(serialize(l3_instance))
    "#
});

snapshot_eval!(interface_using_serialization_roundtrip, {
    "test.zen" => r#"
        # Test that using() information survives serialization and actual deserialization
        
        # Create interface with using() 
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        System = interface(
            power = using(Power()),
            uart = using(Uart()),
        )
        
        # Create original instance
        original = System("ORIGINAL")
        print("=== Original Instance ===")
        print(serialize(original))
        
        # ACTUAL SERIALIZATION/DESERIALIZATION ROUNDTRIP
        serialized = serialize(original)
        deserialized = deserialize(System, serialized)
        
        print("=== Deserialized Instance ===")
        print(serialize(deserialized))
        
        # Verify they behave the same way
        print("=== Comparison ===")
        print("Original:", original)
        print("Deserialized:", deserialized)
        
        # Test that factories still work
        new_from_original = System("NEW_ORIGINAL")
        new_from_deserialized = System("NEW_DESERIALIZED")
        
        print("=== Both factories still work ===")
        print("New from original factory:", serialize(new_from_original))
        print("New from deserialized factory:", serialize(new_from_deserialized))
    "#
});

snapshot_eval!(interface_using_deserialization_roundtrip, {
    "test.zen" => r#"
        # Test complex deserialization with deep using() chains
        
        # Level 1: Basic interface with using()
        Level1 = interface(
            net = using(Net("LEVEL1")),
        )
        
        # Level 2: Interface that uses Level1
        Level2 = interface(
            level1 = using(Level1()),
        )
        
        # Uart interface
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        # Level 3: Interface that uses Level2 plus a different type 
        Level3 = interface(
            level2 = using(Level2()),
            uart = using(Uart()),
        )
        
        # Create and serialize complex instance
        original = Level3("DEEP")
        print("=== Original Deep Instance ===")
        print(serialize(original))
        
        # REAL DESERIALIZATION TEST
        serialized = serialize(original)
        deserialized = deserialize(Level3, serialized)
        
        print("=== Deserialized Deep Instance ===")
        print(serialize(deserialized))
        
        # Test that the complex promotion paths are preserved
        print("=== Verification ===")
        print("Original display:", original)
        print("Deserialized display:", deserialized)
        
        # Create new instances from both to verify factory integrity
        new_original = Level3("NEW_FROM_ORIGINAL")
        new_deserialized = Level3("NEW_FROM_DESERIALIZED")
        
        print("=== Factory Integrity Check ===")
        print("New from original:", serialize(new_original))
        print("New from deserialized:", serialize(new_deserialized))
    "#
});

snapshot_eval!(interface_promotion_deserialization, {
    "test.zen" => r#"
        # Test automatic promotion during deserialization
        
        # Basic interface with Net promotion
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        # Interface with interface promotion  
        Uart = interface(
            TX = Net("UART_TX"),
            RX = Net("UART_RX"),
        )
        
        Usart = interface(
            uart = using(Uart()),
            CK = Net("USART_CK"),
        )
        
        print("=== Test 1: Net promotion ===")
        power = Power()
        power_ser = serialize(power)
        
        net_de = deserialize(Net, power_ser)
        print("Power -> Net:", net_de)
        
        print("=== Test 2: Interface promotion ===")
        usart = Usart()
        usart_ser = serialize(usart)
        
        uart_de = deserialize(Uart, usart_ser)
        print("Usart -> Uart:", uart_de)
        
        print("=== Test 3: Deep transitive promotion ===")
        # Create deep nesting
        Level1 = interface(net = using(Net("L1")))
        Level2 = interface(level1 = using(Level1()))
        Level3 = interface(level2 = using(Level2()))
        
        l3 = Level3()
        l3_ser = serialize(l3)
        
        # Should be able to deserialize as any level
        l2_de = deserialize(Level2, l3_ser)
        l1_de = deserialize(Level1, l3_ser) 
        net_de = deserialize(Net, l3_ser)
        
        print("Level3 -> Level2:", l2_de)
        print("Level3 -> Level1:", l1_de)
        print("Level3 -> Net:", net_de)
    "#
});

snapshot_eval!(interface_promotion_error_messages, {
    "test.zen" => r#"
        # Test error messages for invalid promotion targets
        
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
        )
        
        # Create test data
        usart = Usart()
        usart_ser = serialize(usart)
        
        # This should fail with helpful error message
        invalid = deserialize(Power, usart_ser)
    "#
});

snapshot_eval!(interface_anonymous_interface_error, {
    "test.zen" => r#"
        # Test error for anonymous interface usage in using()
        
        # Create a proper interface that would use an anonymous one
        System = interface(
            power = using(interface(NET = using(Net("VCC")))),  # Anonymous interface should error
        )
        
        # This should fail when the interface tries to get promotion key
        system = System()
    "#
});

snapshot_eval!(interface_comprehensive_net_promotion, {
    "test.zen" => r#"
        # Comprehensive Net promotion testing
        
        # Basic Net promotion
        Power = interface(
            NET = using(Net("VCC")),
            voltage = field(str, "3.3V"),
        )
        
        power = Power()
        net_from_power = deserialize(Net, serialize(power))
        print("Basic Net promotion - Power -> Net:", net_from_power)
        
        # Deep Net promotion through multiple levels
        Level1 = interface(
            net = using(Net("DEEP")),
            meta = field(str, "level1"),
        )
        
        Level2 = interface(
            level1 = using(Level1()),
            extra = field(int, 42),
        )
        
        Level3 = interface(
            level2 = using(Level2()),
        )
        
        l3 = Level3()
        print("Deep promotion test - Level3 instance:", l3)
        
        # Should extract Net via path "level2.level1.net"
        deep_net = deserialize(Net, serialize(l3))
        print("Deep Net promotion - Level3 -> Net:", deep_net)
        
        # Intermediate level promotions
        l2_from_l3 = deserialize(Level2, serialize(l3))
        l1_from_l3 = deserialize(Level1, serialize(l3))
        
        print("Intermediate promotions:")
        print("  Level3 -> Level2:", l2_from_l3)  
        print("  Level3 -> Level1:", l1_from_l3)
        
        # Verify same Net is extracted at all levels
        l2_net = deserialize(Net, serialize(l2_from_l3))
        l1_net = deserialize(Net, serialize(l1_from_l3))
        
        print("Net consistency check:")
        print("  Direct from Level3:", deep_net)
        print("  Via Level2:", l2_net)
        print("  Via Level1:", l1_net)
    "#
});

snapshot_eval!(interface_field_validation_errors, {
    "test.zen" => r#"
        # Test interface field validation error messages
        
        Ground = interface(
            NET = Net("GND")
        )
        
        Power = interface(
            NET = Net("VCC")
        )
        
        Gpio = interface(
            NET = Net("GPIO")
        )
        
        Uart = interface(
            TX = Net("TX"),
            RX = Net("RX"),
        )
        
        System = interface(
            power = Ground(),
            uart = Uart(),
        )
        
        # Valid cases first
        gnd = Ground("MAIN_GND")
        gpio_valid = Gpio("GPIO1", NET=gnd.NET)  # Net -> Net: valid
        print("Valid case:", gpio_valid.NET.name)
        
        system_valid = System("SYS1", power=gnd)  # Ground -> Ground: valid
        print("Valid system:", system_valid.power.NET.name)
        
        # Test error case: Interface where Net expected
        bad_gpio = Gpio("BAD", NET=gnd)  # Ground -> Net: error
    "#
});

snapshot_eval!(interface_field_validation_mixed_types, {
    "test.zen" => r#"
        # Test validation: string where Net expected
        
        Gpio = interface(NET = Net("GPIO"))
        
        # This should error: string where Net expected  
        bad_gpio = Gpio("BAD", NET="not_a_net")
    "#
});

snapshot_eval!(interface_field_validation_net_to_interface, {
    "test.zen" => r#"
        # Test error when Net provided where Interface expected
        
        Ground = interface(NET = Net("GND"))
        System = interface(power = Ground())
        
        net = Net("SOME_NET")
        
        # This should error: Net where Ground interface expected
        bad_system = System("BAD", power=net)
    "#
});
