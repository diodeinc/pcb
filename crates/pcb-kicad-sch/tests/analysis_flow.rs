mod common;

use pcb_kicad_sch::{SchItem, SymbolSlotKey, analysis::SchematicIssue};

#[test]
fn connectivity_graphs_match() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");

    let analysis = fixture.analyze();

    assert!(analysis.is_equivalent(), "{:?}", analysis.issues());
    insta::assert_debug_snapshot!(analysis);
}

#[test]
fn zener_project_reduces_to_connectivity_graph() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");

    insta::assert_debug_snapshot!(fixture.zener_connectivity());
}

#[test]
fn kicad_project_reduces_to_connectivity_graph() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");

    insta::assert_debug_snapshot!(fixture.kicad_connectivity());
}

#[test]
fn broken_route_reports_the_disconnected_net() {
    let mut fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");
    let removed = fixture.remove_kicad_item("7a490608-016e-58f2-8e95-047dc37bfb71");
    assert!(matches!(removed, SchItem::Wire(_)));

    let analysis = fixture.analyze();

    assert!(matches!(
        analysis.issues(),
        [SchematicIssue::DisconnectedNet {
            net_name,
            islands,
            missing_terminals,
        }] if net_name == "MID" && islands.len() == 2 && missing_terminals.is_empty()
    ));
    insta::assert_debug_snapshot!(analysis);
}

#[test]
fn managed_symbol_uuids_follow_the_existing_component_identity() {
    let fixture = common::AnalysisFixture::load("analysis", "simple.zen", "kicad");

    for symbol in fixture.kicad_document().pages[0]
        .items
        .iter()
        .filter_map(|item| {
            if let SchItem::Symbol(symbol) = item {
                Some(symbol)
            } else {
                None
            }
        })
    {
        let component_path = symbol.field_value("Path").expect("managed symbol path");
        let slot = SymbolSlotKey::new(component_path, symbol.unit).expect("component slot");
        assert_eq!(symbol.id, slot.symbol_id());
        assert_eq!(
            slot.layout_sync_footprint_path().to_kicad_string(),
            pcb_sch::kicad_identity::footprint_kiid_path(component_path)
        );
    }
}
