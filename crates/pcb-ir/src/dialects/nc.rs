use crate::common::{Point, Unit};

#[derive(Debug, Clone)]
pub struct NcDocument<Symbol = ()> {
    pub unit: Unit,
    pub objects: Vec<NcObject<Symbol>>,
}

impl<Symbol> NcDocument<Symbol> {
    pub fn new(unit: Unit) -> Self {
        Self {
            unit,
            objects: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct NcObject<Symbol = ()> {
    pub geometry: NcGeometry,
    pub plating: NcPlating,
    pub span: NcSpan<Symbol>,
    pub function: NcFunction,
    pub net: Option<Symbol>,
    pub component: Option<Symbol>,
    pub pin: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub enum NcGeometry {
    Drill {
        at: Point,
        diameter: f64,
    },
    Route {
        start: Point,
        diameter: f64,
        segments: Vec<NcRouteSegment>,
    },
}

impl NcGeometry {
    pub fn diameter(&self) -> f64 {
        match self {
            Self::Drill { diameter, .. } | Self::Route { diameter, .. } => *diameter,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum NcRouteSegment {
    Line { to: Point },
    ClockwiseArc { to: Point, radius: f64 },
    CounterClockwiseArc { to: Point, radius: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NcPlating {
    Plated,
    NonPlated,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NcSpan<Symbol = ()> {
    ThroughBoard,
    FromTo {
        from: Option<Symbol>,
        to: Option<Symbol>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NcFunction {
    Via,
    Component,
}
