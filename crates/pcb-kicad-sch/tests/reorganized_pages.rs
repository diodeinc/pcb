mod common;
use std::collections::BTreeSet;

use pcb_kicad_sch::{
    LabelKind, Point, SchItem, SchPage, Sheet, SymbolField,
    analysis::{SchematicIssue, inspect_schematic},
    reconcile::{plan_reconciliation, plan_repairs},
};

/// A user may reorganize managed content onto their own sheets: as long as
/// connectivity holds, later reconciliation accepts the layout instead of
/// bailing over page ownership.
#[test]
fn user_reorganized_page_stays_reconcilable() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let baseline = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut document = baseline.clone();
    // Move the entire circuit onto a user-created page.
    let root = &mut document.pages[0];
    let moved_items = std::mem::take(&mut root.items);
    let library = root.library.clone();
    let mut user_page = SchPage::new("user-page");
    user_page.file_name = Some("user.kicad_sch".to_string());
    user_page.library = library;
    user_page.items = moved_items;
    document.pages.push(user_page);
    document.pages[0].items.push(SchItem::Sheet(Box::new(Sheet {
        id: "user-sheet".to_string(),
        placed: true,
        at: Some(Point::new(200.0, 100.0)),
        size: Some(Point::new(40.0, 20.0)),
        name: Some(SymbolField::new(
            "Sheetname",
            "User",
            Point::new(200.0, 99.0),
        )),
        file: SymbolField::new("Sheetfile", "user.kicad_sch", Point::new(200.0, 121.0)),
        pins: Vec::new(),
        unsupported: Vec::new(),
    })));
    let plan = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
        .unwrap_or_else(|error| panic!("reorganized page must reconcile: {error:#}"));
    let applied = plan.apply(Some(&document)).unwrap();
    let after = inspect_schematic(&applied, &netlist).unwrap();
    assert!(after.issues.is_empty(), "{:#?}", after.issues);
}

/// A user may split one net across their own top-level pages while each side
/// keeps its original page-scoped driver (a local label, or a hierarchical
/// label with no bridging sheet pin). Those drivers name the islands but never
/// merge them, so the repair must upgrade the net to global drivers instead of
/// concluding every island is already driven.
#[test]
fn cross_page_net_with_stale_page_scoped_drivers_repairs_to_global() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let baseline = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut document = baseline.clone();

    // Move R2 — symbol and the labels on its pins — onto a user-created page.
    let r2_pins = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some("R2.R") => {
                let definition = document.pages[0].library.definitions.get(&symbol.lib_id)?;
                Some(
                    definition
                        .placed_pins(symbol)
                        .ok()?
                        .into_iter()
                        .map(|pin| pin.point)
                        .collect::<Vec<_>>(),
                )
            }
            _ => None,
        })
        .expect("baseline places R2");
    let root = &mut document.pages[0];
    let mut moved = Vec::new();
    root.items.retain(|item| {
        let take = match item {
            SchItem::Symbol(symbol) => symbol.field_value("Path") == Some("R2.R"),
            SchItem::Label(label) => r2_pins.contains(&label.at),
            _ => false,
        };
        if take {
            moved.push(item.clone());
        }
        !take
    });
    assert!(
        moved
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "MID")),
        "the moved cluster keeps its stale MID driver"
    );
    let mut user_page = SchPage::new("user-page");
    user_page.file_name = Some("user.kicad_sch".to_string());
    user_page.library = document.pages[0].library.clone();
    user_page.items = moved;
    document.pages.push(user_page);
    document.pages[0].items.push(SchItem::Sheet(Box::new(Sheet {
        id: "user-sheet".to_string(),
        placed: true,
        at: Some(Point::new(200.0, 100.0)),
        size: Some(Point::new(40.0, 20.0)),
        name: Some(SymbolField::new(
            "Sheetname",
            "User",
            Point::new(200.0, 99.0),
        )),
        file: SymbolField::new("Sheetfile", "user.kicad_sch", Point::new(200.0, 121.0)),
        pins: Vec::new(),
        unsupported: Vec::new(),
    })));

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let key = inspection
        .issues
        .iter()
        .find(|issue| {
            matches!(&issue.issue, SchematicIssue::DisconnectedNet { net_name, .. } if net_name == "MID")
        })
        .expect("MID is disconnected across the two pages")
        .key
        .clone();

    let repaired = plan_repairs(&document, &netlist, &inspection, BTreeSet::from([key]))
        .unwrap_or_else(|error| panic!("cross-page MID must repair: {error:#}"))
        .apply(Some(&document))
        .unwrap();

    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(after.issues.is_empty(), "{:#?}", after.issues);
    let global_mid_pages = repaired
        .pages
        .iter()
        .filter(|page| {
            page.items.iter().any(|item| {
                matches!(
                    item,
                    SchItem::Label(label)
                        if label.text == "MID" && matches!(label.kind, LabelKind::Global { .. })
                )
            })
        })
        .count();
    assert_eq!(
        global_mid_pages, 2,
        "both sides of the split net carry a bridging global driver"
    );
}
