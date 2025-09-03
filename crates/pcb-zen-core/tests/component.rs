#[macro_use]
mod common;

snapshot_eval!(component_properties, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test_props.zen" => r#"
        Component(
            name = "U1",
            pins = {
                "ICLK": Net("ICLK"),
                "Q1": Net("Q1"),
                "Q2": Net("Q2"),
                "Q3": Net("Q3"),
                "Q4": Net("Q4"),
                "GND": Net("GND"),
                "VDD": Net("VDD"),
                "OE": Net("OE"),
            },
            symbol = Symbol(library = "C146731.kicad_sym", name = "NB3N551DG"),
            footprint = "SMD:0805",
            properties = {"CustomProp": "Value123"},
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

snapshot_eval!(component_with_symbol, {
    "test.zen" => r#"
        # Create a symbol
        i2c_symbol = Symbol(
            name="I2C",
            definition=[
                ("SCL", ["1"]),
                ("SDA", ["2"]),
                ("VDD", ["3"]),
                ("GND", ["4"])
            ]
        )
        
        # Create a component using the symbol
        Component(
            name = "I2C_Device",
            footprint = "SOIC-8",
            symbol = i2c_symbol,  # Use Symbol instead of pin_defs
            pins = {
                "SCL": Net("SCL"),
                "SDA": Net("SDA"),
                "VDD": Net("VDD"),
                "GND": Net("GND"),
            }
        )
    "#
});

snapshot_eval!(component_duplicate_pin_names, {
    "test_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "TestSymbol" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "U" (id 0) (at 0 0 0))
    (symbol "TestSymbol_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "TestSymbol_1_1"
      (pin input line (at -12.7 2.54 0) (length 2.54)
        (name "in" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin output line (at 12.7 0 180) (length 2.54)
        (name "out" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
      (pin input line (at -12.7 -2.54 0) (length 2.54)
        (name "in" (effects (font (size 1.27 1.27))))
        (number "3" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    "test.zen" => r#"
        Component(
            name = "test_comp",
            footprint = "test_footprint",
            symbol = Symbol(library = "./test_symbol.kicad_sym"),
            pins = {
                "in": Net("in"),
                "out": Net("out"),
            }
        )
    "#
});
