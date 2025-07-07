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
