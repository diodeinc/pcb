//! Numerically controlled drill operations (millimeters): round holes and
//! straight slots.

use crate::dialects::artwork;
use crate::geom::{Affine2, ContourBuf, LineCap, Paint, PathCmd, Point, StrokeStyle};
use ipc2581::Symbol;

#[derive(Debug, Clone, Default)]
pub struct Document {
    pub objects: Vec<Object>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Object {
    pub geometry: Geometry,
    pub plating: Plating,
    pub span: DrillSpan,
    pub function: Function,
    pub net: Option<Symbol>,
    pub component: Option<Symbol>,
    pub pin: Option<Symbol>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Drill {
        at: Point,
        diameter: f64,
    },
    Slot {
        start: Point,
        end: Point,
        diameter: f64,
    },
}

impl Geometry {
    pub fn diameter(&self) -> f64 {
        match self {
            Self::Drill { diameter, .. } | Self::Slot { diameter, .. } => *diameter,
        }
    }

    /// The cut as artwork in `doc`'s tables: a round tool's flash, or its
    /// stroke from one end of a slot to the other.
    pub fn to_artwork<LayerMeta, ObjectMeta>(
        &self,
        doc: &mut artwork::Document<LayerMeta, ObjectMeta>,
    ) -> artwork::Geometry {
        match *self {
            Self::Drill { at, diameter } => artwork::Geometry::Flash {
                aperture: doc.push_aperture(artwork::Aperture::circle(diameter)),
                transform: Affine2::translation(at),
            },
            Self::Slot {
                start,
                end,
                diameter,
            } => {
                let tool = Paint::Stroke(StrokeStyle::new(diameter, LineCap::Round));
                let centerline =
                    ContourBuf::new(vec![PathCmd::move_to(start), PathCmd::line_to(end)]);
                artwork::Geometry::Stroke {
                    path: doc.push_path(tool, [centerline]),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Plating {
    Plated,
    NonPlated,
}

/// Which layers an operation spans through the stackup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DrillSpan {
    ThroughBoard,
    FromTo {
        from: Option<Symbol>,
        to: Option<Symbol>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Function {
    Via,
    Component,
}
