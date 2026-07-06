#[macro_use]
mod common;

use pcb_zen_core::DiagnosticsPass;
use pcb_zen_core::SortPass;
use pcb_zen_core::lang::component::FrozenComponentValue;
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcb_zen_core::lang::net::FrozenNetValue;
use starlark::errors::EvalSeverity;
use starlark::values::{FrozenValue, ValueLike};

fn eval_single_root_component(source: &str) -> FrozenComponentValue {
    eval_single_root_component_with_files(vec![("test.zen", source)])
}

fn eval_single_root_component_with_files(files: Vec<(&str, &str)>) -> FrozenComponentValue {
    let result = common::eval_zen(
        files
            .into_iter()
            .map(|(path, content)| (path.to_string(), content.to_string()))
            .collect(),
    );
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);

    let output = result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let root_module = module_tree
        .values()
        .find(|module| module.path().is_root())
        .expect("expected root module");

    let components: Vec<_> = root_module.components().cloned().collect();
    assert_eq!(
        components.len(),
        1,
        "expected exactly one root component, got {}",
        components.len()
    );

    components
        .into_iter()
        .next()
        .expect("expected one component")
}

fn frozen_net_id(value: FrozenValue) -> u64 {
    value
        .downcast_ref::<FrozenNetValue>()
        .expect("expected frozen net")
        .net_id()
}

const EXPLICIT_JUMPER_SYMBOL: &str = r#"(kicad_symbol_lib
  (version 20251024)
  (generator "test")
  (symbol "ExplicitJumper"
    (property "Reference" "JP")
    (property "Footprint" File("@kicad-footprints/Jumper.pretty/SolderJumper-2_P1.3mm_Open_Pad1.0x1.5mm.kicad_mod"))
    (jumper_pin_groups ("1" "3"))
    (symbol "ExplicitJumper_1_1"
      (pin passive line (at 0 0 0) (length 2.54) (name "A") (number "1"))
      (pin passive line (at 0 0 0) (length 2.54) (name "B") (number "3"))
    )
  )
)"#;

#[test]
fn component_prefers_part_datasheet_over_component_datasheet() {
    let component = eval_single_root_component(
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    datasheet = "component.pdf",
    part = builtin.Part(
        mpn = "PART-1",
        manufacturer = "MFR",
        datasheet = "https://example.com/part-1.pdf",
    ),
)
"#,
    );

    assert_eq!(
        component.datasheet(),
        Some("https://example.com/part-1.pdf")
    );
}

#[test]
fn component_modifier_can_set_part_datasheet() {
    let component = eval_single_root_component(
        r#"
P1 = Net()
P2 = Net()

def match_part(component):
    component.part = builtin.Part(
        mpn = "HOUSE-1",
        manufacturer = "MFR",
        datasheet = "https://example.com/house-1.pdf",
    )

builtin.add_component_modifier(match_part)

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
)
"#,
    );

    assert_eq!(
        component.datasheet(),
        Some("https://example.com/house-1.pdf")
    );
}

#[test]
fn file_backed_footprint_validation_reports_embedded_file_errors() {
    let result = common::eval_zen(vec![
        (
            "bad.kicad_mod".to_string(),
            r#"
            (footprint "Bad"
              (embedded_files
                (file
                  (name model.step)
                  (type model)
                  (data |not base64|)
                  (checksum "00")
                )
              )
            )
            "#
            .to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
            P1 = Net()
            P2 = Net()
            P3 = Net()
            P4 = Net()

            Component(
                name = "R1",
                footprint = "bad.kicad_mod",
                pin_defs = {"1": "1", "2": "2"},
                pins = {"1": P1, "2": P2},
            )

            Component(
                name = "R2",
                footprint = "bad.kicad_mod",
                pin_defs = {"1": "1", "2": "2"},
                pins = {"1": P3, "2": P4},
            )
            "#
            .to_string(),
        ),
    ]);
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);

    let diagnostics = result
        .output
        .expect("expected eval output")
        .validate_footprints();

    assert!(diagnostics.iter().any(|d| d.is_error()));
    let footprint_diags = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.body.contains("uses invalid footprint"))
        .collect::<Vec<_>>();
    assert_eq!(footprint_diags.len(), 2);
    let footprint_diag = footprint_diags[0];
    assert!(footprint_diag.span.is_some());
    let child = footprint_diag
        .child
        .as_ref()
        .expect("expected inner footprint-file diagnostic");
    assert!(child.body.contains("Invalid KiCad footprint"), "{child:?}");
    assert!(child.body.contains("invalid base64 data"), "{child:?}");
    assert!(child.path.ends_with("bad.kicad_mod"), "{child:?}");
    assert!(child.span.is_some());
}

#[test]
fn component_modifier_part_without_datasheet_clears_stale_part_datasheet() {
    let component = eval_single_root_component(
        r#"
P1 = Net()
P2 = Net()

def match_part(component):
    component.part = builtin.Part(
        mpn = "HOUSE-1",
        manufacturer = "MFR",
    )

builtin.add_component_modifier(match_part)

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = builtin.Part(
        mpn = "PART-1",
        manufacturer = "MFR",
        datasheet = "https://example.com/part-1.pdf",
    ),
)
"#,
    );

    assert_eq!(component.datasheet(), None);
}

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
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
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

#[test]
fn component_explicit_jumper_group_auto_fills_missing_peer() {
    let component = eval_single_root_component_with_files(vec![
        ("explicit_jumper.kicad_sym", EXPLICIT_JUMPER_SYMBOL),
        (
            "test.zen",
            r#"
shared = Net("SHARED")

Component(
    name = "JP1",
    footprint = File("@kicad-footprints/Jumper.pretty/SolderJumper-2_P1.3mm_Open_Pad1.0x1.5mm.kicad_mod"),
    symbol = Symbol(library = "explicit_jumper.kicad_sym", name = "ExplicitJumper"),
    pins = {"A": shared},
)
"#,
        ),
    ]);

    assert!(component.connections().contains_key("A"));
    assert!(component.connections().contains_key("B"));
    assert_eq!(
        frozen_net_id(*component.connections().get("A").unwrap()),
        frozen_net_id(*component.connections().get("B").unwrap())
    );
}

#[test]
fn component_explicit_jumper_group_conflicting_nets_error() {
    let result = common::eval_zen(vec![
        (
            "explicit_jumper.kicad_sym".to_string(),
            EXPLICIT_JUMPER_SYMBOL.to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
Component(
    name = "JP1",
    footprint = File("@kicad-footprints/Jumper.pretty/SolderJumper-2_P1.3mm_Open_Pad1.0x1.5mm.kicad_mod"),
    symbol = Symbol(library = "explicit_jumper.kicad_sym", name = "ExplicitJumper"),
    pins = {"A": Net("LEFT"), "B": Net("RIGHT")},
)
"#
            .to_string(),
        ),
    ]);

    assert!(!result.is_success(), "expected eval failure");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.to_string().contains("Jumpered pins A, B")),
        "expected jumper conflict diagnostic, got {:?}",
        result.diagnostics
    );
}

snapshot_eval!(component_infers_spice_model_from_symbol, {
    "my_model.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL=1k
R1 p n {RVAL}
.ENDS my_resistor
"#,
    "test_sim_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "TestSim" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "R" (id 0) (at 0 0 0))
    (property "Sim.Library" "my_model.lib" (id 1) (at 0 0 0))
    (property "Sim.Name" "my_resistor" (id 2) (at 0 0 0))
    (property "Sim.Device" "SUBCKT" (id 3) (at 0 0 0))
    (property "Sim.Pins" "2=p 1=n" (id 4) (at 0 0 0))
    (property "Sim.Params" "RVAL=2200" (id 5) (at 0 0 0))
    (symbol "TestSim_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "TestSim_1_1"
      (pin passive line (at -12.7 2.54 0) (length 2.54)
        (name "P1" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin passive line (at -12.7 -2.54 0) (length 2.54)
        (name "P2" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    "test.zen" => r#"
        Component(
            name = "R1",
            footprint = "0603",
            symbol = Symbol(library = "./test_sim_symbol.kicad_sym"),
            pins = {
                "P1": Net("A"),
                "P2": Net("B"),
            },
        )
    "#
});

snapshot_eval!(component_ignores_non_subckt_symbol_spice_model, {
    "my_model.lib" => r#"
.SUBCKT my_resistor p n
R1 p n 1k
.ENDS my_resistor
"#,
    "test_invalid_sim_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "TestSim" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "R" (id 0) (at 0 0 0))
    (property "Sim.Library" "my_model.lib" (id 1) (at 0 0 0))
    (property "Sim.Name" "my_resistor" (id 2) (at 0 0 0))
    (property "Sim.Device" "R" (id 3) (at 0 0 0))
    (property "Sim.Pins" "1=p 2=n" (id 4) (at 0 0 0))
    (symbol "TestSim_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "TestSim_1_1"
      (pin passive line (at -12.7 2.54 0) (length 2.54)
        (name "P1" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin passive line (at -12.7 -2.54 0) (length 2.54)
        (name "P2" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    "test.zen" => r#"
        Component(
            name = "R1",
            footprint = "0603",
            symbol = Symbol(library = "./test_invalid_sim_symbol.kicad_sym"),
            pins = {
                "P1": Net("A"),
                "P2": Net("B"),
            },
        )
    "#
});

snapshot_eval!(component_ignores_incomplete_subckt_symbol_spice_model, {
    "test_incomplete_sim_symbol.kicad_sym" => r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "TestSim" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "R" (id 0) (at 0 0 0))
    (property "Sim.Name" "my_resistor" (id 1) (at 0 0 0))
    (property "Sim.Device" "SUBCKT" (id 2) (at 0 0 0))
    (property "Sim.Pins" "1=p 2=n" (id 3) (at 0 0 0))
    (symbol "TestSim_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "TestSim_1_1"
      (pin passive line (at -12.7 2.54 0) (length 2.54)
        (name "P1" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin passive line (at -12.7 -2.54 0) (length 2.54)
        (name "P2" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    "test.zen" => r#"
        Component(
            name = "R1",
            footprint = "0603",
            symbol = Symbol(library = "./test_incomplete_sim_symbol.kicad_sym"),
            pins = {
                "P1": Net("A"),
                "P2": Net("B"),
            },
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
        vcc = io(Net)
        gnd = io(Net)
        
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

#[test]
fn simulation_uses_default_bom_profile() {
    let component = eval_single_root_component(
        r#"
        load("@stdlib/properties.zen", "Simulation")

        Component(
            name = "R1",
            prefix = "R",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"package": "0603", "resistance": "10k"},
            type = "resistor",
        )

        Simulation(
            name = "sim",
            setup = "* noop",
        )
    "#,
    );

    assert!(
        component.mpn().is_some(),
        "expected default simulation BOM profile to assign a house part"
    );
    assert!(
        component.manufacturer().is_some(),
        "expected default simulation BOM profile to assign a manufacturer"
    );
}

#[test]
fn simulation_can_disable_bom_profile() {
    let component = eval_single_root_component(
        r#"
        load("@stdlib/properties.zen", "Simulation")

        Component(
            name = "R1",
            prefix = "R",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"package": "0603", "resistance": "10k"},
            type = "resistor",
        )

        Simulation(
            name = "sim",
            setup = "* noop",
            bom_profile = None,
        )
    "#,
    );

    assert_eq!(
        component.mpn(),
        None,
        "expected bom_profile=None to skip simulation-time house matching"
    );
}

#[test]
fn simulation_modifiers_run_before_bom_profile() {
    let component = eval_single_root_component(
        r#"
        load("@stdlib/properties.zen", "Simulation")

        def assign_custom_part(component):
            if hasattr(component, "resistance"):
                component.part = builtin.Part(mpn = "CUSTOM_MPN", manufacturer = "ACME")

        Component(
            name = "R1",
            prefix = "R",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"package": "0603", "resistance": "10k"},
            type = "resistor",
        )

        Simulation(
            name = "sim",
            setup = "* noop",
            modifiers = [assign_custom_part],
        )
    "#,
    );

    assert_eq!(component.mpn(), Some("CUSTOM_MPN"));
    assert_eq!(component.manufacturer(), Some("ACME"));
}

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

snapshot_eval!(component_modifier_spice_model, {
    "r.lib" => r#"
.SUBCKT my_resistor p n PARAMS: RVAL=1k
R1 p n {RVAL}
.ENDS my_resistor
    "#,
    "test.zen" => r#"
        def assign_spice_model(component):
            if hasattr(component, "resistance"):
                pins = component.pins
                component.spice_model = SpiceModel(
                    "r.lib",
                    "my_resistor",
                    nets=[pins["1"], pins["2"]],
                    args={"RVAL": "1000"},
                )

        builtin.add_component_modifier(assign_spice_model)

        Component(
            name = "R1",
            footprint = "0603",
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": Net("A"), "2": Net("B")},
            properties = {"resistance": "10k"},
        )
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
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
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
                && diag.body.contains("omit it from `pins`")),
        "expected no_connect warning, got: {:?}",
        warnings
    );
}

#[test]
fn warns_for_explicit_not_connected_pin() {
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
NotConnected = builtin.not_connected
symbol = Symbol(library = "nc_pin.kicad_sym")

Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    symbol = symbol,
    pins = {
        "NC": NotConnected(),
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
            .any(|diag| diag.body.contains("explicitly connected to NotConnected")),
        "expected explicit NotConnected warning, got: {:?}",
        warnings
    );
}

#[test]
fn omitting_no_connect_pin_is_allowed() {
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
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    symbol = symbol,
    pins = {},
)
"#
            .to_string(),
        ),
    ]);

    assert!(
        !warning_kinds(&diagnostics).contains("pin.no_connect"),
        "did not expect pin.no_connect warning, got: {:?}",
        diagnostics.warnings()
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
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
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
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
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
        vcc = io(Net)
        gnd = io(Net)
        
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
        vcc = io(Net)
        gnd = io(Net)
        
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
        vcc = io(Net)
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

fn legacy_property_diagnostics(
    diagnostics: &pcb_zen_core::Diagnostics,
    severity: EvalSeverity,
) -> Vec<String> {
    diagnostics
        .diagnostics
        .iter()
        .filter(|diag| {
            matches!(
                (&diag.severity, &severity),
                (EvalSeverity::Warning, EvalSeverity::Warning)
                    | (EvalSeverity::Advice, EvalSeverity::Advice)
                    | (EvalSeverity::Error, EvalSeverity::Error)
            )
        })
        .filter_map(|diag| {
            let kind = diag
                .innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()?
                .kind
                .clone();
            if kind == "deprecated.component_property" {
                Some(diag.body.clone())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn errors_for_each_legacy_component_property_key() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    properties = {
        "do_not_populate": True,
        "Do_not_populate": True,
        "DNP": True,
        "dnp": True,
        "Exclude_from_bom": True,
        "exclude_from_bom": True,
        "Exclude_from_pos_files": True,
        "exclude_from_pos_files": True,
        "type": "resistor",
        "Type": "resistor",
        "mpn": "X",
        "Mpn": "X",
        "manufacturer": "M",
        "Manufacturer": "M",
        "datasheet": "ds.pdf",
        "description": "desc",
        "Description": "desc",
    },
)
"#
        .to_string(),
    )]);
    let mut diagnostics = result.diagnostics;
    SortPass.apply(&mut diagnostics);

    let error_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Error);

    for (legacy_key, typed_kwarg) in [
        ("do_not_populate", "dnp"),
        ("Do_not_populate", "dnp"),
        ("DNP", "dnp"),
        ("dnp", "dnp"),
        ("Exclude_from_bom", "skip_bom"),
        ("exclude_from_bom", "skip_bom"),
        ("Exclude_from_pos_files", "skip_pos"),
        ("exclude_from_pos_files", "skip_pos"),
        ("type", "type"),
        ("Type", "type"),
        ("datasheet", "datasheet"),
        ("description", "description"),
        ("Description", "description"),
    ] {
        let expected = format!(
            "Component 'R1': `properties[\"{legacy_key}\"]` is no longer supported; pass `{typed_kwarg}=...` to Component() instead"
        );
        assert!(
            error_bodies.iter().any(|b| b == &expected),
            "expected legacy-property error for `{legacy_key}`, got: {:?}",
            error_bodies
        );
    }

    for sourcing_key in ["mpn", "Mpn", "manufacturer", "Manufacturer"] {
        let expected = format!(
            "Component 'R1': `properties[\"{sourcing_key}\"]` is no longer supported; pass `part=Part(mpn=..., manufacturer=...)` to Component() instead"
        );
        assert!(
            error_bodies.iter().any(|b| b == &expected),
            "expected sourcing-key error for `{sourcing_key}`, got: {:?}",
            error_bodies
        );
    }
}

#[test]
fn no_legacy_warning_when_typed_kwargs_are_used() {
    let diagnostics = eval_component_diagnostics(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = builtin.Part(mpn = "RC0603FR-071KL", manufacturer = "Yageo"),
    type = "resistor",
    dnp = True,
    skip_bom = True,
    skip_pos = True,
    datasheet = "ds.pdf",
    description = "desc",
    properties = {"resistance": "1k"},
)
"#
        .to_string(),
    )]);

    let warning_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Warning);
    let advice_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Advice);
    assert!(
        warning_bodies.is_empty(),
        "expected no legacy-property warnings, got: {:?}",
        warning_bodies
    );
    assert!(
        advice_bodies.is_empty(),
        "expected no legacy-property advice, got: {:?}",
        advice_bodies
    );
}

#[test]
fn advises_for_mpn_and_manufacturer_kwargs() {
    let diagnostics = eval_component_diagnostics(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "RC0603FR-071KL",
    manufacturer = "Yageo",
)
"#
        .to_string(),
    )]);

    let warning_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Warning);
    let advice_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Advice);

    assert!(
        warning_bodies.is_empty(),
        "expected sourcing kwargs to avoid warnings, got: {:?}",
        warning_bodies
    );

    for kwarg in ["mpn", "manufacturer"] {
        let expected = format!(
            "Component 'R1': `{kwarg}=...` is deprecated; pass `part=Part(mpn=..., manufacturer=...)` to Component() instead"
        );
        assert!(
            advice_bodies.iter().any(|b| b == &expected),
            "expected sourcing-kwarg advice for `{kwarg}`, got: {:?}",
            advice_bodies
        );
    }
}

#[test]
fn empty_legacy_mpn_kwarg_counts_as_missing_part_info() {
    let eval_result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    mpn = "",
    manufacturer = "Yageo",
)
"#
        .to_string(),
    )]);

    assert!(
        eval_result.is_success(),
        "eval failed: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let mut diagnostics = eval_output.to_schematic_with_diagnostics().diagnostics;
    SortPass.apply(&mut diagnostics);

    assert!(
        diagnostics.iter().any(|diag| {
            matches!(diag.severity, EvalSeverity::Error)
                && diag
                    .downcast_error_ref::<CategorizedDiagnostic>()
                    .is_some_and(|c| c.kind == "bom.unspecified")
        }),
        "expected empty legacy mpn to leave part info unspecified, got: {:?}",
        diagnostics
    );
}

#[test]
fn part_kwarg_does_not_warn() {
    let diagnostics = eval_component_diagnostics(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = builtin.Part(mpn = "RC0603FR-071KL", manufacturer = "Yageo"),
)
"#
        .to_string(),
    )]);

    let warning_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Warning);
    let advice_bodies = legacy_property_diagnostics(&diagnostics, EvalSeverity::Advice);
    assert!(
        warning_bodies.is_empty(),
        "expected no warnings when using part=Part(...), got: {:?}",
        warning_bodies
    );
    assert!(
        advice_bodies.is_empty(),
        "expected no advice when using part=Part(...), got: {:?}",
        advice_bodies
    );
}

#[test]
fn legacy_dnp_property_still_takes_effect() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
P1 = Net()
P2 = Net()

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    properties = {"do_not_populate": True},
)
"#
        .to_string(),
    )]);
    let mut diagnostics = result.diagnostics;
    SortPass.apply(&mut diagnostics);

    assert!(
        legacy_property_diagnostics(&diagnostics, EvalSeverity::Error)
            .iter()
            .any(|body| body.contains("`properties[\"do_not_populate\"]` is no longer supported")),
        "expected legacy-property error, got: {:?}",
        diagnostics
    );

    let output = result
        .output
        .expect("expected eval output despite diagnostics");
    let module_tree = output.module_tree();
    let root_module = module_tree
        .values()
        .find(|module| module.path().is_root())
        .expect("expected root module");
    let component = root_module
        .components()
        .next()
        .expect("expected root component");

    assert!(
        component.dnp(),
        "legacy do_not_populate should still set dnp"
    );
}
