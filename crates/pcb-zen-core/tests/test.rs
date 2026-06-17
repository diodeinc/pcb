#[macro_use]
mod common;

snapshot_eval!(net_passing, {
    "MyComponent.zen" => r#"
        ComponentInterface = interface(p1 = Net, p2 = Net)
        component_input = io(ComponentInterface)

        Component(
            name = "capacitor",
            type = "capacitor",
            pin_defs = { "P1": "1", "P2": "2" },
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
            pins = { "P1": component_input.p1, "P2": component_input.p2 },
        )
    "#,
    "test.zen" => r#"
        load("MyComponent.zen", "ComponentInterface")
        MyComponent = Module("MyComponent.zen")

        MyComponent(
            name = "MyComponent",
            component_input = ComponentInterface("INTERFACE"),
        )
    "#,
    "top.zen" => r#"
        Test = Module("test.zen")

        Test(
            name = "Test",
        )
    "#
});

snapshot_eval!(unused_inputs_should_error, {
    "my_module.zen" => r#"
        # empty module with no inputs
    "#,
    "top.zen" => r#"
        MyModule = Module("my_module.zen")

        MyModule(
            name = "MyModule",
            unused = 123,
        )
    "#
});

snapshot_eval!(missing_pins_should_error, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test_missing.zen" => r#"
        # Instantiate the component while omitting several required pins.
        Component(
            name = "Component",
            pins = {
                "ICLK": Net("ICLK"),
                "Q1": Net("Q1"),
            },
            symbol = Symbol(library = "C146731.kicad_sym", name = "NB3N551DG"),
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
        )
    "#
});

snapshot_eval!(unknown_pin_should_error, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test_unknown.zen" => r#"
        # Instantiate the component with an invalid pin included.
        Component(
            name = "Comp",
            pins = {
                "ICLK": Net("ICLK"),
                "Q1": Net("Q1"),
                "Q2": Net("Q2"),
                "Q3": Net("Q3"),
                "Q4": Net("Q4"),
                "GND": Net("GND"),
                "VDD": Net("VDD"),
                "OE": Net("OE"),
                "INVALID": Net("X"),
            },
            symbol = Symbol(library = "C146731.kicad_sym", name = "NB3N551DG"),
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
        )
    "#
});

snapshot_eval!(nested_components, {
    "Component.zen" => r#"
        Component(
            name = "Component",
            pin_defs = {
                "P1": "1",
                "P2": "2",
            },
            pins = {
                "P1": Net("P1"),
                "P2": Net("P2"),
            },
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
        )
    "#,
    "Module.zen" => r#"
        MyComponent = Module("Component.zen")

        MyComponent(
            name = "MyComponent",
        )
    "#,
    "Top.zen" => r#"
        MyModule = Module("Module.zen")

        MyModule(
            name = "MyModule",
        )
    "#
});

snapshot_eval!(net_name_deduplication, {
    "MyModule.zen" => r#"
        _internal_net = Net("INTERNAL")
        Component(
            name = "Component",
            pin_defs = {
                "P1": "1",
            },
            pins = {
                "P1": _internal_net,
            },
            footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0805_2012Metric.kicad_mod"),
        )
    "#,
    "Top.zen" => r#"
        MyModule = Module("MyModule.zen")
        MyModule(name = "MyModule1")
        MyModule(name = "MyModule2")
        MyModule(name = "MyModule3")
    "#
});

snapshot_eval!(duplicate_component_name, {
    "test.zen" => r#"
        vcc = Net(name = "VCC")
        gnd = Net(name = "GND")

        # First component named "R1"
        Component(
            name = "R1",
            footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd}
        )

        # Second component with the same name "R1" - should warn
        Component(
            name = "R1",
            footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd}
        )
    "#
});

snapshot_eval!(duplicate_module_name, {
    "sub.zen" => r#"
        vcc = Net(name = "VCC")
        gnd = Net(name = "GND")

        Component(
            name = "R1",
            footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd}
        )
    "#,
    "test.zen" => r#"
        Sub = Module("sub.zen")

        # First module instance named "sub1"
        Sub(name = "sub1")

        # Second module instance with the same name "sub1" - should warn
        Sub(name = "sub1")
    "#
});

snapshot_eval!(duplicate_module_component_collision, {
    "sub.zen" => r#"
        vcc = Net(name = "VCC")
        gnd = Net(name = "GND")

        Component(
            name = "R1",
            footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd}
        )
    "#,
    "test.zen" => r#"
        Sub = Module("sub.zen")

        vcc = Net(name = "VCC")
        gnd = Net(name = "GND")

        # Component named "widget"
        Component(
            name = "widget",
            footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
            pin_defs = {"1": "1", "2": "2"},
            pins = {"1": vcc, "2": gnd}
        )

        # Module instance with the same name "widget" - should warn about collision
        Sub(name = "widget")
    "#
});

#[test]
#[cfg(not(target_os = "windows"))]
fn duplicate_child_name_has_diagnostic_kind() {
    use pcb_zen_core::lang::error::CategorizedDiagnostic;
    use starlark::errors::EvalSeverity;

    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
vcc = Net(name = "VCC")
gnd = Net(name = "GND")

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": vcc, "2": gnd}
)

Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": vcc, "2": gnd}
)
"#
        .to_string(),
    )]);

    assert!(
        result.is_success(),
        "expected duplicate child name to warn without failing"
    );
    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            matches!(diagnostic.severity, EvalSeverity::Warning)
                && diagnostic
                    .downcast_error_ref::<CategorizedDiagnostic>()
                    .is_some_and(|categorized| categorized.kind == "module.duplicate_child_name")
        }),
        "expected module.duplicate_child_name warning, got: {:?}",
        result.diagnostics
    );
}
