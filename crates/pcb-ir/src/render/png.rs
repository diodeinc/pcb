use resvg::{tiny_skia, usvg};

use crate::dialects::{artwork, mask};
use crate::geom::BBox;
use crate::render::{RenderOptions, SizeConstraint};

/// Rasterize mask layers to a PNG. `Auto` renders at the default maximum
/// dimension; `MaxDimension`/`Fixed` control the output size.
pub fn png<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    options: &RenderOptions,
) -> Result<Vec<u8>, String> {
    let bbox = options.viewport_or(crate::render::bbox(doc, options.layers.as_deref()));
    svg_to_png(&crate::render::svg(doc, &raster_options(options, bbox)))
}

/// Rasterize artwork layers to a PNG, through the same SVG the SVG backend
/// writes.
pub fn artwork_png<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> Result<Vec<u8>, String> {
    let bbox = options.viewport_or(crate::render::artwork_bbox(doc, options.layers.as_deref()));
    svg_to_png(&crate::render::artwork_svg(
        doc,
        &raster_options(options, bbox),
    ))
}

/// Resolve a size constraint into the fixed pixel size a raster needs.
fn raster_options(options: &RenderOptions, bbox: BBox) -> RenderOptions {
    let (width_px, height_px) = match options.size {
        SizeConstraint::Auto => {
            crate::render::pixel_size(bbox, crate::render::DEFAULT_MAX_DIMENSION_PX)
        }
        SizeConstraint::MaxDimension(max) => crate::render::pixel_size(bbox, max),
        SizeConstraint::Fixed {
            width_px,
            height_px,
        } => (width_px, height_px),
    };
    RenderOptions {
        layers: options.layers.clone(),
        viewport: options.viewport,
        size: SizeConstraint::Fixed {
            width_px,
            height_px,
        },
    }
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
