mod common;

use std::collections::BTreeSet;

use common::kicad_builder::{KicadBuilder, TestPin};
use pcb_kicad_sch::{
    LabelSpin, Point, SchItem,
    analysis::analyze_connectivity,
    connectivity::{
        ComponentIdentity, ConnectionGroup, ConnectionOrigin, ConnectivityGraph,
        PhysicalConnectivity, PinVisibility, Terminal,
    },
};

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
fn directive_label_connects_segments_that_cross_at_its_anchor() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((-1.0, 0.0), (1.0, 0.0))
        .wire((0.0, -1.0), (0.0, 1.0))
        .directive_label((0.0, 0.0))
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
fn label_segment_hit_uses_kicads_one_internal_unit_tolerance() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("END", (0.0, 0.0))
        .local_label("ONE_IU", (0.5, 0.0001))
        .local_label("TWO_IU", (0.5, 0.0002));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["END", "ONE_IU"]), names(&["TWO_IU"])]
    );
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
        vec![names(&["CHILD", "PARENT"])]
    );
}

#[test]
fn hierarchy_aliases_are_not_reported_as_unexpected_nets() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("NET", (0.0, 0.0))
        .sheet("child.kicad_sch", &[("PORT", (1.0, 0.0))])
        .add_page("child", "child.kicad_sch")
        .hierarchical_label("PORT", (0.0, 0.0));
    let observed = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let expected = ConnectivityGraph {
        components: Vec::new(),
        groups: vec![ConnectionGroup {
            names: names(&["NET"]),
            terminals: BTreeSet::new(),
            origins: BTreeSet::from([ConnectionOrigin::ZenerNet {
                name: "NET".to_string(),
            }]),
        }],
    };

    let analysis = analyze_connectivity(&expected, &observed);

    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
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
        vec![names(&["A"]), names(&["B"])]
    );
}

#[test]
fn managed_symbols_on_a_reused_sheet_are_rejected_as_ambiguous() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:OnePin", &[TestPin::passive("1", (0.0, 0.0))])
        .sheet("child.kicad_sch", &[])
        .sheet("child.kicad_sch", &[])
        .add_page("child", "child.kicad_sch")
        .component("Test:OnePin", Some("U1"), (0.0, 0.0));

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();

    assert!(error.to_string().contains("managed symbol"));
    assert!(error.to_string().contains("repeated page"));
}

#[test]
fn unmanaged_component_still_contributes_pin_connectivity() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:OnePin", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:OnePin", None, (0.0, 0.0))
        .local_label("SIGNAL", (0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
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
fn no_connect_electrical_pin_still_participates_in_connectivity() {
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

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let signal = graph
        .groups
        .iter()
        .find(|group| group.names.contains("SIGNAL"))
        .expect("signal group");

    assert_eq!(signal.terminals.len(), 1);
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
        .add_root_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 10.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let vcc = graph
        .groups
        .iter()
        .filter(|group| group.names.contains("VCC"))
        .collect::<Vec<_>>();

    assert_eq!(vcc.len(), 1);
    assert_eq!(vcc[0].terminals.len(), 1);
}

#[test]
fn explicitly_visible_power_input_pin_is_not_a_global_connection() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Powered"
              (symbol "Powered_1_1"
                (pin power_in line (at 0 0 0) (length 0) (hide no)
                  (name "VCC") (number "1"))))"#,
        )
        .component("Test:Powered", Some("U1"), (0.0, 0.0))
        .add_root_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 10.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let vcc = graph
        .groups
        .iter()
        .filter(|group| group.names.contains("VCC"))
        .collect::<Vec<_>>();

    assert_eq!(vcc.len(), 1);
    assert!(vcc[0].terminals.is_empty());
}

#[test]
fn wired_hidden_power_input_does_not_create_a_global_connection() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Powered"
              (symbol "Powered_1_1"
                (pin power_in line (at 0 0 0) (length 0) hide
                  (name "VCC") (number "1"))))"#,
        )
        .component("Test:Powered", Some("U1"), (0.0, 0.0))
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("LOCAL", (1.0, 0.0))
        .add_root_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["LOCAL"]), names(&["VCC"])]
    );
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
        .add_root_page("other", "other.kicad_sch")
        .placed_symbol("power:LOCAL", None, Some("LOCAL"), (0.0, 0.0))
        .placed_symbol("power:GLOBAL", None, Some("GLOBAL"), (1.0, 0.0));

    let groups = named_groups(builder.build());

    assert_eq!(
        groups,
        vec![names(&["GLOBAL"]), names(&["LOCAL"]), names(&["LOCAL"]),]
    );
}

#[test]
fn named_driver_island_exposes_its_power_symbol_pin_attachment() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "power:VOUT" (power global)
              (symbol "VOUT_1_1"
                (pin power_in line (at 0 0 0) (length 0)
                  (name "VOUT") (number "1"))))"#,
        )
        .placed_symbol("power:VOUT", None, Some("VOUT"), (12.3456, 23.4567));
    let document = builder.build();
    let symbol_id = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol.id.as_str()),
            _ => None,
        })
        .unwrap();

    let physical = PhysicalConnectivity::from_kicad(&document, PinVisibility::VisibleOnly).unwrap();
    let island = physical
        .islands
        .values()
        .find(|island| island.named_drivers.contains_key("VOUT"))
        .unwrap();
    assert_eq!(island.symbol_pins.len(), 1, "{island:#?}");
    let pin = island.symbol_pins.first().unwrap();

    assert!(island.terminals.is_empty());
    assert_eq!(pin.page_id(), "root");
    assert_eq!(pin.symbol_id(), symbol_id);
    assert_eq!(pin.number(), "1");
    assert_eq!(pin.point(), Point::new(12.3456, 23.4567));
    assert_eq!(pin.outward_spin(), LabelSpin::Left);
}

#[test]
fn bare_power_marker_has_kicads_defined_global_scope() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "power:VCC" (power)
              (symbol "VCC_1_1"
                (pin power_in line (at 0 0 0) (length 0)
                  (name "VCC") (number "1"))))"#,
        )
        .placed_symbol("power:VCC", None, Some("VCC"), (0.0, 0.0))
        .add_root_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 0.0));

    assert_eq!(named_groups(builder.build()), vec![names(&["VCC"])]);
}

#[test]
fn power_flag_is_not_a_power_net_driver() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "power:PWR_FLAG" (power global)
              (symbol "PWR_FLAG_1_1"
                (pin power_out line (at 0 0 0) (length 0)
                  (name "") (number "1"))))"#,
        )
        .placed_symbol("power:PWR_FLAG", None, Some("PWR_FLAG"), (0.0, 0.0))
        .local_label("A", (0.0, 0.0))
        .add_root_page("other", "other.kicad_sch")
        .placed_symbol("power:PWR_FLAG", None, Some("PWR_FLAG"), (10.0, 0.0))
        .local_label("B", (10.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["A"]), names(&["B"])]
    );
}

#[test]
fn placed_pin_alternate_controls_effective_power_type_and_name() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Alternate"
              (symbol "Alternate_1_1"
                (pin passive line (at 0 0 0) (length 0) hide
                  (name "BASE") (number "1")
                  (alternate "VCC" power_in line))))"#,
        )
        .component("Test:Alternate", Some("U1"), (0.0, 0.0))
        .pin_alternate("1", "VCC")
        .add_root_page("other", "other.kicad_sch")
        .global_label("VCC", (10.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let vcc = graph
        .groups
        .iter()
        .filter(|group| group.names.contains("VCC"))
        .collect::<Vec<_>>();
    assert_eq!(vcc.len(), 1);
    assert_eq!(vcc[0].terminals.len(), 1);
}

#[test]
fn placed_alternate_with_duplicate_pin_number_is_ambiguous() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Alternate"
              (symbol "Alternate_1_1"
                (pin passive line (at 0 0 0) (length 0)
                  (name "A") (number "1") (alternate "ALT" passive line))
                (pin passive line (at 10 0 0) (length 0)
                  (name "B") (number "1") (alternate "ALT" passive line))))"#,
        )
        .component("Test:Alternate", Some("U1"), (0.0, 0.0))
        .pin_alternate("1", "ALT");

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();
    let message = format!("{error:#}");

    assert!(
        message.contains("one alternate for 2 definition pins"),
        "{message}"
    );
}

#[test]
fn stacked_pin_numbers_expand_to_exact_logical_numbers() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Stacked"
              (symbol "Stacked_1_1"
                (pin passive line (at 0 0 0) (length 0)
                  (name "P") (number "[1-3]"))))"#,
        )
        .component("Test:Stacked", Some("U1"), (0.0, 0.0))
        .local_label("NET", (0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let Terminal::ComponentPin { pin_numbers, .. } = graph.groups[0].terminals.first().unwrap()
    else {
        panic!("expected component pin");
    };
    assert_eq!(pin_numbers, &names(&["1", "2", "3"]));
    assert!(!pin_numbers.contains("[1-3]"));
}

#[test]
fn oversized_stacked_pin_range_is_rejected() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "Test:Stacked"
              (symbol "Stacked_1_1"
                (pin passive line (at 0 0 0) (length 0)
                  (name "P") (number "[1-4097]"))))"#,
        )
        .component("Test:Stacked", Some("U1"), (0.0, 0.0));

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();
    let message = format!("{error:#}");

    assert!(message.contains("limit of 4096 pins"), "{message}");
}

#[test]
fn connectivity_uses_exact_kicad_internal_units() {
    let mut builder = KicadBuilder::new();
    builder
        .wire((0.0, 0.0), (1.0, 0.0))
        .local_label("A", (0.0, 0.0))
        .wire((1.0001, 0.0), (2.0, 0.0))
        .local_label("B", (2.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["A"]), names(&["B"])]
    );
}

#[test]
fn each_page_uses_its_own_embedded_symbol_definition() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:Shared", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:Shared", Some("U1"), (0.0, 0.0))
        .local_label("FIRST", (0.0, 0.0))
        .add_root_page("second", "second.kicad_sch")
        .define_symbol("Test:Shared", &[TestPin::passive("1", (5.0, 0.0))])
        .component("Test:Shared", Some("U2"), (10.0, 0.0))
        .local_label("SECOND", (15.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();

    assert_eq!(
        named_group_terminal_counts(&graph),
        [("FIRST", 1), ("SECOND", 1)]
    );
}

#[test]
fn missing_embedded_symbol_definition_is_an_error() {
    let mut builder = KicadBuilder::new();
    builder.component("Test:Missing", Some("U1"), (0.0, 0.0));

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("no cached definition Test:Missing")
    );
}

#[test]
fn malformed_stacked_pin_number_remains_literal() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:Invalid", &[TestPin::passive("[3-1]", (0.0, 0.0))])
        .component("Test:Invalid", Some("U1"), (0.0, 0.0))
        .local_label("NET", (0.0, 0.0));

    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let Terminal::ComponentPin { pin_numbers, .. } = graph.groups[0].terminals.first().unwrap()
    else {
        panic!("expected component pin");
    };

    assert!(pin_numbers.contains("[3-1]"));
}

#[test]
fn invalid_power_scope_is_an_error() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol_raw(
            r#"(symbol "power:Invalid" (power guessed)
              (symbol "Invalid_1_1"
                (pin power_in line (at 0 0 0) (length 0)
                  (name "VCC") (number "1"))))"#,
        )
        .placed_symbol("power:Invalid", None, Some("VCC"), (0.0, 0.0));

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();

    assert!(error.to_string().contains("invalid power scope"));
}

#[test]
fn bus_items_fail_connectivity_reduction_explicitly() {
    let mut document = KicadBuilder::new().build();
    document.pages[0]
        .items
        .push(SchItem::Unsupported(pcb_sexpr::parse("(bus)").unwrap()));

    let error = ConnectivityGraph::from_kicad(&document).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("bus connectivity is not supported")
    );
}

#[test]
fn kicad_net_text_uses_exact_escape_and_expression_rules() {
    let mut builder = KicadBuilder::new();
    builder.local_label("${SIGNAL}", (0.0, 0.0));

    let error = ConnectivityGraph::from_kicad(&builder.build()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("unsupported KiCad text expression")
    );

    let mut builder = KicadBuilder::new();
    builder
        .local_label("A{slash}B", (0.0, 0.0))
        .local_label(r"\${SIGNAL}", (1.0, 0.0));

    assert_eq!(
        named_groups(builder.build()),
        vec![names(&["${SIGNAL}"]), names(&["A/B"])]
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

#[test]
fn duplicate_sheet_ids_are_rejected() {
    let mut duplicate_sheets = KicadBuilder::new();
    duplicate_sheets
        .sheet("child.kicad_sch", &[])
        .sheet("child.kicad_sch", &[])
        .add_page("child", "child.kicad_sch");
    let mut document = duplicate_sheets.build();
    let duplicate_id = document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(&sheet.id),
            _ => None,
        })
        .next()
        .unwrap()
        .clone();
    let second_id = document.pages[0]
        .items
        .iter_mut()
        .filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(&mut sheet.id),
            _ => None,
        })
        .nth(1)
        .unwrap();
    *second_id = duplicate_id;
    assert!(
        ConnectivityGraph::from_kicad(&document)
            .unwrap_err()
            .to_string()
            .contains("duplicate sheet id")
    );
}

fn named_groups(document: pcb_kicad_sch::SchDocument) -> Vec<BTreeSet<String>> {
    let mut groups = ConnectivityGraph::from_kicad(&document)
        .unwrap()
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

fn named_group_terminal_counts(graph: &ConnectivityGraph) -> Vec<(&str, usize)> {
    graph
        .groups
        .iter()
        .filter_map(|group| Some((group.names.first()?.as_str(), group.terminals.len())))
        .collect()
}
