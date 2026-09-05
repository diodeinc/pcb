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
    }
    .to_kicad_sch()
    .unwrap();
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
