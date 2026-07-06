use pcb_ir::geom::Unit;
use serde::{Deserialize, Serialize};

use crate::UnitFormat;

/// A length/distance value stored in millimeters (canonical unit for PCB dimensions)
///
/// This type is unit-agnostic - it represents a physical length.
/// Internally stored as mm since that's what IPC-2581 uses.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Length(pub f64);

impl Length {
    /// Create from millimeters (canonical storage unit)
    pub const fn from_mm(mm: f64) -> Self {
        Self(mm)
    }

    /// Get value in millimeters (canonical unit)
    pub fn mm(&self) -> f64 {
        self.0
    }

    /// Get value in inches
    pub fn inch(&self) -> f64 {
        Unit::Inch.from_mm(self.0)
    }

    /// Get value in mils (thousandths of an inch)
    pub fn mil(&self) -> f64 {
        Unit::Inch.from_mm(self.0) * 1000.0
    }
}

impl From<f64> for Length {
    /// Convert from millimeters (assumes input is in mm)
    fn from(mm: f64) -> Self {
        Self(mm)
    }
}

/// Convert millimeters to the requested unit format
pub fn convert_mm(mm: f64, format: UnitFormat) -> String {
    match format {
        UnitFormat::Mm => format!("{:.2}mm", mm),
        UnitFormat::Mil => format!("{:.1}mil", Length(mm).mil()),
        UnitFormat::Inch => format!("{:.4}in", Length(mm).inch()),
    }
}

/// Format board dimensions (width x height)
pub fn format_board_size(width_mm: f64, height_mm: f64, format: UnitFormat) -> String {
    match format {
        UnitFormat::Mm => format!("{:.2}mm × {:.2}mm", width_mm, height_mm),
        UnitFormat::Mil => format!(
            "{:.1}mil × {:.1}mil",
            Length(width_mm).mil(),
            Length(height_mm).mil()
        ),
        UnitFormat::Inch => format!(
            "{:.4}in × {:.4}in",
            Length(width_mm).inch(),
            Length(height_mm).inch()
        ),
    }
}
