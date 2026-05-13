use anyhow::{Context, Result};
use resvg::{tiny_skia, usvg};

use super::ir::GeometryDocument;

const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

pub fn render_layer_png(doc: &GeometryDocument, layer_index: usize) -> Result<Vec<u8>> {
    render_layer_png_with_max_dimension(doc, layer_index, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_layer_png_with_max_dimension(
    doc: &GeometryDocument,
    layer_index: usize,
    max_dimension_px: u32,
) -> Result<Vec<u8>> {
    let (width_px, height_px) = pixel_size_for_layer(doc, layer_index, max_dimension_px);
    let svg = super::svg::render_layer_svg_sized(doc, layer_index, width_px, height_px);
    svg_to_png(&svg)
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options).context("Failed to parse SVG")?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .with_context(|| format!("Failed to allocate {width}x{height} PNG raster"))?;

    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );

    pixmap.encode_png().context("Failed to encode PNG")
}

fn pixel_size_for_layer(
    doc: &GeometryDocument,
    layer_index: usize,
    max_dimension_px: u32,
) -> (u32, u32) {
    let bbox = doc.layers[layer_index].bbox;
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }

    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    let width = (bbox.width() * scale).ceil().max(1.0) as u32;
    let height = (bbox.height() * scale).ceil().max(1.0) as u32;
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ir::{BBox, GeometryLayer, Point};

    #[test]
    fn preserves_layer_aspect_ratio_for_png_size() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 0,
            bbox: BBox {
                min: Point::new(10.0, 20.0),
                max: Point::new(50.0, 40.0),
            },
        });

        assert_eq!(pixel_size_for_layer(&doc, 0, 1600), (1600, 800));
    }
}
