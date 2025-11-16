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

/// Arc segment data for curved polygon edges
/// Represents an arc from the previous point to the current point
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArcSegment {
    /// Arc center point
    pub center: Point,
    /// Arc direction (true = clockwise, false = counter-clockwise)
    pub clockwise: bool,
}

/// Resolved geometry after transformation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolvedGeometry {
    /// Polyline trace with absolute coordinates
    Polyline {
        points: Vec<Point>,
        /// Arc data for each edge (None = straight line, Some = arc to this point)
        /// Index i contains arc data for edge from points[i-1] to points[i]
        arc_segments: Vec<Option<ArcSegment>>,
        line_width: f64,
        line_end: LineEndStyle,
    },

    /// Filled polygon (with optional cutouts/holes and arc data)
    Polygon {
        points: Vec<Point>,
        /// Arc data for each edge (None = straight line, Some = arc to this point)
        /// Index i contains arc data for edge from points[i-1] to points[i]
        arc_segments: Vec<Option<ArcSegment>>,
        cutouts: Vec<Vec<Point>>,
        /// Arc data for cutout edges (one Vec<Option<ArcSegment>> per cutout)
        cutout_arcs: Vec<Vec<Option<ArcSegment>>>,
    },

    /// Circle (pad or via)
    Circle {
        center: Point,
        diameter: f64,
        filled: bool,
        /// Stroke width for HOLLOW circles (from LineDesc)
        line_width: Option<f64>,
    },

    /// Rectangle
    Rectangle {
        center: Point,
        width: f64,
        height: f64,
        filled: bool,
        /// Stroke width for HOLLOW rectangles (from LineDesc)
        line_width: Option<f64>,
    },

    /// Rounded rectangle (preserves corner radii for accurate rendering)
    RoundedRectangle {
        center: Point,
        width: f64,
        height: f64,
        radius: f64,
        /// Per-corner rounding flags
        /// Order: [upper_right, upper_left, lower_right, lower_left]
        /// Matches IPC-2581 RectRound field ordering
        /// true = corner is rounded, false = corner is square
        corners: [bool; 4],
        rotation: f64,
    },

    /// Chamfered rectangle (preserves chamfer size for accurate rendering)
    ChamferedRectangle {
        center: Point,
        width: f64,
        height: f64,
        chamfer: f64,
        /// Per-corner chamfer flags
        /// Order: [upper_right, upper_left, lower_right, lower_left]
        /// Matches IPC-2581 RectCham field ordering
        /// true = corner is chamfered, false = corner is square
        corners: [bool; 4],
        rotation: f64,
    },

    /// Ellipse (preserves parametric form for accurate rendering)
    Ellipse {
        center: Point,
        width: f64,
        height: f64,
        rotation: f64,
    },

    /// Oval / Stadium shape (line segment with semicircular caps)
    /// Per IPC-2581 spec: "rectangle with complete radius (180° arc) at each end"
    /// Different from Ellipse - has flat sides parallel to longer axis
    Oval {
        center: Point,
        width: f64,
        height: f64,
        rotation: f64,
    },

    /// Donut / Annular ring (preserves inner hole for accurate rendering)
    Donut {
        center: Point,
        outer_diameter: f64,
        inner_diameter: f64,
    },

    /// Thermal relief (preserves spoke structure for accurate rendering)
    Thermal {
        center: Point,
        outer_diameter: f64,
        inner_diameter: f64,
        gap: f64,
        spokes: u8,
        rotation: f64,
    },

    /// Padstack reference (to be expanded in Stage 2)
    PadstackRef {
        padstack_name: String,
        center: Point,
        rotation: f64,
        mirror: bool,
        scale: f64,
        layer: String,
        /// Inline primitive override (takes precedence over padstack)
        inline_standard_primitive: Option<String>,
        /// Inline user primitive override (takes precedence over padstack)
        inline_user_primitive: Option<String>,
    },

    /// Group of multiple geometries (from UserSpecial with multiple shapes)
    /// Will be unioned together in Stage 4
    Group { geometries: Vec<ResolvedGeometry> },
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

    /// Conditionally apply mirror
    pub fn mirror_if(&self, should_mirror: bool) -> Self {
        if should_mirror {
            self.mirror()
        } else {
            *self
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

    /// Check if this bounding box intersects with another
    pub fn intersects(&self, other: &Self) -> bool {
        // No overlap if one is completely to the left/right/above/below the other
        !(self.max_x < other.min_x
            || self.min_x > other.max_x
            || self.max_y < other.min_y
            || self.min_y > other.max_y)
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
