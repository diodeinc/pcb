use ipc2581::Symbol;
use ipc2581::types::LayerFunction;
use pcb_ir::common::{LayerRole, Side};
use pcb_ir::dialects::ipc::ProfileSet;
use pcb_ir::dialects::mask::MaskDocument;

type GeometryDocument = pcb_ir::dialects::ipc::GeometryDocument<Symbol, LayerFunction>;

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
    let geom = if include_profiles {
        pcb_ir::dialects::ipc::lower_layer_with_profile_set_to_geom(
            geometry,
            0,
            layer_role(layer.layer_function),
            Side::None,
            profile_set,
        )
    } else {
        pcb_ir::dialects::ipc::lower_layer_to_geom(
            geometry,
            0,
            layer_role(layer.layer_function),
            Side::None,
        )
    };
    pcb_ir::dialects::geom::lower_filled_to_mask(&pcb_ir::dialects::geom::outline_strokes(geom))
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
