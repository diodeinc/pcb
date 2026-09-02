//! Pure reconciliation shared by interactive editors and filesystem adapters.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchPage,
    analysis::{
        ConnectivityInspection, SchematicIssueKey, coarse_key, ensure_no_new_issues,
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
    let desired = compose::reconcile_document(
        document,
        netlist,
        root_file_name,
        issue_selection,
        placement_page_id,
        inspection_before,
    )?;
    let inspection_after = inspect_schematic(&desired, netlist)?;
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
                if inspection_after
                    .issues
                    .iter()
                    .any(|issue| coarse_key(&issue.key) == coarse_key(key))
                {
                    bail!("planned repair did not resolve schematic issue {key:?}");
                }
            }
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
    for (index, page) in after.pages.iter().enumerate() {
        match before.pages.get(index) {
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
    if before.pages.len() > after.pages.len() {
        bail!("reconciliation unexpectedly removed schematic pages");
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchPage, root_page_id};

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
}
