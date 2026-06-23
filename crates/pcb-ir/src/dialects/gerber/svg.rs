use crate::common::BBox;
use crate::dialects::{artwork, mask};

#[derive(Debug, Clone, Copy, Default)]
pub struct SvgOptions {
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
}

pub fn render_svg<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
) -> String {
    render_svg_with_options(doc, SvgOptions::default())
}

pub fn render_svg_sized<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
    width_px: u32,
    height_px: u32,
) -> String {
    render_svg_with_options(
        doc,
        SvgOptions {
            width_px: Some(width_px),
            height_px: Some(height_px),
        },
    )
}

pub fn render_svg_with_options<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
    options: SvgOptions,
) -> String {
    let mask = artwork::compose_to_mask(doc);
    match (options.width_px, options.height_px) {
        (Some(width), Some(height)) => mask::render_svg_sized(&mask, 0, width, height),
        _ => mask::render_svg(&mask, 0),
    }
}

pub fn render_bbox<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
) -> BBox {
    let mask = artwork::compose_to_mask(doc);
    mask::render_bbox(&mask, 0)
}
