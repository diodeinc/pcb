#[macro_use]
mod common;

snapshot_eval!(symbol_with_definition, {
    "test.zen" => r#"
        # Test creating a symbol with explicit definition
        sym = Symbol(
            name="MySymbol",
            definition=[
                ("SCL", ["A1", "A2"]),
                ("SDA", ["B1"]),
                ("VDD", ["C1", "C2", "C3"]),
                ("GND", ["D1"])
            ]
        )

        # Print the symbol for snapshot
        print(sym)
    "#
});

snapshot_eval!(symbol_duplicate_pad_error, {
    "test.zen" => r#"
        # Test that duplicate pad assignments are caught
        sym = Symbol(
            definition=[
                ("SCL", ["A1"]),
                ("SDA", ["A1"])  # This should error - A1 already assigned
            ]
        )
    "#
});

snapshot_eval!(symbol_invalid_definition_format, {
    "test.zen" => r#"
        # Test various invalid definition formats
        sym = Symbol(
            definition=[
                ("SCL", "A1")  # Should be a list, not a string
            ]
        )
    "#
});

snapshot_eval!(symbol_empty_pad_list, {
    "test.zen" => r#"
        # Test that empty pad lists are rejected
        sym = Symbol(
            definition=[
                ("SCL", [])  # Empty pad list
            ]
        )
    "#
});

snapshot_eval!(symbol_requires_parameter, {
    "test.zen" => r#"
        # Test that Symbol requires either definition or library parameter
        sym = Symbol()
    "#
});

snapshot_eval!(symbol_from_library_single, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test.zen" => r#"
        # Test loading a symbol from a library with a single symbol
        sym = Symbol(library="C146731.kicad_sym")
        
        # Verify we can access the pins using attribute access
        # Note: KiCad symbol pins map pad number -> signal name
        # So sym.1 would give us "ICLK" (the signal name for pad 1)
        # But we can't use numeric attributes, so we'll just print the symbol
        
        print(sym)
    "#
});

snapshot_eval!(symbol_from_library_missing_name, {
    "multi_symbol.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "Symbol1"
            (property "Reference" "U" (at 0 0 0))
            (symbol "Symbol1_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "A" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
        (symbol "Symbol2"
            (property "Reference" "U" (at 0 0 0))
            (symbol "Symbol2_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "B" (effects (font (size 1.27 1.27))))
                    (number "2" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test that multi-symbol libraries require a name parameter
        sym = Symbol(library="multi_symbol.kicad_sym")
    "#
});

snapshot_eval!(symbol_from_library_with_name, {
    "multi_symbol.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "Symbol1"
            (property "Reference" "U" (at 0 0 0))
            (symbol "Symbol1_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "A" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
        (symbol "Symbol2"
            (property "Reference" "U" (at 0 0 0))
            (symbol "Symbol2_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "B" (effects (font (size 1.27 1.27))))
                    (number "2" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test loading a specific symbol from a multi-symbol library
        sym = Symbol(library="multi_symbol.kicad_sym", name="Symbol2")
        
        # Print the symbol to verify it's Symbol2
        print(sym)
    "#
});

snapshot_eval!(symbol_tilde_pin_name, {
    "tilde_pins.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "TildePinTest"
            (property "Reference" "U" (at 0 0 0))
            (symbol "TildePinTest_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "~" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
                (pin output line (at 0 0 0) (length 2.54)
                    (name "OUT" (effects (font (size 1.27 1.27))))
                    (number "2" (effects (font (size 1.27 1.27))))
                )
                (pin power_in line (at 0 0 0) (length 2.54)
                    (name "~" (effects (font (size 1.27 1.27))))
                    (number "3" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test that pins with ~ as name use the pin number instead
        sym = Symbol(library="tilde_pins.kicad_sym")
        
        # Print the symbol to see the pin mapping
        # Pins with ~ name should show their number as the signal name
        print(sym)
    "#
});

snapshot_eval!(symbol_positional_library_name, {
    "power.kicad_sym" => r##"(kicad_symbol_lib
        (symbol "GND"
            (property "Reference" "#PWR" (at 0 0 0))
            (symbol "GND_0_1"
                (pin power_in line (at 0 0 0) (length 0) hide
                    (name "GND" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
        (symbol "VCC"
            (property "Reference" "#PWR" (at 0 0 0))
            (symbol "VCC_0_1"
                (pin power_in line (at 0 0 0) (length 0) hide
                    (name "VCC" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"##,
    "test.zen" => r#"
        # Test using positional argument with library:name format
        gnd = Symbol("power.kicad_sym:GND")
        print("GND symbol:", gnd)
        
        # Also test with VCC
        vcc = Symbol("power.kicad_sym:VCC")
        print("VCC symbol:", vcc)
    "#
});

snapshot_eval!(symbol_positional_library_only, {
    "single_symbol.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "SingleSymbol"
            (property "Reference" "U" (at 0 0 0))
            (symbol "SingleSymbol_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "IN" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test using positional argument with just library path (no colon)
        sym = Symbol("single_symbol.kicad_sym")
        print("Symbol:", sym)
    "#
});

snapshot_eval!(symbol_positional_with_named_conflict, {
    "test.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "TestSymbol"
            (property "Reference" "U" (at 0 0 0))
            (symbol "TestSymbol_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "A" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test that mixing positional library:name with named parameters causes error
        sym = Symbol("test.kicad_sym:TestSymbol", name="OtherName")
    "#
});

snapshot_eval!(symbol_positional_invalid_type, {
    "test.zen" => r#"
        # Test that non-string positional argument causes error
        sym = Symbol(123)
    "#
});

snapshot_eval!(symbol_positional_empty_name, {
    "test.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "TestSymbol"
            (property "Reference" "U" (at 0 0 0))
            (symbol "TestSymbol_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "A" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test library:name format with empty name after colon
        sym = Symbol("test.kicad_sym:")
    "#
});

snapshot_eval!(symbol_positional_multiple_colons, {
    "path:with:colons.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "TestSymbol"
            (property "Reference" "U" (at 0 0 0))
            (symbol "TestSymbol_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "A" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test that we use rfind to split on the last colon
        # This allows library paths to contain colons
        sym = Symbol("path:with:colons.kicad_sym:TestSymbol")
        print("Symbol:", sym)
    "#
});
