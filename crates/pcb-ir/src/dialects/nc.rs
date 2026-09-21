//! Numerically controlled drill operations (millimeters): round holes and
//! straight slots.

use crate::geom::Point;

#[derive(Debug, Clone, Default)]
pub struct Document<Symbol = ()> {
    pub objects: Vec<Object<Symbol>>,
}

impl<Symbol> Document<Symbol> {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Object<Symbol = ()> {
    pub geometry: Geometry,
    pub plating: Plating,
    pub span: DrillSpan<Symbol>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Plating {
    Plated,
    NonPlated,
}

/// Which layers an operation spans through the stackup.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DrillSpan<Symbol = ()> {
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
