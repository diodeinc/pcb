//! What a layer function says about its features: role, domain, operation,
//! material, span and side.

use super::*;

pub fn is_copper(function: LayerFunction) -> bool {
    matches!(
        function,
        LayerFunction::Conductor
            | LayerFunction::CondFilm
            | LayerFunction::CondFoil
            | LayerFunction::Plane
            | LayerFunction::Signal
            | LayerFunction::Mixed
    )
}

pub fn layer_role(function: LayerFunction) -> crate::dialects::LayerRole {
    layer_class(function).0
}

/// A layer function's rendering role and the finer fabrication domain of the
/// features on it.
pub(super) fn layer_class(function: LayerFunction) -> (crate::dialects::LayerRole, FeatureDomain) {
    use crate::dialects::LayerRole;
    match function {
        function if is_copper(function) => (LayerRole::Copper, FeatureDomain::Copper),
        LayerFunction::Solderpaste | LayerFunction::Pastemask => {
            (LayerRole::Paste, FeatureDomain::Paste)
        }
        LayerFunction::Soldermask => (LayerRole::Soldermask, FeatureDomain::Soldermask),
        LayerFunction::Silkscreen | LayerFunction::Legend => {
            (LayerRole::Legend, FeatureDomain::Legend)
        }
        LayerFunction::Drill => (LayerRole::Drill, FeatureDomain::Drill),
        LayerFunction::Rout => (LayerRole::Profile, FeatureDomain::Rout),
        LayerFunction::VCut => (LayerRole::Profile, FeatureDomain::VCut),
        LayerFunction::Score => (LayerRole::Profile, FeatureDomain::Score),
        LayerFunction::BoardOutline => (LayerRole::Profile, FeatureDomain::Profile),
        LayerFunction::EdgeChamfer | LayerFunction::EdgePlating => {
            (LayerRole::Profile, FeatureDomain::Other)
        }
        LayerFunction::Assembly
        | LayerFunction::BoardFab
        | LayerFunction::Courtyard
        | LayerFunction::Document
        | LayerFunction::Graphic
        | LayerFunction::Fixture
        | LayerFunction::Probe
        | LayerFunction::Rework => (LayerRole::Mechanical, FeatureDomain::Mechanical),
        _ => (LayerRole::Other, FeatureDomain::Other),
    }
}

pub(super) fn complete_feature_intent(layer: &Layer, feature: &mut GeometryFeature) {
    let layer_intent = intent_for_layer(layer);
    if feature.intent.domain == FeatureDomain::Unknown {
        feature.intent.domain = layer_intent.domain;
    }
    if feature.intent.operation == FeatureOperation::Unknown {
        feature.intent.operation = operation_for_feature(feature, layer_intent.operation);
    }
    if feature.intent.material == FeatureMaterial::Unknown {
        feature.intent.material = material_for_domain(feature.intent.domain);
    }
    if feature.intent.span == FeatureSpan::Unknown {
        feature.intent.span = layer_intent.span;
    }
    if feature.intent.side == crate::dialects::Side::None {
        feature.intent.side = layer_intent.side;
    }
    if feature.intent.role == FeatureRole::Unknown {
        feature.intent.role = role_for_feature(feature);
    }
    if feature.intent.plating == PlatingKind::Unknown {
        feature.intent.plating = plating_for_feature(feature);
    }
    feature.reclassify();
}

pub(super) fn intent_for_layer(layer: &Layer) -> FeatureIntent {
    let domain = layer_class(layer.layer_function).1;
    FeatureIntent {
        domain,
        role: FeatureRole::Unknown,
        operation: operation_for_domain(domain),
        material: material_for_domain(domain),
        plating: PlatingKind::Unknown,
        span: span_for_layer(layer, domain),
        side: side_for_layer(layer.side),
    }
}

pub(super) fn operation_for_domain(domain: FeatureDomain) -> FeatureOperation {
    match domain {
        FeatureDomain::Copper => FeatureOperation::AddMaterial,
        FeatureDomain::Soldermask => FeatureOperation::OpenMask,
        FeatureDomain::Paste => FeatureOperation::AddMaterial,
        FeatureDomain::Legend => FeatureOperation::Print,
        FeatureDomain::Drill => FeatureOperation::Drill,
        FeatureDomain::Rout => FeatureOperation::Route,
        FeatureDomain::VCut | FeatureDomain::Score => FeatureOperation::Score,
        FeatureDomain::Profile => FeatureOperation::Profile,
        FeatureDomain::Mechanical => FeatureOperation::Mark,
        FeatureDomain::Unknown | FeatureDomain::Other => FeatureOperation::Unknown,
    }
}

pub(super) fn operation_for_feature(
    feature: &GeometryFeature,
    layer_operation: FeatureOperation,
) -> FeatureOperation {
    match feature.kind {
        FeatureKind::Hole => FeatureOperation::Drill,
        FeatureKind::Slot => FeatureOperation::Route,
        _ => layer_operation,
    }
}

pub(super) fn material_for_domain(domain: FeatureDomain) -> FeatureMaterial {
    match domain {
        FeatureDomain::Copper => FeatureMaterial::Copper,
        FeatureDomain::Soldermask => FeatureMaterial::Soldermask,
        FeatureDomain::Paste => FeatureMaterial::Paste,
        FeatureDomain::Legend => FeatureMaterial::Ink,
        FeatureDomain::Drill
        | FeatureDomain::Rout
        | FeatureDomain::VCut
        | FeatureDomain::Score
        | FeatureDomain::Profile => FeatureMaterial::Substrate,
        FeatureDomain::Mechanical | FeatureDomain::Other => FeatureMaterial::Other,
        FeatureDomain::Unknown => FeatureMaterial::Unknown,
    }
}

pub(super) fn span_for_layer(layer: &Layer, domain: FeatureDomain) -> FeatureSpan {
    if let Some(span) = layer.span {
        return FeatureSpan::FromTo {
            from: span.from_layer,
            to: span.to_layer,
        };
    }

    match domain {
        FeatureDomain::Drill
        | FeatureDomain::Rout
        | FeatureDomain::VCut
        | FeatureDomain::Score
        | FeatureDomain::Profile => FeatureSpan::ThroughBoard,
        FeatureDomain::Unknown => FeatureSpan::Unknown,
        _ => FeatureSpan::Layer(layer.name),
    }
}

/// An IPC layer side in the IR's vocabulary; a layer on both or neither
/// outer side has none.
pub fn side_for_layer(side: Option<ipc2581::types::ecad::Side>) -> crate::dialects::Side {
    match side {
        Some(ipc2581::types::ecad::Side::Top) => crate::dialects::Side::Top,
        Some(ipc2581::types::ecad::Side::Bottom) => crate::dialects::Side::Bottom,
        Some(ipc2581::types::ecad::Side::Internal) => crate::dialects::Side::Inner,
        _ => crate::dialects::Side::None,
    }
}

pub(super) fn role_for_feature(feature: &GeometryFeature) -> FeatureRole {
    match feature.kind {
        FeatureKind::Hole => FeatureRole::Hole,
        FeatureKind::Slot => FeatureRole::Slot,
        _ => match feature.intent.domain {
            FeatureDomain::VCut | FeatureDomain::Score => FeatureRole::ArraySeparation,
            FeatureDomain::Rout => FeatureRole::Route,
            FeatureDomain::Profile => FeatureRole::BoardOutline,
            FeatureDomain::Copper | FeatureDomain::Unknown => FeatureRole::Conductor,
            _ => FeatureRole::Other,
        },
    }
}

pub(super) fn plating_for_feature(feature: &GeometryFeature) -> PlatingKind {
    match feature.kind {
        FeatureKind::Hole | FeatureKind::Slot | FeatureKind::Padstack => feature.intent.plating,
        _ => PlatingKind::None,
    }
}

pub(super) fn plating_kind(status: PlatingStatus) -> PlatingKind {
    match status {
        PlatingStatus::Plated => PlatingKind::Plated,
        PlatingStatus::NonPlated => PlatingKind::NonPlated,
        PlatingStatus::Via => PlatingKind::Via,
        PlatingStatus::ViaCapped => PlatingKind::ViaCapped,
    }
}
