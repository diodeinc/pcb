use crate::dialects::{artwork, mask};

const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;

pub type RenderResult<T> = Result<T, String>;

pub fn render_png<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
) -> RenderResult<Vec<u8>> {
    render_png_with_max_dimension(doc, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_png_with_max_dimension<LayerMeta: Clone, ObjectMeta: Clone>(
    doc: &artwork::ArtworkDocument<LayerMeta, ObjectMeta>,
    max_dimension_px: u32,
) -> RenderResult<Vec<u8>> {
    let mask = artwork::compose_to_mask(doc);
    mask::render_png_with_max_dimension(&mask, 0, max_dimension_px)
}
