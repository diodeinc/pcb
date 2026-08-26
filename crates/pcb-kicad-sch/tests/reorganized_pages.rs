mod common;
use pcb_kicad_sch::{
    Point, SchItem, SchPage, Sheet, SymbolField, analysis::inspect_schematic,
    reconcile::plan_reconciliation,
};

/// A user may reorganize managed content onto their own sheets: as long as
/// connectivity holds, later reconciliation accepts the layout instead of
/// bailing over page ownership.
#[test]
fn user_reorganized_page_stays_reconcilable() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let baseline = plan_reconciliation(None, &netlist, "simple.kicad_sch").unwrap().apply(None).unwrap();
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
        name: Some(SymbolField::new("Sheetname", "User", Point::new(200.0, 99.0))),
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
