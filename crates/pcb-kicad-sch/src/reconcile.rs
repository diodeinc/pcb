//! Pure reconciliation shared by interactive editors and filesystem adapters.

use anyhow::{Context, Result, bail};
use pcb_sch::Schematic;

use crate::{
    SchDocument, SchPage,
    analysis::{ConnectivityAnalysis, analyze_schematic},
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
    analysis_before: InitialAnalysis,
    analysis_after: ConnectivityAnalysis,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialAnalysis {
    NoDocument,
    Available(ConnectivityAnalysis),
    Invalid { message: String },
}

impl ReconciliationPlan {
    pub fn edits(&self) -> &[DocumentEdit] {
        &self.edits
    }

    pub fn analysis_before(&self) -> &InitialAnalysis {
        &self.analysis_before
    }

    pub fn analysis_after(&self) -> &ConnectivityAnalysis {
        &self.analysis_after
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Apply this plan to the exact document from which it was created.
    pub fn apply(&self, document: Option<&SchDocument>) -> Result<SchDocument> {
        apply_document_edits(document.unwrap_or(&SchDocument::default()), &self.edits)
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
    let analysis_before = match document {
        None => InitialAnalysis::NoDocument,
        Some(document) => match analyze_schematic(document, netlist) {
            Ok(analysis) => InitialAnalysis::Available(analysis),
            Err(error) => InitialAnalysis::Invalid {
                message: format!("{error:#}"),
            },
        },
    };
    let desired = compose::reconcile_document(document, netlist, root_file_name)?;
    let analysis_after = analyze_schematic(&desired, netlist)?;
    if !analysis_after.is_equivalent() {
        bail!(
            "planned schematic is not netlist-equivalent: {:#?}",
            analysis_after.issues()
        );
    }
    let edits = document_edits(document.unwrap_or(&SchDocument::default()), &desired)?;
    let plan = ReconciliationPlan {
        edits,
        analysis_before,
        analysis_after,
    };
    let applied = plan.apply(document)?;
    if applied != desired {
        bail!("reconciliation plan does not reproduce its verified document");
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
        assert_eq!(apply_document_edits(&before, &edits).unwrap(), after);
    }
}
