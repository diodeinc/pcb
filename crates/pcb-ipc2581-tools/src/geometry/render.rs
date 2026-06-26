use ipc2581::Symbol;
use ipc2581::types::LayerFunction;
use pcb_ir::common::{BBox, LayerRole, LineCap, LineJoin, PaintPolarity, Side};
use pcb_ir::dialects::artwork::{
    ArtworkGeometry, ArtworkObject, ArtworkPath, PaintOrder, PaintStage,
};
use pcb_ir::dialects::ipc::{ProfileSet, profile_occurrences_for, transformed_path_payloads};
use pcb_ir::dialects::mask::MaskDocument;

type GeometryDocument = pcb_ir::dialects::ipc::GeometryDocument<Symbol, LayerFunction>;

const DISPLAY_PROFILE_STROKE_WIDTH_MM: f64 = 0.1;

pub fn render_layer_svg(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
) -> String {
    let mask = layer_mask(geometry, include_profiles, profile_set);
    pcb_ir::dialects::mask::render_svg_all(&mask)
}

fn layer_has_content(geometry: &GeometryDocument) -> bool {
    let mask = layer_mask(geometry, false, ProfileSet::RootOnly);
    mask.layers
        .first()
        .map(|layer| layer.shape_count > 0 && !layer.bbox.is_empty())
        .unwrap_or(false)
}

pub fn layer_has_native_content(geometry: &GeometryDocument) -> bool {
    let Some(layer) = geometry.layers.first() else {
        return false;
    };

    let source_layer_ref = layer.source_layer_ref;
    let features = geometry.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .filter(|feature| feature.source_layer_ref == Some(source_layer_ref))
        .cloned()
        .collect::<Vec<_>>();
    if features.is_empty() {
        return false;
    }

    let mut native = geometry.clone();
    native.features = features;
    native.layers[0].feature_start = 0;
    native.layers[0].feature_count = native.features.len() as u32;
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut native);
    layer_has_content(&native)
}

pub fn layer_mask(
    geometry: &GeometryDocument,
    include_profiles: bool,
    profile_set: ProfileSet,
) -> MaskDocument<LayerFunction> {
    let layer = &geometry.layers[0];
    let artwork = if include_profiles {
        let mut artwork = pcb_ir::dialects::ipc::lower_layer_to_artwork(
            geometry,
            0,
            layer_role(layer.layer_function),
            Side::None,
        );
        append_display_profiles(&mut artwork, geometry, profile_set, layer.layer_function);
        artwork
    } else {
        pcb_ir::dialects::ipc::lower_layer_to_artwork(
            geometry,
            0,
            layer_role(layer.layer_function),
            Side::None,
        )
    };
    pcb_ir::dialects::artwork::compose_to_mask(&artwork)
}

fn append_display_profiles(
    artwork: &mut pcb_ir::dialects::artwork::ArtworkDocument<LayerFunction, Option<Symbol>>,
    geometry: &GeometryDocument,
    profile_set: ProfileSet,
    layer_function: LayerFunction,
) {
    let profile_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: Side::None,
        object_start: 0,
        object_count: 0,
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
        for cutout in &geometry.profile_cutouts[occurrence.profile.cutout_start as usize
            ..(occurrence.profile.cutout_start + occurrence.profile.cutout_count) as usize]
        {
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
    artwork: &mut pcb_ir::dialects::artwork::ArtworkDocument<LayerFunction, Option<Symbol>>,
    layer: u32,
    geometry: &GeometryDocument,
    path: u32,
    transform: pcb_ir::common::Affine2,
) {
    let path = artwork.push_path(
        ArtworkPath::stroked(
            DISPLAY_PROFILE_STROKE_WIDTH_MM,
            LineCap::Round,
            LineJoin::Round,
        ),
        transformed_path_payloads(geometry, path, transform),
    );
    let bbox = artwork.paths[path as usize].bbox;
    artwork.push_object(
        layer,
        ArtworkObject {
            paint: PaintPolarity::Dark,
            order: PaintOrder {
                stage: PaintStage::Overlay,
            },
            geometry: ArtworkGeometry::Stroke { path },
            net: None,
            bbox,
            meta: None,
        },
    );
    artwork.layers[layer as usize].bbox = artwork.layers[layer as usize].bbox.union(bbox);
}

pub fn layer_role(function: LayerFunction) -> LayerRole {
    match function {
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => LayerRole::Copper,
        LayerFunction::Solderpaste | LayerFunction::Pastemask => LayerRole::Paste,
        LayerFunction::Soldermask => LayerRole::Soldermask,
        LayerFunction::Silkscreen | LayerFunction::Legend => LayerRole::Legend,
        LayerFunction::Drill => LayerRole::Drill,
        LayerFunction::Rout
        | LayerFunction::VCut
        | LayerFunction::Score
        | LayerFunction::EdgeChamfer
        | LayerFunction::EdgePlating
        | LayerFunction::BoardOutline => LayerRole::Profile,
        LayerFunction::Assembly
        | LayerFunction::BoardFab
        | LayerFunction::Courtyard
        | LayerFunction::Document
        | LayerFunction::Graphic
        | LayerFunction::Fixture
        | LayerFunction::Probe
        | LayerFunction::Rework => LayerRole::Mechanical,
        _ => LayerRole::Other,
    }
}
