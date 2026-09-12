mod common;

use std::collections::{BTreeMap, BTreeSet};

use pcb_kicad_sch::{
    LabelShape, Point, Rotation, SchDocument, SchItem, SchPage, Sheet, SheetPin, Symbol,
    SymbolField, Wire,
    analysis::{SchematicIssue, SchematicIssueKey, inspect_schematic},
    plan_connectivity_repair,
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
        let original = document.clone();
        let selected = plan.apply(Some(&document)).unwrap();
        assert_eq!(document, original, "{name} planning mutated its input");
        assert_eq!(
            plan.revert(&selected).unwrap(),
            original,
            "{name} plan was not reversible"
        );
        let complete = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
            .unwrap_or_else(|error| panic!("failed to plan complete {name}: {error:#}"))
            .apply(Some(&document))
            .unwrap();
        assert_eq!(selected, complete, "{name} repair policy details diverged");
    }
}

fn issue_kind(issue: &SchematicIssue) -> &'static str {
    match issue {
        SchematicIssue::MissingSheet { .. } => "missing-sheet",
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
        SchematicIssue::MissingNoConnect { .. } => "missing-no-connect",
        SchematicIssue::UnexpectedNoConnect { .. } => "unexpected-no-connect",
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
    assert_eq!(
        added.iter().map(|point| point.x).collect::<Vec<_>>(),
        existing.values().map(|point| point.x).collect::<Vec<_>>(),
        "the new batch should align to the existing row's columns"
    );
    assert!(added.iter().all(|point| point.y == added[0].y));
    let existing_bottom = existing
        .values()
        .map(|point| point.y)
        .reduce(f64::max)
        .unwrap();
    let row_gap = added[0].y - existing_bottom;
    assert!(row_gap > 0.0 && row_gap <= 50.8, "{row_gap}");
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
fn relocating_shorted_symbols_reconnects_their_other_nets() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Touch only R1's LEFT pin to R2's RIGHT pin. Their other pins carry
    // MID, which is valid before repair and must survive moving the symbols.
    document.pages[0]
        .items
        .retain(|item| matches!(item, SchItem::Symbol(_)));
    let target = common::pin_point(&document, "R1.R", "1");
    let source = common::pin_point(&document, "R2.R", "2");
    let symbol = managed_symbol_mut(&mut document, "R2.R");
    let new_at = Point::new(
        symbol.at.x + target.x - source.x,
        symbol.at.y + target.y - source.y,
    );
    move_symbol(symbol, new_at);
    for (path, pin, net) in [
        ("R1.R", "1", "LEFT"),
        ("R1.R", "2", "MID"),
        ("R2.R", "1", "MID"),
        ("R2.R", "2", "RIGHT"),
    ] {
        let point = common::pin_point(&document, path, pin);
        document.pages[0]
            .items
            .push(SchItem::Label(pcb_kicad_sch::Label::new(
                format!("{path}-{pin}"),
                net,
                point,
            )));
    }
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert_eq!(inspection.issues.len(), 1, "{:#?}", inspection.issues);
    let issue = &inspection.issues[0];
    assert!(
        matches!(&issue.issue, SchematicIssue::Shorted { net_names, .. }
        if net_names == &BTreeSet::from(["LEFT".into(), "RIGHT".into()]))
    );
    let selected = BTreeSet::from([issue.key.clone()]);
    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &selected,
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(!intent.relocated_symbols().is_empty());
    assert_eq!(
        intent.reconnect_nets(),
        &BTreeSet::from(["LEFT".into(), "MID".into(), "RIGHT".into()])
    );
    assert!(intent.driver_kind("MID", &document.pages[0].id).is_some());

    let plan = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch").unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    assert_eq!(plan.revert(&repaired).unwrap(), document);
    let scoped = plan_repairs(&document, &netlist, &inspection, selected)
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    assert_eq!(scoped, repaired);
    pcb_kicad_sch::verify_connectivity_repair(&document, &inspection, &netlist, &intent, &scoped)
        .unwrap();
}

#[test]
fn directly_overlapping_component_pins_relocate_the_affected_symbols() {
    let mut netlist = common::compile_fixture("analysis", "simple.zen");
    let mut not_connected = netlist.nets.remove("RIGHT").unwrap();
    not_connected.kind = "NotConnected".to_string();
    not_connected.name.clear();
    netlist.nets.insert(String::new(), not_connected);
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let target = common::pin_point(&document, "R1.R", "1");
    let source = common::pin_point(&document, "R2.R", "1");
    let nc_source = common::pin_point(&document, "R2.R", "2");
    let symbol = managed_symbol_mut(&mut document, "R2.R");
    let new_at = Point::new(
        symbol.at.x + target.x - source.x,
        symbol.at.y + target.y - source.y,
    );
    move_symbol(symbol, new_at);
    let moved_id = symbol.id.clone();
    let offset = Point::new(target.x - source.x, target.y - source.y);
    let marker = document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::NoConnect(marker) if marker.at == nc_source => Some(marker),
            _ => None,
        })
        .expect("R2 no-connect marker");
    marker.at = Point::new(marker.at.x + offset.x, marker.at.y + offset.y);
    let marker_id = marker.id.clone();
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

    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([issue.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(intent.removals().contains(
        &pcb_kicad_sch::connectivity::ConnectivityItemRef::NoConnect {
            page_id: document.pages[0].id.clone(),
            id: marker_id,
        }
    ));
    let edited = intent.apply_edits(&document).unwrap();
    let relocated_nc = common::pin_point(&edited, "R2.R", "2");
    assert_eq!(intent.no_connect_additions().len(), 1);
    assert_eq!(intent.no_connect_additions()[0].at, relocated_nc);

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
fn relocation_preserves_a_no_connect_marker_shared_with_a_stationary_pin() {
    let netlist = common::compile_fixture("analysis", "relocated_shared_nc.zen");
    let mut document = plan_reconciliation(None, &netlist, "shared_nc.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let old_stationary_nc = common::pin_point(&document, "R1.R", "2");
    let old_stationary_signal = common::pin_point(&document, "R1.R", "1");
    managed_symbol_mut(&mut document, "R1.R").rotation = Rotation::Deg90;
    let stationary_nc = common::pin_point(&document, "R1.R", "2");
    let stationary_signal = common::pin_point(&document, "R1.R", "1");
    let stationary_marker = document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::NoConnect(marker) if marker.at == old_stationary_nc => Some(marker),
            _ => None,
        })
        .unwrap();
    stationary_marker.at = stationary_nc;
    for item in &mut document.pages[0].items {
        if let SchItem::Label(label) = item
            && label.at == old_stationary_signal
        {
            label.at = stationary_signal;
        }
    }
    let moving_nc = common::pin_point(&document, "R2.R", "2");
    let moving_signal = common::pin_point(&document, "R2.R", "1");
    let blocker_signal = common::pin_point(&document, "R3.R", "1");

    let moving_offset = Point::new(stationary_nc.x - moving_nc.x, stationary_nc.y - moving_nc.y);
    let moving_symbol = managed_symbol_mut(&mut document, "R2.R");
    move_symbol(
        moving_symbol,
        Point::new(
            moving_symbol.at.x + moving_offset.x,
            moving_symbol.at.y + moving_offset.y,
        ),
    );
    let moved_signal = Point::new(
        moving_signal.x + moving_offset.x,
        moving_signal.y + moving_offset.y,
    );
    let blocker_symbol = managed_symbol_mut(&mut document, "R3.R");
    move_symbol(
        blocker_symbol,
        Point::new(
            blocker_symbol.at.x + moved_signal.x - blocker_signal.x,
            blocker_symbol.at.y + moved_signal.y - blocker_signal.y,
        ),
    );
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::NoConnect(marker) if marker.at == moving_nc));
    let shared_marker = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::NoConnect(marker) if marker.at == stationary_nc => Some(marker.clone()),
            _ => None,
        })
        .unwrap();
    let mut missing_shared_marker = document.clone();
    missing_shared_marker.pages[0].items.retain(
        |item| !matches!(item, SchItem::NoConnect(marker) if marker.id == shared_marker.id),
    );

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let issue = inspection
        .issues
        .iter()
        .find(|issue| {
            matches!(
                &issue.issue,
                SchematicIssue::Shorted { net_names, .. }
                    if net_names == &BTreeSet::from(["MID".to_string(), "RIGHT".to_string()])
            )
        })
        .unwrap_or_else(|| panic!("missing direct-pin issue: {:#?}", inspection.issues));
    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([issue.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert!(
        !intent.removals().contains(
            &pcb_kicad_sch::connectivity::ConnectivityItemRef::NoConnect {
                page_id: document.pages[0].id.clone(),
                id: shared_marker.id.clone(),
            }
        ),
        "relocations={:?} removals={:?}",
        intent.relocated_symbols(),
        intent.removals()
    );
    assert!(intent.no_connect_additions().iter().any(|target| {
        target.symbol_id
            == managed_symbols(&document)
                .find(|symbol| symbol.field_value("Path") == Some("R2.R"))
                .unwrap()
                .id
    }));
    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([issue.key.clone()]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();
    assert!(
        repaired.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::NoConnect(marker) if marker == &shared_marker))
    );
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .issues
            .is_empty()
    );

    let inspection = inspect_schematic(&missing_shared_marker, &netlist).unwrap();
    let selected = inspection
        .issues
        .iter()
        .map(|issue| issue.key.clone())
        .collect::<BTreeSet<_>>();
    let intent = plan_connectivity_repair(
        &missing_shared_marker,
        &netlist,
        &inspection,
        &selected,
        &BTreeSet::new(),
    )
    .unwrap();
    let edited = intent.apply_edits(&missing_shared_marker).unwrap();
    let stationary_nc = common::pin_point(&edited, "R1.R", "2");
    let relocated_nc = common::pin_point(&edited, "R2.R", "2");
    let same_point = |left: Point, right: Point| {
        (left.x - right.x).abs() < 1.0e-9 && (left.y - right.y).abs() < 1.0e-9
    };
    assert_eq!(intent.no_connect_additions().len(), 2);
    assert!(
        intent
            .no_connect_additions()
            .iter()
            .any(|target| same_point(target.at, stationary_nc)),
        "stationary={stationary_nc:?} relocated={relocated_nc:?} additions={:?} relocations={:?}",
        intent.no_connect_additions(),
        intent.relocated_symbols()
    );
    assert!(
        intent
            .no_connect_additions()
            .iter()
            .any(|target| same_point(target.at, relocated_nc)),
        "stationary={stationary_nc:?} relocated={relocated_nc:?} additions={:?} relocations={:?}",
        intent.no_connect_additions(),
        intent.relocated_symbols()
    );
    let repaired = plan_repairs(&missing_shared_marker, &netlist, &inspection, selected)
        .unwrap()
        .apply(Some(&missing_shared_marker))
        .unwrap();
    let after = pcb_kicad_sch::verify_connectivity_repair(
        &missing_shared_marker,
        &inspection,
        &netlist,
        &intent,
        &repaired,
    )
    .unwrap();
    assert!(after.issues.is_empty());
    for expected in [stationary_nc, relocated_nc] {
        assert!(repaired.pages.iter().any(|page| {
            page.items.iter().any(
                |item| matches!(item, SchItem::NoConnect(marker) if same_point(marker.at, expected)),
            )
        }));
    }

    let reconciled = plan_reconciliation(
        Some(&missing_shared_marker),
        &netlist,
        "shared_nc.kicad_sch",
    )
    .unwrap()
    .apply(Some(&missing_shared_marker))
    .unwrap();
    assert_eq!(reconciled, repaired);
    assert!(
        inspect_schematic(&reconciled, &netlist)
            .unwrap()
            .issues
            .is_empty()
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

#[test]
fn repeated_shared_sheet_unmanaged_symbol_does_not_bail_repair() {
    use common::kicad_builder::{KicadBuilder, TestPin};

    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root = document
        .pages
        .iter_mut()
        .find(|page| page.file_name.as_deref() == Some("root.kicad_sch"))
        .expect("hierarchy fixture has a root page");

    let mut builder = KicadBuilder::new();
    builder
        .sheet("shared.kicad_sch", &[])
        .sheet("shared.kicad_sch", &[])
        .add_page("shared", "shared.kicad_sch")
        .define_symbol("Test:OnePin", &[TestPin::passive("1", (0.0, 0.0))])
        .component("Test:OnePin", None, (0.0, 0.0));
    let mut shared = builder.build().pages.into_iter();
    root.items.extend(shared.next().unwrap().items);
    document.pages.extend(shared);

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let unbound: Vec<_> = inspection
        .issues
        .iter()
        .filter(|context| matches!(&context.issue, SchematicIssue::UnboundSymbol { .. }))
        .collect();
    assert_eq!(unbound.len(), 1, "{:?}", inspection.issues);

    plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([unbound[0].key.clone()]),
        &BTreeSet::new(),
    )
    .expect("repeated-sheet unbound repair must plan, not bail");
}

/// A KiCad wire joining two pins the netlist marks NotConnected is a real
/// electrical divergence: it is reported, unwired NotConnected pins stay
/// silent, and the repair cuts one segment per path next to the pin instead
/// of tearing down the branches between them.
#[test]
fn wired_not_connected_pins_are_cut_free_locally() {
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

    let mut overlapping = baseline.clone();
    let a = common::pin_point(&overlapping, "R1.R", "2");
    let b = common::pin_point(&overlapping, "R2.R", "2");
    let symbol = managed_symbol_mut(&mut overlapping, "R2.R");
    move_symbol(
        symbol,
        Point::new(symbol.at.x + a.x - b.x, symbol.at.y + a.y - b.y),
    );
    let inspection = inspect_schematic(&overlapping, &netlist).unwrap();
    assert!(
        inspection.issues.iter().any(|issue| matches!(
            &issue.issue,
            SchematicIssue::UnexpectedConnection { terminals, .. } if terminals.len() == 2
        )),
        "overlapping distinct NotConnected pins must report their connection: {:#?}",
        inspection.issues
    );

    let mut document = baseline.clone();
    let a = common::pin_point(&document, "R1.R", "2");
    let b = common::pin_point(&document, "R2.R", "2");
    for (path_index, offset) in [2.54, -2.54].into_iter().enumerate() {
        let mut points = vec![a];
        points.extend((1..10).map(|index| {
            let t = f64::from(index) / 10.0;
            Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t + offset)
        }));
        points.push(b);
        document.pages[0]
            .items
            .extend(
                points
                    .windows(2)
                    .enumerate()
                    .map(|(segment_index, points)| {
                        SchItem::Wire(Wire {
                            id: format!("nc-short-{path_index}-{segment_index}"),
                            a: points[0],
                            b: points[1],
                            unsupported: Vec::new(),
                        })
                    }),
            );
    }
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. })),
        "a wire between NotConnected pins must be reported: {:#?}",
        inspection.issues
    );
    let issue = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
        .unwrap();
    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([issue.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(
        intent.removals().len(),
        2,
        "the local two-wire cut must preserve the other 18 branch segments"
    );
}

#[test]
fn stacked_distinct_no_connects_share_one_marker_and_preserve_conflicts() {
    use pcb_kicad_sch::analysis::marked_no_connect_targets;
    use pcb_sch::{AttributeValue, InstanceKind};

    let mut netlist = common::compile_fixture("multi_pad_nc", "root.zen");
    // Distinct logical A/B terminals, including a hidden occurrence, all share
    // a displayed name and anchor. Pad identities, not names, identify intent.
    for instance in netlist.instances.values_mut() {
        if instance.kind == InstanceKind::Component {
            instance.attributes.insert("__symbol_value".into(), AttributeValue::String(r#"
                (symbol "MULTI_PAD" (symbol "MULTI_PAD_1_1"
                  (pin no_connect line (at -5.08 2.54 0) (length 2.54) (name "NC") (number "1"))
                  (pin no_connect line (at -5.08 2.54 0) (length 2.54) hide (name "NC") (number "2"))
                  (pin no_connect line (at -5.08 2.54 0) (length 2.54) (name "NC") (number "4"))))
            "#.into()));
        }
    }
    for net in netlist.nets.values_mut() {
        net.kind = "NotConnected".into();
    }
    let baseline = plan_reconciliation(None, &netlist, "stacked.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let targets = marked_no_connect_targets(&baseline).unwrap();
    assert_eq!(
        targets
            .iter()
            .map(|target| target.pin_number.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["1", "2", "4"])
    );
    let markers = baseline
        .pages
        .iter()
        .enumerate()
        .flat_map(|(index, page)| {
            page.items.iter().filter_map(move |item| match item {
                SchItem::NoConnect(marker) => Some((index, marker.at)),
                _ => None,
            })
        })
        .collect::<Vec<_>>();
    let [(page_index, at)] = markers.as_slice() else {
        panic!("expected one marker, got {markers:?}");
    };
    let (page_index, at) = (*page_index, *at);
    assert!(
        inspect_schematic(&baseline, &netlist)
            .unwrap()
            .issues
            .is_empty()
    );
    let second = plan_reconciliation(Some(&baseline), &netlist, "stacked.kicad_sch")
        .unwrap()
        .apply(Some(&baseline))
        .unwrap();
    assert_eq!(second, baseline);

    let mut missing = baseline.clone();
    for page in &mut missing.pages {
        page.items
            .retain(|item| !matches!(item, SchItem::NoConnect(_)));
    }
    let inspection = inspect_schematic(&missing, &netlist).unwrap();
    assert_eq!(
        inspection
            .issues
            .iter()
            .filter_map(|issue| match &issue.issue {
                SchematicIssue::MissingNoConnect { pin_number, .. } => Some(pin_number.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["1", "2", "4"]),
        "include the hidden pin and distinguish duplicate display names"
    );
    let selected = inspection
        .issues
        .iter()
        .filter(|issue| matches!(issue.issue, SchematicIssue::MissingNoConnect { .. }))
        .map(|issue| issue.key.clone())
        .collect();
    let repair =
        plan_connectivity_repair(&missing, &netlist, &inspection, &selected, &BTreeSet::new())
            .unwrap();
    assert_eq!(repair.no_connect_additions().len(), 1);

    // A marker still conflicts with B even while it correctly covers A.
    let mut mixed = netlist.clone();
    mixed.nets.get_mut("RIGHT").unwrap().kind = "Net".into();
    assert!(
        inspect_schematic(&baseline, &mixed)
            .unwrap()
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::UnexpectedNoConnect { .. }))
    );

    for attachment in [
        SchItem::Wire(Wire {
            id: "nc-wire".into(),
            a: at,
            b: Point::new(at.x + 2.54, at.y),
            unsupported: Vec::new(),
        }),
        SchItem::Label(pcb_kicad_sch::Label::new("nc-label", "CONNECTED", at)),
    ] {
        let mut connected = baseline.clone();
        connected.pages[page_index].items.push(attachment);
        assert!(marked_no_connect_targets(&connected).unwrap().is_empty());
        assert!(
            inspect_schematic(&connected, &netlist)
                .unwrap()
                .issues
                .iter()
                .any(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
        );
    }

    // An identical symbol UUID and anchor in another file must not inherit NC.
    let mut pages = baseline.clone();
    let mut other = pages.pages[page_index].clone();
    other.id = "other-page".into();
    other.file_name = Some("other.kicad_sch".into());
    other
        .items
        .retain(|item| !matches!(item, SchItem::NoConnect(_)));
    pages.root_page_ids.push(other.id.clone());
    pages.pages.push(other);
    assert_eq!(marked_no_connect_targets(&pages).unwrap(), targets);
    let missing = inspect_schematic(&pages, &netlist)
        .unwrap()
        .issues
        .into_iter()
        .filter_map(|issue| match issue.issue {
            SchematicIssue::MissingNoConnect {
                page_id,
                pin_number,
                ..
            } => Some((page_id, pin_number)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        missing,
        ["1", "2", "4"]
            .into_iter()
            .map(|pin| ("other-page".into(), pin.into()))
            .collect()
    );

    // Two different symbols touching at their NC pins remain a real short.
    let mut overlap = baseline.clone();
    let mut other = overlap.pages[page_index]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol.clone()),
            _ => None,
        })
        .unwrap();
    other.id = "different-symbol".into();
    overlap.pages[page_index].items.push(SchItem::Symbol(other));
    assert!(marked_no_connect_targets(&overlap).unwrap().is_empty());
    assert!(
        inspect_schematic(&overlap, &netlist)
            .unwrap()
            .issues
            .iter()
            .any(|issue| matches!(issue.issue, SchematicIssue::UnexpectedConnection { .. }))
    );
}

#[test]
fn one_missing_no_connect_issue_repairs_all_uncovered_physical_pin_occurrences() {
    let mut netlist = common::compile_fixture("multi_pad_nc", "root.zen");
    let mut not_connected = netlist.nets.remove("LEFT").unwrap();
    not_connected.kind = "NotConnected".to_string();
    not_connected.name.clear();
    netlist.nets.insert(String::new(), not_connected);
    let baseline = plan_reconciliation(None, &netlist, "multi_pad_nc.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let (page_index, symbol) = baseline
        .pages
        .iter()
        .enumerate()
        .find_map(|(page_index, page)| {
            page.items.iter().find_map(|item| match item {
                SchItem::Symbol(symbol) if symbol.field_value("Path") == Some("U1.MULTI_PAD") => {
                    Some((page_index, symbol))
                }
                _ => None,
            })
        })
        .unwrap();
    let pins = baseline.pages[page_index].library.definitions[&symbol.lib_id]
        .placed_pins(symbol)
        .unwrap();
    let visible = pins
        .iter()
        .find(|pin| pin.name == "A" && !pin.hidden)
        .unwrap()
        .point;
    let hidden = pins
        .iter()
        .find(|pin| pin.name == "A" && pin.hidden)
        .unwrap()
        .point;
    let markers = baseline.pages[page_index]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::NoConnect(marker) => Some(marker.at),
            _ => None,
        })
        .collect::<Vec<_>>();
    let same_point = |left: Point, right: Point| {
        (left.x - right.x).abs() < 1.0e-9 && (left.y - right.y).abs() < 1.0e-9
    };
    assert_eq!(markers.len(), 2);
    assert!(markers.iter().any(|point| same_point(*point, visible)));
    assert!(markers.iter().any(|point| same_point(*point, hidden)));

    let mut both_missing = baseline.clone();
    both_missing.pages[page_index]
        .items
        .retain(|item| !matches!(item, SchItem::NoConnect(_)));
    let inspection = inspect_schematic(&both_missing, &netlist).unwrap();
    let missing = inspection
        .issues
        .iter()
        .filter(|issue| matches!(issue.issue, SchematicIssue::MissingNoConnect { .. }))
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 1, "{:#?}", inspection.issues);
    let selected = BTreeSet::from([missing[0].key.clone()]);
    let intent = plan_connectivity_repair(
        &both_missing,
        &netlist,
        &inspection,
        &selected,
        &BTreeSet::new(),
    )
    .unwrap();
    let additions = intent
        .no_connect_additions()
        .iter()
        .map(|target| target.at)
        .collect::<Vec<_>>();
    assert_eq!(additions.len(), 2);
    assert!(additions.iter().any(|point| same_point(*point, visible)));
    assert!(additions.iter().any(|point| same_point(*point, hidden)));
    let repaired = plan_repairs(&both_missing, &netlist, &inspection, selected)
        .unwrap()
        .apply(Some(&both_missing))
        .unwrap();
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .issues
            .is_empty()
    );

    let mut hidden_missing = baseline;
    hidden_missing.pages[page_index].items.retain(
        |item| !matches!(item, SchItem::NoConnect(marker) if same_point(marker.at, hidden)),
    );
    let inspection = inspect_schematic(&hidden_missing, &netlist).unwrap();
    let missing = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::MissingNoConnect { .. }))
        .unwrap();
    let intent = plan_connectivity_repair(
        &hidden_missing,
        &netlist,
        &inspection,
        &BTreeSet::from([missing.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();
    assert_eq!(intent.no_connect_additions().len(), 1);
    assert!(same_point(intent.no_connect_additions()[0].at, hidden));
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
/// physical graph has no finite cut — the teardown fallback must still repair it.
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
        placed: true,
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
