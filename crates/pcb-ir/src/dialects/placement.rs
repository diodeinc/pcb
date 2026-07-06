//! Component placement data (pick-and-place).

use crate::geom::Point;

#[derive(Debug, Clone, Default)]
pub struct Document {
    pub components: Vec<Placement>,
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub designator: String,
    pub value: Option<String>,
    pub package: Option<String>,
    pub part: String,
    pub layer_ref: String,
    pub side: PlacementSide,
    pub mount: PlacementMount,
    pub at: Point,
    pub rotation_degrees: f64,
    pub x_offset: f64,
    pub y_offset: f64,
    pub mirror: bool,
    pub face_up: bool,
    pub scale: f64,
    pub populate: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlacementSide {
    Top,
    Bottom,
    Internal,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlacementMount {
    Smt,
    ThroughHole,
    Embedded,
    PressFit,
    WireBonded,
    Glued,
    Clamped,
    Socketed,
    Formed,
    Other,
}
