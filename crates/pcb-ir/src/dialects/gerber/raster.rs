use crate::dialects::gerber::{GeometryDocument, lower_to_geom};
use crate::dialects::{geom, mask};

const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

pub type RenderResult<T> = Result<T, String>;

pub fn render_png<A: Clone>(doc: &GeometryDocument<A>) -> RenderResult<Vec<u8>> {
    render_png_with_max_dimension(doc, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_png_with_max_dimension<A>(
    doc: &GeometryDocument<A>,
    max_dimension_px: u32,
) -> RenderResult<Vec<u8>>
where
    A: Clone,
{
    let geom = lower_to_geom(doc);
    let mask = geom::lower_filled_to_mask(&geom);
    mask::render_png_with_max_dimension(&mask, 0, max_dimension_px)
}
