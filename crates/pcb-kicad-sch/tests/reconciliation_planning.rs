mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    Label, Point, SchDocument, SchItem, Symbol,
    analysis::{SchematicIssue, inspect_schematic},
    connectivity::ConnectivityItemRef,
    reconcile::{DocumentEdit, plan_component_placement, plan_reconciliation},
    routing::{RoutingPolicy, plan_wire_reroute},
};
use pcb_sch::Schematic;

fn generated_document(netlist: &Schematic, file_name: &str) -> SchDocument {
    plan_reconciliation(None, netlist, file_name)
        .unwrap()
        .apply_all(None)
        .unwrap()
}

fn managed_symbol<'a>(document: &'a SchDocument, path: &str) -> &'a Symbol {
    document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing managed symbol {path}"))
}

fn move_symbol(symbol: &mut Symbol, at: Point) {
    let delta = Point::new(at.x - symbol.at.x, at.y - symbol.at.y);
    symbol.at = at;
    for field in symbol.fields.values_mut() {
        field.at.x += delta.x;
        field.at.y += delta.y;
    }
}

#[test]
fn one_missing_symbol_is_one_applicable_patch() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = generated_document(&netlist, "SingleMissing.kicad_sch");
    let preserved_at = managed_symbol(&document, "R1.R").at;
    let missing_id = managed_symbol(&document, "R2.R").id.clone();
    let mut missing = document.clone();
    for page in &mut missing.pages {
        page.items.retain(|item| item.id() != Some(&missing_id));
    }
    assert!(inspect_schematic(&missing, &netlist).unwrap().issues.iter().any(|issue| {
        matches!(&issue.issue, SchematicIssue::MissingSymbol { slot } if slot.component_path() == "R2.R")
    }));

    let plan = plan_reconciliation(Some(&missing), &netlist, "SingleMissing.kicad_sch").unwrap();
    assert_eq!(plan.patches().len(), 1);
    assert!(matches!(
        plan.patches()[0].edits(),
        [DocumentEdit::ReplacePage { .. }]
    ));
    let repaired = plan.apply_one(Some(&missing), 0).unwrap();
    assert_eq!(repaired, plan.apply_all(Some(&missing)).unwrap());
    assert_eq!(managed_symbol(&repaired, "R1.R").at, preserved_at);
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    assert!(
        plan_reconciliation(Some(&repaired), &netlist, "SingleMissing.kicad_sch")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn component_placement_inserts_one_symbol_without_connectivity_repairs() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = generated_document(&netlist, "TwoMissing.kicad_sch");
    let mut missing = document.clone();
    missing.pages[0].items.clear();
    let inspection = inspect_schematic(&missing, &netlist).unwrap();
    let missing_slots = inspection
        .issues
        .iter()
        .filter_map(|issue| match &issue.issue {
            SchematicIssue::MissingSymbol { slot } => Some(slot.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(missing_slots.len(), 2);

    let plan = plan_reconciliation(Some(&missing), &netlist, "TwoMissing.kicad_sch").unwrap();
    assert_eq!(plan.patches().len(), 1);
    let repaired = plan.apply_one(Some(&missing), 0).unwrap();
    assert!(
        missing_slots
            .iter()
            .all(|slot| managed_symbol(&repaired, slot.component_path()).id == slot.symbol_id())
    );

    let first_patch = plan_component_placement(&missing, &netlist, &missing_slots[0]).unwrap();
    let first = first_patch.apply(Some(&missing)).unwrap();
    assert_eq!(first.pages[0].items.len(), 1);
    assert_eq!(
        managed_symbol(&first, missing_slots[0].component_path()).id,
        missing_slots[0].symbol_id()
    );
    assert!(first.pages.iter().flat_map(|page| &page.items).all(|item| {
        !matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(missing_slots[1].component_path()))
    }));
    let first_inspection = inspect_schematic(&first, &netlist).unwrap();
    assert!(!first_inspection.issues.iter().any(|issue| {
        matches!(&issue.issue, SchematicIssue::MissingSymbol { slot } if slot == &missing_slots[0])
    }));
    assert!(first_inspection.issues.iter().any(|issue| {
        matches!(&issue.issue, SchematicIssue::MissingSymbol { slot } if slot == &missing_slots[1])
    }));
    assert!(
        !first_inspection.analysis.is_equivalent(),
        "placement leaves connectivity repair to reconciliation"
    );

    let second_patch = plan_component_placement(&first, &netlist, &missing_slots[1]).unwrap();
    let placed = second_patch.apply(Some(&first)).unwrap();
    assert_eq!(placed.pages[0].items.len(), 2);
    assert!(
        !inspect_schematic(&placed, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let repair = plan_reconciliation(Some(&placed), &netlist, "TwoMissing.kicad_sch").unwrap();
    let complete = repair.apply_all(Some(&placed)).unwrap();
    assert!(
        inspect_schematic(&complete, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn page_patches_are_safe_and_compose_in_either_order() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = generated_document(&netlist, "PatchComposition.kicad_sch");
    assert!(document.pages.len() >= 2);
    let mut broken = document.clone();
    for page_index in 0..2 {
        broken.pages[page_index]
            .items
            .push(SchItem::Label(Label::new(
                format!("unexpected-{page_index}"),
                format!("EXTRA_{page_index}"),
                Point::new(20.0 + page_index as f64 * 10.0, 20.0),
            )));
    }
    assert_eq!(
        inspect_schematic(&broken, &netlist).unwrap().issues.len(),
        2
    );

    let plan = plan_reconciliation(Some(&broken), &netlist, "PatchComposition.kicad_sch").unwrap();
    assert_eq!(plan.patches().len(), 2);
    for patch in plan.patches() {
        let partial = patch.apply(Some(&broken)).unwrap();
        let inspection = inspect_schematic(&partial, &netlist).unwrap();
        assert_eq!(inspection.issues.len(), 1, "{:#?}", inspection.issues);
    }

    let forward = plan.apply_all(Some(&broken)).unwrap();
    let reverse = plan.patches()[0]
        .apply(Some(&plan.patches()[1].apply(Some(&broken)).unwrap()))
        .unwrap();
    assert_eq!(forward, reverse);
    assert!(
        inspect_schematic(&forward, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
    assert!(
        plan_reconciliation(Some(&forward), &netlist, "PatchComposition.kicad_sch")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reconciliation_routes_to_an_existing_power_symbol_instead_of_duplicating_it() {
    let netlist = common::compile_fixture("hierarchy", "root_symbol_interface.zen");
    let mut document = generated_document(&netlist, "ExistingPowerSymbol.kicad_sch");
    let (power_id, power_at) = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("POWER") => {
                Some((symbol.id.clone(), symbol.at))
            }
            _ => None,
        })
        .expect("generated POWER symbol");
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let power_wire_ids = inspection.analysis.nets["POWER"]
        .islands
        .iter()
        .flat_map(|island| &inspection.physical.islands[island].items)
        .filter_map(|item| match item {
            ConnectivityItemRef::Wire { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert!(!power_wire_ids.is_empty());
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Wire(wire) if power_wire_ids.contains(&wire.id)));
    assert_eq!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .nets["POWER"]
            .connected_islands
            .len(),
        3
    );
    let wire_count_before = document.pages[0]
        .items
        .iter()
        .filter(|item| matches!(item, SchItem::Wire(_)))
        .count();

    let repaired = plan_reconciliation(Some(&document), &netlist, "ExistingPowerSymbol.kicad_sch")
        .unwrap()
        .apply_all(Some(&document))
        .unwrap();
    let power_symbols = repaired.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("POWER") => Some(symbol),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(power_symbols.len(), 1);
    assert_eq!(power_symbols[0].id, power_id);
    assert_eq!(power_symbols[0].at, power_at);
    assert!(
        repaired.pages[0]
            .items
            .iter()
            .filter(|item| matches!(item, SchItem::Wire(_)))
            .count()
            > wire_count_before
    );
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn selected_symbol_reroute_is_a_pure_geometry_patch() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = generated_document(&netlist, "SelectionReroute.kicad_sch");
    let original = document.clone();
    let selected_id = managed_symbol(&document, "R2.R").id.clone();
    let page_id = document.pages[0].id.clone();
    let selected = document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.id == selected_id => Some(symbol),
            _ => None,
        })
        .unwrap();
    move_symbol(selected, Point::new(selected.at.x + 10.16, selected.at.y));
    let edited = document.clone();

    let patch = plan_wire_reroute(
        &document,
        &netlist,
        &page_id,
        &BTreeSet::from([selected_id]),
        RoutingPolicy::default(),
    )
    .unwrap()
    .expect("moved symbol has routable disconnected nets");
    assert_eq!(document, edited, "routing is pure");
    assert_ne!(document, original, "test setup moved the selected symbol");
    let rerouted = patch.apply(Some(&document)).unwrap();
    assert!(
        inspect_schematic(&rerouted, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn selected_wire_reroute_replaces_the_user_selected_segment() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = generated_document(&netlist, "SelectedWireReroute.kicad_sch");
    let page_id = document.pages[0].id.clone();
    let symbol = managed_symbol(&document, "R2.R");
    let pin_points = document.pages[0].library.definitions[&symbol.lib_id]
        .placed_pins(symbol)
        .unwrap()
        .into_iter()
        .map(|pin| pin.point)
        .collect::<Vec<_>>();
    let selected_wire = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Wire(wire) if pin_points.contains(&wire.a) || pin_points.contains(&wire.b) => {
                Some(wire)
            }
            _ => None,
        })
        .expect("generated route connects R2");
    let selected_id = selected_wire.id.clone();
    let original = document.clone();

    let patch = plan_wire_reroute(
        &document,
        &netlist,
        &page_id,
        &BTreeSet::from([selected_id.clone()]),
        RoutingPolicy::default(),
    )
    .unwrap()
    .expect("selected segment is reroutable");
    assert_eq!(document, original, "routing is pure");
    let rerouted = patch.apply(Some(&document)).unwrap();
    assert!(
        rerouted.pages[0]
            .items
            .iter()
            .all(|item| item.id() != Some(&selected_id))
    );
    assert!(
        inspect_schematic(&rerouted, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}
