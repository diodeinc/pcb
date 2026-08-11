mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    Point, SchDocument, SchItem, Wire, connectivity::ConnectivityItemRef,
    repair::plan_connectivity_repair,
};

const PAGE_ID: &str = "67ba23f6-8b58-5596-9d28-7774b90e1e12";
const LEFT_PIN: Point = Point::new(207.01, 146.05);
const MID_PIN: Point = Point::new(207.01, 151.13);

#[test]
fn plans_only_the_unique_bridge_wire_without_mutating_the_document() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");
    let mut document = fixture.kicad_document().clone();
    add_wire(&mut document, "shorting-wire", LEFT_PIN, MID_PIN);
    let original = document.clone();

    let plan = plan_connectivity_repair(&document, fixture.netlist()).unwrap();

    assert_eq!(document, original);
    assert_eq!(
        plan.removals(),
        &BTreeSet::from([ConnectivityItemRef::Wire {
            page_id: PAGE_ID.to_string(),
            id: "shorting-wire".to_string(),
        }])
    );
    assert_eq!(
        plan.reconnect_nets(),
        &BTreeSet::from(["LEFT".to_string(), "MID".to_string()])
    );
}

#[test]
fn rejects_multiple_equally_small_repairs_instead_of_guessing() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");
    let mut document = fixture.kicad_document().clone();
    let bend = Point::new(LEFT_PIN.x + 12.7, LEFT_PIN.y + 12.7);
    add_wire(&mut document, "ambiguous-a", LEFT_PIN, bend);
    add_wire(&mut document, "ambiguous-b", bend, MID_PIN);

    let error = plan_connectivity_repair(&document, fixture.netlist()).unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("multiple equally minimal repairs"),
        "{message}"
    );
    assert!(message.contains("ambiguous-a"), "{message}");
    assert!(message.contains("ambiguous-b"), "{message}");
}

fn add_wire(document: &mut SchDocument, id: &str, a: Point, b: Point) {
    document.pages[0].items.push(SchItem::Wire(Wire {
        id: id.to_string(),
        a,
        b,
        unsupported: Vec::new(),
    }));
}
