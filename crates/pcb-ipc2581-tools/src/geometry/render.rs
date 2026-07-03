use ipc2581::Symbol;

pub use crate::layers::layer_role;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::artwork::{Geometry, Object, PaintOrder, PaintStage};
use pcb_ir::dialects::ipc::{ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side, mask};
use pcb_ir::geom::{BBox, Paint, Polarity, Span, StrokeStyle};

type GeometryDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;
type ArtworkDocument = pcb_ir::dialects::artwork::Document<LayerFunction, Option<Symbol>>;

const DISPLAY_PROFILE_STROKE_WIDTH_MM: f64 = 0.1;

pub fn render_layer_svg(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
) -> String {
    let mask = layer_mask(geometry, include_profiles, profile_set);
    pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::default())
}

fn layer_has_content(geometry: &GeometryDocument) -> bool {
    let mask = layer_mask(geometry, false, ProfileSet::RootOnly);
    mask.layers
        .first()
        .map(|layer| !layer.shapes.is_empty() && !layer.bbox.is_empty())
        .unwrap_or(false)
}

pub fn layer_has_native_content(geometry: &GeometryDocument) -> bool {
    let Some(layer) = geometry.layers.first() else {
        return false;
    };

    let source_layer_ref = layer.source_layer_ref;
    let features = layer
        .features
        .slice(&geometry.features)
        .iter()
        .filter(|feature| feature.source_layer_ref == Some(source_layer_ref))
        .cloned()
        .collect::<Vec<_>>();
    if features.is_empty() {
        return false;
    }

    let mut native = geometry.clone();
    native.layers[0].features = Span::new(0, features.len() as u32);
    native.features = features;
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut native);
    layer_has_content(&native)
}

pub fn layer_mask(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
) -> mask::Document<LayerFunction> {
    let layer = &geometry.layers[0];
    let mut artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
        geometry,
        0,
        layer_role(layer.layer_function),
        Side::None,
    );
    if include_profiles {
        append_display_profiles(&mut artwork, geometry, profile_set, layer.layer_function);
    }
    pcb_ir::dialects::artwork::compose_to_mask(&artwork)
}

fn append_display_profiles(
    artwork: &mut ArtworkDocument,
    geometry: &GeometryDocument,
    profile_set: ProfileSet,
    layer_function: LayerFunction,
) {
    let profile_layer = artwork.push_layer(pcb_ir::dialects::artwork::Layer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: Side::None,
        objects: Span::EMPTY,
        bbox: BBox::empty(),
        meta: layer_function,
    });

    for occurrence in profile_occurrences_for(geometry, profile_set) {
        append_display_profile_path(
            artwork,
            profile_layer,
            geometry,
            occurrence.profile.outer_path,
            occurrence.transform,
        );
        for cutout in occurrence.profile.cutouts.slice(&geometry.profile_cutouts) {
            append_display_profile_path(
                artwork,
                profile_layer,
                geometry,
                cutout.path,
                occurrence.transform,
            );
        }
    }

    pcb_ir::dialects::artwork::normalize_bounds(artwork);
}

fn append_display_profile_path(
    artwork: &mut ArtworkDocument,
    layer: u32,
    geometry: &GeometryDocument,
    path: u32,
    transform: pcb_ir::geom::Affine2,
) {
    let path = artwork.push_path(
        Paint::Stroke(StrokeStyle::round(DISPLAY_PROFILE_STROKE_WIDTH_MM)),
        geometry.transformed_path_contours(path, transform),
    );
    artwork.push_object(
        layer,
        Object {
            polarity: Polarity::Dark,
            order: PaintOrder {
                stage: PaintStage::Overlay,
            },
            geometry: Geometry::Stroke { path },
            bbox: artwork.path_bbox(path),
            meta: None,
        },
    );
}
