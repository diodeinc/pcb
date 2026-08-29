mod common;

use pcb_kicad_sch::{
    Label, LabelKind, Point, SchItem, SchPage, Sheet, SymbolField,
    analysis::{SchematicIssue, inspect_schematic},
    reconcile::plan_reconciliation,
};

/// A user may reorganize managed content onto their own sheets: as long as
/// connectivity holds, later reconciliation accepts the layout instead of
/// bailing over page ownership.
#[test]
fn user_reorganized_page_stays_reconcilable() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let baseline = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply_all(None)
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
    let applied = plan.apply_all(Some(&document)).unwrap();
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
        .apply_all(None)
        .unwrap();
    let mut document = baseline.clone();

    // Reduce the generated topology to local labels directly on every managed
    // pin, then move R2 and its labels to a user-created page. MID retains one
    // stale page-scoped driver on each side of the split.
    let r2 = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some("R2.R") => {
                Some(symbol.clone())
            }
            _ => None,
        })
        .expect("baseline places R2");
    let r2_id = r2.id.clone();
    let baseline_inspection = inspect_schematic(&baseline, &netlist).unwrap();
    let managed_symbol_ids = document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => {
                Some(symbol.id.clone())
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let pin_labels = baseline_inspection
        .analysis
        .nets
        .values()
        .flat_map(|net| {
            net.islands
                .iter()
                .filter_map(|island| baseline_inspection.physical.islands.get(island))
                .flat_map(|island| &island.symbol_pins)
                .filter(|pin| managed_symbol_ids.contains(pin.symbol_id()))
                .map(|pin| (pin.symbol_id().to_string(), net.name.clone(), pin.point()))
        })
        .collect::<Vec<_>>();
    assert!(pin_labels.iter().any(|(_, name, _)| name == "MID"));
    document.pages[0].items.retain(|item| {
        matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() && symbol.id != r2_id)
    });
    document.pages[0].items.extend(
        pin_labels
            .iter()
            .filter(|(symbol_id, _, _)| symbol_id != &r2_id)
            .map(|(symbol_id, name, at)| {
                SchItem::Label(Label::new(format!("local-{symbol_id}-{name}"), name, *at))
            }),
    );
    let mut user_page = SchPage::new("user-page");
    user_page.file_name = Some("user.kicad_sch".to_string());
    user_page.library = document.pages[0].library.clone();
    user_page.items.push(SchItem::Symbol(r2));
    user_page.items.extend(
        pin_labels
            .iter()
            .filter(|(symbol_id, _, _)| symbol_id == &r2_id)
            .map(|(symbol_id, name, at)| {
                SchItem::Label(Label::new(format!("local-{symbol_id}-{name}"), name, *at))
            }),
    );
    document.pages.push(user_page);
    document.pages[0].items.push(SchItem::Sheet(Box::new(Sheet {
        id: "user-sheet".to_string(),
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
    assert!(inspection.issues.iter().any(|issue| {
            matches!(&issue.issue, SchematicIssue::DisconnectedNet { net_name, .. } if net_name == "MID")
        }), "MID is disconnected across the two pages");

    let repaired = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
        .unwrap_or_else(|error| panic!("cross-page MID must repair: {error:#}"))
        .apply_all(Some(&document))
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
