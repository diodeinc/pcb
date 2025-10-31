use crate::{Diagnostics, DiagnosticsPass, SuppressedDiagnostics};
use starlark::errors::EvalSeverity;
use std::path::Path;
use std::sync::Arc;

/// A pass that filters out hidden diagnostics (containing "<hidden>")
pub struct FilterHiddenPass;

impl DiagnosticsPass for FilterHiddenPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        diagnostics.diagnostics.retain(|diag| {
            // Filter out hidden diagnostics
            !diag.body.contains("<hidden>")
        });
    }
}

/// A pass that filters out diagnostics that are too noisy for LSP/editor display
pub struct LspFilterPass {
    workspace_root: std::path::PathBuf,
}

impl LspFilterPass {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

impl DiagnosticsPass for LspFilterPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        let vendor_dir = self.workspace_root.join("vendor");

        diagnostics.diagnostics.retain(|diag| {
            let innermost = diag.innermost();

            // Check if innermost has unstable ref error and is external
            innermost
                .downcast_error_ref::<crate::UnstableRefError>()
                .map(|_| {
                    let path = Path::new(&innermost.path);
                    path.starts_with(&self.workspace_root) && !path.starts_with(&vendor_dir)
                })
                .unwrap_or(true) // Keep non-unstable-ref diagnostics
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

            let innermost = diagnostic.innermost();
            let key = (&innermost.body, &innermost.path, &innermost.span);

            // Check if we already have a similar warning
            if let Some(existing) = result.iter_mut().find(|d| {
                matches!(d.severity, EvalSeverity::Warning) && {
                    let existing_innermost = d.innermost();
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

/// Return sort order for severity (lower numbers come first)
fn severity_sort_order(severity: EvalSeverity) -> u8 {
    match severity {
        EvalSeverity::Warning => 0,
        EvalSeverity::Error => 1,
        EvalSeverity::Advice => 2,
        EvalSeverity::Disabled => 3,
    }
}
