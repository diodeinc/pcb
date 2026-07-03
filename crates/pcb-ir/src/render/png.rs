use resvg::{tiny_skia, usvg};

use crate::dialects::mask;
use crate::render::{RenderOptions, SizeConstraint};

/// Rasterize mask layers to a PNG. `Auto` renders at the default maximum
/// dimension; `MaxDimension`/`Fixed` control the output size.
pub fn png<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    options: &RenderOptions,
) -> Result<Vec<u8>, String> {
    let (width_px, height_px) = match options.size {
        SizeConstraint::Auto => crate::render::pixel_size(
            doc,
            options.layers.as_deref(),
            crate::render::DEFAULT_MAX_DIMENSION_PX,
        ),
        SizeConstraint::MaxDimension(max) => {
            crate::render::pixel_size(doc, options.layers.as_deref(), max)
        }
        SizeConstraint::Fixed {
            width_px,
            height_px,
        } => (width_px, height_px),
    };
    let svg = crate::render::svg(
        doc,
        &RenderOptions {
            layers: options.layers.clone(),
            size: SizeConstraint::Fixed {
                width_px,
                height_px,
            },
        },
    );
    svg_to_png(&svg)
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options)
        .map_err(|err| format!("failed to parse SVG: {err}"))?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("failed to allocate {width}x{height} PNG raster"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|err| format!("failed to encode PNG: {err}"))
}
