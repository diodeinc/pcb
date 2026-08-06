mod common;

use std::collections::BTreeSet;

use common::kicad_builder::{KicadBuilder, TestPin};
use pcb_kicad_sch::connectivity::{ComponentIdentity, ConnectivityGraph, Terminal};

#[test]
fn wires_connect_at_shared_endpoints() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .wire((1.0, 0.0), (2.0, 0.0))
        .local_label("LEFT", (0.0, 0.0))
        .local_label("RIGHT", (2.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["LEFT", "RIGHT"])]
    );
}

#[test]
fn wire_endpoint_does_not_connect_to_segment_without_junction() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((-1.0, 0.0), (1.0, 0.0))
        .wire((0.0, 0.0), (0.0, 1.0))
        .local_label("HORIZONTAL", (-1.0, 0.0))
        .local_label("VERTICAL", (0.0, 1.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["HORIZONTAL"]), names(&["VERTICAL"])]
    );
}

#[test]
fn junction_connects_segments_that_cross_in_their_interiors() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((-1.0, 0.0), (1.0, 0.0))
        .wire((0.0, -1.0), (0.0, 1.0))
        .junction((0.0, 0.0))
        .local_label("HORIZONTAL", (-1.0, 0.0))
        .local_label("VERTICAL", (0.0, -1.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["HORIZONTAL", "VERTICAL"])]
    );
}

#[test]
fn label_connects_to_the_middle_of_a_wire() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((-1.0, 0.0), (1.0, 0.0))
        .local_label("MID", (0.0, 0.0));

    assert_eq!(named_groups(builder.build()), vec![names(&["MID"])]);
}

#[test]
fn same_name_label_kinds_connect_on_one_page() {
    let mut builder = KicadBuilder::new();
    builder
        .local_label("SIGNAL", (0.0, 0.0))
        .hierarchical_label("SIGNAL", (1.0, 0.0))
        .global_label("SIGNAL", (2.0, 0.0));

    assert_eq!(named_groups(builder.build()), vec![names(&["SIGNAL"])]);
}

#[test]
fn sheet_pin_connects_only_to_its_child_hierarchical_label() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("PARENT", (0.0, 0.0))
        .sheet("child.kicad_sch", &[("PORT", (1.0, 0.0))])
        .add_page("child", "child.kicad_sch")
        .wire((0.0, 0.0), (1.0, 0.0))
        .hierarchical_label("PORT", (0.0, 0.0))
        .local_label("CHILD", (1.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["CHILD", "PARENT", "PORT"])]
    );
}

#[test]
fn repeated_child_instances_do_not_merge_same_name_sheet_pins() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("A", (0.0, 0.0))
        .sheet("child.kicad_sch", &[("PORT", (1.0, 0.0))])
        .wire((10.0, 0.0), (11.0, 0.0))
        .local_label("B", (10.0, 0.0))
        .sheet("child.kicad_sch", &[("PORT", (11.0, 0.0))])
        .add_page("child", "child.kicad_sch")
        .hierarchical_label("PORT", (0.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["A", "PORT"]), names(&["B", "PORT"])]
    );
}

#[test]
fn no_connect_marker_preserves_an_isolated_component_pin() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:OnePin", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:OnePin", Some("U1"), (0.0, 0.0))
        .no_connect((0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build());

    assert_eq!(graph.components.len(), 1);
    assert_eq!(graph.groups.len(), 1);
    assert_eq!(graph.groups[0].terminals.len(), 1);
}

#[test]
fn unmanaged_component_still_contributes_pin_connectivity() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:OnePin", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:OnePin", None, (0.0, 0.0))
        .local_label("SIGNAL", (0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build());
    assert_eq!(graph.components.len(), 1);
    assert!(graph.components[0].managed_slot.is_none());
    assert!(graph.groups[0].terminals.iter().any(|terminal| {
        matches!(
            terminal,
            Terminal::ComponentPin {
                component: ComponentIdentity::KiCadSymbol(_),
                ..
            }
        )
    }));
}

#[test]
fn no_connect_electrical_pin_never_joins_geometry() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:NC"
              (symbol "NC_1_1"
                (pin no_connect line (at 0 0 0) (length 0)
                  (name "NC") (number "1"))))"#,
        )
        .component("Test:NC", Some("U1"), (0.0, 0.0))
        .local_label("SIGNAL", (0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build());
    let signal = graph
        .groups
        .iter()
        .find(|group| group.names.contains("SIGNAL"))
        .expect("signal group");

    assert!(signal.terminals.is_empty());
}

#[test]
fn hidden_power_input_pin_is_a_global_connection() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Powered"
              (symbol "Powered_1_1"
                (pin power_in line (at 0 0 0) (length 0) hide
                  (name "VCC") (number "1"))))"#,
        )
        .component("Test:Powered", Some("U1"), (0.0, 0.0))
        .add_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 10.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build());
    let vcc = graph
        .groups
        .iter()
        .filter(|group| group.names.contains("VCC"))
        .collect::<Vec<_>>();

    assert_eq!(vcc.len(), 1);
    assert_eq!(vcc[0].terminals.len(), 1);
}

#[test]
fn local_and_global_power_symbols_have_different_scope() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "power:LOCAL" (power local)
              (symbol "LOCAL_1_1"
                (pin power_in line (at 0 0 0) (length 0)
                  (name "LOCAL") (number "1"))))"#,
        )
        .define_symbol_raw(
            r#"(symbol "power:GLOBAL" (power global)
              (symbol "GLOBAL_1_1"
                (pin power_in line (at 0 0 0) (length 0)
                  (name "GLOBAL") (number "1"))))"#,
        )
        .placed_symbol("power:LOCAL", None, Some("LOCAL"), (0.0, 0.0))
        .placed_symbol("power:GLOBAL", None, Some("GLOBAL"), (1.0, 0.0))
        .add_page("other", "other.kicad_sch")
        .placed_symbol("power:LOCAL", None, Some("LOCAL"), (0.0, 0.0))
        .placed_symbol("power:GLOBAL", None, Some("GLOBAL"), (1.0, 0.0));

    let groups = named_groups(builder.build());

    assert_eq!(
        groups,
        vec![names(&["GLOBAL"]), names(&["LOCAL"]), names(&["LOCAL"]),]
    );
}

#[test]
fn duplicate_pin_numbers_configured_as_jumpers_connect_internally() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Jumper" (duplicate_pin_numbers_are_jumpers yes)
              (symbol "Jumper_1_1"
                (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
                (pin passive line (at 10 0 0) (length 0) (name "B") (number "1"))))"#,
        )
        .component("Test:Jumper", Some("J1"), (0.0, 0.0))
        .local_label("LEFT", (0.0, 0.0))
        .local_label("RIGHT", (10.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["LEFT", "RIGHT"])]
    );
}

#[test]
fn explicit_jumper_pin_groups_connect_different_pin_numbers() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Jumper" (jumper_pin_groups ("1" "2"))
              (symbol "Jumper_1_1"
                (pin passive line (at 0 0 0) (length 0) (name "A") (number "1"))
                (pin passive line (at 10 0 0) (length 0) (name "B") (number "2"))))"#,
        )
        .component("Test:Jumper", Some("J1"), (0.0, 0.0))
        .local_label("LEFT", (0.0, 0.0))
        .local_label("RIGHT", (10.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["LEFT", "RIGHT"])]
    );
}

fn named_groups(document: pcb_kicad_sch::SchDocument) -> Vec<BTreeSet<String>> {
    let mut groups = ConnectivityGraph::from_kicad(&document)
        .groups
        .into_iter()
        .filter(|group| !group.names.is_empty())
        .map(|group| group.names)
        .collect::<Vec<_>>();
    groups.sort();
    groups
}

fn names(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
