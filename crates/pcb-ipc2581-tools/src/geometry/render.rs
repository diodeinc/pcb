use anyhow::Context;
use ipc2581::Symbol;
use pcb_ir::geom::GeometryAccuracy;

pub use crate::layers::layer_role;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::artwork::{Geometry, Object, PaintOrder, PaintStage};
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side, mask};
use pcb_ir::geom::{BBox, Paint, Polarity, Span, StrokeStyle};
use pcb_ir::import::ipc2581::ImportedDesign;

type GeometryDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;
type ArtworkDocument = pcb_ir::dialects::artwork::Document<LayerFunction, Option<Symbol>>;

const DISPLAY_PROFILE_STROKE_WIDTH_MM: f64 = 0.1;

/// Materialize and normalize a layer using the same artwork rules as Gerber export.
pub fn prepare_layer(
    imported: &ImportedDesign,
    layer_name: &str,
    view: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<GeometryDocument> {
    let layer = imported
        .layer_id(layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    let mut geometry = imported.materialize_layer(layer, view, accuracy)?;
    pcb_ir::dialects::ipc::process::normalize_for_artwork(&mut geometry, accuracy)?;
    pcb_ir::dialects::ipc::validate_artwork_ready(&geometry)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' is not artwork-ready"))?;
    Ok(geometry)
}

pub fn render_layer_svg(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set, accuracy)?;
    Ok(pcb_ir::render::artwork_svg(
        &artwork,
        &pcb_ir::render::RenderOptions::default(),
        accuracy,
    )?)
}

pub fn render_layer_png(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> Result<Vec<u8>, String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set, accuracy)
        .map_err(|error| error.to_string())?;
    pcb_ir::render::artwork_png(
        &artwork,
        &pcb_ir::render::RenderOptions::default(),
        accuracy,
    )
}

#[cfg(feature = "cli")]
pub fn render_layer_terminal(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> Result<(), String> {
    let artwork = layer_artwork(geometry, include_profiles, profile_set, accuracy)
        .map_err(|error| error.to_string())?;
    pcb_ir::render::artwork_to_terminal(
        &artwork,
        &pcb_ir::render::RenderOptions::default(),
        accuracy,
    )
}

fn layer_has_content(
    geometry: &GeometryDocument,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<bool> {
    let mask = layer_mask(geometry, false, ProfileSet::RootOnly, accuracy)?;
    Ok(mask
        .layers
        .first()
        .map(|layer| !layer.shapes.is_empty() && !layer.bbox.is_empty())
        .unwrap_or(false))
}

pub fn layer_has_native_content(
    geometry: &GeometryDocument,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<bool> {
    let Some(mut native) = native_layer_document(geometry, accuracy)? else {
        return Ok(false);
    };
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut native, accuracy)?;
    layer_has_content(&native, accuracy)
}

/// Restrict a single-layer document to the features native to its source
/// layer, dropping borrowed features. Returns `None` when nothing is native.
pub fn native_layer_document(
    geometry: &GeometryDocument,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Option<GeometryDocument>> {
    let Some(layer) = geometry.layers.first() else {
        return Ok(None);
    };
    let source_layer_ref = layer.source_layer_ref;
    let mut native = geometry.clone();
    pcb_ir::dialects::ipc::process::retain_features(
        &mut native,
        |feature| feature.source_layer_ref == Some(source_layer_ref),
        accuracy,
    )?;
    Ok((!native.features.is_empty()).then_some(native))
}

/// Lower a single-layer geometry document to artwork, with the display
/// profile outlines a viewer expects overlaid.
pub fn layer_artwork(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<ArtworkDocument> {
    let layer = &geometry.layers[0];
    let mut artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
        geometry,
        0,
        layer_role(layer.layer_function),
        Side::None,
        accuracy,
    )?;
    if include_profiles {
        append_display_profiles(
            &mut artwork,
            geometry,
            profile_set,
            layer.layer_function,
            accuracy,
        )?;
    }
    Ok(artwork)
}

pub fn layer_mask(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<mask::Document<LayerFunction>> {
    Ok(pcb_ir::dialects::artwork::compose_to_mask(
        &layer_artwork(geometry, include_profiles, profile_set, accuracy)?,
        accuracy,
    )?)
}

fn append_display_profiles(
    artwork: &mut ArtworkDocument,
    geometry: &GeometryDocument,
    profile_set: ProfileSet,
    layer_function: LayerFunction,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
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
            accuracy,
        )?;
        for cutout in occurrence.profile.cutouts.slice(&geometry.profile_cutouts) {
            append_display_profile_path(
                artwork,
                profile_layer,
                geometry,
                cutout.path,
                occurrence.transform,
                accuracy,
            )?;
        }
    }

    pcb_ir::dialects::artwork::normalize_bounds(artwork);

    Ok(())
}

fn append_display_profile_path(
    artwork: &mut ArtworkDocument,
    layer: u32,
    geometry: &GeometryDocument,
    path: u32,
    transform: pcb_ir::geom::Affine2,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
    let path = artwork.push_path(
        Paint::Stroke(StrokeStyle::round(DISPLAY_PROFILE_STROKE_WIDTH_MM)),
        geometry.transformed_path_contours(path, transform, accuracy)?,
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

    Ok(())
}
