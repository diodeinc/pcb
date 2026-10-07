//! Pure reconciliation shared by interactive editors and filesystem adapters.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchPage,
    analysis::{
        ConnectivityInspection, SchematicIssueKey, ensure_issues_resolved, ensure_no_new_issues,
        inspect_schematic, issue_summaries,
    },
    component_slots, compose,
};

/// One exact, reversible change to the typed schematic document.
#[derive(Debug, Clone, PartialEq)]
pub enum DocumentEdit {
    SetRootPages {
        before: Vec<String>,
        after: Vec<String>,
    },
    InsertPage {
        index: usize,
        page: SchPage,
    },
    RemovePage {
        index: usize,
        page: SchPage,
    },
    ReplacePage {
        index: usize,
        before: SchPage,
        after: SchPage,
    },
}

/// A verified reconciliation decision with no filesystem side effects.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconciliationPlan {
    edits: Vec<DocumentEdit>,
    initial_inspection: InitialInspection,
    inspection_after: ConnectivityInspection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialInspection {
    NoDocument,
    Available(ConnectivityInspection),
    Invalid { message: String },
}

impl ReconciliationPlan {
    pub fn edits(&self) -> &[DocumentEdit] {
        &self.edits
    }

    pub fn initial_inspection(&self) -> &InitialInspection {
        &self.initial_inspection
    }

    pub fn inspection_after(&self) -> &ConnectivityInspection {
        &self.inspection_after
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Apply this plan to the exact document from which it was created.
    pub fn apply(&self, document: Option<&SchDocument>) -> Result<SchDocument> {
        apply_document_edits(document.unwrap_or(&SchDocument::default()), &self.edits)
    }

    /// Reverse this plan from the exact document produced by [`Self::apply`].
    pub fn revert(&self, document: &SchDocument) -> Result<SchDocument> {
        revert_document_edits(document, &self.edits)
    }
}

fn apply_document_edits(document: &SchDocument, edits: &[DocumentEdit]) -> Result<SchDocument> {
    let mut result = document.clone();
    for edit in edits {
        match edit {
            DocumentEdit::SetRootPages { before, after } => {
                if &result.root_page_ids != before {
                    bail!("reconciliation plan root pages do not match the input document");
                }
                result.root_page_ids.clone_from(after);
            }
            DocumentEdit::InsertPage { index, page } => {
                if *index > result.pages.len() {
                    bail!("reconciliation page insertion index {index} is out of bounds");
                }
                result.pages.insert(*index, page.clone());
            }
            DocumentEdit::RemovePage { index, page } => {
                if result.pages.get(*index) != Some(page) {
                    bail!(
                        "reconciliation removed page '{}' does not match the input document",
                        page.id
                    );
                }
                result.pages.remove(*index);
            }
            DocumentEdit::ReplacePage {
                index,
                before,
                after,
            } => {
                let found = result
                    .pages
                    .get_mut(*index)
                    .with_context(|| format!("reconciliation page index {index} is absent"))?;
                if found != before {
                    bail!(
                        "reconciliation page '{}' does not match the input document",
                        before.id
                    );
                }
                found.clone_from(after);
            }
        }
    }
    Ok(result)
}

fn revert_document_edits(document: &SchDocument, edits: &[DocumentEdit]) -> Result<SchDocument> {
    let mut result = document.clone();
    for edit in edits.iter().rev() {
        match edit {
            DocumentEdit::SetRootPages { before, after } => {
                if &result.root_page_ids != after {
                    bail!("reconciliation plan root pages do not match the repaired document");
                }
                result.root_page_ids.clone_from(before);
            }
            DocumentEdit::InsertPage { index, page } => {
                let found = result
                    .pages
                    .get(*index)
                    .with_context(|| format!("reconciliation page index {index} is absent"))?;
                if found != page {
                    bail!(
                        "reconciliation inserted page '{}' does not match the repaired document",
                        page.id
                    );
                }
                result.pages.remove(*index);
            }
            DocumentEdit::RemovePage { index, page } => {
                if *index > result.pages.len() {
                    bail!("reconciliation page restoration index {index} is out of bounds");
                }
                result.pages.insert(*index, page.clone());
            }
            DocumentEdit::ReplacePage {
                index,
                before,
                after,
            } => {
                let found = result
                    .pages
                    .get_mut(*index)
                    .with_context(|| format!("reconciliation page index {index} is absent"))?;
                if found != after {
                    bail!(
                        "reconciliation page '{}' does not match the repaired document",
                        after.id
                    );
                }
                found.clone_from(before);
            }
        }
    }
    Ok(result)
}

/// Build and verify the exact document edits needed to match a Zener netlist.
///
/// This is the semantic core used by both `pcb apply` and interactive clients.
/// It is pure: callers decide whether and how to persist the returned plan.
pub fn plan_reconciliation(
    document: Option<&SchDocument>,
    netlist: &Schematic,
    root_file_name: &str,
) -> Result<ReconciliationPlan> {
    component_slots::validate_symbol_library_versions(netlist)?;
    let initial_inspection = match document {
        None => InitialInspection::NoDocument,
        Some(document) => match inspect_schematic(document, netlist) {
            Ok(inspection) => InitialInspection::Available(inspection),
            Err(error) => InitialInspection::Invalid {
                message: format!("{error:#}"),
            },
        },
    };
    build_plan(
        document,
        netlist,
        Some(root_file_name),
        None,
        None,
        initial_inspection,
    )
}

/// Project an existing document for read-only use and verify netlist equivalence.
///
/// This runs the full property, library, and topology projection, even when the
/// input connectivity is already equivalent. The owned result is not a reversible
/// edit plan; callers must use [`plan_reconciliation`] for editable workflows.
pub fn reconcile_read_only(
    document: &SchDocument,
    netlist: &Schematic,
) -> Result<(SchDocument, ConnectivityInspection)> {
    component_slots::validate_symbol_library_versions(netlist)?;
    // Complete projection discovers electrical repairs after projecting symbols.
    // Initial inspection only contributes sheet restoration and symbol removals.
    // If neither is possible, even a failed inspection would contribute nothing.
    // Otherwise preserve the existing inspection/error-recovery behavior.
    let inspection_before = if needs_initial_structural_repairs(document, netlist).unwrap_or(true) {
        inspect_schematic(document, netlist).ok()
    } else {
        None
    };
    let (desired, inspection_after) = compose::reconcile_document(
        Some(document),
        netlist,
        None,
        None,
        None,
        inspection_before.as_ref(),
    )?;
    if !inspection_after.analysis.is_equivalent() {
        bail!(
            "planned schematic is not netlist-equivalent: {}",
            issue_summaries(inspection_after.analysis.issues().iter())
        );
    }
    Ok((desired, inspection_after))
}

fn needs_initial_structural_repairs(document: &SchDocument, netlist: &Schematic) -> Result<bool> {
    let expected = component_slots::component_symbol_slots(netlist)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for page in &document.pages {
        for item in &page.items {
            match item {
                crate::SchItem::Sheet(sheet) if !sheet.placed => return Ok(true),
                crate::SchItem::Symbol(symbol) => {
                    if symbol
                        .field_value("Path")
                        .and_then(|path| crate::SymbolSlotKey::new(path, symbol.unit))
                        .is_some_and(|slot| expected.contains(&slot))
                    {
                        continue;
                    }
                    let Some(definition) = page.library.definitions.get(symbol.library_key())
                    else {
                        return Ok(true);
                    };
                    let definition = crate::symbol::ParsedSymbolDefinition::parse(definition)?;
                    if definition.power_scope().is_none()
                        && !definition.is_unmanaged_graphic(symbol)
                    {
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(false)
}

/// Refresh assembly properties owned by the Zener netlist without changing
/// schematic topology, placement, fields, or unmanaged symbols.
///
/// Returns whether any placed managed symbol changed. Interactive editors use
/// this narrow synchronization when accepting a new analysis context; full
/// topology repair remains an explicit action.
pub fn sync_netlist_derived_symbol_properties(
    document: &mut SchDocument,
    netlist: &Schematic,
) -> Result<bool> {
    let instances = component_slots::component_instances(netlist)?;
    let mut changed = false;
    for page in &mut document.pages {
        for item in &mut page.items {
            let crate::SchItem::Symbol(symbol) = item else {
                continue;
            };
            let Some(path) = symbol.field_value("Path") else {
                continue;
            };
            let Some(instance) = instances.get(path) else {
                continue;
            };
            changed |= sync_symbol_netlist_derived_properties(symbol, instance);
        }
    }
    Ok(changed)
}

/// Refresh one placed symbol's assembly properties from its netlist component.
pub fn sync_symbol_netlist_derived_properties(
    symbol: &mut crate::Symbol,
    instance: &pcb_sch::Instance,
) -> bool {
    component_slots::sync_netlist_derived_symbol_properties(symbol, instance)
}

/// Build and verify one exact repair plan for a set of current issues.
///
/// A per-issue repair passes a singleton set. Multiple selected issues use the
/// same planner and mutation policy; selection changes only the repair scope.
/// `inspection` must be the snapshot from which the selected keys were read.
pub fn plan_repairs(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_issue_keys: BTreeSet<SchematicIssueKey>,
) -> Result<ReconciliationPlan> {
    plan_repairs_impl(document, netlist, inspection, selected_issue_keys, None)
}

/// Like [`plan_repairs`], but new symbols for the selected missing-symbol
/// issues are placed on `placement_page_id` instead of their module's page.
/// Net drivers adapt (a net that now spans pages gets global labels), and the
/// usual plan verification still applies.
pub fn plan_repairs_on_page(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_issue_keys: BTreeSet<SchematicIssueKey>,
    placement_page_id: &str,
) -> Result<ReconciliationPlan> {
    plan_repairs_impl(
        document,
        netlist,
        inspection,
        selected_issue_keys,
        Some(placement_page_id),
    )
}

fn plan_repairs_impl(
    document: &SchDocument,
    netlist: &Schematic,
    inspection: &ConnectivityInspection,
    selected_issue_keys: BTreeSet<SchematicIssueKey>,
    placement_page_id: Option<&str>,
) -> Result<ReconciliationPlan> {
    component_slots::validate_symbol_library_versions(netlist)?;
    build_plan(
        Some(document),
        netlist,
        None,
        Some(&selected_issue_keys),
        placement_page_id,
        InitialInspection::Available(inspection.clone()),
    )
}

fn build_plan(
    document: Option<&SchDocument>,
    netlist: &Schematic,
    root_file_name: Option<&str>,
    issue_selection: Option<&BTreeSet<SchematicIssueKey>>,
    placement_page_id: Option<&str>,
    initial_inspection: InitialInspection,
) -> Result<ReconciliationPlan> {
    let inspection_before = match &initial_inspection {
        InitialInspection::Available(inspection) => Some(inspection),
        InitialInspection::NoDocument | InitialInspection::Invalid { .. } => None,
    };
    let (desired, inspection_after) = compose::reconcile_document(
        document,
        netlist,
        root_file_name,
        issue_selection,
        placement_page_id,
        inspection_before,
    )?;
    match issue_selection {
        None => {
            if !inspection_after.analysis.is_equivalent() {
                bail!(
                    "planned schematic is not netlist-equivalent: {}",
                    issue_summaries(inspection_after.analysis.issues().iter())
                );
            }
        }
        Some(selected_keys) => {
            let before = inspection_before
                .context("repairing selected issues requires an existing schematic document")?;
            for key in selected_keys {
                if !before.issues.iter().any(|issue| &issue.key == key) {
                    bail!("schematic issue {key:?} is not present");
                }
            }
            ensure_issues_resolved(&inspection_after, selected_keys, "planned repair")?;
            ensure_no_new_issues(before, &inspection_after, "planned repair")?;
        }
    }
    verified_plan(document, desired, initial_inspection, inspection_after)
}

fn verified_plan(
    document: Option<&SchDocument>,
    desired: SchDocument,
    initial_inspection: InitialInspection,
    inspection_after: ConnectivityInspection,
) -> Result<ReconciliationPlan> {
    let edits = document_edits(document.unwrap_or(&SchDocument::default()), &desired)?;
    let plan = ReconciliationPlan {
        edits,
        initial_inspection,
        inspection_after,
    };
    let applied = plan.apply(document)?;
    if applied != desired {
        bail!("reconciliation plan does not reproduce its verified document");
    }
    if plan.revert(&applied)? != document.cloned().unwrap_or_default() {
        bail!("reconciliation plan does not reverse to its input document");
    }
    Ok(plan)
}

fn document_edits(before: &SchDocument, after: &SchDocument) -> Result<Vec<DocumentEdit>> {
    let mut edits = Vec::new();
    if before.root_page_ids != after.root_page_ids {
        edits.push(DocumentEdit::SetRootPages {
            before: before.root_page_ids.clone(),
            after: after.root_page_ids.clone(),
        });
    }
    let mut retained = before.pages.iter().collect::<Vec<_>>();
    for (index, page) in before.pages.iter().enumerate().rev() {
        if !after.pages.iter().any(|next| next.id == page.id) {
            edits.push(DocumentEdit::RemovePage {
                index,
                page: page.clone(),
            });
            retained.remove(index);
        }
    }
    for (index, page) in after.pages.iter().enumerate() {
        match retained.get(index).copied() {
            Some(previous) if previous.id != page.id => bail!(
                "reconciliation reordered page '{}' to index {index}",
                page.id
            ),
            Some(previous) if previous != page => edits.push(DocumentEdit::ReplacePage {
                index,
                before: previous.clone(),
                after: page.clone(),
            }),
            Some(_) => {}
            None => edits.push(DocumentEdit::InsertPage {
                index,
                page: page.clone(),
            }),
        }
    }
    Ok(edits)
}

#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchPage, root_page_id};

    fn assert_read_only_outcome_matches_plan(document: &SchDocument, netlist: &Schematic) {
        match plan_reconciliation(Some(document), netlist, "root.kicad_sch") {
            Ok(_) => {
                assert_read_only_matches_plan(document, netlist);
            }
            Err(planned) => {
                let direct = reconcile_read_only(document, netlist).unwrap_err();
                assert_eq!(format!("{direct:#}"), format!("{planned:#}"));
            }
        }
    }

    fn assert_read_only_matches_plan(document: &SchDocument, netlist: &Schematic) -> SchDocument {
        let before = document.clone();
        let plan = plan_reconciliation(Some(document), netlist, "root.kicad_sch").unwrap();
        let (projected, inspection) = reconcile_read_only(document, netlist).unwrap();
        assert_eq!(projected, plan.apply(Some(document)).unwrap());
        assert_eq!(&inspection, plan.inspection_after());
        assert_eq!(inspection, inspect_schematic(&projected, netlist).unwrap());
        assert!(inspection.analysis.is_equivalent());
        assert_eq!(document, &before);
        projected
    }

    #[test]
    fn read_only_repairs_changed_topology() {
        let netlist = common::compile_fixture("analysis", "simple.zen");
        let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        document.pages[0]
            .items
            .retain(|item| !matches!(item, crate::SchItem::Label(_)));
        assert!(!needs_initial_structural_repairs(&document, &netlist).unwrap());
        assert!(
            !inspect_schematic(&document, &netlist)
                .unwrap()
                .analysis
                .is_equivalent()
        );
        assert_ne!(assert_read_only_matches_plan(&document, &netlist), document);
    }

    #[test]
    fn read_only_refreshes_properties_even_with_equivalent_topology() {
        let mut netlist = common::compile_fixture("analysis", "simple.zen");
        let document = plan_reconciliation(None, &netlist, "root.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        assert!(!needs_initial_structural_repairs(&document, &netlist).unwrap());
        for instance in netlist
            .instances
            .values_mut()
            .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
        {
            instance
                .attributes
                .insert("dnp".into(), pcb_sch::AttributeValue::Boolean(true));
            instance
                .attributes
                .insert("skip_bom".into(), pcb_sch::AttributeValue::Boolean(true));
        }
        assert!(
            inspect_schematic(&document, &netlist)
                .unwrap()
                .analysis
                .is_equivalent()
        );
        let projected = assert_read_only_matches_plan(&document, &netlist);
        assert_ne!(projected, document);
        assert!(
            projected
                .pages
                .iter()
                .flat_map(|p| &p.items)
                .filter_map(|item| match item {
                    crate::SchItem::Symbol(s) if s.field_value("Path").is_some() => Some(s),
                    _ => None,
                })
                .all(|s| s.dnp && !s.in_bom)
        );
    }

    #[test]
    fn read_only_matches_multi_page_and_no_connect_projection() {
        for (project, entrypoint) in [
            ("hierarchy", "nested_not_connected.zen"),
            ("multi_pad_nc", "root.zen"),
        ] {
            let mut netlist = common::compile_fixture(project, entrypoint);
            if project == "multi_pad_nc" {
                for net in netlist.nets.values_mut() {
                    net.kind = "NotConnected".into();
                }
            }
            let mut document = plan_reconciliation(None, &netlist, "root.kicad_sch")
                .unwrap()
                .apply(None)
                .unwrap();
            if project == "hierarchy" {
                assert!(document.pages.len() > 1);
                for page in &mut document.pages {
                    for item in &mut page.items {
                        if let crate::SchItem::Sheet(sheet) = item {
                            sheet.placed = false;
                        }
                    }
                }
            } else {
                assert!(
                    document
                        .pages
                        .iter()
                        .flat_map(|p| &p.items)
                        .any(|i| matches!(i, crate::SchItem::NoConnect(_)))
                );
                for page in &mut document.pages {
                    page.items
                        .retain(|i| !matches!(i, crate::SchItem::NoConnect(_)));
                }
            }
            assert_eq!(
                needs_initial_structural_repairs(&document, &netlist).unwrap(),
                project == "hierarchy"
            );
            assert_read_only_matches_plan(&document, &netlist);
            if project == "hierarchy" {
                for page in &mut document.pages {
                    page.library.definitions.clear();
                }
                assert!(needs_initial_structural_repairs(&document, &netlist).unwrap());
                assert!(inspect_schematic(&document, &netlist).is_err());
                assert_read_only_outcome_matches_plan(&document, &netlist);
            }
        }
    }

    #[test]
    fn read_only_matches_expected_slot_projection_and_invalid_recovery() {
        let netlist = common::compile_fixture("analysis", "simple.zen");
        let document = plan_reconciliation(None, &netlist, "root.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        let index = document.pages[0]
            .items
            .iter()
            .position(
                |item| matches!(item, crate::SchItem::Symbol(s) if s.field_value("Path").is_some()),
            )
            .unwrap();

        // Missing slots need no initial removal; projection creates them.
        let mut missing = document.clone();
        missing.pages[0].items.remove(index);
        assert!(!needs_initial_structural_repairs(&missing, &netlist).unwrap());
        assert_read_only_matches_plan(&missing, &netlist);

        // A duplicate expected slot is still eligible, with or without a unique UUID.
        for duplicate_uuid in [false, true] {
            let mut duplicate = document.clone();
            let mut item = duplicate.pages[0].items[index].clone();
            if let crate::SchItem::Symbol(symbol) = &mut item
                && !duplicate_uuid {
                    symbol.id = "duplicate-slot".into();
                }
            duplicate.pages[0].items.push(item);
            assert!(!needs_initial_structural_repairs(&duplicate, &netlist).unwrap());
            if duplicate_uuid {
                assert!(inspect_schematic(&duplicate, &netlist).is_err());
            }
            assert_read_only_matches_plan(&duplicate, &netlist);
        }

        // Expected identities can refresh missing cached definitions without inspection.
        let mut uncached = document.clone();
        uncached.pages[0].library.definitions.clear();
        assert!(!needs_initial_structural_repairs(&uncached, &netlist).unwrap());
        assert!(inspect_schematic(&uncached, &netlist).is_err());
        assert_read_only_matches_plan(&uncached, &netlist);

        // Retain the canonical UUID while making its occupant unbound or mismatched.
        // Initial removal is essential before projection tries to fill the missing slot.
        for mismatch in [false, true] {
            let mut unbound = document.clone();
            let crate::SchItem::Symbol(symbol) = &mut unbound.pages[0].items[index] else {
                unreachable!();
            };
            if mismatch {
                symbol.fields.get_mut("Path").unwrap().value = "absent-component".into();
            } else {
                symbol.fields.remove("Path");
            }
            assert!(needs_initial_structural_repairs(&unbound, &netlist).unwrap());
            assert_read_only_matches_plan(&unbound, &netlist);
        }
    }

    #[test]
    fn read_only_native_classification_uses_exact_power_and_graphic_predicates() {
        let netlist = common::compile_fixture("analysis", "simple.zen");
        let document = plan_reconciliation(None, &netlist, "root.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        for power in [false, true] {
            let mut native = document.clone();
            let mut symbol = native.pages[0]
                .items
                .iter()
                .find_map(|item| match item {
                    crate::SchItem::Symbol(symbol) => Some(symbol.clone()),
                    _ => None,
                })
                .unwrap();
            symbol.id = "native-artwork".into();
            symbol.lib_id = "Test:Native".into();
            symbol.lib_name = None;
            symbol.fields.remove("Path");
            symbol.on_board = power;
            symbol.pins.clear();
            let definition = crate::SymbolDefinition::from_kicad_symbol_sexpr(&format!(
                "(symbol \"Test:Native\" {})",
                if power { "(power global)" } else { "" }
            ))
            .unwrap();
            native.pages[0]
                .library
                .definitions
                .insert(symbol.lib_id.clone(), definition);
            native.pages[0].items.push(crate::SchItem::Symbol(symbol));
            assert!(!needs_initial_structural_repairs(&native, &netlist).unwrap());
            assert_read_only_matches_plan(&native, &netlist);
            if !power {
                let crate::SchItem::Symbol(symbol) = native.pages[0].items.last_mut().unwrap()
                else {
                    unreachable!();
                };
                symbol.on_board = true;
                assert!(needs_initial_structural_repairs(&native, &netlist).unwrap());
                assert_read_only_matches_plan(&native, &netlist);
            }
            native.pages[0].library.definitions.remove("Test:Native");
            assert!(needs_initial_structural_repairs(&native, &netlist).unwrap());
            assert_read_only_outcome_matches_plan(&native, &netlist);
            let invalid = crate::SymbolDefinition::from_kicad_symbol_sexpr(
                "(symbol \"Test:Native\" (power invalid))",
            )
            .unwrap();
            native.pages[0]
                .library
                .definitions
                .insert("Test:Native".into(), invalid);
            assert!(needs_initial_structural_repairs(&native, &netlist).is_err());
            assert_read_only_outcome_matches_plan(&native, &netlist);
        }
    }

    #[test]
    fn read_only_matches_malformed_input_and_library_version_errors() {
        let netlist = common::compile_fixture("analysis", "simple.zen");
        let document = plan_reconciliation(None, &netlist, "root.kicad_sch")
            .unwrap()
            .apply(None)
            .unwrap();
        let assert_error = |document: &SchDocument, netlist: &Schematic| {
            let planned =
                plan_reconciliation(Some(document), netlist, "root.kicad_sch").unwrap_err();
            let direct = reconcile_read_only(document, netlist).unwrap_err();
            assert_eq!(format!("{direct:#}"), format!("{planned:#}"));
        };
        assert_error(&SchDocument::default(), &netlist);
        let mut malformed = document.clone();
        malformed.root_page_ids = vec!["absent".into()];
        assert_error(&malformed, &netlist);
        for version in [
            None,
            Some(pcb_sch::AttributeValue::Number(20200101.0)),
            Some(pcb_sch::AttributeValue::String("bad".into())),
        ] {
            let mut invalid = netlist.clone();
            for instance in invalid
                .instances
                .values_mut()
                .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
            {
                instance
                    .attributes
                    .remove(pcb_sch::ATTR_SYMBOL_FORMAT_VERSION);
                if let Some(version) = &version {
                    instance
                        .attributes
                        .insert(pcb_sch::ATTR_SYMBOL_FORMAT_VERSION.into(), version.clone());
                }
            }
            assert_error(&document, &invalid);
        }
    }

    #[test]
    fn document_edits_are_exact_and_reversible_at_the_page_boundary() {
        let before = SchDocument {
            root_page_ids: vec!["old-root".to_string()],
            pages: vec![SchPage::new("old-root")],
        };
        let mut first = before.pages[0].clone();
        first.file_name = Some("main.kicad_sch".to_string());
        let after = SchDocument {
            root_page_ids: vec![root_page_id()],
            pages: vec![first, SchPage::new("child")],
        };
        let edits = document_edits(&before, &after).unwrap();
        let applied = apply_document_edits(&before, &edits).unwrap();
        assert_eq!(applied, after);
        assert_eq!(revert_document_edits(&applied, &edits).unwrap(), before);
    }

    #[test]
    fn removed_pages_are_reversible_with_retained_and_new_pages() {
        let before = SchDocument {
            root_page_ids: vec!["root".to_string()],
            pages: ["root", "obsolete", "retained", "obsolete-child"]
                .map(SchPage::new)
                .to_vec(),
        };
        let mut retained = before.pages[2].clone();
        retained.file_name = Some("renamed.kicad_sch".to_string());
        let after = SchDocument {
            root_page_ids: before.root_page_ids.clone(),
            pages: vec![before.pages[0].clone(), retained, SchPage::new("new")],
        };
        let edits = document_edits(&before, &after).unwrap();
        let applied = apply_document_edits(&before, &edits).unwrap();
        assert_eq!(applied, after);
        assert_eq!(revert_document_edits(&applied, &edits).unwrap(), before);
        assert!(apply_document_edits(&after, &edits).is_err());
    }
}
