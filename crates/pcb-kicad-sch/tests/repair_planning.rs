mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    Point, SchDocument, SchItem, Wire,
    analysis::{SchematicIssue, inspect_schematic},
    connectivity::ConnectivityItemRef,
    reconcile::{plan_reconciliation, plan_repairs},
};

const LEFT_PIN: Point = Point::new(207.01, 146.05);
const MID_PIN: Point = Point::new(207.01, 151.13);

#[test]
fn removes_only_the_unique_bridge_wire() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");
    let mut document = fixture.kicad_document().clone();
    add_wire(&mut document, "shorting-wire", LEFT_PIN, MID_PIN);

    let inspection = inspect_schematic(&document, fixture.netlist()).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .unwrap();
    let plan = plan_repairs(
        &document,
        fixture.netlist(),
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();

    assert!(
        !repaired.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Wire(wire) if wire.id == "shorting-wire"))
    );
}

#[test]
fn replaces_the_affected_region_when_single_item_repairs_are_ambiguous() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let left = label_point(&document, "LEFT");
    let mid = label_point(&document, "MID");
    let bend = Point::new(left.x + 12.7, left.y + 12.7);
    add_wire(&mut document, "ambiguous-a", left, bend);
    add_wire(&mut document, "ambiguous-b", bend, mid);
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .unwrap();
    let short_wires = short
        .items
        .iter()
        .filter(|item| matches!(item, ConnectivityItemRef::Wire { .. }))
        .cloned()
        .collect::<BTreeSet<_>>();

    let plan = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();

    assert!(contains_wire_id(&short_wires, "ambiguous-a"));
    assert!(contains_wire_id(&short_wires, "ambiguous-b"));
    assert!(short_wires.iter().all(|item| !contains(&repaired, item)));

    let all = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    assert!(short_wires.iter().all(|item| !contains(&all, item)));
    assert_eq!(repaired, all);
}

fn label_point(document: &SchDocument, name: &str) -> Point {
    document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == name => Some(label.at),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing generated label {name}"))
}

fn add_wire(document: &mut SchDocument, id: &str, a: Point, b: Point) {
    document.pages[0].items.push(SchItem::Wire(Wire {
        id: id.to_string(),
        a,
        b,
        unsupported: Vec::new(),
    }));
}

fn contains_wire_id(items: &BTreeSet<ConnectivityItemRef>, expected_id: &str) -> bool {
    items
        .iter()
        .any(|item| matches!(item, ConnectivityItemRef::Wire { id, .. } if id == expected_id))
}

fn contains(document: &SchDocument, item_ref: &ConnectivityItemRef) -> bool {
    document.pages.iter().any(|page| {
        page.items.iter().any(|item| match (item, item_ref) {
            (SchItem::Wire(wire), ConnectivityItemRef::Wire { page_id, id }) => {
                page.id == *page_id && wire.id == *id
            }
            _ => false,
        })
    })
}
