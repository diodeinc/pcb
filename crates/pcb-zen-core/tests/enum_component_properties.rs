#[macro_use]
mod common;

snapshot_eval!(enum_property_conversion, {
    "test.zen" => r#"
        Level = enum("LOW", "HIGH", "TRISTATE")
        Orientation = enum("NORTH", "SOUTH", "EAST", "WEST")

        Component(
            name = "U1",
            footprint = "TEST:0402",
            pin_defs = { "IN": "1", "OUT": "2" },
            pins = { "IN": Net("INPUT"), "OUT": Net("OUTPUT") },
            properties = {
                "logic_level": Level("HIGH"),
                "orientation": Orientation("NORTH"),
                "voltage": "3.3V",
                "count": 42,
            }
        )
    "#
});

snapshot_eval!(enum_list_property_conversion, {
    "test.zen" => r#"
        State = enum("IDLE", "RUNNING", "STOPPED")

        Component(
            name = "U2",
            footprint = "TEST:0402",
            pin_defs = { "IN": "1" },
            pins = { "IN": Net("INPUT") },
            properties = {
                "valid_states": [State("IDLE"), State("RUNNING"), State("STOPPED")],
                "mixed_list": [State("IDLE"), "string", 123],
            }
        )
    "#
});
