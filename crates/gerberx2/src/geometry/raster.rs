use resvg::{tiny_skia, usvg};

use super::ir::GeometryDocument;

const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

pub fn render_png(doc: &GeometryDocument) -> crate::Result<Vec<u8>> {
    render_png_with_max_dimension(doc, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_png_with_max_dimension(
    doc: &GeometryDocument,
    max_dimension_px: u32,
) -> crate::Result<Vec<u8>> {
    let (width_px, height_px) = pixel_size(doc, max_dimension_px);
    let svg = super::svg::render_svg_sized(doc, width_px, height_px);
    svg_to_png(&svg)
}

fn svg_to_png(svg: &str) -> crate::Result<Vec<u8>> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options)
        .map_err(|err| crate::GerberError::Render(format!("failed to parse SVG: {err}")))?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
        crate::GerberError::Render(format!("failed to allocate {width}x{height} PNG raster"))
    })?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|err| crate::GerberError::Render(format!("failed to encode PNG: {err}")))
}

fn pixel_size(doc: &GeometryDocument, max_dimension_px: u32) -> (u32, u32) {
    let bbox = super::svg::render_bbox(doc);
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }
    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    (
        (bbox.width() * scale).ceil().max(1.0) as u32,
        (bbox.height() * scale).ceil().max(1.0) as u32,
    )
}
