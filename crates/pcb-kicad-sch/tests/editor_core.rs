mod common;

use std::collections::BTreeSet;

use pcb_kicad_sch::{
    Label, LabelKind, Point, SchDocument, SchItem, SchPage, Symbol, SymbolDefinition,
    SymbolSlotKey,
    analysis::{SchematicIssue, SchematicIssueKey, inspect_schematic},
    connectivity::{PhysicalConnectivity, PinVisibility},
    deterministic_uuid,
    reconcile::{plan_reconciliation, plan_repairs},
};
use pcb_sch::Schematic;

#[test]
fn editor_core_plans_applies_analyzes_and_reopens_in_memory() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let plan = plan_reconciliation(None, &netlist, "Editor.kicad_sch").unwrap();

    assert!(!plan.is_empty());
    assert!(plan.inspection_after().analysis.is_equivalent());
    let document = plan.apply(None).unwrap();
    let physical = PhysicalConnectivity::from_kicad(&document, PinVisibility::VisibleOnly).unwrap();
    assert!(!physical.graph.components.is_empty());
    assert!(!physical.islands.is_empty());
    assert!(
        physical
            .islands
            .values()
            .any(|island| !island.items.is_empty())
    );

    let page = &document.pages[0];
    let symbol = page
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        })
        .unwrap();
    let definition = &page.library.definitions[&symbol.lib_id];
    assert!(!definition.placed_pins(symbol).unwrap().is_empty());
    assert!(symbol.visual_bounds(definition).unwrap().is_some());

    let unchanged = plan_reconciliation(Some(&document), &netlist, "Editor.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
    assert_eq!(unchanged.apply(Some(&document)).unwrap(), document);

    let mut reorganized = document.clone();
    let label = reorganized.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        })
        .unwrap();
    label.spin = match label.spin {
        pcb_kicad_sch::LabelSpin::Right => pcb_kicad_sch::LabelSpin::Left,
        _ => pcb_kicad_sch::LabelSpin::Right,
    };
    let preserved = plan_reconciliation(Some(&reorganized), &netlist, "Editor.kicad_sch").unwrap();
    assert!(preserved.is_empty(), "{:#?}", preserved.edits());
    assert_eq!(preserved.apply(Some(&reorganized)).unwrap(), reorganized);
}

#[test]
fn root_interfaces_use_hierarchical_labels_directly_on_component_pins() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let page = &document.pages[0];
    let labels = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        labels
            .iter()
            .map(|label| label.text.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["INPUT", "OUTPUT"])
    );
    assert!(
        labels
            .iter()
            .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. }))
    );

    let pin_points = page
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) => Some(
                page.library.definitions[&symbol.lib_id]
                    .placed_pins(symbol)
                    .unwrap(),
            ),
            _ => None,
        })
        .flatten()
        .map(|pin| pin.point)
        .collect::<Vec<_>>();
    assert!(
        labels.iter().all(|label| pin_points.contains(&label.at)),
        "labels={labels:#?} pins={pin_points:#?}"
    );
    assert!(
        page.items
            .iter()
            .all(|item| !matches!(item, SchItem::Wire(_)))
    );
}

#[test]
fn generated_hierarchy_connects_sheet_ports_without_label_bridges() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = plan_reconciliation(None, &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();

    assert!(document.pages.iter().all(|page| {
        page.items
            .iter()
            .all(|item| !matches!(item, SchItem::Wire(_)))
    }));
    for page in &document.pages {
        let labels = page.items.iter().filter_map(|item| match item {
            SchItem::Label(label) => Some(label),
            _ => None,
        });
        if document.root_page_ids.contains(&page.id) {
            assert!(labels.clone().all(|label| label.kind == LabelKind::Local));
        } else {
            assert!(
                labels
                    .clone()
                    .all(|label| matches!(label.kind, LabelKind::Hierarchical { .. })),
                "page={} labels={:#?}",
                page.id,
                labels.clone().collect::<Vec<_>>()
            );
        }
        assert!(labels.count() > 0);
    }
}

#[test]
fn replacing_a_modules_only_component_reuses_its_existing_sheet() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let document = plan_reconciliation(None, &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let mut stale = document.clone();
    let filter_a_page = stale
        .pages
        .iter()
        .position(|page| page.file_name.as_deref() == Some("FILTER_A.kicad_sch"))
        .unwrap();
    let symbol = stale.pages[filter_a_page]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path").is_some() => Some(symbol),
            _ => None,
        })
        .unwrap();
    let original_path = symbol.field_value("Path").unwrap().to_string();
    symbol.fields.get_mut("Path").unwrap().value = "FILTER_A.REMOVED".to_string();

    let repaired = plan_reconciliation(Some(&stale), &netlist, "Hierarchy.kicad_sch")
        .unwrap()
        .apply(Some(&stale))
        .unwrap();

    assert_eq!(repaired.pages.len(), document.pages.len());
    assert_eq!(
        repaired
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .filter(|item| {
                matches!(item, SchItem::Sheet(sheet) if sheet.file_name() == "FILTER_A.kicad_sch")
            })
            .count(),
        1
    );
    assert!(repaired.pages[filter_a_page].items.iter().any(|item| {
        matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(original_path.as_str()))
    }));
}

#[test]
fn initializes_and_semantically_adopts_net_symbols() {
    let (netlist, document) = net_symbol_fixture();

    let ground = net_symbol(&document, "GROUND");
    assert!(ground.field_value("Path").is_none());
    assert!(
        !document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "GROUND"))
    );

    let unchanged = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());

    let mut customized = document.clone();
    let ground = net_symbol_mut(&mut customized, "GROUND");
    let old_lib_id = ground.lib_id.clone();
    ground.lib_id = "Custom:Ground".to_string();
    let custom_definition = SymbolDefinition::from_kicad_symbol_sexpr(
        r#"(symbol "Custom:Ground" (power global)
          (symbol "Ground_1_1"
            (pin power_in line (at 0 0 0) (length 0)
              (name "") (number "1"))))"#,
    )
    .unwrap();
    customized.pages[0].library.definitions.remove(&old_lib_id);
    customized.pages[0]
        .library
        .definitions
        .insert(custom_definition.lib_id.clone(), custom_definition);

    let adopted = plan_reconciliation(Some(&customized), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(adopted.is_empty(), "{:#?}", adopted.edits());
}

#[test]
fn preserves_an_equivalent_label_instead_of_preferring_a_net_symbol() {
    let (netlist, mut document) = net_symbol_fixture();
    let ground_index = document.pages[0]
        .items
        .iter()
        .position(|item| {
            matches!(item, SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("GROUND"))
        })
        .unwrap();
    let SchItem::Symbol(ground) = document.pages[0].items.remove(ground_index) else {
        unreachable!("selected a symbol")
    };
    let definition = document.pages[0]
        .library
        .definitions
        .remove(&ground.lib_id)
        .unwrap();
    let ground_pin = definition
        .placed_pins(&ground)
        .unwrap()
        .into_iter()
        .find(|pin| pin.is_power_input())
        .unwrap();
    let (slot, pin_number) = managed_pin_at(&document, ground_pin.point);
    let label_id = deterministic_uuid(format!(
        "zener:net-label:GROUND:{}:{pin_number}",
        slot.symbol_id()
    ));
    document.pages[0].items.push(SchItem::Label(Label::new(
        label_id,
        "GROUND",
        ground_pin.point,
    )));
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );

    let preserved = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();

    assert!(preserved.is_empty(), "{:#?}", preserved.edits());
    assert_eq!(preserved.apply(Some(&document)).unwrap(), document);
}

#[test]
fn moved_net_symbol_does_not_block_a_missing_driver() {
    let (netlist, mut document) = net_symbol_fixture();
    let original = net_symbol(&document, "GROUND").clone();
    let moved_at = Point::new(original.at.x + 100.0, original.at.y);
    move_symbol(net_symbol_mut(&mut document, "GROUND"), moved_at);

    let plan = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();
    let grounds = repaired.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some("GROUND") => {
                Some(symbol)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(grounds.len(), 2);
    assert!(
        grounds
            .iter()
            .any(|symbol| symbol.id == original.id && symbol.at == moved_at)
    );
    assert!(
        grounds
            .iter()
            .any(|symbol| symbol.id != original.id && symbol.at == original.at)
    );
    let unchanged = plan_reconciliation(Some(&repaired), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(unchanged.is_empty(), "{:#?}", unchanged.edits());
}

#[test]
fn preserves_non_naming_power_symbols() {
    let (netlist, mut document) = net_symbol_fixture();
    let mut power_flag = net_symbol(&document, "GROUND").clone();
    power_flag.id = "00000000-0000-0000-0000-000000000097".to_string();
    power_flag.lib_id = "power:PWR_FLAG".to_string();
    power_flag.fields.get_mut("Value").unwrap().value = "PWR_FLAG".to_string();
    power_flag.at.x += 100.0;
    for field in power_flag.fields.values_mut() {
        field.at.x += 100.0;
    }
    document.pages[0].items.push(SchItem::Symbol(power_flag));
    let power_flag_definition = SymbolDefinition::from_kicad_symbol_sexpr(
        r#"(symbol "power:PWR_FLAG" (power global)
          (symbol "PWR_FLAG_1_1"
            (pin power_out line (at 0 0 0) (length 0)
              (name "") (number "1"))))"#,
    )
    .unwrap();
    document.pages[0]
        .library
        .definitions
        .insert(power_flag_definition.lib_id.clone(), power_flag_definition);

    let preserved = plan_reconciliation(Some(&document), &netlist, "NetSymbols.kicad_sch").unwrap();
    assert!(preserved.is_empty(), "{:#?}", preserved.edits());
}

#[test]
fn reconciliation_removes_only_an_unmatched_net_symbol() {
    let (netlist, document) = net_symbol_fixture();
    let mut with_stray = document.clone();
    let mut stray = net_symbol(&with_stray, "GROUND").clone();
    stray.id = "00000000-0000-0000-0000-000000000099".to_string();
    stray.at.x += 100.0;
    for field in stray.fields.values_mut() {
        field.at.x += 100.0;
    }
    stray.fields.get_mut("Value").unwrap().value = "STRAY".to_string();
    with_stray.pages[0].items.push(SchItem::Symbol(stray));

    assert_eq!(
        with_stray.pages[0].items.len(),
        document.pages[0].items.len() + 1
    );

    let repaired = plan_reconciliation(Some(&with_stray), &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(Some(&with_stray))
        .unwrap();
    assert_eq!(repaired, document);
}

#[test]
fn reconciliation_removes_a_matching_net_symbol_from_the_wrong_island() {
    let (netlist, document) = net_symbol_fixture();
    let mut shorted = document.clone();
    let signal_at = shorted.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Label(label) if label.text == "SIGNAL" => Some(label.at),
            _ => None,
        })
        .unwrap();
    let mut misplaced = net_symbol(&shorted, "GROUND").clone();
    misplaced.id = "00000000-0000-0000-0000-000000000098".to_string();
    move_symbol(&mut misplaced, signal_at);
    shorted.pages[0].items.push(SchItem::Symbol(misplaced));

    let repaired = plan_reconciliation(Some(&shorted), &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(Some(&shorted))
        .unwrap();
    assert_eq!(repaired, document);
}

fn net_symbol_fixture() -> (Schematic, SchDocument) {
    let netlist = common::compile_fixture("analysis", "net_symbols.zen");
    let document = plan_reconciliation(None, &netlist, "NetSymbols.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    (netlist, document)
}

fn net_symbol<'a>(document: &'a SchDocument, value: &str) -> &'a Symbol {
    document.pages[0]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some(value) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing net symbol {value}"))
}

fn net_symbol_mut<'a>(document: &'a mut SchDocument, value: &str) -> &'a mut Symbol {
    document.pages[0]
        .items
        .iter_mut()
        .find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Value") == Some(value) => Some(symbol),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing net symbol {value}"))
}

fn move_symbol(symbol: &mut Symbol, at: Point) {
    let dx = at.x - symbol.at.x;
    let dy = at.y - symbol.at.y;
    symbol.at = at;
    for field in symbol.fields.values_mut() {
        field.at.x += dx;
        field.at.y += dy;
    }
}

fn managed_pin_at(document: &SchDocument, point: Point) -> (SymbolSlotKey, String) {
    document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            SchItem::Symbol(symbol) => Some(symbol),
            _ => None,
        })
        .filter_map(|symbol| {
            let path = symbol.field_value("Path")?;
            let slot = SymbolSlotKey::new(path, symbol.unit)?;
            let definition = &document.pages[0].library.definitions[&symbol.lib_id];
            definition
                .placed_pins(symbol)
                .ok()?
                .into_iter()
                .find(|pin| pin.point == point)
                .map(|pin| (slot, pin.number))
        })
        .next()
        .unwrap_or_else(|| panic!("missing managed pin at {point:?}"))
}

#[test]
fn connected_net_missing_its_port_label_reports_missing_port() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Demote the INPUT port's hierarchical label to a local label: every pin
    // stays connected, but the module no longer exposes the port.
    for page in &mut document.pages {
        for item in &mut page.items {
            if let SchItem::Label(label) = item
                && label.text == "INPUT"
            {
                label.kind = LabelKind::Local;
            }
        }
    }

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        matches!(
            inspection.analysis.issues(),
            [SchematicIssue::MissingPort { net_name, ports, .. }]
                if net_name == "INPUT" && ports == &["INPUT".to_string()]
        ),
        "{:#?}",
        inspection.analysis.issues()
    );
}

/// The composer mints deterministic label ids. When the editor has since
/// moved that label to another page (or the id is otherwise taken), a repair
/// must mint a fresh id instead of stealing the existing item back.
#[test]
fn repairing_a_missing_port_never_steals_a_label_from_another_page() {
    let netlist = common::compile_fixture("hierarchy", "root_interface.zen");
    let mut document = plan_reconciliation(None, &netlist, "RootInterface.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root_page_id = document.pages[0].id.clone();
    for item in &mut document.pages[0].items {
        if let SchItem::Label(label) = item
            && label.text == "INPUT"
        {
            label.kind = LabelKind::Local;
        }
    }
    // Another page owns the id the repair would mint for the root's INPUT
    // context endpoint.
    let taken_id = deterministic_uuid(format!("zener:context-endpoint:{root_page_id}:INPUT:INPUT"));
    let mut extra = SchPage::new("extra");
    extra.file_name = Some("Extra.kicad_sch".to_string());
    extra.items.push(SchItem::Label(Label::new(
        &taken_id,
        "OTHER",
        Point::new(10.0, 10.0),
    )));
    document.pages.push(extra);
    document.root_page_ids.push("extra".to_string());

    let inspection = inspect_schematic(&document, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|issue| issue.key == SchematicIssueKey::MissingPort("INPUT".to_string())),
        "{:#?}",
        inspection.analysis.issues()
    );
    let repaired = plan_repairs(
        &document,
        &netlist,
        &inspection,
        std::collections::BTreeSet::from([SchematicIssueKey::MissingPort("INPUT".to_string())]),
    )
    .unwrap()
    .apply(Some(&document))
    .unwrap();

    // The foreign label stays exactly where the user left it.
    let extra_page = repaired
        .pages
        .iter()
        .find(|page| page.id == "extra")
        .unwrap();
    assert!(extra_page.items.iter().any(|item| matches!(
        item,
        SchItem::Label(label) if label.id == taken_id && label.text == "OTHER"
    )));
    // The root regained its INPUT port under a fresh id.
    assert!(repaired.pages[0].items.iter().any(|item| matches!(
        item,
        SchItem::Label(label) if label.text == "INPUT"
            && matches!(label.kind, LabelKind::Hierarchical { .. })
            && label.id != taken_id
    )));
}
