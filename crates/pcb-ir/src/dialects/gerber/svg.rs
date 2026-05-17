use crate::common::BBox;
use crate::dialects::gerber::{GeometryDocument, lower_to_geom};
use crate::dialects::{geom, mask};

#[derive(Debug, Clone, Copy, Default)]
pub struct SvgOptions {
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
}

pub fn render_svg<A: Clone>(doc: &GeometryDocument<A>) -> String {
    render_svg_with_options(doc, SvgOptions::default())
}

pub fn render_svg_sized<A: Clone>(
    doc: &GeometryDocument<A>,
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

pub fn render_svg_with_options<A: Clone>(doc: &GeometryDocument<A>, options: SvgOptions) -> String {
    let geom = lower_to_geom(doc);
    let mask = geom::lower_filled_to_mask(&geom);
    match (options.width_px, options.height_px) {
        (Some(width), Some(height)) => mask::render_svg_sized(&mask, 0, width, height),
        _ => mask::render_svg(&mask, 0),
    }
}

pub fn render_bbox<A: Clone>(doc: &GeometryDocument<A>) -> BBox {
    let geom = lower_to_geom(doc);
    let mask = geom::lower_filled_to_mask(&geom);
    mask::render_bbox(&mask, 0)
}
