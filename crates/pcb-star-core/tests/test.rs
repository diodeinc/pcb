#[macro_use]
mod common;

snapshot_eval!(load_component_factory, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test.star" => r#"
        # Import factory and instantiate.
        load(".", M123 = "C146731")

        M123(
            name = "M123",
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
            footprint = "SMD:0805",
        )
    "#
});

snapshot_eval!(net_passing, {
    "MyComponent.star" => r#"
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
    "test.star" => r#"
        load("MyComponent.star", "ComponentInterface")
        load(".", MyComponent = "MyComponent")

        MyComponent(
            name = "MyComponent",
            input = ComponentInterface("INTERFACE"),
        )
    "#,
    "top.star" => r#"
        load(".", Test = "test")

        Test(
            name = "Test",
        )
    "#
});

snapshot_eval!(unused_inputs_should_error, {
    "my_module.star" => r#"
        # empty module with no inputs
    "#,
    "top.star" => r#"
        load(".", MyModule = "my_module")

        MyModule(
            name = "MyModule",
            unused = 123,
        )
    "#
});

snapshot_eval!(missing_pins_should_error, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test_missing.star" => r#"
        load(".", COMP = "C146731")

        # Instantiate the component while omitting several required pins.
        COMP(
            name = "Component",
            pins = {
                "ICLK": Net("ICLK"),
                "Q1": Net("Q1"),
            },
            footprint = "SMD:0805",
        )
    "#
});

snapshot_eval!(unknown_pin_should_error, {
    "C146731.kicad_sym" => include_str!("resources/C146731.kicad_sym"),
    "test_unknown.star" => r#"
        load(".", COMP = "C146731")

        # Instantiate the component with an invalid pin included.
        COMP(
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
            footprint = "SMD:0805",
        )
    "#
});

snapshot_eval!(nested_components, {
    "Component.star" => r#"
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
    "Module.star" => r#"
        load(".", MyComponent = "Component")

        MyComponent(
            name = "MyComponent",
        )
    "#,
    "Top.star" => r#"
        load(".", MyModule = "Module")

        MyModule(
            name = "MyModule",
        )
    "#
});

snapshot_eval!(net_name_deduplication, {
    "MyModule.star" => r#"
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
    "Top.star" => r#"
        load(".", MyModule = "MyModule")
        MyModule(name = "MyModule1")
        MyModule(name = "MyModule2")
        MyModule(name = "MyModule3")
    "#
});
