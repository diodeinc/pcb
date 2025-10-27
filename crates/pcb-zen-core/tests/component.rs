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
        Component(
            name = "MyComponent1",
            footprint = "SOIC-8",
            symbol = Symbol(library = "ic_symbol.kicad_sym"),
            pins = {
                "IN": Net("in_signal"),
                "OUT": Net("out_signal"),
            }
        )

        # Test that explicit prefix still overrides symbol reference
        Component(
            name = "MyComponent2",
            footprint = "SOIC-8",
            symbol = Symbol(library = "ic_symbol.kicad_sym"),
            prefix = "U",  # Explicit prefix should override
            pins = {
                "IN": Net("in2"),
                "OUT": Net("out2"),
            }
        )

        # The component prefix will be visible in the module snapshot
    "#
});

// ============================================================================
// Component Mutation Tests
// ============================================================================
// Note: Component() now returns None (like Module()), so direct mutation
// outside of modifiers is not allowed. Mutation must happen in modifier functions.

snapshot_eval!(component_modifier_basic, {
    "test.zen" => r#"
        # Test component modifier
        def assign_part(component):
            if hasattr(component, "resistance"):
                component.mpn = "ASSIGNED_MPN"
                component.manufacturer = "ACME"

        builtin.add_component_modifier(assign_part)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # Component will have mpn and manufacturer set by modifier
        # This is verified by the module snapshot
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

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        Component(
            name = "R2",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("C"), "2": Net("D")},
            properties = {"resistance": "100k"},
        )

        Component(
            name = "R3",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("E"), "2": Net("F")},
            properties = {"resistance": "1k"},  # No modifier for this
        )

        # R1 should have 10K_PART, R2 should have 100K_PART, R3 should have no MPN
        # This is verified by the module snapshot
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

        Component(
            name = "TP1",
            footprint = "test_point",
            type = "test_point",
            pin_defs = {"1": "1"},
            pins = {"1": Net("SIGNAL")},
        )

        Component(
            name = "R1",
            footprint = "0603",
            type = "resistor",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )

        # Test point should have dnp=True, resistor should not
        # This is verified by the module snapshot
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

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # Both modifiers should have run, setting mod1_applied, mod2_applied, and mpn
        # This is verified by the module snapshot
    "#
});

snapshot_eval!(component_modifier_parent, {
    "Child.zen" => r#"
        # Child module creates a component
        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # Component should have parent_modified and manufacturer set by parent modifier
        # This is verified by the module snapshot
    "#,
    "test.zen" => r#"
        # Parent module registers a modifier
        def parent_modifier(component):
            if hasattr(component, "resistance"):
                component.parent_modified = "yes"
                component.manufacturer = "ParentVendor"

        builtin.add_component_modifier(parent_modifier)

        # Instantiate child - components in child should get parent modifier
        Child = Module("Child.zen")
        Child(name = "ChildInstance")
    "#
});

snapshot_eval!(component_modifier_child_overrides_parent, {
    "Child.zen" => r#"
        # Child modifier runs first and sets manufacturer
        def child_modifier(component):
            if hasattr(component, "resistance"):
                component.manufacturer = "ChildVendor"

        builtin.add_component_modifier(child_modifier)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # Parent modifier ran AFTER child modifier
        # So final value should be ParentVendor (parent overwrites child)
        # This is verified by the module snapshot
    "#,
    "test.zen" => r#"
        # Parent modifier sets manufacturer
        def parent_modifier(component):
            if hasattr(component, "resistance"):
                component.manufacturer = "ParentVendor"

        builtin.add_component_modifier(parent_modifier)

        Child = Module("Child.zen")
        Child(name = "ChildInstance")
    "#
});

snapshot_eval!(component_modifier_grandparent, {
    "Child.zen" => r#"
        def child_modifier(component):
            if hasattr(component, "resistance"):
                component.child_modified = "yes"

        builtin.add_component_modifier(child_modifier)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # All modifiers should have run (bottom-up: child, parent, grandparent)
        # This is verified by the module snapshot
    "#,
    "Parent.zen" => r#"
        def parent_modifier(component):
            if hasattr(component, "resistance"):
                component.parent_modified = "yes"

        builtin.add_component_modifier(parent_modifier)

        Child = Module("Child.zen")
        Child(name = "ChildInstance")
    "#,
    "test.zen" => r#"
        def grandparent_modifier(component):
            if hasattr(component, "resistance"):
                component.gp_modified = "yes"

        builtin.add_component_modifier(grandparent_modifier)

        # Create hierarchy: Grandparent -> Parent -> Child
        Parent = Module("Parent.zen")
        Parent(name = "ParentInstance")
    "#
});

snapshot_eval!(component_modifier_execution_order, {
    "Child.zen" => r#"
        def child_modifier(component):
            if hasattr(component, "order"):
                component.order = component.order + " -> child"
            else:
                component.order = "child"

        builtin.add_component_modifier(child_modifier)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        # Execution order should be: child first, then parent
        # Component should have order = "child -> parent"
        # This is verified by the module snapshot
    "#,
    "test.zen" => r#"
        def parent_modifier(component):
            if hasattr(component, "order"):
                component.order = component.order + " -> parent"
            else:
                component.order = "parent"

        builtin.add_component_modifier(parent_modifier)

        Child = Module("Child.zen")
        Child(name = "ChildInstance")
    "#
});

snapshot_eval!(current_module_path_root, {
    "test.zen" => r#"
        path = builtin.current_module_path()
        print("Root module path:", path)
        print("Root path length:", len(path))
        print("Is root:", len(path) == 0)
    "#
});

snapshot_eval!(current_module_path_visible, {
    "Child.zen" => r#"
        # Store the module path in component properties so it's visible in snapshot
        path = builtin.current_module_path()

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {
                "module_path": str(path),
                "module_depth": len(path),
                "is_root": len(path) == 0,
            },
        )
    "#,
    "test.zen" => r#"
        path = builtin.current_module_path()
        print("Root module path:", path)
        print("Root is_root:", len(path) == 0)

        Child = Module("Child.zen")
        Child(name = "ChildInstance")

        # Create component in root too
        Component(
            name = "R2",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {
                "module_path": str(path),
                "module_depth": len(path),
                "is_root": len(path) == 0,
            },
        )
    "#
});

snapshot_eval!(current_module_path_nested_visible, {
    "GrandChild.zen" => r#"
        path = builtin.current_module_path()

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {
                "module_path": str(path),
                "module_depth": len(path),
            },
        )
    "#,
    "Child.zen" => r#"
        path = builtin.current_module_path()

        Component(
            name = "R2",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {
                "module_path": str(path),
                "module_depth": len(path),
            },
        )

        GrandChild = Module("GrandChild.zen")
        GrandChild(name = "GrandChildInstance")
    "#,
    "test.zen" => r#"
        path = builtin.current_module_path()
        print("Root module depth:", len(path))

        Child = Module("Child.zen")
        Child(name = "ChildInstance")
    "#
});

snapshot_eval!(current_module_path_conditional_modifier, {
    "Child.zen" => r#"
        def child_modifier(component):
            component.modified_in_child = True

        # This should NOT run in child (not root)
        if len(builtin.current_module_path()) == 0:
            builtin.add_component_modifier(child_modifier)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )

        # Child component should NOT have modified_in_child
        # (because modifier is only added at root)
    "#,
    "test.zen" => r#"
        def root_modifier(component):
            component.modified_in_root = True

        # This SHOULD run in root
        if len(builtin.current_module_path()) == 0:
            builtin.add_component_modifier(root_modifier)

        Child = Module("Child.zen")
        Child(name = "ChildInstance")

        # Create a component in root to verify modifier runs
        Component(
            name = "R2",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
        )

        # Root component should have modified_in_root=True
        # This is verified by the module snapshot
    "#
});
