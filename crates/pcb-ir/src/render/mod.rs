//! Render backends for composed mask documents.
//!
//! All entry points take a [`mask::Document`](crate::dialects::mask::Document)
//! and a [`RenderOptions`]. To render artwork or IPC geometry, lower it first
//! (`artwork::compose_to_mask`, `ipc::lower_layer_to_artwork`).

mod png;
mod svg;
mod term;

pub use png::png;
pub use svg::svg;
pub use term::{can_render_to_terminal, to_terminal, write_kitty_png};

use crate::dialects::mask;
use crate::geom::{BBox, Point};

pub(crate) const VIEWBOX_PADDING_MM: f64 = 1.0;
pub(crate) const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Layer indices to render, in paint order. `None` renders all layers.
    pub layers: Option<Vec<usize>>,
    pub size: SizeConstraint,
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
    let bbox = layer_indices(doc, layers)
        .into_iter()
        .fold(BBox::empty(), |bbox, index| {
            bbox.union(doc.layers[index].bbox)
        });
    if bbox.is_empty() {
        BBox::new(Point::new(0.0, 0.0), Point::new(100.0, 100.0))
    } else {
        bbox.expand(VIEWBOX_PADDING_MM)
    }
}

pub(crate) fn layer_indices<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    layers: Option<&[usize]>,
) -> Vec<usize> {
    match layers {
        Some(layers) => layers.to_vec(),
        None => (0..doc.layers.len()).collect(),
    }
}

/// Pixel dimensions for a raster render under the given constraint.
pub(crate) fn pixel_size<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    layers: Option<&[usize]>,
    max_dimension_px: u32,
) -> (u32, u32) {
    let bbox = bbox(doc, layers);
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }
    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    (
        (bbox.width() * scale).ceil().max(1.0) as u32,
        (bbox.height() * scale).ceil().max(1.0) as u32,
    )
}
