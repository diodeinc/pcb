mod common;

use std::collections::{BTreeMap, BTreeSet};

use pcb_kicad_sch::{
    LabelShape, Point, Rotation, SchDocument, SchItem, SchPage, Sheet, SheetPin, Symbol,
    SymbolField, Wire,
    analysis::{SchematicIssue, SchematicIssueKey, inspect_schematic},
    reconcile::{InitialInspection, plan_reconciliation, plan_repairs, plan_repairs_on_page},
};

const CONNECTION_GRID_MM: f64 = 1.27;

#[test]
fn singleton_primary_issue_and_complete_issue_set_produce_the_same_repair() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let baseline = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    for (name, mutate) in [
        ("missing", remove_first_managed as fn(&mut SchDocument)),
        ("duplicate", duplicate_first_managed),
        ("mismatched-id", mismatch_first_managed_id),
        ("unexpected-managed", add_unexpected_managed),
        ("unbound", add_unbound_symbol),
        ("disconnected-net", disconnect_first_named_island),
        ("unexpected-net", add_unexpected_net),
        ("short", add_short),
    ] {
        let mut document = baseline.clone();
        mutate(&mut document);
        let inspection = inspect_schematic(&document, &netlist).unwrap();
        let key = inspection
            .issues
            .iter()
            .find(|issue| issue_kind(&issue.issue) == name)
            .unwrap_or_else(|| panic!("{name} fixture has no issue"))
            .key
            .clone();
        let plan = plan_repairs(
            &document,
            &netlist,
            &inspection,
            BTreeSet::from([key.clone()]),
        )
        .unwrap_or_else(|error| panic!("failed to plan selected {name}: {error:#}"));
        let selected = plan.apply(Some(&document)).unwrap();
        let complete = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
            .unwrap_or_else(|error| panic!("failed to plan complete {name}: {error:#}"))
            .apply(Some(&document))
            .unwrap();
        assert_eq!(selected, complete, "{name} repair policy details diverged");
    }
}

fn issue_kind(issue: &SchematicIssue) -> &'static str {
    match issue {
        SchematicIssue::MissingSymbol { .. } => "missing",
        SchematicIssue::DuplicateSymbol { .. } => "duplicate",
        SchematicIssue::MismatchedSymbolId { .. } => "mismatched-id",
        SchematicIssue::UnexpectedSymbol { .. } => "unexpected-managed",
        SchematicIssue::UnboundSymbol { .. } => "unbound",
        SchematicIssue::DisconnectedNet { .. } => "disconnected-net",
        SchematicIssue::MissingPort { .. } => "missing-port",
        SchematicIssue::UnexpectedNet { .. } => "unexpected-net",
        SchematicIssue::Shorted { .. } => "short",
        SchematicIssue::UnexpectedConnection { .. } => "unexpected-connection",
    }
}

#[test]
fn selected_issue_repair_preserves_an_unrelated_existing_issue() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    duplicate_first_managed(&mut document);
    add_unexpected_net(&mut document);
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let duplicate = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::DuplicateSymbol { .. }))
        .unwrap();
    let unexpected = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::UnexpectedNet { .. }))
        .unwrap();

    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([duplicate.key.clone()]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();

    assert!(after.issues.iter().any(|issue| issue.key == unexpected.key));
    assert!(
        repaired.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.id == "unexpected-net"))
    );

    let selected_all = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([duplicate.key.clone(), unexpected.key.clone()]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();
    let complete = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    assert_eq!(selected_all, complete);
}

#[test]
fn added_component_batch_docks_without_moving_existing_symbols() {
    let before = common::compile_fixture("analysis", "incremental_before.zen");
    let after = common::compile_fixture("analysis", "incremental_after.zen");
    let document = plan_reconciliation(None, &before, "Incremental.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let existing = managed_symbol_positions(&document);
    let inspection = inspect_schematic(&document, &after).unwrap();
    let missing = inspection
        .issues
        .iter()
        .filter(|issue| matches!(issue.issue, SchematicIssue::MissingSymbol { .. }))
        .map(|issue| issue.key.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(missing.len(), 3);

    let repaired = plan_repairs(&document, &after, &inspection, missing)
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    let all = managed_symbol_positions(&repaired);

    assert_eq!(all.len(), 6);
    assert_eq!(repaired.pages[0].paper, document.pages[0].paper);
    for (path, at) in &existing {
        assert_eq!(all[path], *at, "existing symbol {path} moved");
    }
    let added = all
        .iter()
        .filter(|(path, _)| !existing.contains_key(*path))
        .map(|(_, at)| *at)
        .collect::<Vec<_>>();
    assert_eq!(added.len(), 3);
    assert!(added.iter().enumerate().any(|(index, left)| {
        added[index + 1..]
            .iter()
            .any(|right| left.x == right.x || left.y == right.y)
    }));
    assert!(added.iter().any(|new| {
        existing
            .values()
            .any(|old| new.x == old.x || new.y == old.y)
    }));
}

#[test]
fn complete_reconciliation_recovers_from_invalid_initial_analysis() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    document.pages[0].library.definitions.clear();

    let plan = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch").unwrap();
    assert!(matches!(
        plan.initial_inspection(),
        InitialInspection::Invalid { .. }
    ));
}

#[test]
fn directly_overlapping_component_pins_relocate_the_affected_symbols() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let target = pin_point(&document, "R1.R", "1");
    let source = pin_point(&document, "R2.R", "1");
    let symbol = managed_symbol_mut(&mut document, "R2.R");
    let new_at = Point::new(
        symbol.at.x + target.x - source.x,
        symbol.at.y + target.y - source.y,
    );
    move_symbol(symbol, new_at);
    let moved_id = symbol.id.clone();
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let issue = inspection
        .issues
        .iter()
        .find(|issue| {
            matches!(
                issue.issue,
                SchematicIssue::Shorted { .. } | SchematicIssue::UnexpectedConnection { .. }
            ) && !issue.items.iter().any(|item| {
                matches!(
                    item,
                    pcb_kicad_sch::connectivity::ConnectivityItemRef::Wire { .. }
                        | pcb_kicad_sch::connectivity::ConnectivityItemRef::Junction { .. }
                )
            })
        })
        .unwrap_or_else(|| panic!("missing direct-pin issue: {:#?}", inspection.issues));

    let plan = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([issue.key.clone()]),
    )
    .unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();

    let relocated = repaired
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.id == moved_id => Some(symbol),
            _ => None,
        })
        .expect("relocated symbol remains present");
    assert_ne!(relocated.at, new_at);
    for coordinate in [relocated.at.x, relocated.at.y] {
        let grid_units = coordinate / CONNECTION_GRID_MM;
        assert!((grid_units - grid_units.round()).abs() < 1.0e-9);
    }
    assert_eq!(
        plan_repairs(
            &document,
            &netlist,
            &inspection,
            BTreeSet::from([issue.key.clone()]),
        )
        .unwrap()
        .apply(Some(&document))
        .unwrap(),
        repaired
    );
}

#[test]
fn repairs_an_unexpected_terminal_connection_without_removing_the_symbol() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");
    let mut document = fixture.kicad_document().clone();
    let mut unbound = managed_symbols(&document).next().unwrap().clone();
    unbound.id = "unbound-extra".to_string();
    unbound.fields.remove("Path");
    let moved_at = Point::new(unbound.at.x + 25.4, unbound.at.y);
    move_symbol(&mut unbound, moved_at);
    let definition = &document.pages[0].library.definitions[&unbound.lib_id];
    let pin = definition.placed_pins(&unbound).unwrap()[0].point;
    document.pages[0].items.push(SchItem::Symbol(unbound));
    document.pages[0].items.push(SchItem::Wire(Wire {
        id: "unexpected-terminal-wire".to_string(),
        a: Point::new(207.01, 146.05),
        b: pin,
        unsupported: Vec::new(),
    }));

    let inspection = inspect_schematic(&document, fixture.netlist()).unwrap();
    let issue = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
        .unwrap();
    assert!(
        issue.items.iter().any(
            |item| matches!(item, pcb_kicad_sch::connectivity::ConnectivityItemRef::Wire { id, .. } if id == "unexpected-terminal-wire")
        ),
        "{issue:#?}"
    );
    let plan = plan_repairs(
        &document,
        fixture.netlist(),
        &inspection,
        BTreeSet::from([issue.key.clone()]),
    )
    .unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();

    assert!(
        repaired.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Symbol(symbol) if symbol.id == "unbound-extra"))
    );
    assert!(
        !repaired.pages[0].items.iter().any(
            |item| matches!(item, SchItem::Wire(wire) if wire.id == "unexpected-terminal-wire")
        )
    );
}

fn managed_symbols(document: &SchDocument) -> impl Iterator<Item = &Symbol> {
    document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
}

fn managed_symbol_positions(document: &SchDocument) -> BTreeMap<String, Point> {
    managed_symbols(document)
        .map(|symbol| (symbol.field_value("Path").unwrap().to_string(), symbol.at))
        .collect()
}

fn first_managed_mut(document: &mut SchDocument) -> &mut Symbol {
    document
        .pages
        .iter_mut()
        .flat_map(|page| &mut page.items)
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .unwrap()
}

fn managed_symbol_mut<'a>(document: &'a mut SchDocument, path: &str) -> &'a mut Symbol {
    document
        .pages
        .iter_mut()
        .flat_map(|page| &mut page.items)
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing managed symbol {path}"))
}

fn pin_point(document: &SchDocument, path: &str, number: &str) -> Point {
    for page in &document.pages {
        let Some(symbol) = page.items.iter().find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        }) else {
            continue;
        };
        return page.library.definitions[&symbol.lib_id]
            .placed_pins(symbol)
            .unwrap()
            .into_iter()
            .find(|pin| pin.number == number)
            .unwrap_or_else(|| panic!("missing {path} pin {number}"))
            .point;
    }
    panic!("missing managed symbol {path}")
}

fn remove_first_managed(document: &mut SchDocument) {
    let id = managed_symbols(document).next().unwrap().id.clone();
    document.pages[0]
        .items
        .retain(|item| item.id() != Some(id.as_str()));
}

fn duplicate_first_managed(document: &mut SchDocument) {
    let mut duplicate = managed_symbols(document).next().unwrap().clone();
    duplicate.id = "duplicate-managed".to_string();
    document.pages[0].items.push(SchItem::Symbol(duplicate));
}

fn mismatch_first_managed_id(document: &mut SchDocument) {
    first_managed_mut(document).id = "mismatched-managed".to_string();
}

fn add_unexpected_managed(document: &mut SchDocument) {
    let mut unexpected = managed_symbols(document).next().unwrap().clone();
    unexpected.id = "unexpected-managed".to_string();
    unexpected.fields.get_mut("Path").unwrap().value = "STALE.R".to_string();
    document.pages[0].items.push(SchItem::Symbol(unexpected));
}

fn add_unbound_symbol(document: &mut SchDocument) {
    let mut unbound = managed_symbols(document).next().unwrap().clone();
    unbound.id = "unbound-symbol".to_string();
    unbound.fields.remove("Path");
    let moved_at = Point::new(unbound.at.x + 50.8, unbound.at.y);
    move_symbol(&mut unbound, moved_at);
    document.pages[0].items.push(SchItem::Symbol(unbound));
}

fn disconnect_first_named_island(document: &mut SchDocument) {
    let id = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "MID" => Some(label.id.clone()),
            _ => None,
        })
        .expect("generated MID label");
    document.pages[0]
        .items
        .retain(|item| item.id() != Some(id.as_str()));
}

fn add_unexpected_net(document: &mut SchDocument) {
    document.pages[0]
        .items
        .push(SchItem::Label(pcb_kicad_sch::Label::new(
            "unexpected-net",
            "EXTRA",
            Point::new(100.0, 100.0),
        )));
}

fn add_short(document: &mut SchDocument) {
    let point = |name: &str| {
        document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.text == name => Some(label.at),
                _ => None,
            })
            .unwrap_or_else(|| panic!("generated {name} label"))
    };
    let left = point("LEFT");
    let mid = point("MID");
    document.pages[0].items.push(SchItem::Wire(Wire {
        id: "short".to_string(),
        a: left,
        b: mid,
        unsupported: Vec::new(),
    }));
}

fn move_symbol(symbol: &mut Symbol, at: Point) {
    let delta = Point::new(at.x - symbol.at.x, at.y - symbol.at.y);
    symbol.at = at;
    for field in symbol.fields.values_mut() {
        field.at = Point::new(field.at.x + delta.x, field.at.y + delta.y);
    }
}

/// Symbol locations must address file pages, not page instances, so unbound
/// symbols on generated child sheets can be repaired by issue selection.
#[test]
fn unbound_symbol_on_a_child_sheet_repairs_by_selection() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let baseline = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    let mut document = baseline.clone();
    let child_index = document
        .pages
        .iter()
        .position(|page| page.file_name.as_deref() != Some("root.kicad_sch"))
        .expect("hierarchy fixture has a child page");
    let mut unbound = managed_symbols(&document)
        .next()
        .expect("baseline has managed symbols")
        .clone();
    unbound.id = "unbound-child".to_string();
    unbound.fields.remove("Path");
    let moved_at = Point::new(unbound.at.x + 50.8, unbound.at.y);
    move_symbol(&mut unbound, moved_at);
    let definition = document
        .pages
        .iter()
        .flat_map(|page| page.library.definitions.get(&unbound.lib_id))
        .next()
        .expect("definition exists")
        .clone();
    let child = &mut document.pages[child_index];
    child
        .library
        .definitions
        .entry(unbound.lib_id.clone())
        .or_insert(definition);
    let child_page_id = child.id.clone();
    child.items.push(SchItem::Symbol(unbound));

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let key = inspection
        .issues
        .iter()
        .find(|issue| matches!(&issue.issue, SchematicIssue::UnboundSymbol { .. }))
        .expect("unbound symbol issue reported")
        .key
        .clone();
    // The reported location addresses the file page, not a page instance.
    if let pcb_kicad_sch::analysis::SchematicIssueKey::UnboundSymbol(location) = &key {
        assert_eq!(location.page_id, child_page_id);
    }

    let repaired = plan_repairs(&document, &netlist, &inspection, BTreeSet::from([key]))
        .expect("child-sheet unbound repair plans")
        .apply(Some(&document))
        .unwrap();
    assert!(
        !repaired
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .any(|item| matches!(item, SchItem::Symbol(symbol) if symbol.id == "unbound-child")),
        "the unbound symbol is removed from the child page"
    );
}

/// A KiCad wire joining two pins the netlist marks NotConnected is a real
/// electrical divergence and must be reported, while unwired NotConnected
/// pins stay silent.
#[test]
fn wired_not_connected_pins_report_an_unexpected_connection() {
    let netlist = common::compile_fixture("analysis", "not_connected.zen");
    let baseline = plan_reconciliation(None, &netlist, "not_connected.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let inspection = inspect_schematic(&baseline, &netlist).unwrap();
    assert!(
        inspection.issues.is_empty(),
        "unwired NotConnected pins must not report issues: {:#?}",
        inspection.issues
    );

    let mut document = baseline.clone();
    let a = pin_point(&document, "R1.R", "2");
    let b = pin_point(&document, "R2.R", "2");
    document.pages[0].items.push(SchItem::Wire(Wire {
        id: "nc-short".to_string(),
        a,
        b,
        unsupported: Vec::new(),
    }));
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. })),
        "a wire between NotConnected pins must be reported: {:#?}",
        inspection.issues
    );
}

/// Generated child pages live in their parent page's directory so the
/// parent-relative Sheetfile reference and the project-relative page path
/// resolve to the same file.
#[test]
fn generated_pages_follow_their_parent_directory() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = plan_reconciliation(None, &netlist, "sub/root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    for page in &document.pages {
        let file_name = page.file_name.as_deref().expect("page has a file name");
        assert!(
            file_name.starts_with("sub/") && file_name.matches('/').count() == 1,
            "page '{}' must live next to its parent: {file_name}",
            page.id
        );
    }
    for sheet in document.pages.iter().flat_map(|page| &page.items) {
        if let pcb_kicad_sch::SchItem::Sheet(sheet) = sheet {
            assert!(
                !sheet.file_name().contains('/'),
                "sheet references stay parent-relative: {}",
                sheet.file_name()
            );
        }
    }
}

/// A generated module page that the user emptied of managed symbols is still
/// the module's page: re-applying repopulates it instead of materializing a
/// duplicate sheet with the same deterministic identity.
/// An interactive placement can ask for the missing symbol to be created on
/// the page the user is viewing instead of its module's own page. The plan
/// must place it there and still verify (net drivers adapt to the new span).
#[test]
fn missing_symbol_repair_places_on_the_requested_page() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let baseline = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root_page_id = baseline
        .pages
        .iter()
        .find(|page| page.file_name.as_deref() == Some("root.kicad_sch"))
        .expect("hierarchy fixture has a root page")
        .id
        .clone();

    let mut document = baseline.clone();
    let removed_path = {
        let page = document
            .pages
            .iter_mut()
            .find(|page| page.id != root_page_id)
            .expect("hierarchy fixture has module pages");
        let path = page
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Symbol(symbol) => {
                    symbol.fields.get("Path").map(|field| field.value.clone())
                }
                _ => None,
            })
            .expect("module page has managed symbols");
        page.items.retain(|item| {
            !matches!(
                item,
                SchItem::Symbol(symbol)
                    if symbol.fields.get("Path").is_some_and(|field| field.value == path)
            )
        });
        path
    };

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let key = inspection
        .issues
        .iter()
        .find(|issue| matches!(&issue.issue, SchematicIssue::MissingSymbol { .. }))
        .expect("removing a managed symbol reports it missing")
        .key
        .clone();

    let repaired = plan_repairs_on_page(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([key]),
        &root_page_id,
    )
    .unwrap_or_else(|error| panic!("placement on the viewed page must plan: {error:#}"))
    .apply(Some(&document))
    .unwrap();

    let placed_page = repaired
        .pages
        .iter()
        .find(|page| {
            page.items.iter().any(|item| matches!(
                item,
                SchItem::Symbol(symbol)
                    if symbol.fields.get("Path").is_some_and(|field| field.value == removed_path)
            ))
        })
        .expect("the repaired symbol is placed");
    assert_eq!(
        placed_page.id, root_page_id,
        "the symbol must land on the requested page, not its module page"
    );
}

#[test]
fn emptied_module_page_is_repopulated_not_duplicated() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let baseline = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let module_page_id = baseline
        .pages
        .iter()
        .find(|page| page.file_name.as_deref() != Some("root.kicad_sch"))
        .expect("hierarchy fixture has module pages")
        .id
        .clone();

    let mut document = baseline.clone();
    let module_prefix = {
        let page = document
            .pages
            .iter_mut()
            .find(|page| page.id == module_page_id)
            .unwrap();
        let prefix = page
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Symbol(symbol) => symbol
                    .fields
                    .get("Path")
                    .and_then(|field| field.value.split('.').next())
                    .map(str::to_string),
                _ => None,
            })
            .expect("module page has managed symbols");
        page.items
            .retain(|item| !matches!(item, SchItem::Symbol(_)));
        prefix
    };

    let repaired = plan_reconciliation(Some(&document), &netlist, "root.kicad_sch")
        .unwrap_or_else(|error| panic!("emptied module page must replan: {error:#}"))
        .apply(Some(&document))
        .unwrap();

    let page_count = repaired
        .pages
        .iter()
        .filter(|page| page.id == module_page_id)
        .count();
    assert_eq!(page_count, 1, "the module page is reused, not duplicated");
    let repopulated = repaired
        .pages
        .iter()
        .find(|page| page.id == module_page_id)
        .unwrap()
        .items
        .iter()
        .any(|item| matches!(
            item,
            SchItem::Symbol(symbol)
                if symbol.fields.get("Path").is_some_and(|field| field.value.starts_with(&module_prefix))
        ));
    assert!(repopulated, "the module's symbols return to their page");
}

/// The LTC regression: a hierarchical port label dragged onto another net's
/// pin shorts the two ports. No single island looks inconsistent on its own
/// and the offending label is a hierarchy alias (not a named driver), so the
/// gated searches see nothing — the teardown fallback must still repair it.
#[test]
fn short_from_a_mislabeled_hierarchical_label_is_repairable() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Drag the INPUT port label onto the OUTPUT pin.
    let output_at = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "OUTPUT" => Some(label.at),
            _ => None,
        })
        .expect("composed OUTPUT label");
    for item in &mut document.pages[0].items {
        if let SchItem::Label(label) = item
            && label.text == "INPUT"
        {
            label.at = output_at;
        }
    }

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let shorted = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .expect("mislabeled port shorts the nets");

    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([shorted.key.clone()]),
    )
    .expect("shorts caused by mislabeled labels are repairable")
    .apply(Some(&document))
    .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(
        after.analysis.is_equivalent(),
        "{:#?}",
        after.analysis.issues()
    );
}

/// The other LTC regression: components moved into a user-created subsheet
/// (a page with no module identity) whose nets need reconnecting. Driver
/// placement must inherit the parent page's interface context so the child
/// gets hierarchical labels that bridge through the sheet pins — a local
/// label can never rejoin the parent's side of the net.
#[test]
fn nets_split_into_a_user_created_subsheet_are_repairable() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    // Move R1 to a new subsheet page the composer knows nothing about,
    // leaving the root's port labels behind, bridged by sheet pins placed
    // exactly on them.
    let symbol_index = document.pages[0]
        .items
        .iter()
        .position(|item| matches!(item, SchItem::Symbol(_)))
        .expect("composed symbol");
    let symbol = document.pages[0].items.remove(symbol_index);
    let mut child = SchPage::new("sub");
    child.file_name = Some("Sub.kicad_sch".to_string());
    child.library = document.pages[0].library.clone();
    child.items.push(symbol);

    let pin_at = |text: &str| {
        document.pages[0]
            .items
            .iter()
            .find_map(|item| match item {
                SchItem::Label(label) if label.text == text => Some(label.at),
                _ => None,
            })
            .expect("composed port label")
    };
    let sheet = Sheet {
        id: "sheet-sub".to_string(),
        at: Some(Point::new(0.0, 0.0)),
        size: Some(Point::new(25.4, 25.4)),
        name: Some(SymbolField::new("Sheetname", "sub", Point::new(0.0, 0.0))),
        file: SymbolField::new("Sheetfile", "Sub.kicad_sch", Point::new(0.0, 0.0)),
        pins: ["INPUT", "OUTPUT"]
            .into_iter()
            .map(|name| SheetPin {
                id: format!("pin-{name}"),
                name: name.to_string(),
                at: pin_at(name),
                rotation: Rotation::Deg0,
                shape: LabelShape::Bidirectional,
                unsupported: Vec::new(),
            })
            .collect(),
        unsupported: Vec::new(),
    };
    document.pages[0]
        .items
        .push(SchItem::Sheet(Box::new(sheet)));
    document.pages.push(child);
    // Drop the root's port labels too: the repair must rebuild them anchored
    // at the sheet pins, or the ports float and the nets stay split.
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Label(_)));

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let selected: BTreeSet<_> = inspection
        .issues
        .iter()
        .filter(|issue| {
            matches!(
                issue.key,
                SchematicIssueKey::DisconnectedNet(_) | SchematicIssueKey::MissingPort(_)
            )
        })
        .map(|issue| issue.key.clone())
        .collect();
    assert!(
        selected.iter().any(|key| matches!(
            key,
            SchematicIssueKey::DisconnectedNet(net) | SchematicIssueKey::MissingPort(net)
                if net == "INPUT"
        )),
        "{:#?}",
        inspection.analysis.issues()
    );

    let repaired = plan_repairs(&document, &netlist, &inspection, selected)
        .expect("nets split into a user subsheet are repairable")
        .apply(Some(&document))
        .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(
        after.analysis.is_equivalent(),
        "{:#?}",
        after.analysis.issues()
    );
}

/// Placing a missing symbol must also drive its nets: with the pin terminal
/// satisfied, the net's remaining defect flips from DisconnectedNet to
/// MissingPort, and the projection scope must still cover it so the
/// hierarchical interface labels appear with the placement.
#[test]
fn placing_a_missing_symbol_drives_its_interface_labels() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Everything deleted: an empty page.
    document.pages[0].items.clear();

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let missing = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.key, SchematicIssueKey::MissingSymbol(_)))
        .expect("R1 is missing");

    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([missing.key.clone()]),
    )
    .expect("plan placement")
    .apply(Some(&document))
    .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(
        after.analysis.is_equivalent(),
        "{:#?}",
        after.analysis.issues()
    );
}
