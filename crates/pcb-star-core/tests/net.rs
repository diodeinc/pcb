#[macro_use]
mod common;

snapshot_eval!(net_with_kwargs, {
    "test.star" => r#"
        # Test nets with keyword arguments
        net_a = Net("VCC", voltage="3.3V", type="power")
        net_b = Net("GND", type="ground", protection="esd")
        net_c = Net("DATA", speed="high", impedance=50)
        net_d = Net()  # No name or properties

        # Test that kwargs work with various types
        net_with_types = Net(
            "SIGNAL",
            voltage="5.0",
            current_limit=100,
            protected=True,
            notes="Main signal line",
            list_prop=[1, 2, 3],
            dict_prop={"nested": "value"}
        )

        # Test component connections with nets created using kwargs
        Component(
            name = "resistor",
            type = "resistor",
            pin_defs = {"P1": "1", "P2": "2"},
            footprint = "SMD:0805",
            pins = {
                "P1": net_a,
                "P2": net_b,
            },
        )

        # Additional components to ensure all nets are connected
        Component(
            name = "data_buffer",
            type = "buffer",
            pin_defs = {"IN": "1", "OUT": "2"},
            footprint = "SOT:23",
            pins = {
                "IN": net_c,
                "OUT": net_with_types,
            },
        )
    "#
});

snapshot_eval!(net_kwargs_backwards_compatibility, {
    "test.star" => r#"
        # Test that the old properties dict syntax still works
        # (since properties is now just a regular kwarg)
        net_old_style = Net("OLD", properties={"key": "value"})

        # Test mixing positional name with kwargs
        net_mixed = Net("MIXED", foo="bar", baz=42)

        # Test only kwargs, no positional args
        net_kwargs_only = Net(name="KWARGS_ONLY", type="signal")

        Component(
            name = "test_comp",
            type = "generic",
            pin_defs = {"P1": "1", "P2": "2", "P3": "3"},
            footprint = "TEST:FOOTPRINT",
            pins = {
                "P1": net_old_style,
                "P2": net_mixed,
                "P3": net_kwargs_only,
            },
        )
    "#
});
