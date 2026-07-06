//! The shared geometry substrate for all pcb-ir dialects.
//!
//! All geometry is in millimeters. Unit conversion belongs at format
//! boundaries (parsers and writers); see [`Unit`].

mod affine;
mod arc;
pub mod arcfit;
mod bbox;
pub mod bridge;
pub mod dfm;
pub mod path;
mod point;
pub mod region;
pub mod shapes;
mod store;
mod style;
pub mod tol;

pub use affine::Affine2;
pub use arc::Arc;
pub use bbox::BBox;
pub use path::{ContourBuf, PathCmd, PathOp, Segment, StrokeToFillStyle};
pub use point::{Mirror, Point};
pub use region::{ContourSet, PaintComposer, Ring};
pub(crate) use store::validate_bbox;
pub use store::{Contour, Path, PathArena, Span};
pub use style::{
    FillRule, LineCap, LineJoin, LinePattern, Paint, PaintKind, Polarity, StrokeStyle,
};

/// Measurement unit at a format boundary. All pcb-ir geometry is canonically
/// millimeters; use these conversions when parsing or serializing formats
/// that speak other units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    Millimeter,
    Inch,
}

impl Unit {
    pub const MM_PER_INCH: f64 = 25.4;

    pub fn to_mm(self, value: f64) -> f64 {
        match self {
            Self::Millimeter => value,
            Self::Inch => value * Self::MM_PER_INCH,
        }
    }

    pub fn from_mm(self, value: f64) -> f64 {
        match self {
            Self::Millimeter => value,
            Self::Inch => value / Self::MM_PER_INCH,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A non-fatal geometry problem collected while processing a document.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.severity {
            Severity::Warning => write!(f, "warning: {}", self.message),
            Severity::Error => write!(f, "error: {}", self.message),
        }
    }
}

/// A collection of validation problems.
///
/// Validation collects every problem it finds instead of stopping at the
/// first; the collection is the error type of `validate()` functions.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics(pub Vec<Diagnostic>);

impl Diagnostics {
    pub fn error(&mut self, message: impl Into<String>) {
        self.0.push(Diagnostic::error(message));
    }

    pub fn warning(&mut self, message: impl Into<String>) {
        self.0.push(Diagnostic::warning(message));
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `Ok` when no diagnostics were collected, otherwise `Err(self)`.
    pub fn into_result(self) -> Result<(), Diagnostics> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl std::fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, diagnostic) in self.0.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}
