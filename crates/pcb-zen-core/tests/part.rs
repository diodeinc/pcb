#[macro_use]
mod common;

snapshot_netlist_eval!(part_not_overwritten_by_prop, {
    "test.zen" => r#"
P1 = Net()
P2 = Net()

primary = builtin.Part(
    mpn = "PART-TYPED",
    manufacturer = "MFR-TYPED",
    qualifications = ["Qualified"],
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = primary,
    properties = {
        "part": "legacy-string-value",
        "tag": "ok",
    },
)
"#
});

snapshot_netlist_eval!(part_overrides_scalars, {
    "test.zen" => r#"
P1 = Net()
P2 = Net()

preferred = builtin.Part(
    mpn = "PART-A",
    manufacturer = "MFR-A",
    qualifications = ["Preferred"],
)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = preferred,
    mpn = "PART-B",
    manufacturer = "MFR-B",
)
"#
});

snapshot_netlist_eval!(modifier_updates_part_and_alts, {
    "test.zen" => r#"
P1 = Net()
P2 = Net()

def mutate(component):
    if component.name == "R1":
        component.part = builtin.Part(
            mpn = "PART-MOD",
            manufacturer = "MFR-MOD",
            qualifications = ["Preferred"],
        )
        component.alternatives = [
            builtin.Part(mpn = "ALT-1", manufacturer = "ALT-MFR-1"),
        ]
        component.alternatives.append(
            builtin.Part(mpn = "ALT-2", manufacturer = "ALT-MFR-2")
        )

builtin.add_component_modifier(mutate)

Component(
    name = "R1",
    footprint = "Resistor_SMD:R_0603_1005Metric",
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
)
"#
});
