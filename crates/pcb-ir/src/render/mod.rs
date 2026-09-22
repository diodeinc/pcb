//! Render backends for artwork and composed mask documents.
//!
//! Artwork renders keep the source's structure: apertures stay shared, so
//! repeated geometry stays repeated and polarity runs paint sequentially.
//! Mask renders take an already-composed image. Both take a [`RenderOptions`].

mod png;
mod svg;
#[cfg(not(target_family = "wasm"))]
mod term;

pub use png::{artwork_png, png};
pub use svg::{artwork_svg, svg, svg_path_data};
#[cfg(not(target_family = "wasm"))]
pub use term::{artwork_to_terminal, can_render_to_terminal, write_kitty_png};

use crate::dialects::LayerRole;
use crate::geom::path::{PathCmd, PathOp};
use crate::geom::{AccuracyError, Arc, BBox, EllipticalArc, GeometryAccuracy, Point};

pub(crate) const VIEWBOX_PADDING_MM: f64 = 1.0;
pub(crate) const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;
const POINT_EPSILON_MM: f64 = 1e-9;

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Layer indices to render, in paint order. `None` renders all layers.
    pub layers: Option<Vec<usize>>,
    pub size: SizeConstraint,
    /// Exact viewport in the document's millimeter, Y-up coordinate system.
    /// `None` fits the selected layers with the default padding. This changes
    /// the camera only; callers should cull large documents before rendering.
    pub viewport: Option<BBox>,
    /// Budget for geometry the target cannot draw natively (patterned
    /// strokes, contours already carrying approximation).
    pub accuracy: crate::geom::GeometryAccuracy,
    /// Prefix for every element id the SVG defines. Ids are global to the
    /// document an SVG is inlined into, so renders sharing one HTML page need
    /// distinct prefixes or their apertures and masks resolve to each other's.
    pub id_prefix: String,
    /// Styles by layer index. A layer without one draws in its role's, so a
    /// fabrication layer looks the same in every render; drawings whose
    /// layers are not fabrication layers bring their own.
    pub styles: Vec<LayerStyle>,
}

impl RenderOptions {
    pub fn layer(index: usize) -> Self {
        Self {
            layers: Some(vec![index]),
            ..Self::default()
        }
    }

    pub fn with_size(mut self, size: SizeConstraint) -> Self {
        self.size = size;
        self
    }

    pub fn with_viewport(mut self, viewport: BBox) -> Self {
        self.viewport = Some(viewport);
        self
    }

    pub fn with_id_prefix(mut self, id_prefix: impl Into<String>) -> Self {
        self.id_prefix = id_prefix.into();
        self
    }

    pub fn with_accuracy(mut self, accuracy: crate::geom::GeometryAccuracy) -> Self {
        self.accuracy = accuracy;
        self
    }

    pub fn with_styles(mut self, styles: impl Into<Vec<LayerStyle>>) -> Self {
        self.styles = styles.into();
        self
    }

    pub(crate) fn style(&self, layer: usize, role: LayerRole) -> LayerStyle {
        self.styles
            .get(layer)
            .copied()
            .unwrap_or_else(|| LayerStyle::of(role))
    }

    /// The viewport over layers with these bounds: the explicit one, else
    /// the bounds padded, else a default for a document that draws nothing.
    pub(crate) fn viewport_over(&self, layers: impl IntoIterator<Item = BBox>) -> BBox {
        if let Some(viewport) = self.viewport {
            assert!(
                viewport.is_valid()
                    && !viewport.is_empty()
                    && viewport.width() > 0.0
                    && viewport.height() > 0.0,
                "render viewport must be finite and have positive area"
            );
            return viewport;
        }
        let bbox = layers.into_iter().fold(BBox::empty(), BBox::union);
        if bbox.is_empty() {
            BBox::new(Point::new(0.0, 0.0), Point::new(100.0, 100.0))
        } else {
            bbox.expand(VIEWBOX_PADDING_MM)
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SizeConstraint {
    /// Natural size: SVG in millimeter units, raster at the default maximum
    /// dimension.
    #[default]
    Auto,
    Fixed {
        width_px: u32,
        height_px: u32,
    },
    /// Scale so the longer edge is at most this many pixels.
    MaxDimension(u32),
    /// Scale to fit inside this box, keeping the image's proportions.
    Within {
        width_px: u32,
        height_px: u32,
    },
}

impl SizeConstraint {
    /// The pixel size this asks of a render of `bbox`; `Auto` asks none.
    pub(crate) fn pixels(self, bbox: BBox) -> Option<(u32, u32)> {
        match self {
            Self::Auto => None,
            Self::Fixed {
                width_px,
                height_px,
            } => Some((width_px, height_px)),
            Self::MaxDimension(max) => Some(pixel_size(bbox, max, max)),
            Self::Within {
                width_px,
                height_px,
            } => Some(pixel_size(bbox, width_px, height_px)),
        }
    }
}

pub(crate) fn layer_indices(layer_count: usize, layers: Option<&[usize]>) -> Vec<usize> {
    match layers {
        Some(layers) => layers.to_vec(),
        None => (0..layer_count).collect(),
    }
}

/// Pixel dimensions of the largest raster of this bbox that fits the box.
pub(crate) fn pixel_size(bbox: BBox, width_px: u32, height_px: u32) -> (u32, u32) {
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (width_px, height_px);
    }
    let scale = (f64::from(width_px) / bbox.width()).min(f64::from(height_px) / bbox.height());
    // The edge that binds can land a rounding error past its limit.
    let fit = |extent: f64, limit: u32| ((extent * scale).ceil() as u32).clamp(1, limit.max(1));
    (fit(bbox.width(), width_px), fit(bbox.height(), height_px))
}

/// The budget shared geometry has in its own frame: what the largest scale
/// it is placed at leaves of the document's.
pub(crate) fn local_accuracy(
    accuracy: GeometryAccuracy,
    extent: BBox,
    scale: f64,
) -> Result<GeometryAccuracy, AccuracyError> {
    let numeric = crate::geom::accuracy::numerical_error(extent);
    GeometryAccuracy::new(accuracy.remaining(numeric)? / scale.max(f64::MIN_POSITIVE))
}

/// How a layer draws: one colour, and the opacity its whole image
/// composites at, so overlapping objects never darken each other.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerStyle {
    /// `0xRRGGBB`.
    pub color: u32,
    pub opacity: f64,
}

impl LayerStyle {
    pub fn of(role: LayerRole) -> Self {
        let (color, opacity) = match role {
            LayerRole::Copper => (0xd87822, 0.9),
            LayerRole::Soldermask => (0x159447, 0.55),
            LayerRole::Paste => (0xaeb4bb, 0.9),
            LayerRole::Legend => (0x000000, 0.95),
            LayerRole::Profile => (0x000000, 1.0),
            LayerRole::Drill | LayerRole::Mechanical | LayerRole::Other => (0x5c7cfa, 0.85),
        };
        Self { color, opacity }
    }
}

/// A path command as a backend draws it: an arc knows where it starts, and
/// one too small to curve is the line to its end.
pub(crate) enum Drawn {
    Move(Point),
    Line(Point),
    Arc(EllipticalArc),
    Close,
}

pub(crate) fn drawn(cmds: impl IntoIterator<Item = PathCmd>) -> impl Iterator<Item = Drawn> {
    let mut subpath = Point::default();
    let mut current = Point::default();
    cmds.into_iter().map(move |cmd| {
        let start = current;
        current = match cmd.op {
            PathOp::Close => subpath,
            _ => cmd.p0,
        };
        let arc = match cmd.op {
            PathOp::MoveTo => {
                subpath = cmd.p0;
                return Drawn::Move(cmd.p0);
            }
            PathOp::LineTo => return Drawn::Line(cmd.p0),
            PathOp::Close => return Drawn::Close,
            PathOp::ArcTo => Arc::new(start, cmd.p0, cmd.p1, cmd.clockwise).to_elliptical(),
            PathOp::EllipseTo => EllipticalArc {
                start,
                end: cmd.p0,
                center: cmd.p1,
                x_axis: cmd.p2,
                y_axis: cmd.p3,
                clockwise: cmd.clockwise,
            },
        };
        let (_, minor, _) = arc.principal_axes();
        if arc.is_degenerate() || minor <= POINT_EPSILON_MM {
            Drawn::Line(arc.end)
        } else {
            Drawn::Arc(arc)
        }
    })
}
