use crate::{Polarity, Symbol};
use serde::{Deserialize, Serialize};

/// A feature after transformation resolution (Stage 1 output)
///
/// This represents a flattened, transformed feature ready for geometry conversion.
/// All Location offsets and Xform transformations have been applied.
#[derive(Debug, Clone)]
pub struct ResolvedFeature {
    /// Classification bucket for this feature
    pub bucket: FeatureBucket,

    /// Net name symbol (if electrical feature) - use Ipc2581::resolve() to get string
    pub net: Option<Symbol>,

    /// Polarity (add or remove copper)
    pub polarity: Polarity,

    /// Geometry specification
    pub geometry: ResolvedGeometry,

    /// Final bounding box (after transforms)
    pub bbox: BoundingBox,
}

/// Feature classification buckets for styling and organization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureBucket {
    /// SMD pads (surface mount)
    Smd,
    /// Through-hole pads (component leads, platingStatus=PLATED)
    Pth,
    /// Via pads (plated vias, platingStatus=VIA)
    Via,
    /// Copper traces (Polyline)
    Trace,
    /// Copper pours (filled polygons)
    Fill,
    /// Cutouts (negative geometry)
    Cutout,
    /// Thermal relief patterns
    Thermal,
    /// Antipads (clearances in planes)
    Antipad,
}

/// Resolved geometry after transformation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedGeometry {
    /// Polyline trace with absolute coordinates
    Polyline {
        points: Vec<Point>,
        line_width: f64,
        line_end: LineEndStyle,
    },

    /// Filled polygon
    Polygon {
        points: Vec<Point>,
        has_curves: bool,
    },

    /// Circle (pad or via)
    Circle {
        center: Point,
        diameter: f64,
        filled: bool,
    },

    /// Rectangle
    Rectangle {
        center: Point,
        width: f64,
        height: f64,
        filled: bool,
    },

    /// Padstack reference (to be expanded in Stage 2)
    PadstackRef {
        padstack_name: String,
        center: Point,
        rotation: f64,
        layer: String,
        /// Inline primitive override (takes precedence over padstack)
        inline_standard_primitive: Option<String>,
        /// Inline user primitive override (takes precedence over padstack)
        inline_user_primitive: Option<String>,
    },
}

/// Point in 2D space (millimeters)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Apply rotation around origin (degrees, counter-clockwise)
    pub fn rotate(&self, degrees: f64) -> Self {
        let rad = degrees.to_radians();
        let cos = rad.cos();
        let sin = rad.sin();
        Self {
            x: self.x * cos - self.y * sin,
            y: self.x * sin + self.y * cos,
        }
    }

    /// Apply mirror (flip across y-axis: x → -x)
    pub fn mirror(&self) -> Self {
        Self {
            x: -self.x,
            y: self.y,
        }
    }

    /// Apply scale
    pub fn scale(&self, factor: f64) -> Self {
        Self {
            x: self.x * factor,
            y: self.y * factor,
        }
    }

    /// Apply offset
    pub fn translate(&self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

/// Line end style for trace caps
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineEndStyle {
    Round,
    Square,
    None,
}

/// Axis-aligned bounding box
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl BoundingBox {
    pub fn empty() -> Self {
        Self {
            min_x: f64::INFINITY,
            min_y: f64::INFINITY,
            max_x: f64::NEG_INFINITY,
            max_y: f64::NEG_INFINITY,
        }
    }

    pub fn from_point(p: Point) -> Self {
        Self {
            min_x: p.x,
            min_y: p.y,
            max_x: p.x,
            max_y: p.y,
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    pub fn expand_to_point(&mut self, p: Point) {
        self.min_x = self.min_x.min(p.x);
        self.min_y = self.min_y.min(p.y);
        self.max_x = self.max_x.max(p.x);
        self.max_y = self.max_y.max(p.y);
    }

    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn center(&self) -> Point {
        Point {
            x: (self.min_x + self.max_x) / 2.0,
            y: (self.min_y + self.max_y) / 2.0,
        }
    }
}

/// Resolved features for a single layer
#[derive(Debug, Clone)]
pub struct LayerResolution {
    pub layer_name: String,
    pub features: Vec<ResolvedFeature>,
    pub bbox: BoundingBox,
    pub stats: LayerStats,
}

/// Per-layer statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerStats {
    pub smd_count: usize,
    pub pth_count: usize,
    pub via_count: usize,
    pub trace_count: usize,
    pub fill_count: usize,
    pub cutout_count: usize,
}

impl LayerStats {
    pub fn new() -> Self {
        Self {
            smd_count: 0,
            pth_count: 0,
            via_count: 0,
            trace_count: 0,
            fill_count: 0,
            cutout_count: 0,
        }
    }

    pub fn record(&mut self, bucket: FeatureBucket) {
        match bucket {
            FeatureBucket::Smd => self.smd_count += 1,
            FeatureBucket::Pth => self.pth_count += 1,
            FeatureBucket::Via => self.via_count += 1,
            FeatureBucket::Trace => self.trace_count += 1,
            FeatureBucket::Fill => self.fill_count += 1,
            FeatureBucket::Cutout => self.cutout_count += 1,
            FeatureBucket::Thermal => self.via_count += 1, // Count with vias
            FeatureBucket::Antipad => self.cutout_count += 1, // Count with cutouts
        }
    }
}

impl Default for LayerStats {
    fn default() -> Self {
        Self::new()
    }
}
