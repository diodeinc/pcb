mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    NetDriverKind, Point, SchDocument, SchItem, Wire,
    analysis::{SchematicIssue, SchematicIssueKey, inspect_schematic},
    connectivity::ConnectivityItemRef,
    plan_connectivity_repair,
    reconcile::{plan_reconciliation, plan_repairs},
    verify_connectivity_repair,
};

#[test]
fn chooses_one_deterministic_wire_when_single_item_repairs_are_ambiguous() {
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
    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([short.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();

    assert_eq!(intent.removals().len(), 1);
    assert!(contains_wire_id(intent.removals(), "ambiguous-a"));
    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();
    let all = plan_reconciliation(Some(&document), &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    assert_eq!(repaired, all);
}

#[test]
fn removes_a_two_wire_cut_when_no_single_wire_resolves_the_short() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let left = label_point(&document, "LEFT");
    let mid = label_point(&document, "MID");
    add_wire(&mut document, "parallel-a", left, mid);
    add_wire(&mut document, "parallel-b", left, mid);
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .unwrap();

    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([short.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();

    assert_eq!(intent.removals().len(), 2);
    assert!(contains_wire_id(intent.removals(), "parallel-a"));
    assert!(contains_wire_id(intent.removals(), "parallel-b"));
}

#[test]
fn fully_repairs_a_three_net_short_across_two_bridges() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let left = label_point(&document, "LEFT");
    let mid = label_point(&document, "MID");
    let right = label_point(&document, "RIGHT");
    add_wire(&mut document, "left-mid-short", left, mid);
    add_wire(&mut document, "mid-right-short", mid, right);
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .expect("three nets should be shorted");

    let plan = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .expect("one selected multi-net short should be repaired completely");
    let repaired = plan.apply(Some(&document)).unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();

    assert!(after.analysis.is_equivalent(), "{:#?}", after.issues);
}

#[test]
fn minimal_cut_survives_a_large_island_with_duplicate_wires() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let left = label_point(&document, "LEFT");
    let mid = label_point(&document, "MID");
    // A 24-segment bridge whose first half is drawn twice: no single-wire
    // removal in the doubled half changes connectivity.
    let steps = 24;
    let points = (0..=steps)
        .map(|index| {
            let t = index as f64 / steps as f64;
            Point::new(left.x + (mid.x - left.x) * t, left.y + (mid.y - left.y) * t)
        })
        .collect::<Vec<_>>();
    for index in 0..steps {
        add_wire(
            &mut document,
            &format!("chain-{index}"),
            points[index],
            points[index + 1],
        );
        if index < steps / 2 {
            add_wire(
                &mut document,
                &format!("chain-dup-{index}"),
                points[index],
                points[index + 1],
            );
        }
    }
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let short = inspection
        .issues
        .iter()
        .find(|issue| matches!(issue.issue, SchematicIssue::Shorted { .. }))
        .expect("the bridge shorts LEFT and MID");
    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([short.key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();

    assert_eq!(intent.removals().len(), 1, "{:?}", intent.removals());
    let removed = match intent.removals().iter().next().unwrap() {
        ConnectivityItemRef::Wire { id, .. } => id.clone(),
        other => panic!("expected a wire cut, got {other:?}"),
    };
    let single_half = (steps / 2..steps)
        .map(|index| format!("chain-{index}"))
        .collect::<BTreeSet<_>>();
    assert!(single_half.contains(&removed), "{removed}");

    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        BTreeSet::from([short.key.clone()]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();
    let after = inspect_schematic(&repaired, &netlist).unwrap();
    assert!(after.analysis.is_equivalent(), "{:#?}", after.issues);
}

#[test]
fn forced_wire_removal_reconnects_only_the_net_it_carried() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let mut document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Replace MID's generated labels with one wire so the net's only
    // connection is geometry.
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Label(label) if label.text == "MID"));
    let r1_p2 = common::pin_point(&document, "R1.R", "2");
    let r2_p1 = common::pin_point(&document, "R2.R", "1");
    add_wire(&mut document, "mid-wire", r1_p2, r2_p1);
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection.analysis.is_equivalent(),
        "{:#?}",
        inspection.issues
    );
    let root_page = document.pages[0].id.clone();
    let forced = ConnectivityItemRef::Wire {
        page_id: root_page.clone(),
        id: "mid-wire".to_string(),
    };

    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::new(),
        &BTreeSet::from([forced.clone()]),
    )
    .unwrap();

    assert_eq!(intent.removals(), &BTreeSet::from([forced]));
    assert_eq!(
        intent.reconnect_nets(),
        &BTreeSet::from(["MID".to_string()])
    );
    assert_eq!(
        intent.driver_kind("MID", &root_page),
        Some(&NetDriverKind::Local)
    );
    assert!(intent.driver_kind("LEFT", &root_page).is_none());

    // PCB's own realizer satisfies PCB's verification of the intent.
    let edited = intent.apply_edits(&document).unwrap();
    let edited_inspection = inspect_schematic(&edited, &netlist).unwrap();
    let remaining = edited_inspection
        .issues
        .iter()
        .map(|issue| issue.key.clone())
        .collect::<BTreeSet<_>>();
    assert!(
        remaining.contains(&SchematicIssueKey::DisconnectedNet("MID".to_string())),
        "{remaining:?}"
    );
    let realized = plan_repairs(&edited, &netlist, &edited_inspection, remaining)
        .unwrap()
        .apply(Some(&edited))
        .unwrap();
    let verified =
        verify_connectivity_repair(&document, &inspection, &netlist, &intent, &realized).unwrap();
    assert!(verified.analysis.is_equivalent(), "{:#?}", verified.issues);

    // A realizer that touches anything outside the intent is rejected.
    let mut tampered = realized.clone();
    tampered.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Label(label) if label.text == "RIGHT"));
    let error = verify_connectivity_repair(&document, &inspection, &netlist, &intent, &tampered)
        .expect_err("removing an unrelated label must fail verification");
    assert!(
        error.to_string().contains("outside the intent"),
        "{error:#}"
    );
}

#[test]
fn intent_names_the_net_symbol_driver_for_a_symbol_backed_net() {
    let netlist = common::compile_fixture("analysis", "net_symbols.zen");
    let document = plan_reconciliation(None, &netlist, "net_symbols.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection.analysis.is_equivalent(),
        "{:#?}",
        inspection.issues
    );
    let root_page = document.pages[0].id.clone();
    let ground_symbol = inspection
        .physical
        .islands
        .values()
        .flat_map(|island| island.named_drivers.get("GROUND"))
        .flatten()
        .find(|item| matches!(item, ConnectivityItemRef::Symbol { .. }))
        .cloned()
        .expect("GROUND is driven by a generated net symbol");

    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::new(),
        &BTreeSet::from([ground_symbol]),
    )
    .unwrap();

    assert!(intent.reconnect_nets().contains("GROUND"));
    match intent.driver_kind("GROUND", &root_page) {
        Some(NetDriverKind::NetSymbol(driver)) => {
            assert!(
                driver.definition.lib_id.contains("GND"),
                "{}",
                driver.definition.lib_id
            );
        }
        other => panic!("expected a net symbol driver, got {other:?}"),
    }
}

#[test]
fn forced_removal_rejects_component_symbols() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let document = plan_reconciliation(None, &netlist, "simple.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let root_page = document.pages[0].id.clone();
    let component = document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.fields.contains_key("Path") => {
                Some(ConnectivityItemRef::Symbol {
                    page_id: root_page.clone(),
                    id: symbol.id.clone(),
                })
            }
            _ => None,
        })
        .expect("the fixture places component symbols");

    let error = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::new(),
        &BTreeSet::from([component]),
    )
    .expect_err("a component symbol is not a removable driver");
    assert!(
        error.to_string().contains("not a removable driver symbol"),
        "{error:#}"
    );
}

#[test]
fn intent_uses_a_global_driver_for_islands_cut_off_across_pages() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root_page = document.root_page_ids[0].clone();
    let root_index = document
        .pages
        .iter()
        .position(|page| page.id == root_page)
        .unwrap();
    // Without the root labels, each child's MID island is cut off from the
    // net on the other child page; only a global driver can rejoin them.
    document.pages[root_index]
        .items
        .retain(|item| !matches!(item, SchItem::Label(label) if label.text == "MID"));
    let inspection = inspect_schematic(&document, &netlist).unwrap();
    let key = SchematicIssueKey::DisconnectedNet("MID".to_string());
    assert!(
        inspection.issues.iter().any(|issue| issue.key == key),
        "{:#?}",
        inspection.issues
    );

    let intent = plan_connectivity_repair(
        &document,
        &netlist,
        &inspection,
        &BTreeSet::from([key.clone()]),
        &BTreeSet::new(),
    )
    .unwrap();
    let child_pages = document
        .pages
        .iter()
        .filter(|page| page.id != root_page)
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    assert!(!child_pages.is_empty());
    for page_id in &child_pages {
        assert_eq!(
            intent.driver_kind("MID", page_id),
            Some(&NetDriverKind::Global),
            "{page_id}: {:#?}",
            intent.drivers()
        );
    }

    // PCB's realizer places the same global drivers the intent names.
    let realized = plan_repairs(&document, &netlist, &inspection, BTreeSet::from([key]))
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    let verified =
        verify_connectivity_repair(&document, &inspection, &netlist, &intent, &realized).unwrap();
    assert!(verified.analysis.is_equivalent(), "{:#?}", verified.issues);
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
