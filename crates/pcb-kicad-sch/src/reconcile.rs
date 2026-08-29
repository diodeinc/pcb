//! Pure reconciliation shared by interactive editors and filesystem adapters.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use pcb_sch::Schematic;
use serde::{Deserialize, Serialize};

use crate::{
    SchDocument, SchPage, SymbolSlotKey,
    analysis::{ConnectivityInspection, SchematicIssue, SchematicIssueKey, inspect_schematic},
    component_slots, compose,
};

/// One exact change to the typed schematic document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// One independently verified reconciliation suggestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentPatch {
    edits: Vec<DocumentEdit>,
}

impl DocumentPatch {
    pub(crate) fn new(edits: Vec<DocumentEdit>) -> Self {
        Self { edits }
    }

    pub fn edits(&self) -> &[DocumentEdit] {
        &self.edits
    }

    /// Apply this patch to the exact document for which it was planned.
    pub fn apply(&self, document: Option<&SchDocument>) -> Result<SchDocument> {
        apply_document_edits(document.unwrap_or(&SchDocument::default()), &self.edits)
    }
}

/// An ordered set of independently applicable, mutually compatible patches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationPlan {
    patches: Vec<DocumentPatch>,
}

impl ReconciliationPlan {
    pub fn patches(&self) -> &[DocumentPatch] {
        &self.patches
    }

    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    /// Apply one suggestion to the exact document for which the plan was built.
    pub fn apply_one(
        &self,
        document: Option<&SchDocument>,
        patch_index: usize,
    ) -> Result<SchDocument> {
        self.patches
            .get(patch_index)
            .with_context(|| format!("reconciliation patch index {patch_index} is absent"))?
            .apply(document)
    }

    /// Apply every suggestion in deterministic order.
    pub fn apply_all(&self, document: Option<&SchDocument>) -> Result<SchDocument> {
        let mut result = document.cloned().unwrap_or_default();
        for patch in &self.patches {
            result = patch.apply(Some(&result))?;
        }
        Ok(result)
    }
}

fn apply_document_edits(document: &SchDocument, edits: &[DocumentEdit]) -> Result<SchDocument> {
    let mut result = document.clone();
    for edit in edits {
        match edit {
            DocumentEdit::SetRootPages { before, after } => {
                if &result.root_page_ids != before {
                    bail!("reconciliation patch root pages do not match the input document");
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

/// Globally plan and verify every change needed to match a Zener netlist.
///
/// Planning is pure. The complete issue set is solved together, then the
/// verified result is partitioned into independently safe suggestions.
pub fn plan_reconciliation(
    document: Option<&SchDocument>,
    netlist: &Schematic,
    root_file_name: &str,
) -> Result<ReconciliationPlan> {
    component_slots::validate_symbol_library_versions(netlist)?;
    let inspection_before = document
        .map(|document| inspect_schematic(document, netlist))
        .transpose()
        .ok()
        .flatten();
    let desired = compose::reconcile_document(
        document,
        netlist,
        Some(root_file_name),
        inspection_before.as_ref(),
    )?;
    let inspection_after = inspect_schematic(&desired, netlist)?;
    if !inspection_after.analysis.is_equivalent() {
        bail!(
            "planned schematic is not netlist-equivalent: {}",
            issue_summaries(inspection_after.analysis.issues().iter())
        );
    }

    let before = document.cloned().unwrap_or_default();
    let edits = document_edits(&before, &desired)?;
    let patches = if document.is_none() {
        (!edits.is_empty())
            .then_some(DocumentPatch { edits })
            .into_iter()
            .collect()
    } else if let Some(inspection_before) = inspection_before.as_ref() {
        partition_patches(&before, netlist, inspection_before, edits)?
    } else {
        // An invalid source document has no trustworthy per-patch electrical
        // baseline. Keep its globally verified recovery atomic.
        (!edits.is_empty())
            .then_some(DocumentPatch { edits })
            .into_iter()
            .collect()
    };
    let plan = ReconciliationPlan { patches };
    if plan.apply_all(document)? != desired {
        bail!("reconciliation patches do not reproduce their verified document");
    }
    Ok(plan)
}

/// Place one explicitly selected missing component using PCB's normal
/// hierarchy, placement, and connectivity strategies.
///
/// This is the narrow interactive PLACE operation. It deliberately leaves all
/// other existing issues untouched and is not an issue-scoped reconciliation
/// planner.
pub fn plan_component_placement(
    document: &SchDocument,
    netlist: &Schematic,
    slot: &SymbolSlotKey,
) -> Result<DocumentPatch> {
    component_slots::validate_symbol_library_versions(netlist)?;
    let inspection_before = inspect_schematic(document, netlist)?;
    let missing_key = SchematicIssueKey::MissingSymbol(slot.clone());
    if !inspection_before
        .issues
        .iter()
        .any(|issue| issue.key == missing_key)
    {
        bail!("component slot '{slot}' is not currently missing");
    }

    let desired = compose::place_component(document, netlist, slot)?;
    let inspection_after = inspect_schematic(&desired, netlist)?;
    if inspection_after
        .issues
        .iter()
        .any(|issue| issue.key == missing_key)
    {
        bail!("component placement did not add slot '{slot}'");
    }
    let baseline = inspection_before
        .issues
        .iter()
        .map(|issue| coarse_issue_key(&issue.key))
        .collect::<BTreeSet<_>>();
    let new_issues = inspection_after
        .issues
        .iter()
        .filter(|issue| !baseline.contains(&coarse_issue_key(&issue.key)))
        .collect::<Vec<_>>();
    if !new_issues.is_empty() {
        bail!(
            "component placement would introduce unrelated issues: {}",
            issue_summaries(new_issues.iter().map(|issue| &issue.issue))
        );
    }

    let edits = document_edits(document, &desired)?;
    if edits.is_empty() {
        bail!("component placement for slot '{slot}' produced no edits");
    }
    let patch = DocumentPatch::new(edits);
    if patch.apply(Some(document))? != desired {
        bail!("component placement patch does not reproduce its verified document");
    }
    Ok(patch)
}

fn issue_summaries<'a>(issues: impl Iterator<Item = &'a SchematicIssue>) -> String {
    issues
        .map(SchematicIssue::summary)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Clone)]
struct PatchGroup {
    edits: Vec<(usize, DocumentEdit)>,
}

impl PatchGroup {
    fn patch(&self) -> DocumentPatch {
        DocumentPatch {
            edits: self.edits.iter().map(|(_, edit)| edit.clone()).collect(),
        }
    }

    fn merge(mut self, other: Self) -> Self {
        self.edits.extend(other.edits);
        self.edits.sort_by_key(|(order, _)| *order);
        self
    }
}

fn partition_patches(
    document: &SchDocument,
    netlist: &Schematic,
    inspection_before: &ConnectivityInspection,
    edits: Vec<DocumentEdit>,
) -> Result<Vec<DocumentPatch>> {
    let mut structural = Vec::new();
    let mut groups = Vec::new();
    for (order, edit) in edits.into_iter().enumerate() {
        match edit {
            DocumentEdit::SetRootPages { .. } | DocumentEdit::InsertPage { .. } => {
                structural.push((order, edit));
            }
            DocumentEdit::ReplacePage { .. } => groups.push(PatchGroup {
                edits: vec![(order, edit)],
            }),
        }
    }
    if !structural.is_empty() {
        groups.insert(0, PatchGroup { edits: structural });
    }

    let baseline = inspection_before
        .issues
        .iter()
        .map(|issue| coarse_issue_key(&issue.key))
        .collect::<BTreeSet<_>>();

    loop {
        let unsafe_index = groups
            .iter()
            .position(|group| !patch_is_safe(document, netlist, &baseline, &group.patch()));
        let Some(index) = unsafe_index else {
            break;
        };
        if groups.len() == 1 {
            bail!("globally verified reconciliation patch is not independently safe");
        }

        let safe_partner = (0..groups.len())
            .filter(|other| *other != index)
            .find(|other| {
                let merged = groups[index].clone().merge(groups[*other].clone());
                patch_is_safe(document, netlist, &baseline, &merged.patch())
            });
        let other = safe_partner.unwrap_or_else(|| {
            if index + 1 < groups.len() {
                index + 1
            } else {
                index - 1
            }
        });
        merge_groups(&mut groups, index, other);
    }

    // Individually safe changes can still interact through global labels or
    // hierarchy. Merge any pair whose composition is not safe in either order.
    loop {
        let mut incompatible = None;
        'pairs: for left in 0..groups.len() {
            for right in (left + 1)..groups.len() {
                let left_patch = groups[left].patch();
                let right_patch = groups[right].patch();
                if !patches_are_compatible(document, netlist, &baseline, &left_patch, &right_patch)
                {
                    incompatible = Some((left, right));
                    break 'pairs;
                }
            }
        }
        let Some((left, right)) = incompatible else {
            break;
        };
        merge_groups(&mut groups, left, right);
    }

    groups.sort_by_key(|group| group.edits[0].0);
    let patches = groups
        .into_iter()
        .map(|group| group.patch())
        .collect::<Vec<_>>();
    let mut applied = document.clone();
    for patch in &patches {
        applied = patch.apply(Some(&applied))?;
    }
    let inspection = inspect_schematic(&applied, netlist)?;
    if !inspection.analysis.is_equivalent() {
        bail!(
            "applying all reconciliation patches is not netlist-equivalent: {}",
            issue_summaries(inspection.analysis.issues().iter())
        );
    }
    Ok(patches)
}

fn merge_groups(groups: &mut Vec<PatchGroup>, left: usize, right: usize) {
    let (first, second) = if left < right {
        (left, right)
    } else {
        (right, left)
    };
    let right = groups.remove(second);
    let left = groups.remove(first);
    groups.insert(first, left.merge(right));
}

fn patch_is_safe(
    document: &SchDocument,
    netlist: &Schematic,
    baseline: &BTreeSet<SchematicIssueKey>,
    patch: &DocumentPatch,
) -> bool {
    patch
        .apply(Some(document))
        .and_then(|candidate| inspect_schematic(&candidate, netlist))
        .is_ok_and(|inspection| {
            inspection
                .issues
                .iter()
                .all(|issue| baseline.contains(&coarse_issue_key(&issue.key)))
        })
}

fn patches_are_compatible(
    document: &SchDocument,
    netlist: &Schematic,
    baseline: &BTreeSet<SchematicIssueKey>,
    left: &DocumentPatch,
    right: &DocumentPatch,
) -> bool {
    for (first, second) in [(left, right), (right, left)] {
        let Ok(first_document) = first.apply(Some(document)) else {
            return false;
        };
        let Ok(combined) = second.apply(Some(&first_document)) else {
            return false;
        };
        let Ok(inspection) = inspect_schematic(&combined, netlist) else {
            return false;
        };
        if inspection
            .issues
            .iter()
            .any(|issue| !baseline.contains(&coarse_issue_key(&issue.key)))
        {
            return false;
        }
    }
    true
}

/// Strip volatile item fingerprints for before/after issue identity checks.
pub(crate) fn coarse_issue_key(key: &SchematicIssueKey) -> SchematicIssueKey {
    let mut key = key.clone();
    match &mut key {
        SchematicIssueKey::UnexpectedNet { items, .. }
        | SchematicIssueKey::UnexpectedConnection { items, .. }
        | SchematicIssueKey::Shorted { items, .. } => items.clear(),
        SchematicIssueKey::MissingSymbol(_)
        | SchematicIssueKey::DuplicateSymbol(_)
        | SchematicIssueKey::MismatchedSymbolId { .. }
        | SchematicIssueKey::UnexpectedSymbol(_)
        | SchematicIssueKey::UnboundSymbol(_)
        | SchematicIssueKey::DisconnectedNet(_)
        | SchematicIssueKey::MissingPort(_) => {}
    }
    key
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
    fn document_patches_apply_exact_page_changes() {
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
        let patch = DocumentPatch {
            edits: document_edits(&before, &after).unwrap(),
        };
        assert_eq!(patch.apply(Some(&before)).unwrap(), after);
    }
}
