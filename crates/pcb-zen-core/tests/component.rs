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
    "duplicate_pins_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
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
            symbol = Symbol(library = "./duplicate_pins_symbol.kicad_sym"),
            pins = {
                "in": Net("in"),
                "out": Net("out"),
            }
        )
    "#
});

snapshot_eval!(component_with_manufacturer, {
    "test.zen" => r#"
        Component(
            name = "test_comp",
            footprint = "test_footprint",
            manufacturer = "test_manufacturer",
            pin_defs = {
                "in": "1",
                "out": "2",
            },
            pins = {
                "in": Net("in"),
                "out": Net("out"),
            },
        )
    "#
});

snapshot_eval!(component_manufacturer_from_symbol, {
    "test_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "TestSymbol" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "U" (id 0) (at 0 0 0))
    (property "Manufacturer_Name" "ACME Corp" (id 1) (at 0 0 0))
    (symbol "TestSymbol_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "TestSymbol_1_1"
      (pin input line (at -12.7 2.54 0) (length 2.54)
        (name "VCC" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin output line (at 12.7 0 180) (length 2.54)
        (name "GND" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
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
                "VCC": Net("VCC"),
                "GND": Net("GND"),
            }
        )
    "#
});

snapshot_eval!(component_with_dnp_kwarg, {
    "test.zen" => r#"
        Component(
            name = "test_comp_dnp",
            footprint = "test_footprint",
            dnp = True,
            pin_defs = {
                "in": "1",
                "out": "2",
            },
            pins = {
                "in": Net("in"),
                "out": Net("out"),
            },
        )
    "#
});

snapshot_eval!(component_inherits_reference_prefix, {
    "ic_symbol.kicad_sym" => r#"(kicad_symbol_lib
        (symbol "MyIC"
            (property "Reference" "IC" (at 0 0 0))
            (symbol "MyIC_0_1"
                (pin input line (at 0 0 0) (length 2.54)
                    (name "IN" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
                (pin output line (at 0 0 0) (length 2.54)
                    (name "OUT" (effects (font (size 1.27 1.27))))
                    (number "2" (effects (font (size 1.27 1.27))))
                )
            )
        )
    )"#,
    "test.zen" => r#"
        # Test that component inherits reference prefix "IC" from symbol
        # when no explicit prefix is provided
        comp1 = Component(
            name = "MyComponent1",
            footprint = "SOIC-8",
            symbol = Symbol(library = "ic_symbol.kicad_sym"),
            pins = {
                "IN": Net("in_signal"),
                "OUT": Net("out_signal"),
            }
        )
        
        # Verify the prefix was inherited
        print("Component prefix:", comp1.prefix)
        
        # Test that explicit prefix still overrides symbol reference
        comp2 = Component(
            name = "MyComponent2",
            footprint = "SOIC-8",
            symbol = Symbol(library = "ic_symbol.kicad_sym"),
            prefix = "U",  # Explicit prefix should override
            pins = {
                "IN": Net("in2"),
                "OUT": Net("out2"),
            }
        )
        
        print("Component with explicit prefix:", comp2.prefix)
    "#
});

// ============================================================================
// Component Mutation Tests
// ============================================================================

snapshot_eval!(component_mutation_mpn, {
    "test.zen" => r#"
        # Test MPN mutation
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )
        
        print("MPN before:", comp.mpn)
        comp.mpn = "RC0603FR-0710KL"
        print("MPN after:", comp.mpn)
    "#
});

snapshot_eval!(component_mutation_manufacturer, {
    "test.zen" => r#"
        # Test manufacturer mutation
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )
        
        print("Manufacturer before:", comp.manufacturer)
        comp.manufacturer = "Yageo"
        print("Manufacturer after:", comp.manufacturer)
    "#
});

snapshot_eval!(component_mutation_dnp, {
    "test.zen" => r#"
        # Test DNP mutation
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )
        
        print("DNP before:", comp.dnp)
        comp.dnp = True
        print("DNP after:", comp.dnp)
        comp.dnp = False
        print("DNP reset:", comp.dnp)
    "#
});

snapshot_eval!(component_mutation_properties, {
    "test.zen" => r#"
        # Test property mutation
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        print("Resistance before:", comp.resistance)
        comp.resistance = "100k"
        print("Resistance after:", comp.resistance)
        
        # Test adding new property
        comp.voltage_rating = "50V"
        print("New property voltage_rating:", comp.voltage_rating)
    "#
});

snapshot_eval!(component_mutation_multiple, {
    "test.zen" => r#"
        # Test multiple mutations together
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        # Mutate multiple fields
        comp.mpn = "RC0603FR-07100KL"
        comp.manufacturer = "Yageo"
        comp.dnp = True
        comp.resistance = "100k"
        comp.tolerance = "1%"
        
        print("MPN:", comp.mpn)
        print("Manufacturer:", comp.manufacturer)
        print("DNP:", comp.dnp)
        print("Resistance:", comp.resistance)
        print("Tolerance:", comp.tolerance)
    "#
});

snapshot_eval!(component_modifier_basic, {
    "test.zen" => r#"
        # Test component modifier
        def assign_part(component):
            if hasattr(component, "resistance"):
                component.mpn = "ASSIGNED_MPN"
                component.manufacturer = "ACME"
        
        builtin.add_component_modifier(assign_part)
        
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        print("After modifier - MPN:", comp.mpn)
        print("After modifier - Manufacturer:", comp.manufacturer)
    "#
});

snapshot_eval!(component_modifier_conditional, {
    "test.zen" => r#"
        # Test conditional component modifier
        def assign_preferred_resistor(component):
            if hasattr(component, "resistance"):
                resistance = str(component.resistance)
                if resistance == "10k":
                    component.mpn = "10K_PART"
                    component.manufacturer = "Vendor_10K"
                elif resistance == "100k":
                    component.mpn = "100K_PART"
                    component.manufacturer = "Vendor_100K"
        
        builtin.add_component_modifier(assign_preferred_resistor)
        
        r1 = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        r2 = Component(
            name = "R2",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("C"), "2": Net("D")},
            properties = {"resistance": "100k"},
        )
        
        r3 = Component(
            name = "R3",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("E"), "2": Net("F")},
            properties = {"resistance": "1k"},  # No modifier for this
        )
        
        print("R1 (10k) - MPN:", r1.mpn, "Manufacturer:", r1.manufacturer)
        print("R2 (100k) - MPN:", r2.mpn, "Manufacturer:", r2.manufacturer)
        print("R3 (1k) - MPN:", r3.mpn, "Manufacturer:", r3.manufacturer)
    "#
});

snapshot_eval!(component_modifier_dnp, {
    "test.zen" => r#"
        # Test component modifier setting DNP
        def mark_dnp_for_test_points(component):
            if hasattr(component, "type"):
                if str(component.type) == "test_point":
                    component.dnp = True
        
        builtin.add_component_modifier(mark_dnp_for_test_points)
        
        tp1 = Component(
            name = "TP1",
            footprint = "test_point",
            type = "test_point",
            pin_defs = {"1": "1"},
            pins = {"1": Net("SIGNAL")},
        )
        
        r1 = Component(
            name = "R1",
            footprint = "0603",
            type = "resistor",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )
        
        print("Test point DNP:", tp1.dnp)
        print("Resistor DNP:", r1.dnp)
    "#
});

snapshot_eval!(component_modifier_multiple, {
    "test.zen" => r#"
        # Test multiple component modifiers in sequence
        def modifier1(component):
            if hasattr(component, "resistance"):
                component.mod1_applied = "yes"
        
        def modifier2(component):
            if hasattr(component, "resistance"):
                component.mod2_applied = "yes"
                component.mpn = "FINAL_MPN"
        
        builtin.add_component_modifier(modifier1)
        builtin.add_component_modifier(modifier2)
        
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        print("Modifier 1 applied:", comp.mod1_applied)
        print("Modifier 2 applied:", comp.mod2_applied)
        print("Final MPN:", comp.mpn)
    "#
});

snapshot_eval!(component_has_attr_dynamic, {
    "test.zen" => r#"
        # Test has_attr with dynamically added properties
        comp = Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
        
        print("Has resistance:", hasattr(comp, "resistance"))
        print("Has voltage_rating (before):", hasattr(comp, "voltage_rating"))
        
        comp.voltage_rating = "50V"
        print("Has voltage_rating (after):", hasattr(comp, "voltage_rating"))
        print("Has nonexistent_prop:", hasattr(comp, "nonexistent_prop"))
    "#
});
