use anyhow::{Context, Result};
use log::warn;
use pcb_zen_core::diagnostics::{Diagnostic, Diagnostics};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use serde::{Deserialize, Serialize};
use starlark::errors::EvalSeverity;
use std::sync::Arc;

/// KiCad ERC report structure matching the JSON schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErcReport {
    pub coordinate_units: String,
    pub date: String,
    pub kicad_version: String,
    pub source: String,
    #[serde(default)]
    pub sheets: Vec<ErcSheet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErcSheet {
    pub path: String,
    pub uuid_path: String,
    pub violations: Vec<ErcViolation>,
}

/// A single ERC violation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErcViolation {
    #[serde(rename = "type")]
    pub violation_type: String,
    pub severity: String,
    pub description: String,
    pub items: Vec<ErcItem>,
    /// Whether this violation has been excluded by the user in KiCad
    #[serde(default)]
    pub excluded: bool,
    /// Optional user comment (present in some KiCad versions)
    #[serde(default)]
    pub comment: Option<String>,
}

/// An item involved in an ERC violation (symbol pin, net label, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErcItem {
    pub description: String,
    #[serde(default)]
    pub pos: Option<ErcPosition>,
    pub uuid: String,
}

/// Position of an ERC item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErcPosition {
    pub x: f64,
    pub y: f64,
}

impl ErcReport {
    /// Parse an ERC report from JSON string
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("Failed to parse ERC JSON report")
    }

    /// Parse an ERC report from a JSON file
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path.as_ref()).context("Failed to read ERC report file")?;
        Self::from_json(&contents)
    }

    /// Add ERC violations to an existing diagnostics list
    pub fn add_to_diagnostics(&self, diagnostics: &mut Diagnostics, sch_path: &str) {
        for sheet in &self.sheets {
            for violation in &sheet.violations {
                match violation.to_diagnostic(sch_path, &sheet.path) {
                    Ok(diagnostic) => diagnostics.diagnostics.push(diagnostic),
                    Err(e) => {
                        warn!("Failed to convert ERC violation to diagnostic: {}", e);
                    }
                }
            }
        }
    }

    /// Convert the ERC report to diagnostics
    pub fn to_diagnostics(&self, sch_path: &str) -> Diagnostics {
        let mut diagnostics = Diagnostics::default();
        self.add_to_diagnostics(&mut diagnostics, sch_path);
        diagnostics
    }
}

impl ErcViolation {
    /// Convert an ERC violation to a diagnostic
    pub fn to_diagnostic(&self, sch_path: &str, sheet_path: &str) -> Result<Diagnostic> {
        let kind = format!("schematic.erc.{}", self.violation_type);

        let mut message = format!("[{}] {}", self.violation_type, self.description);
        if !sheet_path.is_empty() {
            message.push_str(&format!("\n  Sheet: {}", sheet_path));
        }
        if let Some(comment) = &self.comment {
            if !comment.trim().is_empty() {
                message.push_str(&format!("\n  Comment: {}", comment.trim()));
            }
        }

        let valid_items: Vec<_> = self
            .items
            .iter()
            .filter(|i| !i.description.is_empty())
            .collect();
        if !valid_items.is_empty() {
            for item in valid_items {
                if let Some(pos) = &item.pos {
                    message.push_str(&format!(
                        "\n  - {} at ({:.3}, {:.3})",
                        item.description, pos.x, pos.y
                    ));
                } else {
                    message.push_str(&format!("\n  - {}", item.description));
                }
            }
        }

        let severity = match self.severity.as_str() {
            "error" => EvalSeverity::Error,
            "warning" => EvalSeverity::Warning,
            _ => EvalSeverity::Warning,
        };

        let categorized = CategorizedDiagnostic::new(message.clone(), kind.clone())?;

        Ok(Diagnostic {
            path: sch_path.to_string(),
            span: None,
            severity,
            body: message,
            call_stack: None,
            child: None,
            source_error: Some(Arc::new(anyhow::Error::new(categorized))),
            suppressed: self.excluded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_ERC_JSON: &str = r#"{
        "$schema": "https://schemas.kicad.org/erc.v1.json",
        "coordinate_units": "mm",
        "date": "2026-01-01T00:00:00Z",
        "kicad_version": "9.0.0",
        "source": "example.kicad_sch",
        "sheets": [
            {
                "path": "/",
                "uuid_path": "00000000-0000-0000-0000-000000000000",
                "violations": [
                    {
                        "type": "pin_not_connected",
                        "severity": "warning",
                        "description": "Pin not connected",
                        "items": [
                            {
                                "description": "U1 pin 1",
                                "pos": { "x": 10.0, "y": 20.0 },
                                "uuid": "11111111-1111-1111-1111-111111111111"
                            }
                        ]
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn test_parse_erc_json() {
        let report = ErcReport::from_json(SAMPLE_ERC_JSON).expect("Failed to parse ERC JSON");
        assert_eq!(report.kicad_version, "9.0.0");
        assert_eq!(report.sheets.len(), 1);
        assert_eq!(report.sheets[0].violations.len(), 1);
        assert_eq!(
            report.sheets[0].violations[0].violation_type,
            "pin_not_connected"
        );
    }

    #[test]
    fn test_to_diagnostics() {
        let report = ErcReport::from_json(SAMPLE_ERC_JSON).unwrap();
        let diagnostics = report.to_diagnostics("example.kicad_sch");
        assert_eq!(diagnostics.diagnostics.len(), 1);
        let d = &diagnostics.diagnostics[0];
        assert!(matches!(d.severity, EvalSeverity::Warning));
        assert!(d.body.contains("Pin not connected"));
    }
}
