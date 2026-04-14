#[macro_use]
mod common;

use pcb_zen_core::DiagnosticsPass;
use pcb_zen_core::SortPass;
use pcb_zen_core::lang::error::CategorizedDiagnostic;

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

snapshot_eval!(module_dnp_propagates_to_children, {
    "SubModule.zen" => r#"
        # Child module with multiple components
        vcc = io("vcc", Net)
        gnd = io("gnd", Net)
        
        Component(
            name = "R1",
            footprint = "0402",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd},
            properties = {"resistance": "10k"}
        )
        
        Component(
            name = "C1",
            footprint = "0402",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd},
            properties = {"capacitance": "100nF"}
        )
    "#,
    "test.zen" => r#"
        # Load and instantiate module with dnp=True
        SubMod = Module("SubModule.zen")
        
        vcc = Net("VCC")
        gnd = Net("GND")
        
        # This module and all its child components should be DNP
        SubMod(
            name = "sub_dnp",
            vcc = vcc,
            gnd = gnd,
            dnp = True
        )
        
        # This module should NOT be DNP
        SubMod(
            name = "sub_normal",
            vcc = vcc,
            gnd = gnd
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
                component.part = builtin.Part(mpn="ASSIGNED_MPN", manufacturer="ACME")

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
                    component.part = builtin.Part(mpn="10K_PART", manufacturer="Vendor_10K")
                elif resistance == "100k":
                    component.part = builtin.Part(mpn="100K_PART", manufacturer="Vendor_100K")

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
                component.part = builtin.Part(mpn="FINAL_MPN", manufacturer="ACME")

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
                component.part = builtin.Part(mpn="PARENT_MPN", manufacturer="ParentVendor")

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
                component.part = builtin.Part(mpn="CHILD_MPN", manufacturer="ChildVendor")

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
                component.part = builtin.Part(mpn="PARENT_MPN", manufacturer="ParentVendor")

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

snapshot_eval!(component_modifier_order_independent, {
    "test.zen" => r#"
        # Test that modifiers apply to all components regardless of registration order
        # Components created BEFORE modifier registration should also be modified

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

        # Register modifier AFTER components are created
        def assign_manufacturer(component):
            if hasattr(component, "resistance"):
                component.part = builtin.Part(mpn="POST_MPN", manufacturer="PostRegistrationVendor")
                component.modified = "yes"

        builtin.add_component_modifier(assign_manufacturer)

        # Create more components AFTER modifier registration
        Component(
            name = "R3",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("E"), "2": Net("F")},
            properties = {"resistance": "1k"},
        )

        # ALL three components should have manufacturer and modified set
        # R1 and R2 were created before modifier registration
        # R3 was created after modifier registration
        # All should be modified because modifiers now apply at end of evaluation
        # This is verified by the module snapshot
    "#
});

fn warning_kinds(diagnostics: &pcb_zen_core::Diagnostics) -> std::collections::HashSet<String> {
    diagnostics
        .warnings()
        .into_iter()
        .filter_map(|diag| {
            diag.innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()
                .map(|err| err.kind.clone())
        })
        .collect()
}

fn eval_component_diagnostics(files: Vec<(String, String)>) -> pcb_zen_core::Diagnostics {
    let result = common::eval_zen(files);
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);
    let mut diagnostics = result.diagnostics;
    SortPass.apply(&mut diagnostics);
    diagnostics
}

#[test]
fn warns_for_no_connect_pin() {
    let diagnostics = eval_component_diagnostics(vec![
        (
            "nc_pin.kicad_sym".to_string(),
            r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "NcPin"
    (property "Reference" "U")
    (symbol "NcPin_0_1"
      (pin no_connect line
        (at 0 0 0)
        (length 2.54)
        (name "NC")
        (number "1")
      )
    )
  )
)"#
            .to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
symbol = Symbol(library = "nc_pin.kicad_sym")

Component(
    name = "U1",
    footprint = "TEST:0402",
    symbol = symbol,
    pins = {
        "NC": Net("SIG"),
    },
)
"#
            .to_string(),
        ),
    ]);
    let warnings = diagnostics.warnings();
    let kinds = warning_kinds(&diagnostics);

    assert!(kinds.contains("pin.no_connect"));
    assert!(
        warnings
            .iter()
            .any(|diag| diag.body.contains("marked no_connect")
                && diag.body.contains("NotConnected()")),
        "expected no_connect warning, got: {:?}",
        warnings
    );
}

#[test]
fn warns_for_power_pin_on_plain_net() {
    let diagnostics = eval_component_diagnostics(vec![
        (
            "power_pin.kicad_sym".to_string(),
            r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "PowerPin"
    (property "Reference" "U")
    (symbol "PowerPin_0_1"
      (pin power_in line
        (at 0 0 0)
        (length 2.54)
        (name "VCC")
        (number "1")
      )
    )
  )
)"#
            .to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
symbol = Symbol(library = "power_pin.kicad_sym")

Component(
    name = "U1",
    footprint = "TEST:0402",
    symbol = symbol,
    pins = {
        "VCC": Net("VCC"),
    },
)
"#
            .to_string(),
        ),
    ]);
    let warnings = diagnostics.warnings();
    let kinds = warning_kinds(&diagnostics);

    assert!(kinds.contains("pin.power_net"));
    assert!(
        warnings.iter().any(
            |diag| diag.body.contains("power pin") && diag.body.contains("Power() or Ground()")
        ),
        "expected power pin warning, got: {:?}",
        warnings
    );
}

#[test]
fn alternate_pin_suppresses_warning() {
    let diagnostics = eval_component_diagnostics(vec![
        (
            "alt_pin.kicad_sym".to_string(),
            r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "AltPin"
    (property "Reference" "U")
    (symbol "AltPin_0_1"
      (pin power_in line
        (at 0 0 0)
        (length 2.54)
        (name "PIO1")
        (number "1")
        (alternate "GPIO1" input line)
      )
    )
  )
)"#
            .to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
symbol = Symbol(library = "alt_pin.kicad_sym")

Component(
    name = "U1",
    footprint = "TEST:0402",
    symbol = symbol,
    pins = {
        "PIO1": Net("SIG"),
    },
)
"#
            .to_string(),
        ),
    ]);
    let kinds = warning_kinds(&diagnostics);

    assert!(!kinds.contains("pin.power_net"));
    assert!(
        diagnostics
            .warnings()
            .iter()
            .all(|diag| !diag.body.contains("power pin")),
        "did not expect power pin warning, got: {:?}",
        diagnostics.warnings()
    );
}

snapshot_eval!(module_schematic_collapse, {
    "SubModule.zen" => r#"
        vcc = io("vcc", Net)
        gnd = io("gnd", Net)
        
        Component(
            name = "R1",
            footprint = "0402",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd},
        )
    "#,
    "test.zen" => r#"
        SubMod = Module("SubModule.zen")
        
        vcc = Net("VCC")
        gnd = Net("GND")
        
        SubMod(
            name = "collapsed_module",
            vcc = vcc,
            gnd = gnd,
            schematic = "collapse"
        )
    "#
});

snapshot_eval!(module_schematic_embed, {
    "SubModule.zen" => r#"
        vcc = io("vcc", Net)
        gnd = io("gnd", Net)
        
        Component(
            name = "R1",
            footprint = "0402",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd},
        )
    "#,
    "test.zen" => r#"
        SubMod = Module("SubModule.zen")
        
        vcc = Net("VCC")
        gnd = Net("GND")
        
        SubMod(
            name = "embedded_module",
            vcc = vcc,
            gnd = gnd,
            schematic = "embed"
        )
    "#
});

snapshot_eval!(module_schematic_invalid, {
    "SubModule.zen" => r#"
        vcc = io("vcc", Net)
    "#,
    "test.zen" => r#"
        SubMod = Module("SubModule.zen")
        
        SubMod(
            name = "invalid_module",
            vcc = Net("VCC"),
            schematic = "invalid_value"
        )
    "#
});

#[test]
fn component_modifier_match_component_ignores_electrical_checks() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
        load("@stdlib/bom/helpers.zen", "match_component")

        builtin.add_component_modifier(
            match_component(
                match={"resistance": "10k"},
                parts=("RC0603FR-0710KL", "Yageo"),
            )
        )

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )

        def check_ok(module):
            return

        builtin.add_electrical_check(
            name = "noop",
            check_fn = check_ok,
        )
    "#
        .to_string(),
    )]);

    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);
}
