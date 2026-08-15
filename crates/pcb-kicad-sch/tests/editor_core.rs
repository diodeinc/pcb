mod common;

use pcb_kicad_sch::{
    Point, SchDocument, SchItem, Symbol, SymbolDefinition,
    connectivity::{ConnectivityItemRef, PhysicalConnectivity, PinVisibility},
    reconcile::plan_reconciliation,
    repair::{ConnectivityRepairPlan, plan_connectivity_repair},
};
use pcb_sch::Schematic;

#[test]
fn editor_core_plans_applies_analyzes_and_reopens_in_memory() {
    let netlist = common::compile_fixture("analysis", "simple.zen");
    let plan = plan_reconciliation(None, &netlist, "Editor.kicad_sch").unwrap();

    assert!(!plan.is_empty());
    assert!(plan.analysis_after().is_equivalent());
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
fn pure_repair_removes_only_an_unmatched_net_symbol() {
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

    let repair = plan_connectivity_repair(&with_stray, &netlist).unwrap();
    assert_only_symbol_removal(&repair, "00000000-0000-0000-0000-000000000099");
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
fn pure_repair_removes_a_matching_net_symbol_from_the_wrong_island() {
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

    let repair = plan_connectivity_repair(&shorted, &netlist).unwrap();
    assert_only_symbol_removal(&repair, "00000000-0000-0000-0000-000000000098");

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

fn assert_only_symbol_removal(plan: &ConnectivityRepairPlan, id: &str) {
    assert_eq!(plan.removals().len(), 1);
    assert!(matches!(
        plan.removals().iter().next(),
        Some(ConnectivityItemRef::Symbol { id: removed, .. }) if removed == id
    ));
}
