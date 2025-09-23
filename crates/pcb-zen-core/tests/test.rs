#[macro_use]
mod common;

snapshot_eval!(net_passing, {
    "MyComponent.zen" => r#"
        ComponentInterface = interface(p1 = Net, p2 = Net)
        input = io("input", ComponentInterface)

        Component(
            name = "capacitor",
            type = "capacitor",
            pin_defs = { "P1": "1", "P2": "2" },
            footprint = "SMD:0805",
            pins = { "P1": input.p1, "P2": input.p2 },
        )
    "#,
    "test.zen" => r#"
        load("MyComponent.zen", "ComponentInterface")
        MyComponent = Module("MyComponent.zen")

        MyComponent(
            name = "MyComponent",
            input = ComponentInterface("INTERFACE"),
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
            footprint = "SMD:0805",
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
            footprint = "SMD:0805",
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
            footprint = "SMD:0805",
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
            footprint = "SMD:0805",
        )
    "#,
    "Top.zen" => r#"
        MyModule = Module("MyModule.zen")
        MyModule(name = "MyModule1")
        MyModule(name = "MyModule2")
        MyModule(name = "MyModule3")
    "#
});
