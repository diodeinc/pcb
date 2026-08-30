//! Render backends for artwork and composed mask documents.
//!
//! Artwork renders keep the source's structure: apertures stay shared, so
//! repeated geometry stays repeated and polarity runs paint sequentially.
//! Mask renders take an already-composed image. Both take a [`RenderOptions`].

mod png;
mod svg;
mod term;

pub use png::{artwork_png, png};
pub use svg::{artwork_svg, svg, svg_path_data};
pub use term::{artwork_to_terminal, can_render_to_terminal, to_terminal, write_kitty_png};

use crate::dialects::{artwork, mask};
use crate::geom::{BBox, Point};

pub(crate) const VIEWBOX_PADDING_MM: f64 = 1.0;
pub(crate) const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Layer indices to render, in paint order. `None` renders all layers.
    pub layers: Option<Vec<usize>>,
    pub size: SizeConstraint,
    /// Exact viewport in the document's millimeter, Y-up coordinate system.
    /// `None` fits the selected layers with the default padding. This changes
    /// the camera only; callers should cull large documents before rendering.
    pub viewport: Option<BBox>,
}

impl RenderOptions {
    pub fn layer(index: usize) -> Self {
        Self {
            layers: Some(vec![index]),
            ..Self::default()
        }
    }

    pub fn layers(indices: impl Into<Vec<usize>>) -> Self {
        Self {
            layers: Some(indices.into()),
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

    pub(crate) fn viewport_or(&self, fitted: BBox) -> BBox {
        let Some(viewport) = self.viewport else {
            return fitted;
        };
        assert!(
            viewport.is_valid()
                && !viewport.is_empty()
                && viewport.width() > 0.0
                && viewport.height() > 0.0,
            "render viewport must be finite and have positive area"
        );
        viewport
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
}

/// The bbox a render of these layers covers (padded; falls back to a default
/// viewport for empty documents).
pub fn bbox<LayerMeta>(doc: &mask::Document<LayerMeta>, layers: Option<&[usize]>) -> BBox {
    padded_bbox(
        layer_indices(doc.layers.len(), layers)
            .into_iter()
            .map(|index| doc.layers[index].bbox),
    )
}

pub(crate) fn artwork_bbox<LayerMeta, ObjectMeta>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    layers: Option<&[usize]>,
) -> BBox {
    padded_bbox(
        layer_indices(doc.layers.len(), layers)
            .into_iter()
            .map(|index| doc.layers[index].bbox),
    )
}

pub(crate) fn padded_bbox(bboxes: impl IntoIterator<Item = BBox>) -> BBox {
    let bbox = bboxes.into_iter().fold(BBox::empty(), BBox::union);
    if bbox.is_empty() {
        BBox::new(Point::new(0.0, 0.0), Point::new(100.0, 100.0))
    } else {
        bbox.expand(VIEWBOX_PADDING_MM)
    }
}

pub(crate) fn layer_indices(layer_count: usize, layers: Option<&[usize]>) -> Vec<usize> {
    match layers {
        Some(layers) => layers.to_vec(),
        None => (0..layer_count).collect(),
    }
}

/// Pixel dimensions for a raster render of this bbox under the given constraint.
pub(crate) fn pixel_size(bbox: BBox, max_dimension_px: u32) -> (u32, u32) {
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }
    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    (
        (bbox.width() * scale).ceil().max(1.0) as u32,
        (bbox.height() * scale).ceil().max(1.0) as u32,
    )
}
