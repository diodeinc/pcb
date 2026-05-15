use resvg::{tiny_skia, usvg};

use crate::dialects::ipc::GeometryDocument;

const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

pub fn render_layer_png<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    flat_style: F,
) -> Result<Vec<u8>, String>
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    render_layer_png_with_max_dimension(doc, layer_index, DEFAULT_MAX_DIMENSION_PX, flat_style)
}

pub fn render_layer_png_with_max_dimension<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    max_dimension_px: u32,
    flat_style: F,
) -> Result<Vec<u8>, String>
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    let (width_px, height_px) = pixel_size_for_layer(doc, layer_index, max_dimension_px);
    let svg = super::svg::render_layer_svg_sized_with_style(
        doc,
        layer_index,
        width_px,
        height_px,
        flat_style,
    );
    svg_to_png(&svg)
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options)
        .map_err(|err| format!("Failed to parse SVG: {err}"))?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("Failed to allocate {width}x{height} PNG raster"))?;

    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );

    pixmap
        .encode_png()
        .map_err(|err| format!("Failed to encode PNG: {err}"))
}

pub fn pixel_size_for_layer<S, L>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    max_dimension_px: u32,
) -> (u32, u32) {
    let bbox = super::svg::render_layer_bbox(doc, layer_index);
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }

    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    let width = (bbox.width() * scale).ceil().max(1.0) as u32;
    let height = (bbox.height() * scale).ceil().max(1.0) as u32;
    (width, height)
}
