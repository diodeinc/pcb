use crate::geometry::ir::Point;

#[derive(Debug, Clone)]
pub struct ArtworkLayer {
    pub file_function: Vec<String>,
    pub objects: Vec<ArtworkObject>,
}

#[derive(Debug, Clone)]
pub enum ArtworkObject {
    Region {
        contours: Vec<ArtworkContour>,
        attributes: ObjectAttributes,
    },
    Stroke {
        width: f64,
        contours: Vec<ArtworkContour>,
        aperture_function: String,
        attributes: ObjectAttributes,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ObjectAttributes {
    pub net: Option<String>,
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
