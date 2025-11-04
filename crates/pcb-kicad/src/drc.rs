use anyhow::{Context, Result};
use pcb_zen_core::diagnostics::{Diagnostic, Diagnostics};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use serde::{Deserialize, Serialize};
use starlark::errors::EvalSeverity;
use std::sync::Arc;

/// KiCad DRC report structure matching the JSON schema
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrcReport {
    pub coordinate_units: String,
    pub date: String,
    pub kicad_version: String,
    pub source: String,
    pub violations: Vec<DrcViolation>,
    pub unconnected_items: Vec<serde_json::Value>,
    pub schematic_parity: Vec<serde_json::Value>,
}

/// A single DRC violation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrcViolation {
    #[serde(rename = "type")]
    pub violation_type: String,
    pub severity: String,
    pub description: String,
    pub items: Vec<DrcItem>,
    /// Whether this violation has been excluded by the user in KiCad
    #[serde(default)]
    pub excluded: bool,
}

/// An item involved in a DRC violation (track, via, pad, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrcItem {
    pub description: String,
    pub pos: DrcPosition,
    pub uuid: String,
}

/// Position of a DRC item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrcPosition {
    pub x: f64,
    pub y: f64,
}

impl DrcReport {
    /// Parse a DRC report from JSON string
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("Failed to parse DRC JSON report")
    }

    /// Parse a DRC report from a JSON file
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let contents =
            std::fs::read_to_string(path.as_ref()).context("Failed to read DRC report file")?;
        Self::from_json(&contents)
    }

    /// Convert the DRC report to diagnostics
    pub fn to_diagnostics(&self, pcb_path: &str) -> Diagnostics {
        let mut diagnostics = Vec::new();

        for violation in &self.violations {
            if let Ok(diagnostic) = violation.to_diagnostic(pcb_path) {
                diagnostics.push(diagnostic);
            }
        }

        Diagnostics { diagnostics }
    }

    /// Check if the report has any violations
    pub fn has_violations(&self) -> bool {
        !self.violations.is_empty()
    }

    /// Check if the report has any error-level violations
    pub fn has_errors(&self) -> bool {
        self.violations.iter().any(|v| v.severity == "error")
    }

    /// Check if the report has any warning-level violations
    pub fn has_warnings(&self) -> bool {
        self.violations.iter().any(|v| v.severity == "warning")
    }

    /// Get count of violations by severity
    pub fn violation_counts(&self) -> (usize, usize) {
        let errors = self
            .violations
            .iter()
            .filter(|v| v.severity == "error")
            .count();
        let warnings = self
            .violations
            .iter()
            .filter(|v| v.severity == "warning")
            .count();
        (errors, warnings)
    }
}

impl DrcViolation {
    /// Convert a DRC violation to a diagnostic
    pub fn to_diagnostic(&self, pcb_path: &str) -> Result<Diagnostic> {
        // Map KiCad DRC violation types to hierarchical diagnostic kinds
        let kind = format!("layout.drc.{}", self.violation_type);

        // Build a detailed message with category prefix and item information
        let mut message = format!("[{}] {}", self.violation_type, self.description);

        // Only add items section if there are items with valid descriptions
        let valid_items: Vec<_> = self
            .items
            .iter()
            .filter(|i| !i.description.is_empty())
            .collect();
        if !valid_items.is_empty() {
            // Add items with consistent 2-space indentation (no "Items:" label)
            for item in valid_items {
                message.push_str(&format!(
                    "\n  - {} at ({:.3}, {:.3})",
                    item.description, item.pos.x, item.pos.y
                ));
            }
        }

        // Map KiCad severity to our severity
        let severity = match self.severity.as_str() {
            "error" => EvalSeverity::Error,
            "warning" => EvalSeverity::Warning,
            _ => EvalSeverity::Warning,
        };

        // Create categorized diagnostic
        let categorized = CategorizedDiagnostic::new(message.clone(), kind.clone())?;

        // Build diagnostic
        Ok(Diagnostic {
            path: pcb_path.to_string(),
            span: None, // PCB files don't have source spans
            severity,
            body: message,
            call_stack: None,
            child: None,
            source_error: Some(Arc::new(anyhow::Error::new(categorized))),
            suppressed: self.excluded, // Map KiCad exclusions to suppressed diagnostics
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DRC_JSON: &str = r#"{
        "$schema": "https://schemas.kicad.org/drc.v1.json",
        "coordinate_units": "mm",
        "date": "2025-11-04T12:48:51-0500",
        "kicad_version": "9.0.5",
        "schematic_parity": [],
        "source": "layout.kicad_pcb",
        "unconnected_items": [],
        "violations": [
            {
                "description": "Clearance violation (netclass '50Ohm SE' clearance 0.2000 mm; actual 0.1510 mm)",
                "items": [
                    {
                        "description": "Track [MCU.QSPI_IO3] on In2.Cu, length 0.9239 mm",
                        "pos": {
                            "x": 137.703288,
                            "y": 105.755
                        },
                        "uuid": "73a755bc-6b87-438e-95c8-24612401333c"
                    },
                    {
                        "description": "Via [MCU.QSPI_CLK] on F.Cu - B.Cu",
                        "pos": {
                            "x": 137.840388,
                            "y": 105.325
                        },
                        "uuid": "cb5a5146-4eb1-449e-997e-a7851cb3090e"
                    }
                ],
                "severity": "error",
                "type": "clearance"
            },
            {
                "description": "Silkscreen overlap (board setup constraints silk clearance 0.1000 mm; actual 0.0950 mm)",
                "items": [
                    {
                        "description": "Segment of R3 on F.Silkscreen",
                        "pos": {
                            "x": 144.603641,
                            "y": 99.675
                        },
                        "uuid": "693af33e-7cbf-40a5-98d4-012b0e622558"
                    }
                ],
                "severity": "warning",
                "type": "silk_overlap"
            }
        ]
    }"#;

    #[test]
    fn test_parse_drc_json() {
        let report = DrcReport::from_json(SAMPLE_DRC_JSON).expect("Failed to parse DRC JSON");

        assert_eq!(report.kicad_version, "9.0.5");
        assert_eq!(report.source, "layout.kicad_pcb");
        assert_eq!(report.violations.len(), 2);

        // Check first violation
        let v1 = &report.violations[0];
        assert_eq!(v1.violation_type, "clearance");
        assert_eq!(v1.severity, "error");
        assert_eq!(v1.items.len(), 2);

        // Check second violation
        let v2 = &report.violations[1];
        assert_eq!(v2.violation_type, "silk_overlap");
        assert_eq!(v2.severity, "warning");
        assert_eq!(v2.items.len(), 1);
    }

    #[test]
    fn test_violation_counts() {
        let report = DrcReport::from_json(SAMPLE_DRC_JSON).unwrap();

        assert!(report.has_violations());
        assert!(report.has_errors());
        assert!(report.has_warnings());

        let (errors, warnings) = report.violation_counts();
        assert_eq!(errors, 1);
        assert_eq!(warnings, 1);
    }

    #[test]
    fn test_to_diagnostics() {
        let report = DrcReport::from_json(SAMPLE_DRC_JSON).unwrap();
        let diagnostics = report.to_diagnostics("layout/layout.kicad_pcb");

        assert_eq!(diagnostics.diagnostics.len(), 2);

        // Check first diagnostic (error)
        let d1 = &diagnostics.diagnostics[0];
        assert!(matches!(d1.severity, EvalSeverity::Error));
        assert_eq!(d1.path, "layout/layout.kicad_pcb");
        assert!(d1.body.contains("Clearance violation"));
        assert!(d1.body.contains("Track [MCU.QSPI_IO3]"));

        // Check second diagnostic (warning)
        let d2 = &diagnostics.diagnostics[1];
        assert!(matches!(d2.severity, EvalSeverity::Warning));
        assert!(d2.body.contains("Silkscreen overlap"));
    }

    #[test]
    fn test_categorized_diagnostic_kind() {
        let report = DrcReport::from_json(SAMPLE_DRC_JSON).unwrap();
        let violation = &report.violations[0];
        let diagnostic = violation.to_diagnostic("test.kicad_pcb").unwrap();

        // Check that the source error is a CategorizedDiagnostic with correct kind
        if let Some(source_error) = &diagnostic.source_error {
            let categorized = source_error.downcast_ref::<CategorizedDiagnostic>();
            assert!(categorized.is_some());
            let categorized = categorized.unwrap();
            assert_eq!(categorized.kind, "layout.drc.clearance");
        } else {
            panic!("Expected source_error to be set");
        }
    }

    #[test]
    fn test_excluded_violation_becomes_suppressed() {
        let json = r#"{
            "$schema": "https://schemas.kicad.org/drc.v1.json",
            "coordinate_units": "mm",
            "date": "2025-11-04T12:48:51-0500",
            "kicad_version": "9.0.5",
            "schematic_parity": [],
            "source": "layout.kicad_pcb",
            "unconnected_items": [],
            "violations": [
                {
                    "description": "Clearance violation (excluded by user)",
                    "items": [],
                    "severity": "error",
                    "type": "clearance",
                    "excluded": true
                },
                {
                    "description": "Clearance violation (not excluded)",
                    "items": [],
                    "severity": "error",
                    "type": "clearance"
                }
            ]
        }"#;

        let report = DrcReport::from_json(json).unwrap();
        assert_eq!(report.violations.len(), 2);

        // Check that excluded field is properly parsed
        assert!(report.violations[0].excluded);
        assert!(!report.violations[1].excluded);

        // Check that excluded violation becomes suppressed diagnostic
        let diagnostics = report.to_diagnostics("test.kicad_pcb");
        assert_eq!(diagnostics.diagnostics.len(), 2);
        assert!(diagnostics.diagnostics[0].suppressed);
        assert!(!diagnostics.diagnostics[1].suppressed);
    }
}
