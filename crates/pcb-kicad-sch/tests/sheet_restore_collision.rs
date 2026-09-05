mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    Point, SchItem, Wire,
    analysis::{SchematicIssue, inspect_schematic},
    reconcile::{plan_reconciliation, plan_repairs},
};

#[test]
fn restoring_a_sheet_repairs_connectivity_introduced_at_its_pin() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    let root = document
        .pages
        .iter_mut()
        .find(|page| page.file_name.as_deref() == Some("root.kicad_sch"))
        .expect("fixture has a root page");
    let source_pin = root
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) if sheet.file.value == "FILTER_A.kicad_sch" => {
                sheet.placed = false;
                sheet
                    .pins
                    .iter()
                    .find(|pin| pin.name == "INPUT")
                    .map(|pin| pin.at)
            }
            _ => None,
        })
        .expect("FILTER_A has an INPUT sheet pin");
    let sink = root
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "SINK" => Some(label.at),
            _ => None,
        })
        .expect("fixture drives SINK on the root page");

    // Model user wiring added while the restored sheet was absent. Without
    // its pin, this merely extends SINK; placing the sheet makes the wire
    // incorrectly join FILTER_A.INPUT (SOURCE) to SINK.
    root.items.retain(|item| {
        !matches!(item, SchItem::Label(label) if label.text == "SOURCE" && label.at == source_pin)
    });
    let elbow = Point::new(sink.x + 25.4, source_pin.y);
    root.items.extend([
        SchItem::Wire(Wire {
            id: "wire-at-unplaced-sheet-pin".to_string(),
            a: source_pin,
            b: elbow,
            unsupported: Vec::new(),
        }),
        SchItem::Wire(Wire {
            id: "wire-from-sheet-pin-to-sink".to_string(),
            a: elbow,
            b: sink,
            unsupported: Vec::new(),
        }),
    ]);

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let missing_sheet = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::MissingSheet { .. }))
        .expect("unplaced hierarchy is reported")
        .key
        .clone();
    assert!(
        !inspection.issues.iter().any(|issue| matches!(
            issue.issue,
            SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. }
        )),
        "the collision is latent until the sheet pin is restored: {:#?}",
        inspection.issues
    );

    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([missing_sheet]),
    )
    .expect("restoring the selected sheet also repairs its newly exposed collision")
    .apply(Some(&document))
    .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(after.analysis.is_equivalent(), "{:#?}", after.issues);
}
