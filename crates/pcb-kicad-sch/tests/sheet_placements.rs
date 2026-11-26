mod common;

use common::kicad_builder::KicadBuilder;
use pcb_kicad_sch::{
    SchDocument, SchItem, Sheet,
    analysis::{SchematicIssue, inspect_schematic},
    parse_kicad_sch_page, patch_page_source,
    reconcile::plan_reconciliation,
    restore_sheet_placements, sync_sheet_placements,
};
use serde_json::json;

fn sheets(document: &SchDocument) -> impl Iterator<Item = &Sheet> {
    document
        .pages
        .iter()
        .flat_map(|page| page.items.iter())
        .filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet.as_ref()),
            _ => None,
        })
}

#[test]
fn deleted_source_sheet_restores_as_unplaced_and_reconciles_without_losing_child() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let baseline = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let root_index = baseline
        .pages
        .iter()
        .position(|page| {
            page.file_name.as_deref() == Some("root.kicad_sch")
                && page.items.iter().any(|i| matches!(i, SchItem::Sheet(_)))
        })
        .expect("fixture root has a child sheet");
    let original_sheet = baseline.pages[root_index]
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) => Some((**sheet).clone()),
            _ => None,
        })
        .unwrap();
    let mut project = json!({"unrelated":{"keep":true}, "diode":{"other":"value"}});
    assert!(sync_sheet_placements(&mut project, &baseline).unwrap());
    assert_eq!(project["unrelated"]["keep"], true);
    assert_eq!(project["diode"]["other"], "value");

    let source = SchDocument {
        pages: vec![baseline.pages[root_index].clone()],
        root_page_ids: vec![baseline.pages[root_index].id.clone()],
        ..Default::default()
    }
    .pages[0]
        .to_kicad_sch();
    let mut source_without_sheet = baseline.pages[root_index].clone();
    source_without_sheet
        .items
        .retain(|item| !matches!(item, SchItem::Sheet(sheet) if sheet.id == original_sheet.id));
    let patched = patch_page_source(&source, &source_without_sheet)
        .unwrap()
        .expect("sheet deletion changes source");
    let mut reopened_root = parse_kicad_sch_page(Some("root.kicad_sch"), &patched).unwrap();
    assert!(
        !reopened_root
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Sheet(sheet) if sheet.id == original_sheet.id))
    );
    restore_sheet_placements(&mut reopened_root, &project).unwrap();
    let restored = reopened_root
        .items
        .iter()
        .find_map(|item| match item {
            SchItem::Sheet(sheet) if sheet.id == original_sheet.id => Some(sheet.as_ref()),
            _ => None,
        })
        .expect("metadata restores relationship");
    assert_eq!(restored.id, original_sheet.id);
    assert_eq!(restored.pins, original_sheet.pins);
    assert_eq!(restored.at, original_sheet.at);
    assert_eq!(restored.size, original_sheet.size);
    assert!(!restored.placed);

    let mut reopened = baseline.clone();
    reopened.pages[root_index] = reopened_root;
    assert_eq!(
        reopened.pages.len(),
        baseline.pages.len(),
        "child page is retained"
    );
    let inspection = inspect_schematic(&reopened, &netlist).unwrap();
    assert!(
        inspection
            .issues
            .iter()
            .any(|i| matches!(i.issue, SchematicIssue::MissingSheet { .. }))
    );
    assert!(
        !inspection
            .issues
            .iter()
            .any(|i| matches!(i.issue, SchematicIssue::MissingSymbol { .. })),
        "child symbols remain present: {:#?}",
        inspection.issues
    );

    let repaired = plan_reconciliation(Some(&reopened), &netlist, "root.kicad_sch")
        .unwrap()
        .apply(Some(&reopened))
        .unwrap();
    let repaired_sheet = sheets(&repaired)
        .find(|sheet| sheet.id == original_sheet.id)
        .unwrap();
    assert!(repaired_sheet.placed);
    assert_eq!(repaired_sheet.pins, original_sheet.pins);
    assert_eq!(repaired_sheet.at, original_sheet.at);
    assert_eq!(repaired_sheet.size, original_sheet.size);
    assert_eq!(
        repaired
            .pages
            .iter()
            .find(|p| p.id != baseline.pages[root_index].id),
        baseline
            .pages
            .iter()
            .find(|p| p.id != baseline.pages[root_index].id),
        "existing child content is preserved"
    );
    let second = plan_reconciliation(Some(&repaired), &netlist, "root.kicad_sch")
        .unwrap()
        .apply(Some(&repaired))
        .unwrap();
    assert_eq!(second, repaired, "second reconciliation is a no-op");
}

#[test]
fn restore_is_scoped_to_exact_parent_and_exact_sheet_uuid() {
    let mut builder = KicadBuilder::new();
    builder
        .sheet("shared.kicad_sch", &[])
        .sheet("shared.kicad_sch", &[])
        .add_root_page("other", "other.kicad_sch");
    let original = builder.build();
    let ids = sheets(&original).map(|s| s.id.clone()).collect::<Vec<_>>();
    let mut metadata = json!({});
    sync_sheet_placements(&mut metadata, &original).unwrap();
    let mut root = original.pages[0].clone();
    root.items
        .retain(|item| !matches!(item, SchItem::Sheet(sheet) if sheet.id == ids[0]));
    restore_sheet_placements(&mut root, &metadata).unwrap();
    assert!(
        root.items.iter().any(
            |item| matches!(item, SchItem::Sheet(sheet) if sheet.id == ids[0] && !sheet.placed)
        ),
        "missing UUID must restore even when another instance of the same child file exists"
    );
    assert_eq!(
        root.items
            .iter()
            .filter(|i| matches!(i, SchItem::Sheet(_)))
            .count(),
        2
    );

    let mut other = original.pages[1].clone();
    restore_sheet_placements(&mut other, &metadata).unwrap();
    assert!(
        !other
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Sheet(_))),
        "relationship must not ghost into another parent"
    );
}

#[test]
fn sync_records_deliberate_relationship_removal() {
    let mut builder = KicadBuilder::new();
    builder.sheet("child.kicad_sch", &[]);
    let mut document = builder.build();
    let mut project = json!({"diode":{"other":17}, "outside":"preserved"});
    sync_sheet_placements(&mut project, &document).unwrap();
    document.pages[0]
        .items
        .retain(|item| !matches!(item, SchItem::Sheet(_)));
    assert!(sync_sheet_placements(&mut project, &document).unwrap());
    restore_sheet_placements(&mut document.pages[0], &project).unwrap();
    assert!(
        !document.pages[0]
            .items
            .iter()
            .any(|item| matches!(item, SchItem::Sheet(_))),
        "dissolved relationship must not resurrect"
    );
    assert_eq!(project["diode"]["other"], 17);
    assert_eq!(project["outside"], "preserved");
}

#[test]
fn invalid_metadata_and_escaping_paths_are_rejected() {
    let mut page = KicadBuilder::new().build().pages.remove(0);
    let malformed = json!({"diode":{"schematic_sheets":{"not":"an array"}}});
    assert!(
        restore_sheet_placements(&mut page, &malformed)
            .unwrap_err()
            .to_string()
            .contains("must be an array")
    );

    let mut builder = KicadBuilder::new();
    builder.sheet("../outside.kicad_sch", &[]);
    let mut project = json!({});
    sync_sheet_placements(&mut project, &builder.build()).unwrap();
    assert!(
        restore_sheet_placements(&mut page, &project)
            .unwrap_err()
            .to_string()
            .contains("escapes project directory")
    );
}

#[test]
fn legacy_parent_filenames_migrate_to_stable_ids() {
    let mut builder = KicadBuilder::new();
    builder.sheet("child.kicad_sch", &[]);
    let mut document = builder.build();
    let mut metadata = json!({});
    sync_sheet_placements(&mut metadata, &document).unwrap();
    metadata["diode"]["schematic_sheets"][0]
        .as_object_mut()
        .unwrap()
        .remove("parent_id");
    document.pages[0].items.clear();
    restore_sheet_placements(&mut document.pages[0], &metadata).unwrap();
    assert_eq!(sheets(&document).count(), 1);
    sync_sheet_placements(&mut metadata, &document).unwrap();
    assert_eq!(
        metadata["diode"]["schematic_sheets"][0]["parent_id"],
        document.pages[0].id
    );
    let mut unrelated = document.pages[0].clone();
    unrelated.id = "replacement-page-at-same-path".into();
    unrelated.items.clear();
    restore_sheet_placements(&mut unrelated, &metadata).unwrap();
    assert!(
        unrelated.items.is_empty(),
        "UUID takes precedence over a reused filename"
    );
}

#[test]
fn generated_sheet_pins_follow_the_module_interface() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Emulate a module whose OUTPUT port was renamed to STALE in an earlier
    // version: the generated sheet has a STALE pin and child label instead.
    let sheet_id = pcb_kicad_sch::deterministic_uuid("zener:module-sheet:FILTER_A");
    let child_file = sheets(&document)
        .find(|sheet| sheet.id == sheet_id)
        .unwrap()
        .file_name()
        .to_string();
    for page in &mut document.pages {
        for item in &mut page.items {
            match item {
                SchItem::Sheet(sheet) if sheet.id == sheet_id => {
                    let pin = sheet.pins.iter_mut().find(|p| p.name == "OUTPUT").unwrap();
                    pin.name = "STALE".to_string();
                    pin.id =
                        pcb_kicad_sch::deterministic_uuid("zener:module-sheet-pin:FILTER_A:STALE");
                }
                SchItem::Label(label)
                    if label.id
                        == pcb_kicad_sch::deterministic_uuid(
                            "zener:module-parent-driver:FILTER_A:OUTPUT:MID",
                        ) =>
                {
                    label.id = pcb_kicad_sch::deterministic_uuid(
                        "zener:module-parent-driver:FILTER_A:STALE:MID",
                    );
                }
                SchItem::Label(label)
                    if page.file_name.as_deref() == Some(child_file.as_str())
                        && label.text == "OUTPUT" =>
                {
                    label.text = "STALE".to_string();
                }
                _ => {}
            }
        }
    }

    let repaired = plan_reconciliation(Some(&document), &netlist, "")
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    let pins = sheets(&repaired)
        .find(|sheet| sheet.id == sheet_id)
        .unwrap()
        .pins
        .iter()
        .map(|pin| pin.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(pins, ["INPUT", "OUTPUT"].into());
    // The removed pin's parent label is not left stacked on a new pin.
    let parent = repaired
        .pages
        .iter()
        .find(|page| {
            page.items
                .iter()
                .any(|item| matches!(item, SchItem::Sheet(sheet) if sheet.id == sheet_id))
        })
        .unwrap();
    for pin in &sheets(&repaired).find(|sheet| sheet.id == sheet_id).unwrap().pins {
        let labels = parent
            .items
            .iter()
            .filter(|item| matches!(item, SchItem::Label(label) if label.at == pin.at))
            .count();
        assert_eq!(labels, 1, "{}", pin.name);
    }
    assert!(
        !repaired
            .pages
            .iter()
            .flat_map(|page| &page.items)
            .any(|item| matches!(item, SchItem::Label(label) if label.text == "STALE"))
    );
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn nets_bridged_by_sheet_pins_get_no_global_labels() {
    let netlist = common::compile_fixture("hierarchy", "root_with_parts.zen");
    let document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    let globals = document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            SchItem::Label(label)
                if matches!(label.kind, pcb_kicad_sch::LabelKind::Global { .. }) =>
            {
                Some(label.text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(globals.is_empty(), "{globals:?}");
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}

#[test]
fn ports_already_joined_by_global_labels_get_no_sheet_pin() {
    let netlist = common::compile_fixture("hierarchy", "root.zen");
    let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Join FILTER_A's OUTPUT (net MID) by global labels instead of its sheet
    // pin, as older pcb versions did: an equivalent organization to keep.
    let sheet_id = pcb_kicad_sch::deterministic_uuid("zener:module-sheet:FILTER_A");
    let child_file = sheets(&document)
        .find(|sheet| sheet.id == sheet_id)
        .unwrap()
        .file_name()
        .to_string();
    let mut pin_at = None;
    for page in &mut document.pages {
        for item in &mut page.items {
            if let SchItem::Sheet(sheet) = item
                && sheet.id == sheet_id
            {
                let index = sheet.pins.iter().position(|p| p.name == "OUTPUT").unwrap();
                pin_at = Some(sheet.pins.remove(index).at);
            }
        }
    }
    let global = pcb_kicad_sch::LabelKind::Global {
        shape: pcb_kicad_sch::LabelShape::Bidirectional,
    };
    for page in &mut document.pages {
        let is_child = page.file_name.as_deref() == Some(child_file.as_str());
        for item in &mut page.items {
            if let SchItem::Label(label) = item {
                if is_child && label.text == "OUTPUT" {
                    label.text = "MID".to_string();
                    label.kind = global;
                } else if !is_child
                    && pin_at.is_some_and(|at| {
                        (at.x - label.at.x).abs() < 1e-6 && (at.y - label.at.y).abs() < 1e-6
                    })
                {
                    label.kind = global;
                }
            }
        }
    }
    for page in &mut document.pages {
        if page.file_name.as_deref() == Some("root.kicad_sch") {
            for item in &mut page.items {
                if let SchItem::Label(label) = item
                    && label.text == "MID"
                {
                    label.kind = global;
                }
            }
        }
    }
    assert!(
        inspect_schematic(&document, &netlist)
            .unwrap()
            .analysis
            .is_equivalent(),
        "the global-label organization must start out equivalent"
    );

    let plan = plan_reconciliation(Some(&document), &netlist, "").unwrap();
    let repaired = plan.apply(Some(&document)).unwrap();
    let pins = sheets(&repaired)
        .find(|sheet| sheet.id == sheet_id)
        .unwrap()
        .pins
        .iter()
        .map(|pin| pin.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(pins, ["INPUT"]);
}

#[test]
fn a_port_added_on_a_connected_net_gets_its_own_child_label() {
    let netlist = common::compile_fixture("hierarchy", "root_with_alias.zen");
    let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
        .unwrap()
        .apply(None)
        .unwrap();
    // Emulate a schematic drawn before IN_MONITOR joined the module: the net
    // still crosses the sheet through IN, so it stays connected.
    let sheet_id = pcb_kicad_sch::deterministic_uuid("zener:module-sheet:MONITOR");
    let child_file = sheets(&document)
        .find(|sheet| sheet.id == sheet_id)
        .unwrap()
        .file_name()
        .to_string();
    let child_labels = |document: &SchDocument| {
        document
            .pages
            .iter()
            .filter(|page| page.file_name.as_deref() == Some(child_file.as_str()))
            .flat_map(|page| &page.items)
            .filter_map(|item| match item {
                SchItem::Label(label)
                    if matches!(label.kind, pcb_kicad_sch::LabelKind::Hierarchical { .. }) =>
                {
                    Some(label.text.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(child_labels(&document).contains(&"IN_MONITOR".to_string()));
    for page in &mut document.pages {
        let is_child = page.file_name.as_deref() == Some(child_file.as_str());
        page.items.retain(|item| {
            !(is_child && matches!(item, SchItem::Label(label) if label.text == "IN_MONITOR"))
        });
        for item in &mut page.items {
            if let SchItem::Sheet(sheet) = item
                && sheet.id == sheet_id
            {
                sheet.pins.retain(|pin| pin.name != "IN_MONITOR");
            }
        }
    }

    let repaired = plan_reconciliation(Some(&document), &netlist, "")
        .unwrap()
        .apply(Some(&document))
        .unwrap();
    assert!(
        sheets(&repaired)
            .find(|sheet| sheet.id == sheet_id)
            .unwrap()
            .pins
            .iter()
            .any(|pin| pin.name == "IN_MONITOR")
    );
    let labels = child_labels(&repaired);
    assert!(labels.contains(&"IN_MONITOR".to_string()), "{labels:?}");
    assert!(
        inspect_schematic(&repaired, &netlist)
            .unwrap()
            .analysis
            .is_equivalent()
    );
}
