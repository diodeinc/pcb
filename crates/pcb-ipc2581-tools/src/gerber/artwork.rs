use ipc2581::types::{LayerFunction, Side};

use crate::geometry::ir::{LineCap, Point};

#[derive(Debug, Clone)]
pub struct ArtworkLayer {
    pub function: LayerFunction,
    pub side: Option<Side>,
    pub objects: Vec<ArtworkObject>,
}

#[derive(Debug, Clone)]
pub enum ArtworkObject {
    Region {
        contours: Vec<ArtworkContour>,
    },
    Stroke {
        width: f64,
        line_cap: LineCap,
        contours: Vec<ArtworkContour>,
    },
}

#[derive(Debug, Clone)]
pub struct ArtworkContour {
    pub segments: Vec<ArtworkSegment>,
}

#[derive(Debug, Clone, Copy)]
pub enum ArtworkSegment {
    Line {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        end: Point,
        center: Point,
        clockwise: bool,
    },
}
