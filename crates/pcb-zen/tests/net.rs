mod common;
use common::TestProject;

/// Test that nets created in a parent module and passed to a child module
/// retain the parent's scoping when cast via io().
///
/// This is a regression test for a bug where nets appearing in multiple modules'
/// `introduced_nets` would get overwritten with the child module's scoped name.
///
/// Example scenario:
/// - Parent creates `VIN = Power("VIN")` (typed net)
/// - Child has `VIN = io("VIN", Power)` and casts with `Net(VIN)`
/// - The net should be named "VIN", not "Child.VIN"
#[test]
fn net_scoping_preserves_parent_name_on_cast() {
    let env = TestProject::new();

    // Child module: receives Power typed net via io(), explicitly casts to Net
    // The Net(...) cast causes the net to appear in the child's introduced_nets,
    // but the canonical name should remain from the parent.
    env.add_file(
        "child.zen",
        r#"
# Define Power type
Power = builtin.net_type("Power")

# Typed net inputs
VIN = io("VIN", Power)
GND = io("GND", Power)

# Explicitly cast to Net - this triggers the bug where the net gets
# re-registered in the child's introduced_nets
VIN_NET = Net(VIN)
GND_NET = Net(GND)

# Internal net created in this module - should be prefixed
INTERNAL = Net("INTERNAL")

Component(
    name = "R1",
    prefix = "R",
    footprint = "SMD:0402",
    pin_defs = { "P1": "1", "P2": "2" },
    pins = { "P1": VIN_NET, "P2": GND_NET },
)

Component(
    name = "R2",
    prefix = "R",
    footprint = "SMD:0402",
    pin_defs = { "P1": "1", "P2": "2" },
    pins = { "P1": INTERNAL, "P2": GND_NET },
)
"#,
    );

    // Parent module: creates typed Power nets and passes them to child
    env.add_file(
        "top.zen",
        r#"
# Define Power type
Power = builtin.net_type("Power")

Child = Module("child.zen")

# Create Power typed nets in parent - should keep names without child prefix
VIN = Power("VIN")
GND = Power("GND")

Child(
    name = "Regulator",
    VIN = VIN,
    GND = GND,
)
"#,
    );

    // Expected net names:
    // - "VIN" (created in root as Power, cast in child)
    // - "GND" (created in root as Power, cast in child)
    // - "Regulator.INTERNAL" (created in child, should be prefixed)
    star_snapshot!(env, "top.zen");
}
