mod common;

use std::collections::BTreeSet;

use common::kicad_builder::{KicadBuilder, TestPin};
use pcb_kicad_sch::{
    SchDocument, SchItem,
    connectivity::{ConnectivityGraph, Terminal},
    kicad::KicadSchSource,
};

fn raw(document: &mut SchDocument, page: usize, text: &str) {
    document.pages[page]
        .items
        .push(SchItem::Unsupported(pcb_sexpr::parse(text).unwrap()));
}

fn named_groups(document: &SchDocument) -> BTreeSet<BTreeSet<String>> {
    ConnectivityGraph::from_kicad(document)
        .unwrap()
        .groups
        .into_iter()
        .map(|group| group.names)
        .filter(|names| !names.is_empty())
        .collect()
}

fn names(groups: &[&[&str]]) -> BTreeSet<BTreeSet<String>> {
    groups
        .iter()
        .map(|names| names.iter().map(|name| (*name).to_owned()).collect())
        .collect()
}

#[test]
fn global_buses_map_vectors_by_ordinal_without_globalizing_local_buses() {
    let mut builder = KicadBuilder::new();
    builder
        .global_label("D[0..1]", (0.0, 0.0))
        .local_label("A[5..4]", (0.0, 0.0))
        .local_label("A4", (10.0, 0.0))
        .local_label("A5", (20.0, 0.0))
        .add_root_page("second", "second.kicad_sch")
        .global_label("D[0..1]", (0.0, 0.0))
        .local_label("B[8..9]", (0.0, 0.0))
        .local_label("B8", (10.0, 0.0))
        .local_label("B9", (20.0, 0.0))
        .add_root_page("third", "third.kicad_sch")
        .local_label("D[0..1]", (0.0, 0.0))
        .local_label("D0", (10.0, 0.0))
        .add_root_page("fourth", "fourth.kicad_sch")
        .global_label("D0", (0.0, 0.0));
    assert_eq!(
        named_groups(&builder.build()),
        names(&[&["A4", "B8", "D0"], &["A5", "B9"], &["D0"]])
    );
}

#[test]
fn intermediate_bus_members_connect_without_breakouts() {
    // Verified against KiCad 10.0.6 exports of a hand-written three-sheet
    // circuit: common member names connect without a scalar breakout on the
    // middle sheet. Sharing one member must not join the entire bundles.
    for (group, expected) in [
        ("{A4 A5}", names(&[&["A4", "B0"], &["A5", "B1"]])),
        ("{A4 C5}", names(&[&["A4", "B0"], &["A5"], &["B1"]])),
        ("{C4 C5}", names(&[&["A4"], &["A5"], &["B0"], &["B1"]])),
    ] {
        let mut builder = KicadBuilder::new();
        builder
            .sheet("middle.kicad_sch", &[("A[4..5]", (0.0, 0.0))])
            .local_label("A[4..5]", (0.0, 0.0))
            .local_label("A4", (10.0, 0.0))
            .local_label("A5", (20.0, 0.0))
            .add_page("middle", "middle.kicad_sch")
            .hierarchical_label("A[4..5]", (0.0, 0.0))
            .local_label(group, (10.0, 0.0))
            .sheet("leaf.kicad_sch", &[("B[0..1]", (10.0, 0.0))])
            .add_page("leaf", "leaf.kicad_sch")
            .hierarchical_label("B[0..1]", (0.0, 0.0))
            .local_label("B0", (10.0, 0.0))
            .local_label("B1", (20.0, 0.0));
        assert_eq!(named_groups(&builder.build()), expected, "{group}");
    }
}

#[test]
fn crossing_buses_only_connect_at_a_junction_and_never_to_crossing_wires() {
    let mut builder = KicadBuilder::new();
    builder
        .local_label("A[0..1]", (-10.0, 0.0))
        .local_label("B[0..1]", (0.0, -10.0))
        .local_label("A0", (30.0, 0.0))
        .local_label("B0", (40.0, 0.0))
        .wire((-5.0, 0.0), (5.0, 0.0))
        .local_label("SCALAR", (-5.0, 0.0));
    let mut document = builder.build();
    raw(&mut document, 0, "(bus (pts (xy -10 0) (xy 10 0)))");
    raw(&mut document, 0, "(bus (pts (xy 0 -10) (xy 0 10)))");
    assert_eq!(
        named_groups(&document),
        names(&[&["A0"], &["B0"], &["SCALAR"]])
    );
    document.pages[0]
        .items
        .push(SchItem::Junction(pcb_kicad_sch::Junction {
            id: "junction".into(),
            at: Default::default(),
            unsupported: vec![],
        }));
    assert_eq!(
        named_groups(&document),
        names(&[&["A0", "B0"], &["SCALAR"]])
    );
}

#[test]
fn repeated_bus_sheets_are_isolated_and_ports_match_original_text() {
    let mut builder = KicadBuilder::new();
    builder
        .local_label("A[0..1]", (0.0, 0.0))
        .sheet("child.kicad_sch", &[("B[0..1]", (0.0, 0.0))])
        .local_label("C[0..1]", (10.0, 0.0))
        .sheet("child.kicad_sch", &[("B[0..1]", (10.0, 0.0))])
        .local_label("A0", (20.0, 0.0))
        .local_label("C0", (30.0, 0.0))
        .add_page("child", "child.kicad_sch")
        .hierarchical_label("B[0..1]", (0.0, 0.0))
        .local_label("B0", (10.0, 0.0));
    let mut document = builder.build();
    assert_eq!(
        named_groups(&document),
        names(&[&["A0", "B0"], &["C0", "B0"]])
    );
    for item in &mut document.pages[1].items {
        if let SchItem::Label(label) = item
            && label.text == "B[0..1]"
        {
            label.text = "B[1..0]".into();
        }
    }
    assert_eq!(named_groups(&document), names(&[&["A0"], &["B0"], &["C0"]]));
}

#[test]
fn root_hierarchical_buses_expose_individual_interface_ports() {
    let mut builder = KicadBuilder::new();
    builder
        .hierarchical_label("{D[1..0] RW}", (0.0, 0.0))
        .local_label("D1", (10.0, 0.0));
    let graph = ConnectivityGraph::from_kicad(&builder.build()).unwrap();
    let ports = graph
        .groups
        .iter()
        .map(|group| group.terminals.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ports,
        ["D0", "D1", "RW"]
            .into_iter()
            .map(|name| BTreeSet::from([Terminal::InterfacePort {
                name: name.to_owned()
            }]))
            .collect()
    );
}

#[test]
fn aliased_groups_match_members_by_name_across_hierarchy_and_roundtrip() {
    let mut builder = KicadBuilder::new();
    builder
        .local_label("Parent{LINK}", (0.0, 0.0))
        .sheet("child.kicad_sch", &[("Child{DATA CLK}", (0.0, 0.0))])
        .local_label("Parent.CLK", (10.0, 0.0))
        .local_label("Parent.DATA", (20.0, 0.0))
        .add_page("child", "child.kicad_sch")
        .hierarchical_label("Child{DATA CLK}", (0.0, 0.0))
        .local_label("Child.CLK", (10.0, 0.0))
        .local_label("Child.DATA", (20.0, 0.0));
    let mut document = builder.build();
    // Alias definitions are shared across sheets; reversed group ordering
    // must not connect CLK to DATA as ordinal vector matching would.
    raw(
        &mut document,
        1,
        "(bus_alias \"LINK\" (members \"CLK\" \"DATA\"))",
    );
    let expected = names(&[&["Parent.CLK", "Child.CLK"], &["Parent.DATA", "Child.DATA"]]);
    assert_eq!(named_groups(&document), expected);

    let files = document.to_kicad_sch_files();
    let roundtrip = SchDocument::from_kicad_sch_files(files.iter().map(|file| KicadSchSource {
        file_name: file.file_name.as_deref(),
        content: &file.content,
        is_root: file.file_name.as_deref() == Some("root.kicad_sch"),
    }))
    .unwrap();
    assert_eq!(named_groups(&roundtrip), expected);
}

#[test]
fn entries_do_not_assign_unlabelled_wires_or_connect_diagonal_interiors() {
    let mut builder = KicadBuilder::new();
    builder
        .define_symbol("Test:Pin", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:Pin", None, (-2.0, 2.0))
        .component("Test:Pin", None, (-4.0, 4.0))
        .component("Test:Pin", None, (-1.0, 1.0))
        .local_label("D[0..1]", (0.0, 0.0))
        .local_label("D0", (-2.0, 2.0));
    let mut document = builder.build();
    raw(&mut document, 0, "(bus (pts (xy 0 0) (xy 0 10)))");
    raw(&mut document, 0, "(bus_entry (at 0 0) (size -2 2))");
    raw(&mut document, 0, "(bus_entry (at -4 4) (size 4 0))");
    let graph = ConnectivityGraph::from_kicad(&document).unwrap();
    assert_eq!(
        graph
            .groups
            .iter()
            .filter(|group| !group.terminals.is_empty())
            .count(),
        3
    );
    assert!(graph.groups.iter().all(|group| group.terminals.len() <= 1));
    assert_eq!(named_groups(&document), names(&[&["D0"]]));

    let mut builder = KicadBuilder::new();
    builder
        .local_label("LEFT", (-2.0, 2.0))
        .local_label("RIGHT", (2.0, 2.0));
    let mut document = builder.build();
    raw(&mut document, 0, "(bus_entry (at -2 2) (size 2 -2))");
    raw(&mut document, 0, "(bus_entry (at 0 0) (size 2 2))");
    assert_eq!(
        named_groups(&document),
        names(&[&["LEFT"], &["RIGHT"]]),
        "entries do not conduct directly into other entries"
    );
    document.pages[0]
        .items
        .push(SchItem::Wire(pcb_kicad_sch::Wire {
            id: "shared-wire".into(),
            a: Default::default(),
            b: pcb_kicad_sch::Point::new(0.0, -2.0),
            unsupported: vec![],
        }));
    assert_eq!(
        named_groups(&document),
        names(&[&["LEFT", "RIGHT"]]),
        "both entry ports can connect to the same wire"
    );
}

#[test]
fn apply_preserves_buses_and_repairs_only_scalar_connections() {
    use pcb_kicad_sch::{
        Junction, Point, Wire,
        analysis::{SchematicIssue, inspect_schematic},
        reconcile::plan_reconciliation,
    };

    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut bus_labels = KicadBuilder::new();
    bus_labels
        .local_label("{SIGNALS}", (200.0, 100.0))
        .local_label("X[0..1]", (200.0, 100.0));
    document.pages[0]
        .items
        .extend(bus_labels.build().pages.remove(0).items);
    raw(
        &mut document,
        0,
        "(bus_alias \"SIGNALS\" (members \"LEFT\" \"MID\"))",
    );
    raw(
        &mut document,
        0,
        "(bus (pts (xy 200 100) (xy 200 130)) (uuid \"bus\"))",
    );
    raw(
        &mut document,
        0,
        "(bus_entry (at 200 100) (size -2 2) (uuid \"entry\"))",
    );
    assert!(
        plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
            .unwrap()
            .is_empty()
    );

    let mid_label = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "MID" => Some(label.clone()),
            _ => None,
        })
        .unwrap();
    let mid = mid_label.at;
    let mut broken = document.clone();
    let mut wrong = mid_label;
    wrong.id = "wrong-breakout".into();
    wrong.text = "X0".into();
    broken.pages[0].items.push(SchItem::Label(wrong));
    let inspection = inspect_schematic(&broken, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
    );
    let plan = plan_reconciliation(Some(&broken), &netlist, "simple.kicad_sch").unwrap();
    assert_eq!(
        plan.apply(Some(&broken)).unwrap(),
        document,
        "remove the wrong scalar label, not the bus"
    );
    assert_eq!(plan.revert(&document).unwrap(), broken);

    // A wire cut must not make orphan cleanup remove a bus junction where
    // that scalar wire happened to cross the bus.
    let left = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "LEFT" => Some(label.at),
            _ => None,
        })
        .unwrap();
    let cross = Point::new((left.x + mid.x) / 2.0, (left.y + mid.y) / 2.0);
    raw(
        &mut document,
        0,
        &format!(
            "(bus (pts (xy {} {}) (xy {} {})) (uuid \"crossing-bus\"))",
            cross.x,
            cross.y - 10.0,
            cross.x,
            cross.y + 10.0
        ),
    );
    document.pages[0].items.push(SchItem::Junction(Junction {
        id: "bus-junction".into(),
        at: cross,
        unsupported: vec![],
    }));
    let mut broken = document.clone();
    broken.pages[0].items.push(SchItem::Wire(Wire {
        id: "scalar-short".into(),
        a: left,
        b: mid,
        unsupported: vec![],
    }));
    let plan = plan_reconciliation(Some(&broken), &netlist, "simple.kicad_sch").unwrap();
    assert_eq!(
        plan.apply(Some(&broken)).unwrap(),
        document,
        "preserve crossing bus junction"
    );
}
