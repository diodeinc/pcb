mod common;

use pcb_kicad_sch::{
    SchItem,
    connectivity::{PhysicalConnectivity, PinVisibility},
    reconcile::plan_reconciliation,
};

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
