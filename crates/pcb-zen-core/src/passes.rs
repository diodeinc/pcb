use crate::lang::error::CategorizedDiagnostic;
use crate::{Diagnostic, Diagnostics, DiagnosticsPass, SuppressedDiagnostics};
use starlark::errors::EvalSeverity;
use std::collections::HashMap;
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
    _workspace_root: std::path::PathBuf,
}

impl LspFilterPass {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self {
            _workspace_root: workspace_root,
        }
    }
}

impl DiagnosticsPass for LspFilterPass {
    fn apply(&self, _diagnostics: &mut Diagnostics) {}
}

/// Suppress diagnostics by kind or severity.
/// Special patterns: "warnings", "errors" suppress by severity.
/// Hierarchical matching: "electrical" matches "electrical.voltage_mismatch".
pub struct SuppressPass {
    patterns: Vec<String>,
}

impl SuppressPass {
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }
}

impl DiagnosticsPass for SuppressPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        for diag in &mut diagnostics.diagnostics {
            diag.suppressed |= self.patterns.iter().any(|p| match p.as_str() {
                "warnings" => matches!(diag.severity, EvalSeverity::Warning),
                "errors" => matches!(diag.severity, EvalSeverity::Error),
                _ => {
                    // Check innermost diagnostic for categorization, since that's where
                    // the kind is set (e.g., from warn(kind="bom.match_generic"))
                    let innermost = diag.innermost();
                    innermost
                        .downcast_error_ref::<CategorizedDiagnostic>()
                        .is_some_and(|c| c.kind == *p || c.kind.starts_with(&format!("{p}.")))
                }
            });
        }
    }
}

/// A pass that sorts diagnostics by key for deterministic output.
/// Should be applied before rendering.
pub struct SortPass;

impl DiagnosticsPass for SortPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        diagnostics.diagnostics.sort_by(|a, b| a.cmp_key(b));
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

/// A pass that suppresses diagnostics based on inline `# suppress:` comments in source code.
///
/// Supports two modes:
/// - End-of-line: `code()  # suppress: pattern` (suppresses that line only)
/// - Standalone: `# suppress: pattern` on its own line (suppresses next line)
///
/// Checks all spans in the diagnostic tree (primary span and call stack) for matching patterns.
pub struct CommentSuppressPass {
    source_cache: std::cell::RefCell<SourceCache>,
}

impl CommentSuppressPass {
    pub fn new() -> Self {
        Self {
            source_cache: std::cell::RefCell::new(SourceCache::new()),
        }
    }
}

impl Default for CommentSuppressPass {
    fn default() -> Self {
        Self::new()
    }
}

impl DiagnosticsPass for CommentSuppressPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        let mut cache = self.source_cache.borrow_mut();

        for diag in &mut diagnostics.diagnostics {
            if should_suppress_diagnostic(diag, &mut cache) {
                diag.suppressed = true;
            }
        }
    }
}

/// Source file cache to avoid repeated I/O
struct SourceCache {
    files: HashMap<String, Vec<String>>,
}

impl SourceCache {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
        }
    }

    /// Get line content for a given path and line number (0-indexed)
    fn get_line(&mut self, path: &str, line_number: usize) -> Option<&str> {
        let lines = self.files.entry(path.to_string()).or_insert_with(|| {
            std::fs::read_to_string(path)
                .ok()
                .map(|content| content.lines().map(String::from).collect())
                .unwrap_or_default()
        });

        lines.get(line_number).map(|s| s.as_str())
    }
}

/// Check if a diagnostic should be suppressed based on inline comments
fn should_suppress_diagnostic(diagnostic: &Diagnostic, cache: &mut SourceCache) -> bool {
    // Walk entire diagnostic tree
    let mut current = Some(diagnostic);
    while let Some(diag) = current {
        // Check primary span
        if let Some(span) = &diag.span
            && check_span_for_suppression(diag, cache, &diag.path, span.begin.line)
        {
            return true;
        }

        // Check call stack frames
        if let Some(call_stack) = &diag.call_stack {
            for frame in &call_stack.frames {
                if let Some(loc) = &frame.location {
                    let span = loc.resolve_span();
                    if check_span_for_suppression(diag, cache, loc.file.filename(), span.begin.line)
                    {
                        return true;
                    }
                }
            }
        }

        current = diag.child.as_deref();
    }

    false
}

/// Check if a span (and optionally its previous line) has a suppression comment
fn check_span_for_suppression(
    diag: &Diagnostic,
    cache: &mut SourceCache,
    path: &str,
    line: usize,
) -> bool {
    // Check current line
    if has_matching_suppression(diag, cache, path, line) {
        return true;
    }

    // Check previous line (only if it's a standalone comment)
    line > 0
        && is_standalone_suppress_comment(cache, path, line - 1)
        && has_matching_suppression(diag, cache, path, line - 1)
}

/// Check if a line has suppression patterns that match the diagnostic
fn has_matching_suppression(
    diag: &Diagnostic,
    cache: &mut SourceCache,
    path: &str,
    line: usize,
) -> bool {
    extract_suppress_patterns(cache, path, line)
        .is_some_and(|patterns| patterns.iter().any(|p| matches_pattern(diag, p)))
}

/// Check if a line contains only a standalone suppress comment (no code before the comment)
fn is_standalone_suppress_comment(cache: &mut SourceCache, path: &str, line_number: usize) -> bool {
    cache.get_line(path, line_number).is_some_and(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with('#') && trimmed.to_lowercase().contains("suppress:")
    })
}

/// Extract suppression patterns from a source line
fn extract_suppress_patterns(
    cache: &mut SourceCache,
    path: &str,
    line_number: usize,
) -> Option<Vec<String>> {
    let line = cache.get_line(path, line_number)?;
    let line_lower = line.to_lowercase();

    // Find "suppress:" marker (with or without space after #)
    let suppress_idx = line_lower
        .find("# suppress:")
        .or_else(|| line_lower.find("#suppress:"))?;

    // Extract text after "suppress:" until end or next comment
    let marker_len = if line_lower[suppress_idx..].starts_with("# suppress:") {
        "# suppress:".len()
    } else {
        "#suppress:".len()
    };

    let pattern_text = line[suppress_idx + marker_len..].split('#').next()?.trim();

    // Parse comma-separated patterns
    let patterns: Vec<String> = pattern_text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    (!patterns.is_empty()).then_some(patterns)
}

/// Check if a pattern matches a diagnostic
fn matches_pattern(diagnostic: &Diagnostic, pattern: &str) -> bool {
    let pattern_lower = pattern.to_lowercase();

    match pattern_lower.as_str() {
        "all" => true,
        "warnings" => matches!(diagnostic.severity, EvalSeverity::Warning),
        "errors" => matches!(diagnostic.severity, EvalSeverity::Error),
        _ => {
            // Check if diagnostic has a categorized kind
            diagnostic
                .innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()
                .is_some_and(|c| c.kind == pattern || c.kind.starts_with(&format!("{pattern}.")))
        }
    }
}

/// A pass that promotes diagnostics matching specified patterns from Advice to Warning severity.
///
/// This is useful for:
/// - `-W style` in CLI to promote style hints to warnings (visible in output)
/// - CI pipelines that want to enforce style conventions
///
/// Patterns work hierarchically: "style" matches "style.naming.io", "style.naming.config", etc.
pub struct PromotePass {
    patterns: Vec<String>,
}

impl PromotePass {
    pub fn new(patterns: Vec<String>) -> Self {
        Self { patterns }
    }
}

impl DiagnosticsPass for PromotePass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        for diag in &mut diagnostics.diagnostics {
            // Only promote Advice severity diagnostics
            if !matches!(diag.severity, EvalSeverity::Advice) {
                continue;
            }

            let should_promote = self.patterns.iter().any(|p| {
                // Check innermost diagnostic for categorization
                diag.innermost()
                    .downcast_error_ref::<CategorizedDiagnostic>()
                    .is_some_and(|c| c.kind == *p || c.kind.starts_with(&format!("{p}.")))
            });

            if should_promote {
                diag.severity = EvalSeverity::Warning;
            }
        }
    }
}

/// A pass that promotes all style-related diagnostics from Advice to Warning severity.
///
/// This is specifically for LSP use where we want style hints to be more visible.
/// Style diagnostics have kinds starting with "style." (e.g., "style.naming.io").
pub struct StylePromotePass;

impl DiagnosticsPass for StylePromotePass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        for diag in &mut diagnostics.diagnostics {
            // Only promote Advice severity diagnostics
            if !matches!(diag.severity, EvalSeverity::Advice) {
                continue;
            }

            let is_style = diag
                .innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()
                .is_some_and(|c| c.kind.starts_with("style.") || c.kind == "style");

            if is_style {
                diag.severity = EvalSeverity::Warning;
            }
        }
    }
}

/// A pass that exports diagnostics to JSON file
pub struct JsonExportPass {
    output_path: std::path::PathBuf,
    source_file: String,
}

impl JsonExportPass {
    pub fn new(output_path: std::path::PathBuf, source_file: String) -> Self {
        Self {
            output_path,
            source_file,
        }
    }
}

impl DiagnosticsPass for JsonExportPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        let report = crate::DiagnosticsReport::from_diagnostics(diagnostics, &self.source_file);

        let json = match serde_json::to_string_pretty(&report) {
            Ok(json) => json,
            Err(e) => {
                eprintln!("Failed to serialize diagnostics: {}", e);
                return;
            }
        };

        if let Err(e) = std::fs::write(&self.output_path, &json) {
            eprintln!(
                "Failed to write diagnostics to {}: {}",
                self.output_path.display(),
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Diagnostic;
    use starlark::errors::EvalSeverity;
    use std::path::Path;

    #[test]
    fn test_suppress_pass_checks_innermost_diagnostic() {
        // Create an innermost diagnostic with a categorized kind
        let innermost = Diagnostic::new(
            "No house cap for 10uF 1206",
            EvalSeverity::Warning,
            Path::new("test.zen"),
        )
        .with_source_error(Some(
            crate::lang::error::CategorizedDiagnostic::new(
                "No house cap for 10uF 1206".to_string(),
                "bom.match_generic".to_string(),
            )
            .unwrap(),
        ));

        // Wrap it in a parent diagnostic (simulating module boundaries)
        let parent = Diagnostic::new(
            "Warning from `Capacitor`",
            EvalSeverity::Warning,
            Path::new("test.zen"),
        )
        .with_child(innermost.boxed());

        let mut diagnostics = Diagnostics {
            diagnostics: vec![parent],
        };

        // Apply suppress pass with "bom.match_generic"
        let suppress_pass = SuppressPass::new(vec!["bom.match_generic".to_string()]);
        suppress_pass.apply(&mut diagnostics);

        // The diagnostic should be suppressed
        assert!(diagnostics.diagnostics[0].suppressed);
    }

    #[test]
    fn test_suppress_pass_hierarchical_matching() {
        // Create a diagnostic with "bom.match_generic" kind
        let diag = Diagnostic::new("No house cap", EvalSeverity::Warning, Path::new("test.zen"))
            .with_source_error(Some(
                crate::lang::error::CategorizedDiagnostic::new(
                    "No house cap".to_string(),
                    "bom.match_generic".to_string(),
                )
                .unwrap(),
            ));

        let mut diagnostics = Diagnostics {
            diagnostics: vec![diag],
        };

        // Apply suppress pass with parent kind "bom" (should match "bom.match_generic")
        let suppress_pass = SuppressPass::new(vec!["bom".to_string()]);
        suppress_pass.apply(&mut diagnostics);

        // The diagnostic should be suppressed via hierarchical matching
        assert!(diagnostics.diagnostics[0].suppressed);
    }
}
