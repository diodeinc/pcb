use crate::{Diagnostic, Diagnostics, DiagnosticsPass, SuppressedDiagnostics};
use starlark::errors::EvalSeverity;
use std::sync::Arc;

/// A pass that promotes diagnostics based on deny rules
pub struct PromoteDeniedPass {
    deny_warnings: bool,
}

impl PromoteDeniedPass {
    pub fn new(deny: &[String]) -> Self {
        Self {
            deny_warnings: deny.contains(&"warnings".to_string()),
        }
    }
}

impl DiagnosticsPass for PromoteDeniedPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        if self.deny_warnings {
            for diagnostic in &mut diagnostics.diagnostics {
                promote_diagnostic_to_error(diagnostic);
            }
        }
    }
}

/// A pass that filters out diagnostics based on certain criteria
pub struct FilterPass;

impl DiagnosticsPass for FilterPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        diagnostics.diagnostics.retain(|diag| {
            // Filter out hidden diagnostics
            !diag.body.contains("<hidden>")
        });
    }
}

/// A pass that sorts diagnostics by severity (warnings first, then errors) while maintaining stability
pub struct SortPass;

impl DiagnosticsPass for SortPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        diagnostics
            .diagnostics
            .sort_by_key(|diag| severity_sort_order(diag.severity));
    }
}

/// A pass that aggregates similar warnings by combining them into a single representative warning
pub struct AggregatePass;

impl DiagnosticsPass for AggregatePass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        let mut result = Vec::new();

        for diagnostic in &diagnostics.diagnostics {
            // Only aggregate warnings
            if !matches!(diagnostic.severity, EvalSeverity::Warning) {
                result.push(diagnostic.clone());
                continue;
            }

            let innermost = get_innermost_diagnostic(diagnostic);
            let key = (&innermost.body, &innermost.path, &innermost.span);

            // Check if we already have a similar warning
            if let Some(existing) = result.iter_mut().find(|d| {
                matches!(d.severity, EvalSeverity::Warning) && {
                    let existing_innermost = get_innermost_diagnostic(d);
                    (
                        &existing_innermost.body,
                        &existing_innermost.path,
                        &existing_innermost.span,
                    ) == key
                }
            }) {
                // Add to suppressed list
                let suppressed = existing
                    .downcast_error_ref::<SuppressedDiagnostics>()
                    .map(|s| s.suppressed.clone())
                    .unwrap_or_default();

                let mut new_suppressed = suppressed;
                new_suppressed.push(diagnostic.clone());

                let suppressed_error = SuppressedDiagnostics {
                    suppressed: new_suppressed,
                };
                existing.source_error = Some(Arc::new(suppressed_error.into()));
            } else {
                // First occurrence, add as-is
                result.push(diagnostic.clone());
            }
        }

        diagnostics.diagnostics = result;
    }
}

/// Recursively promote a diagnostic and all its children to error severity
fn promote_diagnostic_to_error(diagnostic: &mut Diagnostic) {
    if matches!(diagnostic.severity, EvalSeverity::Warning) {
        diagnostic.severity = EvalSeverity::Error;
    }
    if let Some(ref mut child) = diagnostic.child {
        promote_diagnostic_to_error(child);
    }
}

/// Get the innermost diagnostic in a chain (follows child pointers to the end)
fn get_innermost_diagnostic(diagnostic: &Diagnostic) -> &Diagnostic {
    let mut current = diagnostic;
    while let Some(ref child) = current.child {
        current = child;
    }
    current
}

/// Return sort order for severity (lower numbers come first)
fn severity_sort_order(severity: EvalSeverity) -> u8 {
    match severity {
        EvalSeverity::Warning => 0,
        EvalSeverity::Error => 1,
        EvalSeverity::Advice => 2,
        EvalSeverity::Disabled => 3,
    }
}
