use anyhow::{Context, Result};
use pcb_zen_core::diagnostics::{Diagnostic, Diagnostics};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use serde::{Deserialize, Serialize};
use starlark::errors::EvalSeverity;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Deserialize, Serialize)]
pub struct SyncReport {
    pub source: PathBuf,
    pub netlist_source: PathBuf,
    pub timestamp: String,
    pub change_count: usize,
    pub changes: Vec<SyncChange>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SyncChange {
    pub change_type: String,
    pub severity: String,
    pub category: String,
    pub description: String,
    pub items: Vec<SyncChangeItem>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SyncChangeItem {
    pub description: String,
    pub uuid: Option<String>,
    pub hierarchical_path: Option<String>,
    pub pos: Option<Position>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

impl SyncReport {
    pub fn from_json_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read sync changes JSON from {}", path.display()))?;
        let report: SyncReport = serde_json::from_str(&content).with_context(|| {
            format!("Failed to parse sync changes JSON from {}", path.display())
        })?;
        Ok(report)
    }

    pub fn to_diagnostics(&self) -> Result<Diagnostics> {
        let mut diagnostics = Vec::new();

        for change in &self.changes {
            diagnostics.push(change.to_diagnostic(&self.source)?);
        }

        Ok(Diagnostics { diagnostics })
    }
}

impl SyncChange {
    pub fn to_diagnostic(&self, source_path: &Path) -> Result<Diagnostic> {
        // Kind: layout.sync.{change_type} (category is ignored if empty)
        let kind = if self.category.is_empty() {
            format!("layout.sync.{}", self.change_type)
        } else {
            format!("layout.sync.{}.{}", self.category, self.change_type)
        };

        // Format message with items (match DRC format - no prefix in brackets)
        let mut body = format!("[{}] {}", self.change_type, self.description);
        for item in &self.items {
            body.push_str("\n  ");
            body.push_str(&item.description);
            if let Some(pos) = &item.pos {
                body.push_str(&format!(" at ({:.3}, {:.3}) mm", pos.x, pos.y));
            }
        }

        // Map severity
        let severity = match self.severity.as_str() {
            "error" => EvalSeverity::Error,
            "warning" => EvalSeverity::Warning,
            _ => EvalSeverity::Warning,
        };

        Ok(Diagnostic {
            path: source_path.display().to_string(),
            span: None,
            severity,
            body,
            call_stack: None,
            child: None,
            source_error: Some(Arc::new(
                CategorizedDiagnostic::new(self.description.clone(), kind)
                    .context("Failed to create categorized diagnostic")?
                    .into(),
            )),
            suppressed: false,
        })
    }
}
