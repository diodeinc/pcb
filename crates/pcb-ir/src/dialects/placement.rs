use crate::common::Point;

#[derive(Debug, Clone, Default)]
pub struct PlacementDocument {
    pub components: Vec<ComponentPlacement>,
}

#[derive(Debug, Clone)]
pub struct ComponentPlacement {
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

impl PlacementSide {
    pub fn as_cpl_layer(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Internal => "internal",
            Self::Unknown => "unknown",
        }
    }
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
