use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    FillDesc, FillProperty, HoleShape as IpcHoleShape, LayerFunction, LineEnd, LineProperty,
    PadUse, PlatingStatus, Polarity, PolyStep, SlotShape, StandardPrimitive, UserPrimitive,
    UserShapeType, Xform,
    ecad::{Layer, SetFeature, Step, StepRepeat, StepType},
};
use ipc2581::{Interner, Ipc2581, Symbol};

use crate::dialects::ipc::*;
use crate::geom::Polarity as GeometryPolarity;
use crate::geom::path::transform_cmds;
use crate::geom::*;

mod assembly;

pub use crate::dialects::assembly::{
    BomReferenceId, ComponentDefinitionId, ComponentOccurrenceId, LayoutOccurrenceId,
    PackageDefinitionId, Population as PopulationState,
};

pub type GeometryDocument = crate::dialects::ipc::Document<Symbol, LayerFunction>;
type GeometryLayer = crate::dialects::ipc::Layer<Symbol, LayerFunction>;
type GeometryFeature = crate::dialects::ipc::Feature<Symbol>;

/// One self-contained, source-faithful IPC design.
///
/// Geometry is stored once in step-local coordinates. Layout and feature
/// occurrences are derived by joining those definitions to the layout graph;
/// final layer images are lowerings, not independent design state.
#[derive(Debug, Clone)]
pub struct ImportedDesign {
    strings: Interner,
    pub revision: String,
    pub content: ipc2581::types::Content,
    pub specs: HashMap<Symbol, ipc2581::types::Spec>,
    pub logistic_header: Option<ipc2581::types::LogisticHeader>,
    pub history_record: Option<ipc2581::types::HistoryRecord>,
    pub boms: Vec<ipc2581::types::Bom>,
    pub avl: Option<ipc2581::types::Avl>,
    pub geometry: GeometryDocument,
    pub layer_definitions: Vec<Layer>,
    pub stackups: Vec<ipc2581::types::Stackup>,
    pub steps: Vec<Step>,
    pub step_layers: Vec<StepLayer>,
    pub packages: Vec<PackageDefinition>,
    pub components: Vec<ComponentDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StepLayer {
    pub step: u32,
    pub layer: LayerId,
    pub document_layer: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureDefinitionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureOccurrenceId {
    pub feature: FeatureDefinitionId,
    pub layout: LayoutOccurrenceId,
    pub placement: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct FeatureOccurrence {
    pub id: FeatureOccurrenceId,
    pub root_from_local: Affine2,
    pub board: Option<LayoutOccurrenceId>,
    pub root_from_board: Affine2,
    pub board_from_local: Option<Affine2>,
}

pub fn feature_occurrence_id(feature: &GeometryFeature) -> Option<FeatureOccurrenceId> {
    Some(FeatureOccurrenceId {
        feature: FeatureDefinitionId(feature.source.definition?),
        layout: feature
            .source_instance
            .map(LayoutOccurrenceId::Instance)
            .unwrap_or(LayoutOccurrenceId::Root),
        placement: feature.source_placement,
    })
}

#[derive(Debug, Clone)]
pub struct ComponentDefinition {
    pub step: u32,
    pub source_index: u32,
    pub source: ipc2581::types::Component,
    pub package: Option<PackageDefinitionId>,
    pub bom_references: Vec<BomReferenceId>,
    pub local_from_component: Affine2,
    pub population: PopulationState,
}

#[derive(Debug, Clone)]
pub struct PackageDefinition {
    pub step: u32,
    pub source_index: u32,
    pub source: ipc2581::types::Package,
}

#[derive(Debug, Clone, Copy)]
pub struct ComponentOccurrence {
    pub id: ComponentOccurrenceId,
    pub root_from_component: Affine2,
    pub board: Option<LayoutOccurrenceId>,
    pub root_from_board: Affine2,
    pub board_from_component: Option<Affine2>,
    pub population: PopulationState,
}

#[derive(Debug, Clone, Copy)]
struct StepOccurrence {
    step: u32,
    layout: LayoutOccurrenceId,
    root_from_step: Affine2,
    board: Option<LayoutOccurrenceId>,
    root_from_board: Affine2,
}

pub const FAB_PANEL_STEP_NAME: &str = "fab_panel";

pub const COPPER_BALANCE_ATTRIBUTE_NAME: &str = "diode.copper_balance";
pub const COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME: &str = "diode.copper_balance_lattice";
pub const COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_x_mm";
pub const COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_y_mm";
pub const COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_pitch_mm";
pub const COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME: &str = "diode.copper_balance_void_radius_mm";
pub const COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_void_corner_radius_mm";
pub const COPPER_BALANCE_LATTICE_VALUE: &str = "staggered-hex-v1";

fn primary_step<'a>(ipc: &Ipc2581, steps: &'a [Step]) -> Option<&'a Step> {
    ipc.content()
        .step_refs
        .first()
        .and_then(|step_ref| steps.iter().find(|step| step.name == *step_ref))
        .or_else(|| steps.first())
}

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
    use crate::dialects::LayerRole;
    if is_copper(function) {
        return LayerRole::Copper;
    }
    match function {
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

fn parse_copper_balance_attribute(value: &str) -> Result<CopperBalanceKind> {
    match value {
        "plane" => Ok(CopperBalanceKind::Plane),
        "full_void" => Ok(CopperBalanceKind::FullVoid),
        "edge_void" => Ok(CopperBalanceKind::EdgeVoid),
        "boundary_web" => Ok(CopperBalanceKind::BoundaryWeb),
        _ => bail!("unknown diode.copper_balance value '{value}'"),
    }
}

#[derive(Debug, Clone, Copy)]
struct ProfileRange {
    start: u32,
    count: u32,
    bbox: BBox,
}

struct LayoutBuildContext<'a> {
    ipc: &'a Ipc2581,
    steps: &'a [Step],
}

#[derive(Debug, Clone, Copy)]
struct LayoutParent<'a> {
    step: &'a Step,
    transform: Affine2,
    layout_step: u32,
    instance: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct LayoutInstanceSpec {
    repeat: u32,
    parent_instance: Option<u32>,
    child_step: u32,
    source_step_ref: Symbol,
    parent_step_ref: Symbol,
    transform: Affine2,
    repeat_index_x: u32,
    repeat_index_y: u32,
    repeat_count_x: u32,
    repeat_count_y: u32,
    repeat_pitch_x: f64,
    repeat_pitch_y: f64,
}

struct ExtractContext<'a> {
    strings: &'a Interner,
    padstacks: HashMap<Symbol, &'a ipc2581::types::PadStackDef>,
    line_descs: HashMap<Symbol, ipc2581::types::LineDesc>,
    fill_descs: HashMap<Symbol, ipc2581::types::FillDesc>,
    standard_primitives: HashMap<Symbol, &'a StandardPrimitive>,
    user_primitives: HashMap<Symbol, &'a UserPrimitive>,
}

#[derive(Debug, Clone, Copy)]
struct IpcPlacement {
    center: Point,
    xform: Xform,
    transform: Affine2,
}

fn ipc_placement(location: Point, xform: Option<Xform>) -> IpcPlacement {
    let xform = xform.unwrap_or_default();
    let offset = Affine2::placement(
        Point::default(),
        xform.rotation,
        Mirror::across_y(xform.mirror),
        xform.scale,
    )
    .transform_vector(Point::new(xform.x_offset, xform.y_offset));
    let center = Point::new(location.x + offset.x, location.y + offset.y);
    let transform = Affine2::placement(
        center,
        xform.rotation,
        Mirror::across_y(xform.mirror),
        xform.scale,
    );

    IpcPlacement {
        center,
        xform,
        transform,
    }
}

fn apply_ipc_placement(feature: &mut GeometryFeature, placement: IpcPlacement) {
    feature.transform = placement.transform;
    feature.center = placement.center;
    feature.rotation_degrees = placement.xform.rotation;
    feature.scale = placement.xform.scale;
}

#[derive(Debug, Clone, Copy)]
struct StrokedFeatureStyle {
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    width: f64,
    line_cap: LineCap,
    line_pattern: LinePattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimitivePaint {
    Fill,
    Hollow,
    Void,
}

fn populate_ipc_specs(doc: &mut GeometryDocument, ipc: &Ipc2581) {
    let Some(ecad) = ipc.ecad() else {
        return;
    };

    doc.specs.clear();
    doc.spec_items.clear();
    doc.spec_properties.clear();

    let mut specs = ecad.cad_header.specs.values().collect::<Vec<_>>();
    specs.sort_by(|left, right| ipc.resolve(left.name).cmp(ipc.resolve(right.name)));

    for spec in specs {
        let item_start = doc.spec_items.len() as u32;
        for item in &spec.items {
            let property_start = doc.spec_properties.len() as u32;
            doc.spec_properties
                .extend(item.properties.iter().map(|property| SpecProperty {
                    value: property.value,
                    text: property.text,
                    unit: property.unit,
                    plus_tol: property.plus_tol,
                    minus_tol: property.minus_tol,
                    tol_percent: property.tol_percent,
                }));
            doc.spec_items.push(SpecItem {
                element: item.element,
                kind: map_spec_item_kind(item.kind),
                item_type: item.item_type,
                comment: item.comment,
                properties: Span::new(
                    property_start,
                    doc.spec_properties.len() as u32 - property_start,
                ),
            });
        }
        doc.specs.push(Spec {
            name: spec.name,
            items: Span::new(item_start, doc.spec_items.len() as u32 - item_start),
        });
    }
}

fn map_spec_item_kind(kind: ipc2581::types::ecad::SpecItemKind) -> SpecItemKind {
    match kind {
        ipc2581::types::ecad::SpecItemKind::General => SpecItemKind::General,
        ipc2581::types::ecad::SpecItemKind::Dielectric => SpecItemKind::Dielectric,
        ipc2581::types::ecad::SpecItemKind::Conductor => SpecItemKind::Conductor,
        ipc2581::types::ecad::SpecItemKind::SurfaceFinish => SpecItemKind::SurfaceFinish,
        ipc2581::types::ecad::SpecItemKind::VCut => SpecItemKind::VCut,
        ipc2581::types::ecad::SpecItemKind::Other => SpecItemKind::Other,
    }
}

fn push_spec_refs(doc: &mut GeometryDocument, spec_refs: &[Symbol]) -> Span {
    let start = doc.spec_refs.len() as u32;
    doc.spec_refs
        .extend(spec_refs.iter().copied().map(|spec| SpecRef { spec }));
    Span::new(start, doc.spec_refs.len() as u32 - start)
}

#[derive(Debug, Clone, Copy)]
struct CopperBalanceMetadata {
    kind: CopperBalanceKind,
    void: Option<CopperBalanceVoidMetadata>,
}

#[derive(Debug, Clone, Copy)]
struct CopperBalanceVoidMetadata {
    lattice_origin: Point,
    lattice_pitch_mm: f64,
    radius_mm: f64,
    corner_radius_mm: f64,
}

fn set_copper_balance_metadata(
    ipc: &Ipc2581,
    set: &ipc2581::types::FeatureSet,
) -> Result<Option<CopperBalanceMetadata>> {
    let kind = nonstandard_attribute(ipc, set, COPPER_BALANCE_ATTRIBUTE_NAME, "STRING")?;
    let auxiliary_attributes = [
        (COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME, "STRING"),
        (COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME, "DOUBLE"),
    ];
    let Some(kind) = kind else {
        if auxiliary_attributes.iter().any(|(name, _)| {
            set.nonstandard_attributes
                .iter()
                .any(|attribute| ipc.resolve(attribute.name) == *name)
        }) {
            bail!("copper-balance lattice metadata requires diode.copper_balance");
        }
        return Ok(None);
    };
    let kind = parse_copper_balance_attribute(kind)?;
    let void = if kind == CopperBalanceKind::FullVoid {
        let lattice = required_nonstandard_attribute(
            ipc,
            set,
            COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME,
            "STRING",
        )?;
        if lattice != COPPER_BALANCE_LATTICE_VALUE {
            bail!("unsupported copper-balance lattice '{lattice}'");
        }
        let metadata = CopperBalanceVoidMetadata {
            lattice_origin: Point::new(
                required_double_attribute(
                    ipc,
                    set,
                    COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME,
                )?,
                required_double_attribute(
                    ipc,
                    set,
                    COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME,
                )?,
            ),
            lattice_pitch_mm: required_double_attribute(
                ipc,
                set,
                COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME,
            )?,
            radius_mm: required_double_attribute(
                ipc,
                set,
                COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME,
            )?,
            corner_radius_mm: required_double_attribute(
                ipc,
                set,
                COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME,
            )?,
        };
        if !metadata.lattice_origin.x.is_finite()
            || !metadata.lattice_origin.y.is_finite()
            || !metadata.lattice_pitch_mm.is_finite()
            || metadata.lattice_pitch_mm <= 0.0
            || !metadata.radius_mm.is_finite()
            || metadata.radius_mm <= 0.0
            || !metadata.corner_radius_mm.is_finite()
            || metadata.corner_radius_mm <= 0.0
        {
            bail!("copper-balance lattice and rounded-hex dimensions must be finite and positive");
        }
        crate::geom::shapes::rounded_hexagon(metadata.radius_mm, metadata.corner_radius_mm, 0.0)
            .context("copper-balance rounded-hex dimensions are invalid")?;
        Some(metadata)
    } else {
        for (name, attribute_type) in auxiliary_attributes {
            if nonstandard_attribute(ipc, set, name, attribute_type)?.is_some() {
                bail!("copper-balance {kind:?} set must not carry lattice metadata");
            }
        }
        None
    };
    Ok(Some(CopperBalanceMetadata { kind, void }))
}

fn required_double_attribute(
    ipc: &Ipc2581,
    set: &ipc2581::types::FeatureSet,
    name: &str,
) -> Result<f64> {
    let value = required_nonstandard_attribute(ipc, set, name, "DOUBLE")?;
    value
        .parse::<f64>()
        .with_context(|| format!("{name} has invalid DOUBLE value '{value}'"))
}

fn required_nonstandard_attribute<'a>(
    ipc: &'a Ipc2581,
    set: &'a ipc2581::types::FeatureSet,
    name: &str,
    expected_type: &str,
) -> Result<&'a str> {
    nonstandard_attribute(ipc, set, name, expected_type)?
        .with_context(|| format!("copper-balance set is missing {name}"))
}

fn nonstandard_attribute<'a>(
    ipc: &'a Ipc2581,
    set: &'a ipc2581::types::FeatureSet,
    name: &str,
    expected_type: &str,
) -> Result<Option<&'a str>> {
    let mut attributes = set
        .nonstandard_attributes
        .iter()
        .filter(|attribute| ipc.resolve(attribute.name) == name);
    let Some(attribute) = attributes.next() else {
        return Ok(None);
    };
    if attributes.next().is_some() {
        bail!("{name} attribute occurs more than once");
    }
    let attr_type = attribute
        .attr_type
        .map(|attr_type| ipc.resolve(attr_type))
        .with_context(|| format!("{name} attribute has no type"))?;
    if attr_type != expected_type {
        bail!("{name} attribute must have type {expected_type}, got '{attr_type}'");
    }
    attribute
        .value
        .map(|value| ipc.resolve(value))
        .with_context(|| format!("{name} attribute has no value"))
        .map(Some)
}

fn push_feature_set_record(
    doc: &mut GeometryDocument,
    layer: u32,
    source_set_index: u32,
    set: &ipc2581::types::FeatureSet,
    polarity: GeometryPolarity,
) -> u32 {
    let spec_refs = push_spec_refs(doc, &set.spec_refs);
    let set_id = doc.feature_sets.len() as u32;
    doc.feature_sets.push(FeatureSet {
        layer,
        source_set_index,
        source_geometry_ref: set.geometry,
        component_ref: set.component_ref,
        geometry_usage: set.geometry_usage.map(map_geometry_usage),
        net: set.net,
        polarity,
        spec_refs,
        features: Span::new(doc.features.len() as u32, 0),
        bbox: BBox::empty(),
    });
    set_id
}

fn map_geometry_usage(usage: ipc2581::types::GeometryUsage) -> GeometryUsage {
    match usage {
        ipc2581::types::GeometryUsage::Thieving => GeometryUsage::Thieving,
        ipc2581::types::GeometryUsage::ThermalRelief => GeometryUsage::ThermalRelief,
        ipc2581::types::GeometryUsage::Text => GeometryUsage::Text,
        ipc2581::types::GeometryUsage::Teardrop => GeometryUsage::Teardrop,
        ipc2581::types::GeometryUsage::Graphic => GeometryUsage::Graphic,
        ipc2581::types::GeometryUsage::None => GeometryUsage::None,
    }
}

fn push_extracted_feature(
    doc: &mut GeometryDocument,
    set_id: u32,
    source_layer_ref: Symbol,
    copper_balance: Option<CopperBalanceMetadata>,
    mut feature: GeometryFeature,
    layer_bbox: &mut BBox,
) {
    feature.source_layer_ref = Some(source_layer_ref);
    feature.set = Some(set_id);
    feature.flags.copper_balance = copper_balance.map(|metadata| metadata.kind);
    feature.flags.copper_balance_void = copper_balance.and_then(|metadata| {
        metadata.void.map(|void| CopperBalanceVoid {
            radius_mm: void.radius_mm,
            corner_radius_mm: void.corner_radius_mm,
        })
    });
    let bbox = feature.bbox;
    *layer_bbox = layer_bbox.union(bbox);
    let set = &mut doc.feature_sets[set_id as usize];
    set.bbox = set.bbox.union(bbox);
    set.features.count += 1;
    doc.features.push(feature);
}

fn complete_feature_intent(layer: &Layer, feature: &mut GeometryFeature) {
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

fn intent_for_layer(layer: &Layer) -> FeatureIntent<Symbol> {
    let domain = domain_for_layer(layer.layer_function);
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

fn domain_for_layer(function: LayerFunction) -> FeatureDomain {
    if is_copper(function) {
        return FeatureDomain::Copper;
    }
    match function {
        LayerFunction::Soldermask => FeatureDomain::Soldermask,
        LayerFunction::Solderpaste | LayerFunction::Pastemask => FeatureDomain::Paste,
        LayerFunction::Silkscreen | LayerFunction::Legend => FeatureDomain::Legend,
        LayerFunction::Drill => FeatureDomain::Drill,
        LayerFunction::Rout => FeatureDomain::Rout,
        LayerFunction::VCut => FeatureDomain::VCut,
        LayerFunction::Score => FeatureDomain::Score,
        LayerFunction::BoardOutline => FeatureDomain::Profile,
        LayerFunction::Assembly
        | LayerFunction::BoardFab
        | LayerFunction::Courtyard
        | LayerFunction::Document
        | LayerFunction::Graphic
        | LayerFunction::Fixture
        | LayerFunction::Probe
        | LayerFunction::Rework => FeatureDomain::Mechanical,
        _ => FeatureDomain::Other,
    }
}

fn operation_for_domain(domain: FeatureDomain) -> FeatureOperation {
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

fn operation_for_feature(
    feature: &GeometryFeature,
    layer_operation: FeatureOperation,
) -> FeatureOperation {
    match feature.kind {
        FeatureKind::Hole => FeatureOperation::Drill,
        FeatureKind::Slot => FeatureOperation::Route,
        _ => layer_operation,
    }
}

fn material_for_domain(domain: FeatureDomain) -> FeatureMaterial {
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

fn span_for_layer(layer: &Layer, domain: FeatureDomain) -> FeatureSpan<Symbol> {
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

fn side_for_layer(side: Option<ipc2581::types::ecad::Side>) -> crate::dialects::Side {
    match side {
        Some(ipc2581::types::ecad::Side::Top) => crate::dialects::Side::Top,
        Some(ipc2581::types::ecad::Side::Bottom) => crate::dialects::Side::Bottom,
        Some(ipc2581::types::ecad::Side::Internal) => crate::dialects::Side::Inner,
        _ => crate::dialects::Side::None,
    }
}

fn role_for_feature(feature: &GeometryFeature) -> FeatureRole {
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

fn plating_for_feature(feature: &GeometryFeature) -> PlatingKind {
    match feature.kind {
        FeatureKind::Hole | FeatureKind::Slot | FeatureKind::Padstack => feature.intent.plating,
        _ => PlatingKind::None,
    }
}

fn plating_kind(status: PlatingStatus) -> PlatingKind {
    match status {
        PlatingStatus::Plated => PlatingKind::Plated,
        PlatingStatus::NonPlated => PlatingKind::NonPlated,
        PlatingStatus::Via => PlatingKind::Via,
        PlatingStatus::ViaCapped => PlatingKind::ViaCapped,
    }
}

/// Import the complete source design once, retaining step-local geometry.
pub fn import_design(ipc: &Ipc2581) -> Result<ImportedDesign> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut geometry = extract_layout(ipc)?;

    // The layout traversal only needs reachable steps, while the canonical
    // document retains every source step definition.
    for step in &ecad.cad_data.steps {
        ensure_layout_step_for_step(&mut geometry, step);
    }

    let mut step_layers = Vec::new();
    for step in &ecad.cad_data.steps {
        let step_id = geometry
            .layout
            .steps
            .iter()
            .position(|candidate| candidate.source_step_ref == step.name)
            .context("imported IPC step is missing from the layout graph")?
            as u32;
        for (layer_index, source_layer) in ecad.cad_data.layers.iter().enumerate() {
            let layer_name = ipc.resolve(source_layer.name);
            let local = extract_step_layer_local(
                ipc,
                step,
                &ecad.cad_data.layers,
                source_layer,
                layer_name,
            )?;
            if local.features.is_empty() {
                continue;
            }

            let document_layer = geometry.layers.len() as u32;
            let set_start = geometry.feature_sets.len() as u32;
            let feature_start = geometry.features.len() as u32;
            let bbox = append_transformed_layer(
                &mut geometry,
                &local,
                0,
                Affine2::IDENTITY,
                0,
                None,
                document_layer,
            )?;
            for definition in feature_start..geometry.features.len() as u32 {
                let feature = &mut geometry.features[definition as usize];
                feature.source.definition = Some(definition);
                if let Some(group) = feature.placement_group {
                    feature.source_placement = Some(
                        geometry.feature_placement_groups[group as usize]
                            .placements
                            .start,
                    );
                }
            }
            let spec_refs = push_spec_refs(&mut geometry, &source_layer.spec_refs);
            geometry.layers.push(GeometryLayer {
                name: layer_name.to_owned(),
                source_layer_ref: source_layer.name,
                layer_function: source_layer.layer_function,
                spec_refs,
                sets: Span::new(set_start, geometry.feature_sets.len() as u32 - set_start),
                features: Span::new(
                    feature_start,
                    geometry.features.len() as u32 - feature_start,
                ),
                bbox,
            });
            step_layers.push(StepLayer {
                step: step_id,
                layer: LayerId(layer_index as u32),
                document_layer,
            });
        }
    }
    crate::dialects::ipc::process::normalize_bounds(&mut geometry);

    let mut packages = Vec::new();
    let mut package_ids = HashMap::new();
    for step in &ecad.cad_data.steps {
        let step_id = geometry
            .layout
            .steps
            .iter()
            .position(|candidate| candidate.source_step_ref == step.name)
            .context("package step is missing from the layout graph")? as u32;
        for (source_index, package) in step.packages.iter().enumerate() {
            let id = PackageDefinitionId(packages.len() as u32);
            package_ids.insert(package.name, id);
            packages.push(PackageDefinition {
                step: step_id,
                source_index: source_index as u32,
                source: package.clone(),
            });
        }
    }

    let mut components = Vec::new();
    for step in &ecad.cad_data.steps {
        let step_id = geometry
            .layout
            .steps
            .iter()
            .position(|candidate| candidate.source_step_ref == step.name)
            .context("component step is missing from the layout graph")?
            as u32;
        for (source_index, component) in step.components.iter().enumerate() {
            let placement = ipc_placement(
                Point::new(component.location.x, component.location.y),
                component.xform,
            );
            let package = component
                .package_ref
                .and_then(|reference| package_ids.get(&reference).copied());
            let bom_references = component
                .ref_des
                .map(|reference| component_bom_references(ipc, step.name, reference))
                .unwrap_or_default();
            let population = population_state(ipc.boms(), &bom_references);
            components.push(ComponentDefinition {
                step: step_id,
                source_index: source_index as u32,
                source: component.clone(),
                package,
                bom_references,
                local_from_component: placement.transform,
                population,
            });
        }
    }

    Ok(ImportedDesign {
        strings: ipc.interner().clone(),
        revision: ipc.revision().to_owned(),
        content: ipc.content().clone(),
        specs: ecad.cad_header.specs.clone(),
        logistic_header: ipc.logistic_header().cloned(),
        history_record: ipc.history_record().cloned(),
        boms: ipc.boms().to_vec(),
        avl: ipc.avl().cloned(),
        geometry,
        layer_definitions: ecad.cad_data.layers.clone(),
        stackups: ecad.cad_data.stackups.clone(),
        steps: ecad.cad_data.steps.clone(),
        step_layers,
        packages,
        components,
    })
}

fn component_bom_references(
    ipc: &Ipc2581,
    source_step: Symbol,
    component: Symbol,
) -> Vec<BomReferenceId> {
    let mut matches = Vec::new();
    for (bom_index, bom) in ipc.boms().iter().enumerate() {
        if bom.header.as_ref().is_some_and(|header| {
            !header.step_refs.is_empty() && !header.step_refs.contains(&source_step)
        }) {
            continue;
        }
        for (item_index, item) in bom.items.iter().enumerate() {
            for (designator_index, designator) in item.designators.iter().enumerate() {
                let ipc2581::types::BomDesignator::Reference(reference) = designator else {
                    continue;
                };
                if reference.name == component {
                    matches.push(BomReferenceId {
                        bom: bom_index as u32,
                        item: item_index as u32,
                        designator: designator_index as u32,
                    });
                }
            }
        }
    }
    matches
}

fn population_state(
    boms: &[ipc2581::types::Bom],
    references: &[BomReferenceId],
) -> PopulationState {
    references
        .iter()
        .filter_map(|id| bom_reference(boms, *id)?.populate)
        .map(|populate| {
            if populate {
                PopulationState::Populate
            } else {
                PopulationState::DoNotPopulate
            }
        })
        .fold(
            PopulationState::Unspecified,
            |state, incoming| match state {
                PopulationState::Unspecified => incoming,
                PopulationState::Conflicting => PopulationState::Conflicting,
                state if state == incoming => state,
                PopulationState::Populate | PopulationState::DoNotPopulate => {
                    PopulationState::Conflicting
                }
            },
        )
}

fn bom_reference(
    boms: &[ipc2581::types::Bom],
    reference: BomReferenceId,
) -> Option<&ipc2581::types::BomRefDes> {
    let designator = boms
        .get(reference.bom as usize)?
        .items
        .get(reference.item as usize)?
        .designators
        .get(reference.designator as usize)?;
    match designator {
        ipc2581::types::BomDesignator::Reference(reference) => Some(reference),
        _ => None,
    }
}

impl ImportedDesign {
    pub fn resolve(&self, symbol: Symbol) -> &str {
        self.strings.resolve(symbol)
    }

    pub fn bom(&self) -> Option<&ipc2581::types::Bom> {
        self.content
            .bom_refs
            .iter()
            .find_map(|reference| self.boms.iter().find(|bom| bom.name == *reference))
            .or_else(|| self.boms.first())
    }

    pub fn resolve_enterprise(&self, enterprise_ref: Symbol) -> Option<&str> {
        let enterprise = self
            .logistic_header
            .as_ref()?
            .enterprises
            .iter()
            .find(|enterprise| enterprise.id == enterprise_ref)?;
        match enterprise.name.map(|name| self.resolve(name))? {
            "Manufacturer" | "NONE" | "N/A" | "" => None,
            name => Some(name),
        }
    }

    pub fn layer_definition(&self, layer: LayerId) -> Option<&Layer> {
        self.layer_definitions.get(layer.0 as usize)
    }

    pub fn feature_definition(&self, feature: FeatureDefinitionId) -> Option<&GeometryFeature> {
        self.geometry.features.get(feature.0 as usize)
    }

    pub fn component_definition(
        &self,
        component: ComponentDefinitionId,
    ) -> Option<&ComponentDefinition> {
        self.components.get(component.0 as usize)
    }

    pub fn package_definition(&self, package: PackageDefinitionId) -> Option<&PackageDefinition> {
        self.packages.get(package.0 as usize)
    }

    pub fn bom_reference(&self, reference: BomReferenceId) -> Option<&ipc2581::types::BomRefDes> {
        bom_reference(&self.boms, reference)
    }

    pub fn bom_item(&self, reference: BomReferenceId) -> Option<&ipc2581::types::BomItem> {
        self.boms
            .get(reference.bom as usize)?
            .items
            .get(reference.item as usize)
    }

    pub fn layer_id(&self, name: &str) -> Option<LayerId> {
        self.layer_definitions
            .iter()
            .position(|layer| self.resolve(layer.name) == name)
            .map(|index| LayerId(index as u32))
    }

    pub fn step_id(&self, source_step_ref: Symbol) -> Option<u32> {
        self.geometry
            .layout
            .steps
            .iter()
            .position(|step| step.source_step_ref == source_step_ref)
            .map(|index| index as u32)
    }

    /// Materialize one canonical step-local layer definition without layout
    /// occurrences. This is the source-faithful input for hierarchical
    /// manufacturing lowerings.
    pub fn materialize_step_layer(&self, step: u32, layer: LayerId) -> Result<GeometryDocument> {
        let definition = self
            .layer_definition(layer)
            .context("layer id is outside the imported design")?;
        let step_definition = self
            .geometry
            .layout
            .steps
            .get(step as usize)
            .context("step id is outside the imported design")?;
        let step_layer = self
            .step_layers
            .iter()
            .find(|candidate| candidate.step == step && candidate.layer == layer);

        let mut target = GeometryDocument::new();
        self.copy_step_layout_sidecar(&mut target, step);
        target.diagnostics = self.geometry.diagnostics.clone();
        target.specs = self.geometry.specs.clone();
        target.spec_items = self.geometry.spec_items.clone();
        target.spec_properties = self.geometry.spec_properties.clone();

        let bbox = if let Some(step_layer) = step_layer {
            append_transformed_layer(
                &mut target,
                &self.geometry,
                step_layer.document_layer as usize,
                Affine2::IDENTITY,
                0,
                None,
                0,
            )?
        } else {
            BBox::empty()
        };
        let spec_refs = push_spec_refs(&mut target, &definition.spec_refs);
        target.layers.push(GeometryLayer {
            name: self.resolve(definition.name).to_owned(),
            source_layer_ref: definition.name,
            layer_function: definition.layer_function,
            spec_refs,
            sets: Span::new(0, target.feature_sets.len() as u32),
            features: Span::new(0, target.features.len() as u32),
            bbox,
        });
        target.layout.steps[0].source_step_ref = step_definition.source_step_ref;
        crate::dialects::ipc::process::normalize_bounds(&mut target);
        Ok(target)
    }

    pub fn feature_occurrences(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<Vec<FeatureOccurrence>> {
        let step_occurrences = self.step_occurrences(scope)?;
        let mut occurrences = Vec::new();
        for step_occurrence in &step_occurrences {
            let Some(step_layer) = self.step_layers.iter().find(|step_layer| {
                step_layer.layer == layer && step_layer.step == step_occurrence.step
            }) else {
                continue;
            };
            let source_layer = &self.geometry.layers[step_layer.document_layer as usize];
            for feature_index in source_layer.features.indices() {
                let feature = &self.geometry.features[feature_index as usize];
                match feature.placement_group {
                    Some(group) => {
                        let group = self.geometry.feature_placement_groups[group as usize];
                        for placement in group.placements.indices() {
                            let root_from_local = step_occurrence
                                .root_from_step
                                .concat(self.geometry.feature_placements[placement as usize]);
                            occurrences.push(self.feature_occurrence(
                                feature_index,
                                Some(placement),
                                *step_occurrence,
                                root_from_local,
                            ));
                        }
                    }
                    None => occurrences.push(self.feature_occurrence(
                        feature_index,
                        None,
                        *step_occurrence,
                        step_occurrence.root_from_step,
                    )),
                }
            }
        }
        Ok(occurrences)
    }

    fn feature_occurrence(
        &self,
        feature: u32,
        placement: Option<u32>,
        step: StepOccurrence,
        root_from_local: Affine2,
    ) -> FeatureOccurrence {
        FeatureOccurrence {
            id: FeatureOccurrenceId {
                feature: FeatureDefinitionId(feature),
                layout: step.layout,
                placement,
            },
            root_from_local,
            board: step.board,
            root_from_board: step.root_from_board,
            board_from_local: step
                .board
                .and_then(|_| step.root_from_board.inverse())
                .map(|board_from_root| board_from_root.concat(root_from_local)),
        }
    }

    pub fn feature_region(&self, occurrence: FeatureOccurrence) -> ContourSet {
        let feature = &self.geometry.features[occurrence.id.feature.0 as usize];
        ContourSet::from_placed_painted_paths(
            &self.geometry.arena,
            feature
                .paths
                .slice(&self.geometry.arena.paths)
                .iter()
                .map(|path| (path, occurrence.root_from_local)),
            tol::REGION_MM,
        )
    }

    pub fn component_occurrences(&self, scope: ArtworkScope) -> Result<Vec<ComponentOccurrence>> {
        let steps = self.step_occurrences(scope)?;
        let mut occurrences = Vec::new();
        for (component_index, component) in self.components.iter().enumerate() {
            for step in steps
                .iter()
                .filter(|occurrence| occurrence.step == component.step)
            {
                let root_from_component =
                    step.root_from_step.concat(component.local_from_component);
                occurrences.push(ComponentOccurrence {
                    id: ComponentOccurrenceId {
                        component: ComponentDefinitionId(component_index as u32),
                        layout: step.layout,
                    },
                    root_from_component,
                    board: step.board,
                    root_from_board: step.root_from_board,
                    board_from_component: step
                        .board
                        .and_then(|_| step.root_from_board.inverse())
                        .map(|board_from_root| board_from_root.concat(root_from_component)),
                    population: component.population,
                });
            }
        }
        Ok(occurrences)
    }

    pub fn materialize_layer(
        &self,
        layer: LayerId,
        scope: ArtworkScope,
    ) -> Result<GeometryDocument> {
        let definition = self
            .layer_definitions
            .get(layer.0 as usize)
            .context("layer id is outside the imported design")?;
        let step_occurrences = self.step_occurrences(scope)?;
        let mut target = GeometryDocument::new();
        if matches!(scope, ArtworkScope::Board | ArtworkScope::ArrayLocal) {
            if let Some(occurrence) = step_occurrences.first() {
                self.copy_step_layout_sidecar(&mut target, occurrence.step);
            }
        } else {
            self.copy_layout_sidecar(&mut target);
        }
        target.diagnostics = self.geometry.diagnostics.clone();
        target.specs = self.geometry.specs.clone();
        target.spec_items = self.geometry.spec_items.clone();
        target.spec_properties = self.geometry.spec_properties.clone();
        let feature_start = 0;
        let set_start = 0;
        let mut bbox = BBox::empty();
        let mut source_set_offset = 0;
        for occurrence in &step_occurrences {
            let Some(step_layer) = self
                .step_layers
                .iter()
                .find(|step_layer| step_layer.layer == layer && step_layer.step == occurrence.step)
            else {
                continue;
            };
            let source_layer = step_layer.document_layer as usize;
            bbox = bbox.union(append_transformed_layer(
                &mut target,
                &self.geometry,
                source_layer,
                occurrence.root_from_step,
                source_set_offset,
                occurrence.layout.source_instance(),
                0,
            )?);
            source_set_offset = source_set_offset
                .checked_add(source_layer_set_span(&self.geometry, source_layer)?)
                .context("layout contains too many source feature sets")?;
        }
        let spec_refs = push_spec_refs(&mut target, &definition.spec_refs);
        target.layers.push(GeometryLayer {
            name: self.resolve(definition.name).to_owned(),
            source_layer_ref: definition.name,
            layer_function: definition.layer_function,
            spec_refs,
            sets: Span::new(set_start, target.feature_sets.len() as u32),
            features: Span::new(feature_start, target.features.len() as u32),
            bbox,
        });
        crate::dialects::ipc::process::normalize_bounds(&mut target);
        Ok(target)
    }

    fn copy_layout_sidecar(&self, target: &mut GeometryDocument) {
        let mut copied_paths = HashMap::new();
        let mut copy_path = |source: u32, target: &mut GeometryDocument| {
            *copied_paths.entry(source).or_insert_with(|| {
                let copied = target.arena.paths.len() as u32;
                target
                    .arena
                    .append_path_from(&self.geometry.arena, source, Affine2::IDENTITY);
                copied
            })
        };

        target.profiles = self.geometry.profiles.clone();
        target.profile_cutouts = self.geometry.profile_cutouts.clone();
        for profile_index in 0..target.profiles.len() {
            let source = target.profiles[profile_index].outer_path;
            let copied = copy_path(source, target);
            target.profiles[profile_index].outer_path = copied;
        }
        for cutout_index in 0..target.profile_cutouts.len() {
            let source = target.profile_cutouts[cutout_index].path;
            let copied = copy_path(source, target);
            target.profile_cutouts[cutout_index].path = copied;
        }
        target.layout = self.geometry.layout.clone();
    }

    fn copy_step_layout_sidecar(&self, target: &mut GeometryDocument, step: u32) {
        let source_step = &self.geometry.layout.steps[step as usize];
        for profile_index in source_step.profiles.indices() {
            let source_profile = &self.geometry.profiles[profile_index as usize];
            let outer_path = target.arena.paths.len() as u32;
            target.arena.append_path_from(
                &self.geometry.arena,
                source_profile.outer_path,
                Affine2::IDENTITY,
            );
            let cutout_start = target.profile_cutouts.len() as u32;
            for source_cutout in source_profile.cutouts.slice(&self.geometry.profile_cutouts) {
                let path = target.arena.paths.len() as u32;
                target.arena.append_path_from(
                    &self.geometry.arena,
                    source_cutout.path,
                    Affine2::IDENTITY,
                );
                target.profile_cutouts.push(StepProfileCutout {
                    path,
                    bbox: source_cutout.bbox,
                });
            }
            target.profiles.push(StepProfile {
                outer_path,
                cutouts: Span::new(
                    cutout_start,
                    target.profile_cutouts.len() as u32 - cutout_start,
                ),
                bbox: source_profile.bbox,
            });
        }
        let mut step = source_step.clone();
        step.profiles = Span::new(0, target.profiles.len() as u32);
        target.layout.steps.push(step);
        target.layout.root_step = Some(0);
    }

    pub fn composed_layer_image(&self, layer: LayerId, scope: ArtworkScope) -> Result<ContourSet> {
        let definition = self
            .layer_definitions
            .get(layer.0 as usize)
            .context("layer id is outside the imported design")?;
        let mut document = self.materialize_layer(layer, scope)?;
        crate::dialects::ipc::process::normalize_for_artwork(&mut document);
        crate::dialects::ipc::validate_artwork_ready(&document).map_err(anyhow::Error::msg)?;
        let artwork = crate::dialects::ipc::lower_layer_to_artwork(
            &document,
            0,
            layer_role(definition.layer_function),
            side_for_layer(definition.side),
        );
        let (mut images, _) = crate::dialects::artwork::compose_attributed(&artwork, |_| ());
        Ok(images.pop().map_or_else(
            || ContourSet::empty(tol::REGION_MM),
            |image| ContourSet::new(image.image, FillRule::NonZero, tol::REGION_MM),
        ))
    }

    fn step_occurrences(&self, scope: ArtworkScope) -> Result<Vec<StepOccurrence>> {
        let root_step = self
            .geometry
            .layout
            .root_step
            .context("IPC-2581 primary step has no canonical layout root")?;
        let root_definition = self
            .geometry
            .layout
            .steps
            .get(root_step as usize)
            .context("canonical layout root references a missing step")?;
        let root = StepOccurrence {
            step: root_step,
            layout: LayoutOccurrenceId::Root,
            root_from_step: Affine2::IDENTITY,
            board: (root_definition.kind == LayoutStepKind::Board)
                .then_some(LayoutOccurrenceId::Root),
            root_from_board: Affine2::IDENTITY,
        };
        if scope == ArtworkScope::ArrayLocal {
            return Ok(vec![root]);
        }

        let mut occurrences = vec![root];
        self.append_step_occurrences(root, scope == ArtworkScope::ArraySupport, &mut occurrences)?;
        if scope != ArtworkScope::Board {
            return Ok(occurrences);
        }

        let board = occurrences
            .into_iter()
            .find(|occurrence| {
                self.geometry.layout.steps[occurrence.step as usize].kind == LayoutStepKind::Board
            })
            .with_context(|| {
                format!(
                    "IPC-2581 primary step '{}' does not reference a board step",
                    self.resolve(root_definition.source_step_ref)
                )
            })?;
        Ok(vec![StepOccurrence {
            step: board.step,
            layout: LayoutOccurrenceId::Root,
            root_from_step: Affine2::IDENTITY,
            board: Some(LayoutOccurrenceId::Root),
            root_from_board: Affine2::IDENTITY,
        }])
    }

    fn append_step_occurrences(
        &self,
        parent: StepOccurrence,
        support_only: bool,
        occurrences: &mut Vec<StepOccurrence>,
    ) -> Result<()> {
        for (repeat_index, repeat) in
            self.geometry
                .layout
                .repeats
                .iter()
                .enumerate()
                .filter(|(_, repeat)| {
                    repeat.parent_step == parent.step
                        && repeat.parent_instance == parent.layout.source_instance()
                })
        {
            for instance_index in repeat.instances.indices() {
                let instance = self
                    .geometry
                    .layout
                    .instances
                    .get(instance_index as usize)
                    .context("layout repeat references a missing instance")?;
                if instance.repeat != repeat_index as u32
                    || instance.parent_instance != parent.layout.source_instance()
                    || instance.child_step != repeat.child_step
                {
                    bail!("layout repeat and instance relationships are inconsistent");
                }
                let step = self
                    .geometry
                    .layout
                    .steps
                    .get(instance.child_step as usize)
                    .context("layout instance references a missing step")?;
                if support_only && step.kind == LayoutStepKind::Board {
                    continue;
                }
                let layout = LayoutOccurrenceId::Instance(instance_index);
                let (board, root_from_board) = if step.kind == LayoutStepKind::Board {
                    (Some(layout), instance.transform)
                } else {
                    (parent.board, parent.root_from_board)
                };
                let occurrence = StepOccurrence {
                    step: instance.child_step,
                    layout,
                    root_from_step: instance.transform,
                    board,
                    root_from_board,
                };
                occurrences.push(occurrence);
                self.append_step_occurrences(occurrence, support_only, occurrences)?;
            }
        }
        Ok(())
    }
}

pub fn extract_layer(ipc: &Ipc2581, layer_name: &str) -> Result<GeometryDocument> {
    extract_layer_for_view(ipc, layer_name, ArtworkScope::ArrayFlattened)
}

pub fn extract_layer_for_view(
    ipc: &Ipc2581,
    layer_name: &str,
    view: ArtworkScope,
) -> Result<GeometryDocument> {
    let design = import_design(ipc)?;
    let layer = design
        .layer_id(layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    design.materialize_layer(layer, view)
}

pub fn extract_layout(ipc: &Ipc2581) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step =
        primary_step(ipc, &ecad.cad_data.steps).context("IPC-2581 ECAD section has no Step")?;
    let mut doc = GeometryDocument::new();
    append_layout_geometry(&mut doc, ipc, &ecad.cad_data.steps, step)?;
    if ipc.resolve(step.name) == FAB_PANEL_STEP_NAME
        && let Some(root_step) = doc.layout.root_step
    {
        doc.layout.steps[root_step as usize].purpose = LayoutPurpose::FabricationPanel;
    }
    populate_ipc_specs(&mut doc, ipc);
    crate::dialects::ipc::process::normalize_bounds(&mut doc);
    Ok(doc)
}

pub fn extract_step_layer_local(
    ipc: &Ipc2581,
    step: &Step,
    layers: &[Layer],
    layer: &Layer,
    layer_name: &str,
) -> Result<GeometryDocument> {
    let content = ipc.content();
    let context = ExtractContext {
        strings: ipc.interner(),
        padstacks: step
            .padstack_defs
            .iter()
            .map(|padstack| (padstack.name, padstack))
            .collect(),
        line_descs: content
            .dictionary_line_desc
            .entries
            .iter()
            .map(|entry| (entry.id, entry.line_desc))
            .collect(),
        fill_descs: content
            .dictionary_fill_desc
            .entries
            .iter()
            .map(|entry| (entry.id, entry.fill_desc))
            .collect(),
        standard_primitives: content
            .dictionary_standard
            .entries
            .iter()
            .map(|entry| (entry.id, &entry.primitive))
            .collect(),
        user_primitives: content
            .dictionary_user
            .entries
            .iter()
            .map(|entry| (entry.id, &entry.primitive))
            .collect(),
    };

    let mut doc = GeometryDocument::new();
    let feature_start = doc.features.len() as u32;
    let set_start = doc.feature_sets.len() as u32;
    let spec_refs = push_spec_refs(&mut doc, &layer.spec_refs);
    let layer_index = doc.layers.len() as u32;
    doc.layers.push(GeometryLayer {
        name: layer_name.to_string(),
        source_layer_ref: layer.name,
        layer_function: layer.layer_function,
        spec_refs,
        sets: Span::new(set_start, 0),
        features: Span::new(feature_start, 0),
        bbox: BBox::empty(),
    });

    let mut layer_bbox = BBox::empty();
    let layer_polarity = map_polarity(layer.polarity.unwrap_or(Polarity::Positive));
    let source_step_kind = layout_step_kind(step);

    for layer_feature in step
        .layer_features
        .iter()
        .filter(|feature| feature.layer_ref == layer.name)
    {
        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            let polarity = set.polarity.map(map_polarity).unwrap_or(layer_polarity);
            let copper_balance = set_copper_balance_metadata(ipc, set)?;
            if copper_balance.is_some_and(|metadata| metadata.void.is_some())
                && set.features.len() != 1
            {
                bail!("copper-balance full_void set must contain exactly one feature group");
            }
            let set_id =
                push_feature_set_record(&mut doc, layer_index, set_index as u32, set, polarity);

            for (feature_index, set_feature) in set.features.iter().enumerate() {
                let source = SourceRef {
                    set_index: set_index as u32,
                    feature_index: feature_index as u32,
                    definition: None,
                };
                let features = extract_set_feature(
                    &context,
                    layer.name,
                    set.net,
                    polarity,
                    source,
                    set_feature,
                    &mut doc,
                )?;
                validate_copper_balance_structure(copper_balance, set_feature, &features, &doc)?;

                for mut feature in features {
                    feature.source_step_ref = Some(step.name);
                    feature.source_step_kind = source_step_kind;
                    complete_feature_intent(layer, &mut feature);
                    push_extracted_feature(
                        &mut doc,
                        set_id,
                        layer_feature.layer_ref,
                        copper_balance,
                        feature,
                        &mut layer_bbox,
                    );
                }
            }
        }
    }

    for layer_feature in &step.layer_features {
        let Some(source_layer) = layers
            .iter()
            .find(|candidate| candidate.name == layer_feature.layer_ref)
        else {
            continue;
        };
        let is_drill_layer = source_layer.layer_function == LayerFunction::Drill;
        let is_fabrication_layer = source_layer.layer_function.is_fabrication();

        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            let polarity = set.polarity.map(map_polarity).unwrap_or(layer_polarity);
            let copper_balance = set_copper_balance_metadata(ipc, set)?;
            let mut emitted = Vec::new();

            if is_drill_layer && source_layer.name == layer.name {
                for (feature_index, set_feature) in set.features.iter().enumerate() {
                    if let SetFeature::Hole(hole) = set_feature {
                        let feature = extract_hole(
                            SourceRef {
                                set_index: set_index as u32,
                                feature_index: feature_index as u32,
                                definition: None,
                            },
                            set.geometry,
                            hole,
                            &mut doc,
                        );
                        emitted.push(feature);
                    }
                }
            }

            if is_fabrication_layer {
                for (feature_index, set_feature) in set.features.iter().enumerate() {
                    if let SetFeature::Slot(slot) = set_feature
                        && slot_applies_to_layer(source_layer, layer, layers, slot)
                    {
                        let feature = extract_slot(
                            &context,
                            SourceRef {
                                set_index: set_index as u32,
                                feature_index: feature_index as u32,
                                definition: None,
                            },
                            set.geometry,
                            slot,
                            &mut doc,
                        )?;
                        emitted.push(feature);
                    }
                }
            }

            if !emitted.is_empty() {
                let set_id =
                    push_feature_set_record(&mut doc, layer_index, set_index as u32, set, polarity);
                for mut feature in emitted {
                    feature.source_step_ref = Some(step.name);
                    feature.source_step_kind = source_step_kind;
                    complete_feature_intent(source_layer, &mut feature);
                    push_extracted_feature(
                        &mut doc,
                        set_id,
                        layer_feature.layer_ref,
                        copper_balance,
                        feature,
                        &mut layer_bbox,
                    );
                }
            }
        }
    }

    let layer = &mut doc.layers[layer_index as usize];
    layer.features.count = doc.features.len() as u32 - feature_start;
    layer.sets.count = doc.feature_sets.len() as u32 - set_start;
    layer.bbox = layer_bbox;

    Ok(doc)
}

fn extract_set_feature(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    set_feature: &SetFeature,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    match set_feature {
        SetFeature::Pad(pad) => Ok(extract_pad(
            context, layer_ref, net, polarity, source, pad, doc,
        )?
        .into_iter()
        .collect()),
        SetFeature::Fiducial(fiducial) => Ok(extract_fiducial(
            context, net, polarity, source, fiducial, doc,
        )?
        .into_iter()
        .collect()),
        SetFeature::Trace(trace) => Ok(extract_trace(context, net, polarity, source, trace, doc)
            .into_iter()
            .collect()),
        SetFeature::UserPrimitive(primitive) => {
            extract_inline_user_primitive(context, net, polarity, source, primitive, doc)
        }
        SetFeature::Polygon(polygon) => {
            Ok(vec![extract_polygon(net, polarity, source, polygon, doc)])
        }
        SetFeature::Line(line) => Ok(vec![extract_line(
            context, net, polarity, source, line, doc,
        )]),
        SetFeature::Arc(arc) => Ok(vec![extract_arc(context, net, polarity, source, arc, doc)]),
        SetFeature::Polyline(polyline) => Ok(vec![extract_feature_polyline(
            context, net, polarity, source, polyline, doc,
        )]),
        SetFeature::StandardPrimitiveRef(primitive_ref) => extract_feature_primitive(
            context,
            net,
            polarity,
            source,
            primitive_ref,
            FeaturePrimitiveKind::Standard,
            doc,
        ),
        SetFeature::UserPrimitiveRef(primitive_ref) => extract_feature_primitive(
            context,
            net,
            polarity,
            source,
            primitive_ref,
            FeaturePrimitiveKind::User,
            doc,
        ),
        SetFeature::PlacementGroup(group) => {
            extract_feature_placement_group(context, layer_ref, net, polarity, source, group, doc)
        }
        SetFeature::Hole(_) | SetFeature::Slot(_) => Ok(Vec::new()),
    }
}

fn extract_feature_placement_group(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    group: &ipc2581::types::ecad::FeaturePlacementGroup,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let placement_start = doc.feature_placements.len() as u32;
    doc.feature_placements.extend(
        group.locations.iter().map(|location| {
            ipc_placement(Point::new(location.x, location.y), group.xform).transform
        }),
    );
    let placements = Span::new(
        placement_start,
        doc.feature_placements.len() as u32 - placement_start,
    );
    let group_id = doc.feature_placement_groups.len() as u32;
    doc.feature_placement_groups.push(FeaturePlacementGroup {
        placements,
        features: Span::EMPTY,
    });

    let feature_start = doc.features.len() as u32;
    let mut features = Vec::new();
    for child in &group.features {
        for mut feature in
            extract_set_feature(context, layer_ref, net, polarity, source, child, doc)?
        {
            if feature.placement_group.is_some() {
                bail!("nested IPC feature placement groups are not supported");
            }
            feature.placement_group = Some(group_id);
            feature.bbox = doc.placed_paths_bbox(&feature);
            features.push(feature);
        }
    }
    doc.feature_placement_groups[group_id as usize].features =
        Span::new(feature_start, features.len() as u32);
    Ok(features)
}

/// Validate a full_void set against its declared lattice metadata: one
/// shared contour flashed by an identity-Xform placement group whose
/// locations are distinct on-lattice sites.
fn validate_copper_balance_structure(
    metadata: Option<CopperBalanceMetadata>,
    set_feature: &SetFeature,
    features: &[GeometryFeature],
    doc: &GeometryDocument,
) -> Result<()> {
    let Some(void) = metadata.and_then(|metadata| metadata.void) else {
        return Ok(());
    };
    let SetFeature::PlacementGroup(source_group) = set_feature else {
        bail!("copper-balance full_void set must contain one placement group");
    };
    if source_group.xform != Some(Xform::default()) {
        bail!("copper-balance lattice placement group must carry an identity Xform");
    }
    let [feature] = features else {
        bail!("copper-balance lattice placement group must contain one feature");
    };
    let group_id = feature
        .placement_group
        .context("copper-balance void feature has no placement group")?;
    let group = doc.feature_placement_groups[group_id as usize];
    if group.placements.len() != source_group.locations.len() {
        bail!("copper-balance lattice locations did not produce matching placements");
    }
    validate_copper_balance_void_shape(doc, feature, void)?;

    let lattice = crate::geom::copper_balance::DenseCopperLattice {
        origin: void.lattice_origin,
        pitch_mm: void.lattice_pitch_mm,
    };
    let mut sites = BTreeSet::new();
    for location in &source_group.locations {
        let point = Point::new(location.x, location.y);
        let (site, center) = lattice.nearest_site(point);
        if point.distance_to(center) > LATTICE_COORDINATE_TOLERANCE_MM {
            bail!(
                "copper-balance void location ({}, {}) is not on its declared lattice",
                point.x,
                point.y
            );
        }
        if !sites.insert((site.column, site.row)) {
            bail!("copper-balance lattice contains a duplicate site");
        }
    }
    Ok(())
}

const LATTICE_COORDINATE_TOLERANCE_MM: f64 = 5e-5;

fn validate_copper_balance_void_shape(
    doc: &GeometryDocument,
    feature: &GeometryFeature,
    metadata: CopperBalanceVoidMetadata,
) -> Result<()> {
    let actual = crate::dialects::ipc::contour_flash_aperture(doc, feature)
        .context("copper-balance void is not one filled rigid contour")?;
    let crate::dialects::artwork::ApertureShape::Contour { outline, fill_rule } = actual else {
        bail!("copper-balance void did not lower to a contour aperture");
    };
    let expected =
        crate::geom::shapes::rounded_hexagon(metadata.radius_mm, metadata.corner_radius_mm, 0.0)
            .context("copper-balance rounded-hex dimensions are invalid")?;
    let actual = ContourSet::from_contours(&[outline], fill_rule, 1e-5);
    let expected = ContourSet::from_contours(&[expected], FillRule::NonZero, 1e-5);
    let mismatch = actual.difference(&expected).area() + expected.difference(&actual).area();
    if mismatch > 1e-5 {
        bail!(
            "copper-balance void contour disagrees with its rounded-hex metadata by {mismatch} mm^2"
        );
    }
    Ok(())
}

pub fn step_repeat_transform(repeat: &StepRepeat, ix: u32, iy: u32) -> Affine2 {
    Affine2::placement(
        Point::new(
            repeat.x + ix as f64 * repeat.dx,
            repeat.y + iy as f64 * repeat.dy,
        ),
        repeat.angle,
        Mirror::across_y(repeat.mirror),
        1.0,
    )
}

fn source_layer_set_span(source: &GeometryDocument, layer_index: usize) -> Result<u32> {
    let layer = &source.layers[layer_index];
    let mut span = 0;
    for set in layer.sets.slice(&source.feature_sets) {
        let set_end = set
            .source_set_index
            .checked_add(1)
            .context("Source feature set index overflow")?;
        span = span.max(set_end);
    }
    Ok(span)
}

fn append_span<T: Clone>(target: &mut Vec<T>, source: &[T], span: Span) -> Span {
    let start = target.len() as u32;
    target.extend(span.slice(source).iter().cloned());
    Span::new(start, span.count)
}

fn append_transformed_layer(
    target: &mut GeometryDocument,
    source: &GeometryDocument,
    layer_index: usize,
    transform: Affine2,
    source_set_offset: u32,
    source_instance: Option<u32>,
    target_layer: u32,
) -> Result<BBox> {
    let layer = &source.layers[layer_index];
    let mut layer_bbox = BBox::empty();
    let mut placement_groups = HashMap::<u32, u32>::new();

    for source_set_index in layer.sets.indices() {
        let source_set = &source.feature_sets[source_set_index as usize];
        let spec_refs = append_span(
            &mut target.spec_refs,
            &source.spec_refs,
            source_set.spec_refs,
        );
        let target_set = target.feature_sets.len() as u32;
        target.feature_sets.push(FeatureSet {
            layer: target_layer,
            source_set_index: source_set
                .source_set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?,
            source_geometry_ref: source_set.source_geometry_ref,
            component_ref: source_set.component_ref,
            geometry_usage: source_set.geometry_usage,
            net: source_set.net,
            polarity: source_set.polarity,
            spec_refs,
            features: Span::new(target.features.len() as u32, 0),
            bbox: BBox::empty(),
        });

        for feature in source_set.features.slice(&source.features) {
            let spec_refs =
                append_span(&mut target.spec_refs, &source.spec_refs, feature.spec_refs);
            let target_placement_group = if let Some(source_group_id) = feature.placement_group {
                if let Some(&target_group_id) = placement_groups.get(&source_group_id) {
                    Some(target_group_id)
                } else {
                    let source_group = &source.feature_placement_groups[source_group_id as usize];
                    let placement_start = target.feature_placements.len() as u32;
                    target.feature_placements.extend(
                        source_group
                            .placements
                            .slice(&source.feature_placements)
                            .iter()
                            .map(|&placement| transform.concat(placement)),
                    );
                    let target_group_id = target.feature_placement_groups.len() as u32;
                    target.feature_placement_groups.push(FeaturePlacementGroup {
                        placements: Span::new(
                            placement_start,
                            target.feature_placements.len() as u32 - placement_start,
                        ),
                        features: Span::new(
                            target.features.len() as u32,
                            source_group.features.count,
                        ),
                    });
                    placement_groups.insert(source_group_id, target_group_id);
                    Some(target_group_id)
                }
            } else {
                None
            };
            let path_start = target.arena.paths.len() as u32;
            for path_index in feature.paths.indices() {
                target.arena.append_path_from(
                    &source.arena,
                    path_index,
                    if target_placement_group.is_some() {
                        Affine2::IDENTITY
                    } else {
                        transform
                    },
                );
            }
            let path_count = target.arena.paths.len() as u32 - path_start;
            let paths = Span::new(path_start, path_count);
            let bbox = if target_placement_group.is_some() {
                feature.bbox.transformed(transform)
            } else {
                target.arena.paths_bbox(paths)
            };

            let mut feature = feature.clone();
            feature.spec_refs = spec_refs;
            if target_placement_group.is_none() {
                feature.transform = transform.concat(feature.transform);
                feature.center = transform.transform_point(feature.center);
            }
            feature.bbox = bbox;
            feature.paths = paths;
            feature.placement_group = target_placement_group;
            feature.source_instance = source_instance;
            feature.set = Some(target_set);
            feature.source.set_index = feature
                .source
                .set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?;
            feature.pin_refs =
                append_span(&mut target.pin_refs, &source.pin_refs, feature.pin_refs);
            target.features.push(feature);
            let target_set_record = &mut target.feature_sets[target_set as usize];
            target_set_record.features.count += 1;
            target_set_record.bbox = target_set_record.bbox.union(bbox);
            layer_bbox = layer_bbox.union(bbox);
        }
    }

    Ok(layer_bbox)
}

fn append_layout_geometry(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    steps: &[Step],
    primary_step: &Step,
) -> Result<()> {
    if is_panel_step(primary_step) {
        append_panel_geometry(doc, ipc, steps, primary_step)
    } else if is_board_step(primary_step) {
        doc.layout.root_step = Some(ensure_layout_step_for_step(doc, primary_step));
        Ok(())
    } else {
        Ok(())
    }
}

fn append_panel_geometry(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    steps: &[Step],
    panel_step: &Step,
) -> Result<()> {
    let panel_profiles = append_step_profile(doc, panel_step);
    let root_layout_step = push_or_update_layout_step(doc, panel_step, panel_profiles);
    doc.layout.root_step = Some(root_layout_step);
    let context = LayoutBuildContext { ipc, steps };
    let parent = LayoutParent {
        step: panel_step,
        transform: Affine2::identity(),
        layout_step: root_layout_step,
        instance: None,
    };
    let mut stack = vec![panel_step.name];
    append_layout_repeats(doc, &context, parent, &mut stack)?;

    Ok(())
}

fn append_layout_repeats(
    doc: &mut GeometryDocument,
    context: &LayoutBuildContext<'_>,
    parent: LayoutParent<'_>,
    stack: &mut Vec<Symbol>,
) -> Result<()> {
    for repeat in &parent.step.step_repeats {
        let source_step = context
            .steps
            .iter()
            .find(|step| step.name == repeat.step_ref)
            .with_context(|| {
                format!(
                    "StepRepeat references unknown Step '{}'",
                    context.ipc.resolve(repeat.step_ref)
                )
            })?;

        if stack.contains(&source_step.name) {
            bail!(
                "StepRepeat cycle references Step '{}'",
                context.ipc.resolve(source_step.name)
            );
        }

        let child_layout_step = ensure_layout_step_for_step(doc, source_step);
        let layout_repeat = push_layout_repeat(
            doc,
            parent.layout_step,
            parent.instance,
            child_layout_step,
            source_step.name,
            repeat,
        );

        // Bound expansion where it happens, including nested repeats. Empty
        // repeats keep their metadata without iterating a potentially huge ny.
        if repeat.nx == 0 || repeat.ny == 0 {
            continue;
        }
        if stack.len() >= 64 {
            bail!("IPC layout nesting exceeds the limit of 64 Steps");
        }
        if (repeat.nx as usize)
            .checked_mul(repeat.ny as usize)
            .and_then(|count| doc.layout.instances.len().checked_add(count))
            .is_none_or(|count| count > 100_000)
        {
            bail!("IPC layout exceeds the limit of 100000 Step instances");
        }

        let mut pending_panel_instances = Vec::new();
        for iy in 0..repeat.ny {
            for ix in 0..repeat.nx {
                let transform = parent
                    .transform
                    .concat(step_repeat_transform(repeat, ix, iy));
                let layout_instance = push_layout_instance(
                    doc,
                    LayoutInstanceSpec {
                        repeat: layout_repeat,
                        parent_instance: parent.instance,
                        child_step: child_layout_step,
                        source_step_ref: source_step.name,
                        parent_step_ref: parent.step.name,
                        transform,
                        repeat_index_x: ix,
                        repeat_index_y: iy,
                        repeat_count_x: repeat.nx,
                        repeat_count_y: repeat.ny,
                        repeat_pitch_x: repeat.dx,
                        repeat_pitch_y: repeat.dy,
                    },
                );
                if is_panel_step(source_step) {
                    pending_panel_instances.push((source_step, transform, layout_instance));
                }
            }
        }

        for (source_step, transform, layout_instance) in pending_panel_instances {
            stack.push(source_step.name);
            append_layout_repeats(
                doc,
                context,
                LayoutParent {
                    step: source_step,
                    transform,
                    layout_step: child_layout_step,
                    instance: Some(layout_instance),
                },
                stack,
            )?;
            stack.pop();
        }
    }

    Ok(())
}

fn ensure_layout_step_for_step(doc: &mut GeometryDocument, step: &Step) -> u32 {
    if let Some(index) = doc
        .layout
        .steps
        .iter()
        .position(|layout_step| layout_step.source_step_ref == step.name)
    {
        return index as u32;
    }

    let profiles = append_step_profile(doc, step);
    push_or_update_layout_step(doc, step, profiles)
}

fn push_or_update_layout_step(
    doc: &mut GeometryDocument,
    step: &Step,
    profiles: ProfileRange,
) -> u32 {
    if let Some(index) = doc
        .layout
        .steps
        .iter()
        .position(|layout_step| layout_step.source_step_ref == step.name)
    {
        let layout_step = &mut doc.layout.steps[index];
        if layout_step.profiles.is_empty() && profiles.count > 0 {
            layout_step.profiles = Span::new(profiles.start, profiles.count);
            layout_step.bbox = profiles.bbox;
        }
        return index as u32;
    }

    let index = doc.layout.steps.len() as u32;
    doc.layout.steps.push(LayoutStep {
        source_step_ref: step.name,
        kind: layout_step_kind(step),
        purpose: LayoutPurpose::Product,
        datum: step
            .datum
            .map(|datum| Point::new(datum.x, datum.y))
            .unwrap_or_default(),
        profiles: Span::new(profiles.start, profiles.count),
        bbox: profiles.bbox,
    });
    index
}

fn push_layout_repeat(
    doc: &mut GeometryDocument,
    parent_step: u32,
    parent_instance: Option<u32>,
    child_step: u32,
    source_step_ref: Symbol,
    repeat: &StepRepeat,
) -> u32 {
    let repeat_index = doc.layout.repeats.len() as u32;

    doc.layout.repeats.push(LayoutRepeat {
        parent_step,
        parent_instance,
        child_step,
        source_step_ref,
        x: repeat.x,
        y: repeat.y,
        nx: repeat.nx,
        ny: repeat.ny,
        dx: repeat.dx,
        dy: repeat.dy,
        angle: repeat.angle,
        mirror: repeat.mirror,
        instances: Span::new(doc.layout.instances.len() as u32, 0),
        bbox: BBox::empty(),
    });
    repeat_index
}

fn push_layout_instance(doc: &mut GeometryDocument, spec: LayoutInstanceSpec) -> u32 {
    let instance_index = doc.layout.instances.len() as u32;
    let repeat_record = &mut doc.layout.repeats[spec.repeat as usize];
    if repeat_record.instances.is_empty() {
        repeat_record.instances.start = instance_index;
    }
    repeat_record.instances.count += 1;

    doc.layout.instances.push(LayoutInstance {
        repeat: spec.repeat,
        parent_instance: spec.parent_instance,
        child_step: spec.child_step,
        source_step_ref: spec.source_step_ref,
        parent_step_ref: spec.parent_step_ref,
        transform: spec.transform,
        repeat_index_x: spec.repeat_index_x,
        repeat_index_y: spec.repeat_index_y,
        repeat_count_x: spec.repeat_count_x,
        repeat_count_y: spec.repeat_count_y,
        repeat_pitch_x: spec.repeat_pitch_x,
        repeat_pitch_y: spec.repeat_pitch_y,
        bbox: BBox::empty(),
    });
    instance_index
}

fn layout_step_kind(step: &Step) -> LayoutStepKind {
    match step.step_type {
        Some(StepType::Board) => LayoutStepKind::Board,
        Some(StepType::Pallet) => LayoutStepKind::Panel,
        Some(StepType::Ic) => LayoutStepKind::Ic,
        None if !step.step_repeats.is_empty() => LayoutStepKind::Panel,
        None => LayoutStepKind::Board,
    }
}

pub fn is_panel_step(step: &Step) -> bool {
    matches!(step.step_type, Some(StepType::Pallet))
        || (step.step_type.is_none() && !step.step_repeats.is_empty())
}

fn is_board_step(step: &Step) -> bool {
    matches!(step.step_type, Some(StepType::Board))
        || (step.step_type.is_none() && step.step_repeats.is_empty())
}

fn slot_applies_to_layer(
    source_layer: &Layer,
    target_layer: &Layer,
    layers: &[Layer],
    slot: &ipc2581::types::Slot,
) -> bool {
    if source_layer.name != target_layer.name && target_layer.layer_function.is_fabrication() {
        return false;
    }

    if slot.z_axis_dim {
        return source_layer.name == target_layer.name;
    }

    layer_span_applies_to_layer(source_layer, target_layer, layers)
}

fn layer_span_applies_to_layer(
    source_layer: &Layer,
    target_layer: &Layer,
    layers: &[Layer],
) -> bool {
    if source_layer.name == target_layer.name {
        return true;
    }

    let Some(span) = source_layer.span else {
        return false;
    };

    let Some(target_index) = layer_index(layers, target_layer.name) else {
        return false;
    };
    let from_index = span
        .from_layer
        .and_then(|layer| layer_index(layers, layer))
        .unwrap_or(0);
    let to_index = span
        .to_layer
        .and_then(|layer| layer_index(layers, layer))
        .unwrap_or(layers.len().saturating_sub(1));
    let start = from_index.min(to_index);
    let end = from_index.max(to_index);

    (start..=end).contains(&target_index)
}

fn layer_index(layers: &[Layer], layer_ref: Symbol) -> Option<usize> {
    layers.iter().position(|layer| layer.name == layer_ref)
}

fn extract_pad(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    pad: &ipc2581::types::Pad,
    doc: &mut GeometryDocument,
) -> Result<Option<GeometryFeature>> {
    let Some(padstack_ref) = pad.padstack_def_ref else {
        doc.warn("Skipping pad without PadStackDefRef");
        return Ok(None);
    };
    let Some(x) = pad.x else {
        doc.warn("Skipping pad without x coordinate");
        return Ok(None);
    };
    let Some(y) = pad.y else {
        doc.warn("Skipping pad without y coordinate");
        return Ok(None);
    };
    // A pad may carry its own primitive, so a padstack definition refines
    // (hole plating, per-layer shapes) rather than gates.
    let padstack = context.padstacks.get(&padstack_ref).copied();

    let role = match padstack
        .and_then(|padstack| padstack.hole_def.as_ref())
        .map(|hole| hole.plating_status)
    {
        Some(PlatingStatus::Via | PlatingStatus::ViaCapped) => FeatureRole::Via,
        _ => FeatureRole::Pad,
    };

    let Some(primitive_ref) = pad_primitive_ref(pad, padstack, layer_ref) else {
        doc.warn(format!(
            "Skipping padstack '{}' because it has no regular primitive for layer '{}'",
            context.strings.resolve(padstack_ref),
            context.strings.resolve(layer_ref)
        ));
        return Ok(None);
    };
    let placement = ipc_placement(Point::new(x, y), pad.xform);

    let path_start = doc.arena.paths.len() as u32;
    let paint = match primitive_ref {
        PrimitiveRef::Standard(primitive_ref) => {
            let Some(primitive) = context.standard_primitives.get(&primitive_ref).copied() else {
                doc.warn(format!(
                    "Skipping padstack '{}' because primitive '{}' is missing",
                    context.strings.resolve(padstack_ref),
                    context.strings.resolve(primitive_ref)
                ));
                return Ok(None);
            };
            lower_standard_primitive(context, doc, primitive, placement.transform)?
        }
        PrimitiveRef::User(primitive_ref) => {
            let Some(primitive) = context.user_primitives.get(&primitive_ref).copied() else {
                doc.warn(format!(
                    "Skipping padstack '{}' because user primitive '{}' is missing",
                    context.strings.resolve(padstack_ref),
                    context.strings.resolve(primitive_ref)
                ));
                return Ok(None);
            };
            lower_user_primitive(context, doc, primitive, placement.transform)
        }
    };
    let path_count = doc.arena.paths.len() as u32 - path_start;
    if path_count == 0 {
        return Ok(None);
    }
    let paths = Span::new(path_start, path_count);
    let bbox = doc.arena.paths_bbox(paths);

    let mut feature = GeometryFeature::new(
        FeatureKind::Padstack,
        if paint == PrimitivePaint::Void {
            GeometryPolarity::Clear
        } else {
            polarity
        },
    );
    feature.net = net;
    feature.source = source;
    feature.bbox = bbox;
    feature.paths = paths;
    feature.intent.role = role;
    apply_ipc_placement(&mut feature, placement);
    feature.padstack_ref = Some(padstack_ref);
    feature.primitive_ref = Some(primitive_ref);
    feature.intent.plating = padstack
        .and_then(|padstack| padstack.hole_def.as_ref())
        .map(|hole| plating_kind(hole.plating_status))
        .unwrap_or(PlatingKind::None);
    feature.flags.expanded_padstack = true;
    feature.flags.lowered_to_paths = true;
    feature.flags.clears_previous_in_set = paint == PrimitivePaint::Void;
    if let Some(pin_ref) = &pad.pin_ref {
        feature.pin_refs = Span::new(doc.pin_refs.len() as u32, 1);
        doc.pin_refs.push(PinRef {
            component_ref: pin_ref.component_ref,
            pin: pin_ref.pin,
            title: pin_ref.title,
        });
    }

    Ok(Some(feature))
}

fn pad_primitive_ref(
    pad: &ipc2581::types::Pad,
    padstack: Option<&ipc2581::types::PadStackDef>,
    layer_ref: Symbol,
) -> Option<PrimitiveRef<Symbol>> {
    let pad_defs = padstack.map_or(&[][..], |padstack| &padstack.pad_defs);
    let pad_def = pad_defs
        .iter()
        .find(|pad_def| pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Regular)
        .or_else(|| {
            pad_defs.iter().find(|pad_def| {
                pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Thermal
            })
        });
    pad.standard_primitive_ref
        .map(PrimitiveRef::Standard)
        .or_else(|| pad.user_primitive_ref.map(PrimitiveRef::User))
        .or_else(|| {
            pad_def.and_then(|pad_def| {
                pad_def
                    .standard_primitive_ref
                    .map(PrimitiveRef::Standard)
                    .or_else(|| pad_def.user_primitive_ref.map(PrimitiveRef::User))
            })
        })
}

#[derive(Debug, Clone, Copy)]
enum FeaturePrimitiveKind {
    Standard,
    User,
}

fn extract_feature_primitive(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    primitive_ref: &ipc2581::types::ecad::FeaturePrimitiveRef,
    primitive_kind: FeaturePrimitiveKind,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let transform = Affine2::placement(
        Point::new(primitive_ref.x, primitive_ref.y),
        0.0,
        Mirror::NONE,
        1.0,
    );
    let path_start = doc.arena.paths.len() as u32;
    let (paint, primitive_ref) = match primitive_kind {
        FeaturePrimitiveKind::Standard => {
            let Some(primitive) = context.standard_primitives.get(&primitive_ref.id).copied()
            else {
                doc.warn(format!(
                    "Skipping feature because standard primitive '{}' is missing",
                    context.strings.resolve(primitive_ref.id)
                ));
                return Ok(Vec::new());
            };
            (
                lower_standard_primitive(context, doc, primitive, transform)?,
                PrimitiveRef::Standard(primitive_ref.id),
            )
        }
        FeaturePrimitiveKind::User => {
            let Some(primitive) = context.user_primitives.get(&primitive_ref.id).copied() else {
                doc.warn(format!(
                    "Skipping feature because user primitive '{}' is missing",
                    context.strings.resolve(primitive_ref.id)
                ));
                return Ok(Vec::new());
            };
            (
                lower_user_primitive(context, doc, primitive, transform),
                PrimitiveRef::User(primitive_ref.id),
            )
        }
    };

    primitive_features_from_paths(
        doc,
        primitive_path_feature(
            net,
            polarity,
            source,
            transform,
            path_start,
            paint,
            Some(primitive_ref),
        ),
    )
}

fn extract_inline_user_primitive(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    primitive: &ipc2581::types::ecad::FeatureUserPrimitive,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let transform =
        Affine2::placement(Point::new(primitive.x, primitive.y), 0.0, Mirror::NONE, 1.0);
    let path_start = doc.arena.paths.len() as u32;
    let paint = lower_user_primitive(context, doc, &primitive.primitive, transform);
    primitive_features_from_paths(
        doc,
        primitive_path_feature(net, polarity, source, transform, path_start, paint, None),
    )
}

fn primitive_path_feature(
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    transform: Affine2,
    path_start: u32,
    paint: PrimitivePaint,
    primitive_ref: Option<PrimitiveRef<Symbol>>,
) -> GeometryFeature {
    let mut feature = GeometryFeature::new(
        FeatureKind::Primitive,
        if paint == PrimitivePaint::Void {
            GeometryPolarity::Clear
        } else {
            polarity
        },
    );
    feature.net = net;
    feature.source = source;
    feature.transform = transform;
    feature.paths = Span::new(path_start, 0);
    feature.primitive_ref = primitive_ref;
    feature.flags.lowered_to_paths = true;
    feature
}

fn primitive_features_from_paths(
    doc: &GeometryDocument,
    mut feature: GeometryFeature,
) -> Result<Vec<GeometryFeature>> {
    feature.paths.count = doc.arena.paths.len() as u32 - feature.paths.start;
    if feature.paths.is_empty() {
        return Ok(Vec::new());
    }
    process::split_primitive_feature_path_runs(doc, feature).map_err(|error| {
        anyhow::anyhow!("failed to split IPC primitive into homogeneous path features: {error}")
    })
}

fn extract_fiducial(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    fiducial: &ipc2581::types::ecad::Fiducial,
    doc: &mut GeometryDocument,
) -> Result<Option<GeometryFeature>> {
    let placement = ipc_placement(
        Point::new(fiducial.location.x, fiducial.location.y),
        fiducial.xform,
    );

    let path_start = doc.arena.paths.len() as u32;
    let (paint, primitive_ref, outer_diameter) = match &fiducial.shape {
        ipc2581::types::ecad::FiducialShape::Primitive(primitive) => (
            lower_standard_primitive(context, doc, primitive, placement.transform)?,
            None,
            standard_primitive_outer_diameter(primitive),
        ),
        ipc2581::types::ecad::FiducialShape::StandardPrimitiveRef(primitive_ref) => {
            let Some(primitive) = context.standard_primitives.get(primitive_ref).copied() else {
                doc.warn(format!(
                    "Skipping fiducial because standard primitive '{}' is missing",
                    context.strings.resolve(*primitive_ref)
                ));
                return Ok(None);
            };
            (
                lower_standard_primitive(context, doc, primitive, placement.transform)?,
                Some(PrimitiveRef::Standard(*primitive_ref)),
                standard_primitive_outer_diameter(primitive),
            )
        }
    };

    let path_count = doc.arena.paths.len() as u32 - path_start;
    if path_count == 0 {
        return Ok(None);
    }
    let paths = Span::new(path_start, path_count);

    let mut feature = GeometryFeature::new(
        FeatureKind::Primitive,
        if paint == PrimitivePaint::Void {
            GeometryPolarity::Clear
        } else {
            polarity
        },
    );
    feature.net = net;
    feature.source = source;
    feature.intent.role = FeatureRole::Fiducial;
    feature.fiducial_kind = map_fiducial_kind(fiducial.kind);
    feature.bbox = doc.arena.paths_bbox(paths);
    feature.paths = paths;
    apply_ipc_placement(&mut feature, placement);
    feature.outer_diameter = outer_diameter.unwrap_or_default();
    feature.primitive_ref = primitive_ref;
    feature.flags.lowered_to_paths = true;
    if let Some(pin_ref) = &fiducial.pin_ref {
        feature.pin_refs = Span::new(doc.pin_refs.len() as u32, 1);
        doc.pin_refs.push(PinRef {
            component_ref: pin_ref.component_ref,
            pin: pin_ref.pin,
            title: pin_ref.title,
        });
    }
    Ok(Some(feature))
}

fn map_fiducial_kind(kind: ipc2581::types::ecad::FiducialKind) -> FiducialKind {
    match kind {
        ipc2581::types::ecad::FiducialKind::BadBoardMark => FiducialKind::BadBoard,
        ipc2581::types::ecad::FiducialKind::Global => FiducialKind::Global,
        ipc2581::types::ecad::FiducialKind::GoodPanelMark => FiducialKind::GoodPanel,
        ipc2581::types::ecad::FiducialKind::Local => FiducialKind::Local,
    }
}

fn standard_primitive_outer_diameter(primitive: &StandardPrimitive) -> Option<f64> {
    match primitive {
        StandardPrimitive::Circle(circle) => Some(circle.shape.diameter),
        StandardPrimitive::Donut(donut) => Some(donut.shape.outer_diameter),
        _ => None,
    }
}

fn extract_trace(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    trace: &ipc2581::types::Trace,
    doc: &mut GeometryDocument,
) -> Option<GeometryFeature> {
    if trace.points.is_empty() {
        return None;
    }
    let line_desc_ref = match trace.line_desc_ref {
        Some(line_desc_ref) => line_desc_ref,
        None => {
            doc.warn("Skipping trace without LineDescRef");
            return None;
        }
    };
    let Some(line_desc) = context.line_descs.get(&line_desc_ref).copied() else {
        doc.warn(format!(
            "Skipping trace referencing missing LineDesc '{}'",
            context.strings.resolve(line_desc_ref)
        ));
        return None;
    };

    Some(push_stroked_trace(
        doc,
        StrokedFeatureStyle {
            net,
            polarity,
            source,
            width: line_desc.line_width,
            line_cap: map_line_cap(line_desc.line_end),
            line_pattern: map_line_pattern(line_desc.line_property),
        },
        trace,
    ))
}

fn extract_line(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    line: &ipc2581::types::ecad::Line,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let (line_width, line_cap, line_pattern) = resolve_feature_line_style(
        context,
        line.line_desc_ref,
        line.line_width,
        line.line_end,
        line.line_property,
    );

    push_stroked_polyline(
        doc,
        StrokedFeatureStyle {
            net,
            polarity,
            source,
            width: line_width,
            line_cap,
            line_pattern,
        },
        vec![
            Point::new(line.start_x, line.start_y),
            Point::new(line.end_x, line.end_y),
        ],
    )
}

fn extract_feature_polyline(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    polyline: &ipc2581::types::ecad::FeaturePolyline,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let (line_width, line_cap, line_pattern) = resolve_feature_line_style(
        context,
        polyline.line_desc_ref,
        polyline.line_width,
        polyline.line_end,
        polyline.line_property,
    );

    push_stroked_steps(
        doc,
        StrokedFeatureStyle {
            net,
            polarity,
            source,
            width: line_width,
            line_cap,
            line_pattern,
        },
        Point::new(polyline.begin.x, polyline.begin.y),
        &polyline.steps,
    )
}

fn extract_arc(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    arc: &ipc2581::types::ecad::FeatureArc,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let (line_width, line_cap, line_pattern) = resolve_feature_line_style(
        context,
        arc.line_desc_ref,
        arc.line_width,
        arc.line_end,
        arc.line_property,
    );

    push_stroked_arc(
        doc,
        StrokedFeatureStyle {
            net,
            polarity,
            source,
            width: line_width,
            line_cap,
            line_pattern,
        },
        Point::new(arc.start.x, arc.start.y),
        Point::new(arc.end.x, arc.end.y),
        Point::new(arc.center.x, arc.center.y),
        arc.clockwise,
    )
}

fn resolve_feature_line_style(
    context: &ExtractContext<'_>,
    line_desc_ref: Option<Symbol>,
    inline_width: f64,
    inline_end: Option<LineEnd>,
    inline_property: Option<LineProperty>,
) -> (f64, LineCap, LinePattern) {
    let line_desc =
        line_desc_ref.and_then(|line_desc_ref| context.line_descs.get(&line_desc_ref).copied());
    let width = line_desc
        .map(|desc| desc.line_width)
        .unwrap_or(inline_width);
    let line_cap = line_desc
        .map(|desc| map_line_cap(desc.line_end))
        .or_else(|| inline_end.map(map_line_cap))
        .unwrap_or(LineCap::Round);
    let line_pattern = map_line_pattern(
        line_desc
            .and_then(|desc| desc.line_property)
            .or(inline_property),
    );
    (width, line_cap, line_pattern)
}

fn extract_polygon(
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    polygon: &ipc2581::types::Polygon,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let path_start = doc.arena.paths.len() as u32;
    push_polygon_path(doc, polygon, Affine2::identity(), FillRule::NonZero);
    let paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);

    let mut feature = GeometryFeature::new(FeatureKind::Polygon, polarity);
    feature.net = net;
    feature.source = source;
    feature.bbox = doc.arena.paths_bbox(paths);
    feature.paths = paths;
    feature.flags.lowered_to_paths = true;
    feature
}

fn append_step_profile(doc: &mut GeometryDocument, step: &Step) -> ProfileRange {
    let start = doc.profiles.len() as u32;
    let Some(profile) = &step.profile else {
        return ProfileRange {
            start,
            count: 0,
            bbox: BBox::empty(),
        };
    };

    let outer_path = push_profile_polygon(doc, &profile.polygon);
    let cutout_start = doc.profile_cutouts.len() as u32;
    for cutout in &profile.cutouts {
        let path = push_profile_polygon(doc, cutout);
        doc.profile_cutouts.push(StepProfileCutout {
            path,
            bbox: doc.arena.paths[path as usize].bbox,
        });
    }
    let cutout_count = doc.profile_cutouts.len() as u32 - cutout_start;
    let bbox = doc.arena.paths[outer_path as usize].bbox;
    doc.profiles.push(StepProfile {
        outer_path,
        cutouts: Span::new(cutout_start, cutout_count),
        bbox,
    });
    ProfileRange {
        start,
        count: doc.profiles.len() as u32 - start,
        bbox,
    }
}

fn push_profile_polygon(doc: &mut GeometryDocument, polygon: &ipc2581::types::Polygon) -> u32 {
    let contour = polygon_contour(polygon, Affine2::identity());
    doc.push_path(Paint::None, [contour])
}

fn push_stroked_polyline(
    doc: &mut GeometryDocument,
    style: StrokedFeatureStyle,
    points: Vec<Point>,
) -> GeometryFeature {
    let mut bbox = BBox::empty();
    let mut cmds = Vec::new();
    for (index, point) in points.iter().copied().enumerate() {
        bbox.include_point(point);
        cmds.push(if index == 0 {
            PathCmd::move_to(point)
        } else {
            PathCmd::line_to(point)
        });
    }

    let path_start = doc.arena.paths.len() as u32;
    doc.push_path(stroked_paint(style), [ContourBuf::from_parts(bbox, cmds)]);
    bbox = bbox.expand(style.width / 2.0);

    let mut feature = GeometryFeature::new(FeatureKind::Trace, style.polarity);
    feature.net = style.net;
    feature.source = style.source;
    feature.bbox = bbox;
    feature.paths = Span::new(path_start, 1);
    feature.stroke_width = style.width;
    feature.line_cap = style.line_cap;
    feature.flags.lowered_to_paths = true;
    feature
}

fn push_stroked_arc(
    doc: &mut GeometryDocument,
    style: StrokedFeatureStyle,
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
) -> GeometryFeature {
    let bbox = Arc::new(start, end, center, clockwise).bbox();

    let path_start = doc.arena.paths.len() as u32;
    doc.push_path(
        stroked_paint(style),
        [ContourBuf::from_parts(
            bbox,
            vec![
                PathCmd::move_to(start),
                PathCmd::arc_to(end, center, clockwise),
            ],
        )],
    );
    let bbox = bbox.expand(style.width / 2.0);

    let mut feature = GeometryFeature::new(FeatureKind::Trace, style.polarity);
    feature.net = style.net;
    feature.source = style.source;
    feature.bbox = bbox;
    feature.paths = Span::new(path_start, 1);
    feature.stroke_width = style.width;
    feature.line_cap = style.line_cap;
    feature.flags.lowered_to_paths = true;
    feature
}

fn push_stroked_trace(
    doc: &mut GeometryDocument,
    style: StrokedFeatureStyle,
    trace: &ipc2581::types::Trace,
) -> GeometryFeature {
    if trace.steps.is_empty() {
        let points = trace
            .points
            .iter()
            .map(|point| Point::new(point.x, point.y))
            .collect();
        return push_stroked_polyline(doc, style, points);
    }

    push_stroked_steps(
        doc,
        style,
        Point::new(trace.points[0].x, trace.points[0].y),
        &trace.steps,
    )
}

fn push_stroked_steps(
    doc: &mut GeometryDocument,
    style: StrokedFeatureStyle,
    begin: Point,
    steps: &[PolyStep],
) -> GeometryFeature {
    let mut current = begin;
    let mut bbox = BBox::from_point(current);
    let mut cmds = vec![PathCmd::move_to(current)];
    for step in steps {
        match step {
            PolyStep::Segment(segment) => {
                current = Point::new(segment.point.x, segment.point.y);
                bbox.include_point(current);
                cmds.push(PathCmd::line_to(current));
            }
            PolyStep::Curve(curve) => {
                let end = Point::new(curve.point.x, curve.point.y);
                let center = Point::new(curve.center.x, curve.center.y);
                bbox = bbox.union(Arc::new(current, end, center, curve.clockwise).bbox());
                cmds.push(PathCmd::arc_to(end, center, curve.clockwise));
                current = end;
            }
        }
    }

    let path_start = doc.arena.paths.len() as u32;
    doc.push_path(stroked_paint(style), [ContourBuf::from_parts(bbox, cmds)]);
    bbox = bbox.expand(style.width / 2.0);

    let mut feature = GeometryFeature::new(FeatureKind::Trace, style.polarity);
    feature.net = style.net;
    feature.source = style.source;
    feature.bbox = bbox;
    feature.paths = Span::new(path_start, 1);
    feature.stroke_width = style.width;
    feature.line_cap = style.line_cap;
    feature.flags.lowered_to_paths = true;
    feature
}

fn stroked_paint(style: StrokedFeatureStyle) -> Paint {
    let mut stroke = StrokeStyle::new(style.width, style.line_cap);
    stroke.pattern = style.line_pattern;
    Paint::Stroke(stroke)
}

fn extract_hole(
    source: SourceRef,
    padstack_ref: Option<Symbol>,
    hole: &ipc2581::types::Hole,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let placement = ipc_placement(Point::new(hole.x, hole.y), hole.xform);
    let path_start = doc.arena.paths.len() as u32;
    match hole.shape {
        IpcHoleShape::Circle => {
            push_ellipse_path(doc, placement.transform, hole.diameter, hole.diameter)
        }
        IpcHoleShape::Square => {
            push_rect_path(doc, placement.transform, hole.diameter, hole.diameter)
        }
    }
    let paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);

    let mut feature = GeometryFeature::new(FeatureKind::Hole, GeometryPolarity::Dark);
    feature.source = source;
    feature.source_name = hole.name;
    feature.spec_refs = push_spec_refs(doc, &hole.spec_refs);
    feature.bbox = doc.arena.paths_bbox(paths);
    feature.paths = paths;
    apply_ipc_placement(&mut feature, placement);
    feature.hole_shape = match hole.shape {
        IpcHoleShape::Circle => HoleShape::Round,
        IpcHoleShape::Square => HoleShape::Square,
    };
    feature.outer_diameter = hole.diameter * feature.scale;
    feature.padstack_ref = padstack_ref;
    feature.intent.plating = plating_kind(hole.plating_status);
    feature.flags.lowered_to_paths = true;
    feature
}

fn extract_slot(
    context: &ExtractContext<'_>,
    source: SourceRef,
    padstack_ref: Option<Symbol>,
    slot: &ipc2581::types::Slot,
    doc: &mut GeometryDocument,
) -> Result<GeometryFeature> {
    let placement = ipc_placement(Point::new(slot.x, slot.y), slot.xform);
    let path_start = doc.arena.paths.len() as u32;
    let mut primitive_size = None;

    match &slot.shape {
        SlotShape::Outline(polygon) => {
            push_polygon_path(doc, polygon, placement.transform, FillRule::NonZero);
        }
        SlotShape::Primitive(primitive) => {
            if let StandardPrimitive::Oval(oval) = primitive {
                primitive_size = Some((oval.shape.size.width, oval.shape.size.height));
            }
            let _ = lower_standard_primitive(context, doc, primitive, placement.transform)?;
        }
    }

    let paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);
    let mut feature = GeometryFeature::new(FeatureKind::Slot, GeometryPolarity::Dark);
    feature.source = source;
    feature.source_name = slot.name;
    feature.bbox = doc.arena.paths_bbox(paths);
    feature.paths = paths;
    apply_ipc_placement(&mut feature, placement);
    if let Some((width, height)) = primitive_size {
        feature.width = width;
        feature.height = height;
        feature.outer_diameter = width.min(height) * feature.scale;
        feature.stroke_width = feature.outer_diameter;
    }
    feature.padstack_ref = padstack_ref;
    feature.intent.plating = plating_kind(slot.plating_status);
    feature.flags.lowered_to_paths = true;
    Ok(feature)
}

fn lower_standard_primitive(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    primitive: &StandardPrimitive,
    transform: Affine2,
) -> Result<PrimitivePaint> {
    let paint = primitive_paint(context, primitive);
    if standard_primitive_has_no_area(primitive) {
        return Ok(paint);
    }

    let path_start = doc.arena.paths.len() as u32;
    match primitive {
        StandardPrimitive::Circle(circle) => {
            push_ellipse_path(doc, transform, circle.shape.diameter, circle.shape.diameter);
        }
        StandardPrimitive::Ellipse(ellipse) => {
            push_ellipse_path(
                doc,
                transform,
                ellipse.shape.size.width,
                ellipse.shape.size.height,
            );
        }
        StandardPrimitive::Oval(oval) => {
            push_oval_path(
                doc,
                transform,
                oval.shape.size.width,
                oval.shape.size.height,
            );
        }
        StandardPrimitive::RectCenter(rect) => {
            push_rect_path(
                doc,
                transform,
                rect.shape.size.width,
                rect.shape.size.height,
            );
        }
        StandardPrimitive::RectCorner(rect) => {
            let points = vec![
                Point::new(rect.shape.lower_left.x, rect.shape.lower_left.y),
                Point::new(rect.shape.upper_right.x, rect.shape.lower_left.y),
                Point::new(rect.shape.upper_right.x, rect.shape.upper_right.y),
                Point::new(rect.shape.lower_left.x, rect.shape.upper_right.y),
            ];
            push_closed_points_path(doc, transform, points, FillRule::NonZero);
        }
        StandardPrimitive::Diamond(diamond) => {
            let hw = diamond.shape.size.width / 2.0;
            let hh = diamond.shape.size.height / 2.0;
            push_closed_points_path(
                doc,
                transform,
                vec![
                    Point::new(0.0, -hh),
                    Point::new(hw, 0.0),
                    Point::new(0.0, hh),
                    Point::new(-hw, 0.0),
                ],
                FillRule::NonZero,
            );
        }
        StandardPrimitive::Hexagon(hexagon) => {
            push_regular_polygon_path(doc, transform, 6, hexagon.shape.point_to_point / 2.0);
        }
        StandardPrimitive::Octagon(octagon) => {
            push_regular_polygon_path(doc, transform, 8, octagon.shape.point_to_point / 2.0);
        }
        StandardPrimitive::Triangle(triangle) => {
            let hw = triangle.shape.base / 2.0;
            let hh = triangle.shape.height / 2.0;
            push_closed_points_path(
                doc,
                transform,
                vec![
                    Point::new(0.0, -hh),
                    Point::new(hw, hh),
                    Point::new(-hw, hh),
                ],
                FillRule::NonZero,
            );
        }
        StandardPrimitive::Donut(donut) => {
            push_donut_path(
                doc,
                transform,
                donut.shape.outer_diameter,
                donut.shape.inner_diameter,
            );
        }
        StandardPrimitive::Thermal(thermal) => {
            let spoke_width = thermal
                .shape
                .spoke_width
                .unwrap_or(thermal.shape.outer_diameter - thermal.shape.inner_diameter)
                .max(0.0);
            push_thermal_path(
                doc,
                transform,
                thermal.shape.outer_diameter,
                thermal.shape.inner_diameter,
                spoke_width,
                thermal.shape.spoke_count,
                thermal.shape.spoke_start_angle.unwrap_or(45.0),
            );
        }
        StandardPrimitive::Contour(contour) => {
            push_contour_path(doc, contour, transform);
        }
        StandardPrimitive::RectRound(rect) => {
            push_rounded_rect_path(
                doc,
                transform,
                rect.shape.size.width,
                rect.shape.size.height,
                rect.shape.radius,
                [
                    rect.shape.upper_right,
                    rect.shape.lower_right,
                    rect.shape.lower_left,
                    rect.shape.upper_left,
                ],
            );
        }
        StandardPrimitive::RectCham(rect) => {
            push_chamfered_rect_path(
                doc,
                transform,
                rect.shape.size.width,
                rect.shape.size.height,
                rect.shape.chamfer,
                [
                    rect.shape.upper_right,
                    rect.shape.lower_right,
                    rect.shape.lower_left,
                    rect.shape.upper_left,
                ],
            );
        }
        StandardPrimitive::Butterfly(butterfly) => {
            push_butterfly_path(doc, transform, butterfly.shape.shape, butterfly.shape.size);
        }
        StandardPrimitive::Moire(moire) => {
            push_moire_path(doc, transform, moire);
        }
    }

    match paint {
        PrimitivePaint::Fill => {}
        PrimitivePaint::Hollow => {
            let Some(line_desc) = primitive_line_desc(context, primitive) else {
                doc.warn("Skipping hollow primitive without LineDescRef");
                make_paths_unpainted(doc, path_start);
                return Ok(paint);
            };
            make_paths_stroked(
                doc,
                path_start,
                line_desc.line_width,
                map_line_cap(line_desc.line_end),
                map_line_pattern(line_desc.line_property),
            );
        }
        PrimitivePaint::Void => {}
    }

    Ok(paint)
}

fn standard_primitive_has_no_area(primitive: &StandardPrimitive) -> bool {
    match primitive {
        StandardPrimitive::Circle(circle) => circle.shape.diameter <= 0.0,
        StandardPrimitive::Ellipse(ellipse) => {
            ellipse.shape.size.width <= 0.0 || ellipse.shape.size.height <= 0.0
        }
        StandardPrimitive::Oval(oval) => {
            oval.shape.size.width <= 0.0 || oval.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCenter(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCorner(rect) => {
            rect.shape.upper_right.x <= rect.shape.lower_left.x
                || rect.shape.upper_right.y <= rect.shape.lower_left.y
        }
        StandardPrimitive::RectRound(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCham(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::Diamond(diamond) => {
            diamond.shape.size.width <= 0.0 || diamond.shape.size.height <= 0.0
        }
        StandardPrimitive::Hexagon(hexagon) => hexagon.shape.point_to_point <= 0.0,
        StandardPrimitive::Octagon(octagon) => octagon.shape.point_to_point <= 0.0,
        StandardPrimitive::Triangle(triangle) => {
            triangle.shape.base <= 0.0 || triangle.shape.height <= 0.0
        }
        StandardPrimitive::Donut(donut) => {
            donut.shape.outer_diameter <= 0.0
                || donut.shape.inner_diameter >= donut.shape.outer_diameter
        }
        StandardPrimitive::Thermal(thermal) => thermal.shape.outer_diameter <= 0.0,
        StandardPrimitive::Butterfly(_)
        | StandardPrimitive::Contour(_)
        | StandardPrimitive::Moire(_) => false,
    }
}

fn lower_user_primitive(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    primitive: &UserPrimitive,
    transform: Affine2,
) -> PrimitivePaint {
    match primitive {
        UserPrimitive::UserSpecial(user_special) => {
            let mut paint = PrimitivePaint::Fill;
            let mut pending_contours = Vec::new();
            let mut pending_fill_key = user_fill_key(None);
            for shape in &user_special.shapes {
                if user_shape_is_filled_contour(shape) {
                    let fill_key = user_fill_key(shape.fill_desc);
                    if !pending_contours.is_empty() && pending_fill_key != fill_key {
                        flush_user_contours(doc, &mut pending_contours);
                    }
                    pending_fill_key = fill_key;
                    push_user_shape_contours(&mut pending_contours, &shape.shape, transform);
                    continue;
                }
                flush_user_contours(doc, &mut pending_contours);
                let path_start = doc.arena.paths.len() as u32;
                let mut nested_paint = None;
                match &shape.shape {
                    UserShapeType::Circle(circle) => {
                        push_ellipse_path(doc, transform, circle.diameter, circle.diameter);
                    }
                    UserShapeType::RectCenter(rect) => {
                        push_rect_path(doc, transform, rect.size.width, rect.size.height);
                    }
                    UserShapeType::Oval(oval) => {
                        push_oval_path(doc, transform, oval.size.width, oval.size.height);
                    }
                    UserShapeType::RectRound(rect) => {
                        push_rounded_rect_path(
                            doc,
                            transform,
                            rect.size.width,
                            rect.size.height,
                            rect.radius,
                            [
                                rect.upper_right,
                                rect.lower_right,
                                rect.lower_left,
                                rect.upper_left,
                            ],
                        );
                    }
                    UserShapeType::Polygon(polygon) => {
                        push_polygon_path(doc, polygon, transform, FillRule::NonZero);
                    }
                    UserShapeType::Contour(contour) => {
                        push_contour_path(doc, contour, transform);
                    }
                    UserShapeType::Line(line) => {
                        let line_desc = user_shape_line_desc(context, shape);
                        push_user_line_path(doc, line, transform, line_desc);
                    }
                    UserShapeType::Arc(arc) => {
                        let line_desc = user_shape_line_desc(context, shape);
                        push_user_arc_path(doc, arc, transform, line_desc);
                    }
                    UserShapeType::Polyline(polyline) => {
                        let line_desc = user_shape_line_desc(context, shape);
                        push_user_polyline_path(doc, polyline, transform, line_desc);
                    }
                    UserShapeType::UserPrimitiveRef(primitive_ref) => {
                        if let Some(primitive) = context.user_primitives.get(primitive_ref).copied()
                        {
                            nested_paint =
                                Some(lower_user_primitive(context, doc, primitive, transform));
                        } else {
                            make_paths_unpainted(doc, path_start);
                        }
                    }
                }

                match shape.fill_desc {
                    Some(fill_desc) if fill_desc.fill_property == FillProperty::Hollow => {
                        if let Some(line_desc) = user_shape_line_desc(context, shape) {
                            make_paths_stroked(
                                doc,
                                path_start,
                                line_desc.line_width,
                                map_line_cap(line_desc.line_end),
                                map_line_pattern(line_desc.line_property),
                            );
                        } else {
                            make_paths_unpainted(doc, path_start);
                        }
                        paint = PrimitivePaint::Hollow;
                    }
                    Some(fill_desc) if fill_desc.fill_property == FillProperty::Void => {
                        paint = PrimitivePaint::Void;
                    }
                    Some(_) => {}
                    None => {
                        if let Some(nested_paint) = nested_paint {
                            paint = nested_paint;
                        }
                    }
                }
            }
            flush_user_contours(doc, &mut pending_contours);
            paint
        }
    }
}

/// Grouping key for consecutive filled user-shape contours: contours sharing a
/// source fill description are merged into one even-odd compound path.
fn user_fill_key(fill_desc: Option<FillDesc>) -> (FillProperty, Option<f64>, Option<f64>) {
    match fill_desc {
        Some(desc) if matches!(desc.fill_property, FillProperty::Hatch | FillProperty::Mesh) => {
            (desc.fill_property, desc.angle1, desc.angle2)
        }
        Some(desc) => (desc.fill_property, None, None),
        None => (FillProperty::Fill, None, None),
    }
}

fn user_shape_is_filled_contour(shape: &ipc2581::types::UserShape) -> bool {
    matches!(
        shape.fill_desc.map(|desc| desc.fill_property),
        None | Some(FillProperty::Fill | FillProperty::Hatch | FillProperty::Mesh)
    ) && matches!(
        &shape.shape,
        UserShapeType::Polygon(_) | UserShapeType::Contour(_)
    )
}

fn push_user_shape_contours(out: &mut Vec<ContourBuf>, shape: &UserShapeType, transform: Affine2) {
    match shape {
        UserShapeType::Polygon(polygon) => out.push(polygon_contour(polygon, transform)),
        UserShapeType::Contour(contour) => push_contour_payloads(out, contour, transform),
        _ => {}
    }
}

fn flush_user_contours(doc: &mut GeometryDocument, contours: &mut Vec<ContourBuf>) {
    if contours.is_empty() {
        return;
    }
    doc.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        std::mem::take(contours),
    );
}

fn user_shape_line_desc(
    context: &ExtractContext<'_>,
    shape: &ipc2581::types::UserShape,
) -> Option<ipc2581::types::LineDesc> {
    shape.line_desc.or_else(|| {
        shape
            .line_desc_ref
            .and_then(|line_desc_ref| context.line_descs.get(&line_desc_ref).copied())
    })
}

fn push_user_line_path(
    doc: &mut GeometryDocument,
    line: &ipc2581::types::primitives::Line,
    transform: Affine2,
    line_desc: Option<ipc2581::types::LineDesc>,
) {
    let start = transform.transform_point(Point::new(line.start.x, line.start.y));
    let end = transform.transform_point(Point::new(line.end.x, line.end.y));
    let width = line_desc.map(|desc| desc.line_width).unwrap_or(0.25);
    let line_cap = line_desc
        .map(|desc| map_line_cap(desc.line_end))
        .unwrap_or(LineCap::Round);
    let line_pattern = map_line_pattern(line_desc.and_then(|desc| desc.line_property));
    let bbox = BBox::from_point(start).union(BBox::from_point(end));
    let mut stroke = StrokeStyle::new(width, line_cap);
    stroke.pattern = line_pattern;
    doc.push_path(
        Paint::Stroke(stroke),
        [ContourBuf::from_parts(
            bbox,
            vec![PathCmd::move_to(start), PathCmd::line_to(end)],
        )],
    );
}

fn push_user_arc_path(
    doc: &mut GeometryDocument,
    arc: &ipc2581::types::Arc,
    transform: Affine2,
    line_desc: Option<ipc2581::types::LineDesc>,
) {
    let start = transform.transform_point(Point::new(arc.start.x, arc.start.y));
    let end = transform.transform_point(Point::new(arc.end.x, arc.end.y));
    let center = transform.transform_point(Point::new(arc.center.x, arc.center.y));
    let clockwise = if transform.determinant() < 0.0 {
        !arc.clockwise
    } else {
        arc.clockwise
    };
    let width = line_desc.map(|desc| desc.line_width).unwrap_or(0.25);
    let line_cap = line_desc
        .map(|desc| map_line_cap(desc.line_end))
        .unwrap_or(LineCap::Round);
    let line_pattern = map_line_pattern(line_desc.and_then(|desc| desc.line_property));
    let bbox = Arc::new(start, end, center, clockwise).bbox();
    let mut stroke = StrokeStyle::new(width, line_cap);
    stroke.pattern = line_pattern;
    doc.push_path(
        Paint::Stroke(stroke),
        [ContourBuf::from_parts(
            bbox,
            vec![
                PathCmd::move_to(start),
                PathCmd::arc_to(end, center, clockwise),
            ],
        )],
    );
}

fn push_user_polyline_path(
    doc: &mut GeometryDocument,
    polyline: &ipc2581::types::Polyline,
    transform: Affine2,
    line_desc: Option<ipc2581::types::LineDesc>,
) {
    let width = line_desc.map(|desc| desc.line_width).unwrap_or(0.25);
    let line_cap = line_desc
        .map(|desc| map_line_cap(desc.line_end))
        .unwrap_or(LineCap::Round);
    let line_pattern = map_line_pattern(line_desc.and_then(|desc| desc.line_property));
    let mut current = Point::new(polyline.begin.x, polyline.begin.y);
    let start = transform.transform_point(current);
    let mut bbox = BBox::from_point(start);
    let mut cmds = vec![PathCmd::move_to(start)];

    for step in &polyline.steps {
        match step {
            PolyStep::Segment(segment) => {
                current = Point::new(segment.point.x, segment.point.y);
                let point = transform.transform_point(current);
                bbox.include_point(point);
                cmds.push(PathCmd::line_to(point));
            }
            PolyStep::Curve(curve) => {
                let end = Point::new(curve.point.x, curve.point.y);
                let center = Point::new(curve.center.x, curve.center.y);
                let start = transform.transform_point(current);
                let end = transform.transform_point(end);
                let center = transform.transform_point(center);
                let clockwise = if transform.determinant() < 0.0 {
                    !curve.clockwise
                } else {
                    curve.clockwise
                };
                bbox = bbox.union(Arc::new(start, end, center, clockwise).bbox());
                cmds.push(PathCmd::arc_to(end, center, clockwise));
                current = Point::new(curve.point.x, curve.point.y);
            }
        }
    }

    let mut stroke = StrokeStyle::new(width, line_cap);
    stroke.pattern = line_pattern;
    doc.push_path(Paint::Stroke(stroke), [ContourBuf::from_parts(bbox, cmds)]);
}

fn push_polygon_path(
    doc: &mut GeometryDocument,
    polygon: &ipc2581::types::Polygon,
    transform: Affine2,
    fill_rule: FillRule,
) {
    let contour = polygon_contour(polygon, transform);
    doc.push_path(Paint::Fill { rule: fill_rule }, [contour]);
}

fn primitive_paint(context: &ExtractContext<'_>, primitive: &StandardPrimitive) -> PrimitivePaint {
    match primitive_fill_property(context, primitive) {
        Some(FillProperty::Hollow) => PrimitivePaint::Hollow,
        Some(FillProperty::Void) => PrimitivePaint::Void,
        _ => PrimitivePaint::Fill,
    }
}

fn primitive_fill_property(
    context: &ExtractContext<'_>,
    primitive: &StandardPrimitive,
) -> Option<FillProperty> {
    let style = primitive_style(primitive);
    style.fill_property.or_else(|| {
        style
            .fill_desc_ref
            .and_then(|reference| context.fill_descs.get(&reference))
            .map(|description| description.fill_property)
    })
}

fn primitive_line_desc(
    context: &ExtractContext<'_>,
    primitive: &StandardPrimitive,
) -> Option<ipc2581::types::LineDesc> {
    let style = primitive_style(primitive);
    style.line_desc.or_else(|| {
        style
            .line_desc_ref
            .and_then(|reference| context.line_descs.get(&reference).copied())
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct StandardPrimitiveStyle {
    fill_property: Option<FillProperty>,
    line_desc: Option<ipc2581::types::LineDesc>,
    line_desc_ref: Option<Symbol>,
    fill_desc_ref: Option<Symbol>,
}

fn primitive_style(primitive: &StandardPrimitive) -> StandardPrimitiveStyle {
    fn styled<T>(styled: &ipc2581::types::Styled<T>) -> StandardPrimitiveStyle {
        StandardPrimitiveStyle {
            fill_property: styled
                .fill_desc
                .map(|description| description.fill_property)
                .or(styled.fill_property),
            line_desc: styled.line_desc,
            line_desc_ref: styled.line_desc_ref,
            fill_desc_ref: styled.fill_desc_ref,
        }
    }

    match primitive {
        StandardPrimitive::Circle(value) => styled(value),
        StandardPrimitive::RectCenter(value) => styled(value),
        StandardPrimitive::RectRound(value) => styled(value),
        StandardPrimitive::RectCham(value) => styled(value),
        StandardPrimitive::RectCorner(value) => styled(value),
        StandardPrimitive::Oval(value) => styled(value),
        StandardPrimitive::Butterfly(value) => styled(value),
        StandardPrimitive::Diamond(value) => styled(value),
        StandardPrimitive::Donut(value) => styled(value),
        StandardPrimitive::Ellipse(value) => styled(value),
        StandardPrimitive::Hexagon(value) => styled(value),
        StandardPrimitive::Octagon(value) => styled(value),
        StandardPrimitive::Thermal(value) => styled(value),
        StandardPrimitive::Triangle(value) => styled(value),
        StandardPrimitive::Moire(_) | StandardPrimitive::Contour(_) => {
            StandardPrimitiveStyle::default()
        }
    }
}

fn make_paths_stroked(
    doc: &mut GeometryDocument,
    path_start: u32,
    width: f64,
    line_cap: LineCap,
    line_pattern: LinePattern,
) {
    let mut stroke = StrokeStyle::new(width, line_cap);
    stroke.pattern = line_pattern;
    for path in &mut doc.arena.paths[path_start as usize..] {
        path.paint = Paint::Stroke(stroke);
    }
}

fn make_paths_unpainted(doc: &mut GeometryDocument, path_start: u32) {
    for path in &mut doc.arena.paths[path_start as usize..] {
        path.paint = Paint::None;
    }
}

fn polygon_contour(polygon: &ipc2581::types::Polygon, transform: Affine2) -> ContourBuf {
    let mut cmds = Vec::new();
    let mut current = Point::new(polygon.begin.x, polygon.begin.y);
    let start = transform.transform_point(current);
    let mut bbox = BBox::from_point(start);
    cmds.push(PathCmd::move_to(start));

    for step in &polygon.steps {
        match step {
            PolyStep::Segment(segment) => {
                current = Point::new(segment.point.x, segment.point.y);
                let p = transform.transform_point(current);
                bbox.include_point(p);
                cmds.push(PathCmd::line_to(p));
            }
            PolyStep::Curve(curve) => {
                let end = Point::new(curve.point.x, curve.point.y);
                let center = Point::new(curve.center.x, curve.center.y);
                let start = transform.transform_point(current);
                let end = transform.transform_point(end);
                let center = transform.transform_point(center);
                let clockwise = if transform.determinant() < 0.0 {
                    !curve.clockwise
                } else {
                    curve.clockwise
                };
                bbox = bbox.union(Arc::new(start, end, center, clockwise).bbox());
                cmds.push(PathCmd::arc_to(end, center, clockwise));
                current = Point::new(curve.point.x, curve.point.y);
            }
        }
    }
    cmds.push(PathCmd::close());
    ContourBuf::from_parts(bbox, cmds)
}

fn push_contour_path(
    doc: &mut GeometryDocument,
    contour: &ipc2581::types::Contour,
    transform: Affine2,
) {
    let mut contours = Vec::new();
    push_contour_payloads(&mut contours, contour, transform);
    doc.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        contours,
    );
}

fn push_contour_payloads(
    out: &mut Vec<ContourBuf>,
    contour: &ipc2581::types::Contour,
    transform: Affine2,
) {
    out.reserve(1 + contour.cutouts.len());
    out.push(polygon_contour(&contour.polygon, transform));
    for cutout in &contour.cutouts {
        out.push(polygon_contour(cutout, transform));
    }
}

fn push_closed_points_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    points: Vec<Point>,
    fill_rule: FillRule,
) {
    if points.is_empty() {
        return;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(points.len() + 1);
    for (index, point) in points.into_iter().enumerate() {
        let p = transform.transform_point(point);
        bbox.include_point(p);
        cmds.push(if index == 0 {
            PathCmd::move_to(p)
        } else {
            PathCmd::line_to(p)
        });
    }
    cmds.push(PathCmd::close());
    doc.push_path(
        Paint::Fill { rule: fill_rule },
        [ContourBuf::from_parts(bbox, cmds)],
    );
}

fn push_rect_path(doc: &mut GeometryDocument, transform: Affine2, width: f64, height: f64) {
    let hw = width / 2.0;
    let hh = height / 2.0;
    push_closed_points_path(
        doc,
        transform,
        vec![
            Point::new(-hw, -hh),
            Point::new(hw, -hh),
            Point::new(hw, hh),
            Point::new(-hw, hh),
        ],
        FillRule::NonZero,
    );
}

fn push_rounded_rect_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    width: f64,
    height: f64,
    radius: f64,
    corners: [bool; 4],
) {
    let hw = width / 2.0;
    let hh = height / 2.0;
    let r = radius.min(hw).min(hh).max(0.0);
    if r == 0.0 || !corners.iter().any(|corner| *corner) {
        push_rect_path(doc, transform, width, height);
        return;
    }

    let k = 0.552_284_749_830_793_6;
    let use_arcs = affine_preserves_circles(transform);
    let [upper_right, lower_right, lower_left, upper_left] = corners;
    let mut cmds = Vec::new();

    cmds.push(PathCmd::move_to(Point::new(
        -hw + if lower_left { r } else { 0.0 },
        -hh,
    )));

    cmds.push(PathCmd::line_to(Point::new(
        hw - if lower_right { r } else { 0.0 },
        -hh,
    )));
    if lower_right {
        if use_arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(hw, -hh + r),
                Point::new(hw - r, -hh + r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(hw - r + k * r, -hh),
                Point::new(hw, -hh + r - k * r),
                Point::new(hw, -hh + r),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        hw,
        hh - if upper_right { r } else { 0.0 },
    )));
    if upper_right {
        if use_arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(hw - r, hh),
                Point::new(hw - r, hh - r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(hw, hh - r + k * r),
                Point::new(hw - r + k * r, hh),
                Point::new(hw - r, hh),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw + if upper_left { r } else { 0.0 },
        hh,
    )));
    if upper_left {
        if use_arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(-hw, hh - r),
                Point::new(-hw + r, hh - r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(-hw + r - k * r, hh),
                Point::new(-hw, hh - r + k * r),
                Point::new(-hw, hh - r),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw,
        -hh + if lower_left { r } else { 0.0 },
    )));
    if lower_left {
        if use_arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(-hw + r, -hh),
                Point::new(-hw + r, -hh + r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(-hw, -hh + r - k * r),
                Point::new(-hw + r - k * r, -hh),
                Point::new(-hw + r, -hh),
            ));
        }
    }
    cmds.push(PathCmd::close());

    let contour = transform_cmds(cmds, transform);
    doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [contour],
    );
}

fn push_chamfered_rect_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    width: f64,
    height: f64,
    chamfer: f64,
    corners: [bool; 4],
) {
    let hw = width / 2.0;
    let hh = height / 2.0;
    let c = chamfer.min(hw).min(hh).max(0.0);
    if c == 0.0 || !corners.iter().any(|corner| *corner) {
        push_rect_path(doc, transform, width, height);
        return;
    }

    let [upper_right, lower_right, lower_left, upper_left] = corners;
    let mut points = Vec::with_capacity(8);

    points.push(Point::new(-hw + if lower_left { c } else { 0.0 }, -hh));

    points.push(Point::new(hw - if lower_right { c } else { 0.0 }, -hh));
    if lower_right {
        points.push(Point::new(hw, -hh + c));
    }

    points.push(Point::new(hw, hh - if upper_right { c } else { 0.0 }));
    if upper_right {
        points.push(Point::new(hw - c, hh));
    }

    points.push(Point::new(-hw + if upper_left { c } else { 0.0 }, hh));
    if upper_left {
        points.push(Point::new(-hw, hh - c));
    }

    points.push(Point::new(-hw, -hh + if lower_left { c } else { 0.0 }));

    push_closed_points_path(doc, transform, points, FillRule::NonZero);
}

fn push_regular_polygon_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    sides: usize,
    radius: f64,
) {
    let points = (0..sides)
        .map(|index| {
            let angle = -std::f64::consts::FRAC_PI_2
                + (index as f64 * std::f64::consts::TAU / sides as f64);
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect();
    push_closed_points_path(doc, transform, points, FillRule::NonZero);
}

fn push_ellipse_path(doc: &mut GeometryDocument, transform: Affine2, width: f64, height: f64) {
    let contour = if nearly_equal(width, height) && affine_preserves_circles(transform) {
        circle_contour(transform, width)
    } else {
        ellipse_contour(transform, width, height)
    };
    doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [contour],
    );
}

fn circle_contour(transform: Affine2, diameter: f64) -> ContourBuf {
    let radius = diameter / 2.0;
    let center = transform.transform_point(Point::default());
    let points = [
        transform.transform_point(Point::new(radius, 0.0)),
        transform.transform_point(Point::new(0.0, radius)),
        transform.transform_point(Point::new(-radius, 0.0)),
        transform.transform_point(Point::new(0.0, -radius)),
        transform.transform_point(Point::new(radius, 0.0)),
    ];
    let clockwise = transform.determinant() < 0.0;
    let mut bbox = BBox::empty();
    for pair in points.windows(2) {
        bbox = bbox.union(Arc::new(pair[0], pair[1], center, clockwise).bbox());
    }
    let cmds = vec![
        PathCmd::move_to(points[0]),
        PathCmd::arc_to(points[1], center, clockwise),
        PathCmd::arc_to(points[2], center, clockwise),
        PathCmd::arc_to(points[3], center, clockwise),
        PathCmd::arc_to(points[4], center, clockwise),
        PathCmd::close(),
    ];
    ContourBuf::from_parts(bbox, cmds)
}

fn ellipse_contour(transform: Affine2, width: f64, height: f64) -> ContourBuf {
    let rx = width / 2.0;
    let ry = height / 2.0;
    let k = 0.552_284_749_830_793_6;
    let local = [
        (
            Point::new(rx, 0.0),
            Point::new(rx, k * ry),
            Point::new(k * rx, ry),
            Point::new(0.0, ry),
        ),
        (
            Point::new(0.0, ry),
            Point::new(-k * rx, ry),
            Point::new(-rx, k * ry),
            Point::new(-rx, 0.0),
        ),
        (
            Point::new(-rx, 0.0),
            Point::new(-rx, -k * ry),
            Point::new(-k * rx, -ry),
            Point::new(0.0, -ry),
        ),
        (
            Point::new(0.0, -ry),
            Point::new(k * rx, -ry),
            Point::new(rx, -k * ry),
            Point::new(rx, 0.0),
        ),
    ];

    let start = transform.transform_point(local[0].0);
    let mut bbox = BBox::from_point(start);
    let mut cmds = vec![PathCmd::move_to(start)];
    for (_, c1, c2, end) in local {
        let c1 = transform.transform_point(c1);
        let c2 = transform.transform_point(c2);
        let end = transform.transform_point(end);
        bbox.include_point(c1);
        bbox.include_point(c2);
        bbox.include_point(end);
        cmds.push(PathCmd::cubic_to(c1, c2, end));
    }
    cmds.push(PathCmd::close());
    ContourBuf::from_parts(bbox, cmds)
}

fn push_oval_path(doc: &mut GeometryDocument, transform: Affine2, width: f64, height: f64) {
    if (width - height).abs() < 1e-9 {
        push_ellipse_path(doc, transform, width, height);
        return;
    }

    let k = 0.552_284_749_830_793_6;
    let mut local_cmds = Vec::new();
    if width > height {
        let r = height / 2.0;
        let a = (width - height) / 2.0;
        local_cmds.push(PathCmd::move_to(Point::new(a, -r)));
        local_cmds.push(PathCmd::line_to(Point::new(-a, -r)));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(-a - k * r, -r),
            Point::new(-a - r, -k * r),
            Point::new(-a - r, 0.0),
        ));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(-a - r, k * r),
            Point::new(-a - k * r, r),
            Point::new(-a, r),
        ));
        local_cmds.push(PathCmd::line_to(Point::new(a, r)));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(a + k * r, r),
            Point::new(a + r, k * r),
            Point::new(a + r, 0.0),
        ));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(a + r, -k * r),
            Point::new(a + k * r, -r),
            Point::new(a, -r),
        ));
    } else {
        let r = width / 2.0;
        let a = (height - width) / 2.0;
        local_cmds.push(PathCmd::move_to(Point::new(r, -a)));
        local_cmds.push(PathCmd::line_to(Point::new(r, a)));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(r, a + k * r),
            Point::new(k * r, a + r),
            Point::new(0.0, a + r),
        ));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(-k * r, a + r),
            Point::new(-r, a + k * r),
            Point::new(-r, a),
        ));
        local_cmds.push(PathCmd::line_to(Point::new(-r, -a)));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(-r, -a - k * r),
            Point::new(-k * r, -a - r),
            Point::new(0.0, -a - r),
        ));
        local_cmds.push(PathCmd::cubic_to(
            Point::new(k * r, -a - r),
            Point::new(r, -a - k * r),
            Point::new(r, -a),
        ));
    }
    local_cmds.push(PathCmd::close());

    let contour = transform_cmds(local_cmds, transform);
    doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [contour],
    );
}

fn affine_preserves_circles(transform: Affine2) -> bool {
    let sx = transform.m00.hypot(transform.m10);
    let sy = transform.m01.hypot(transform.m11);
    let dot = transform.m00 * transform.m01 + transform.m10 * transform.m11;
    sx > GEOMETRY_EPSILON
        && sy > GEOMETRY_EPSILON
        && nearly_equal(sx, sy)
        && dot.abs() <= GEOMETRY_EPSILON * sx.max(sy).max(1.0)
}

fn nearly_equal(left: f64, right: f64) -> bool {
    (left - right).abs() <= GEOMETRY_EPSILON * left.abs().max(right.abs()).max(1.0)
}

const GEOMETRY_EPSILON: f64 = 1e-9;

fn push_donut_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    outer_diameter: f64,
    inner_diameter: f64,
) {
    doc.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        [
            ellipse_contour(transform, outer_diameter, outer_diameter),
            ellipse_contour(transform, inner_diameter, inner_diameter),
        ],
    );
}

fn push_butterfly_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    shape: ipc2581::types::ButterflyShape,
    size: f64,
) {
    let radius = size / 2.0;
    match shape {
        ipc2581::types::ButterflyShape::Round => doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [
                circular_sector_contour(transform, radius, 90.0, 180.0),
                circular_sector_contour(transform, radius, 270.0, 360.0),
            ],
        ),
        ipc2581::types::ButterflyShape::Square => doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [
                rect_contour(transform, -radius, 0.0, 0.0, radius),
                rect_contour(transform, 0.0, -radius, radius, 0.0),
            ],
        ),
    };
}

fn push_moire_path(doc: &mut GeometryDocument, transform: Affine2, moire: &ipc2581::types::Moire) {
    for index in 0..moire.ring_number {
        let centerline_diameter = moire.diameter - 2.0 * index as f64 * moire.ring_gap;
        let outer_diameter = centerline_diameter + moire.ring_width;
        let inner_diameter = centerline_diameter - moire.ring_width;
        if outer_diameter <= 0.0 {
            break;
        }

        if inner_diameter > 0.0 {
            push_donut_path(doc, transform, outer_diameter, inner_diameter);
        } else {
            push_ellipse_path(doc, transform, outer_diameter, outer_diameter);
        }
    }

    if let (Some(width), Some(length)) = (moire.line_width, moire.line_length) {
        let angle = moire.line_angle.unwrap_or(0.0);
        push_rect_path(
            doc,
            transform.concat(Affine2::placement(
                Point::default(),
                angle,
                Mirror::NONE,
                1.0,
            )),
            length,
            width,
        );
        push_rect_path(
            doc,
            transform.concat(Affine2::placement(
                Point::default(),
                angle + 90.0,
                Mirror::NONE,
                1.0,
            )),
            length,
            width,
        );
    }
}

fn push_thermal_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    outer_diameter: f64,
    inner_diameter: f64,
    spoke_width: f64,
    spoke_count: u32,
    spoke_start_angle: f64,
) {
    if spoke_count == 0 {
        push_donut_path(doc, transform, outer_diameter, inner_diameter);
        return;
    }

    let outer_radius = outer_diameter / 2.0;
    let inner_radius = inner_diameter / 2.0;
    let length = (outer_radius - inner_radius).max(0.0);
    for index in 0..spoke_count {
        let angle = spoke_start_angle.to_radians()
            + index as f64 * std::f64::consts::TAU / spoke_count as f64;
        let center_radius = inner_radius + length / 2.0;
        let center = Point::new(center_radius * angle.cos(), center_radius * angle.sin());
        let spoke_transform = transform.concat(Affine2::placement(
            center,
            angle.to_degrees(),
            Mirror::NONE,
            1.0,
        ));
        push_rect_path(doc, spoke_transform, length, spoke_width);
    }
}

fn circular_sector_contour(
    transform: Affine2,
    radius: f64,
    start_degrees: f64,
    end_degrees: f64,
) -> ContourBuf {
    let start_angle = start_degrees.to_radians();
    let end_angle = end_degrees.to_radians();
    let start = Point::new(radius * start_angle.cos(), radius * start_angle.sin());
    let end = Point::new(radius * end_angle.cos(), radius * end_angle.sin());
    transform_cmds(
        [
            PathCmd::move_to(Point::default()),
            PathCmd::line_to(start),
            PathCmd::arc_to(end, Point::default(), false),
            PathCmd::close(),
        ],
        transform,
    )
}

fn rect_contour(transform: Affine2, x0: f64, y0: f64, x1: f64, y1: f64) -> ContourBuf {
    transform_cmds(
        [
            PathCmd::move_to(Point::new(x0, y0)),
            PathCmd::line_to(Point::new(x1, y0)),
            PathCmd::line_to(Point::new(x1, y1)),
            PathCmd::line_to(Point::new(x0, y1)),
            PathCmd::close(),
        ],
        transform,
    )
}

fn map_polarity(polarity: Polarity) -> GeometryPolarity {
    match polarity {
        Polarity::Positive => GeometryPolarity::Dark,
        Polarity::Negative => GeometryPolarity::Clear,
    }
}

fn map_line_cap(line_end: LineEnd) -> LineCap {
    match line_end {
        LineEnd::None => LineCap::Butt,
        LineEnd::Round => LineCap::Round,
        LineEnd::Square => LineCap::Square,
    }
}

fn map_line_pattern(line_property: Option<LineProperty>) -> LinePattern {
    match line_property {
        Some(LineProperty::Solid) | None => LinePattern::Solid,
        Some(LineProperty::Dotted) => LinePattern::Dotted,
        Some(LineProperty::Dashed) => LinePattern::Dashed,
        Some(LineProperty::Center) => LinePattern::Center,
        Some(LineProperty::Phantom) => LinePattern::Phantom,
        Some(LineProperty::Erase) => LinePattern::Erase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_ipc_line_properties_to_ir_patterns() {
        assert_eq!(map_line_pattern(None), LinePattern::Solid);
        assert_eq!(
            map_line_pattern(Some(LineProperty::Solid)),
            LinePattern::Solid
        );
        assert_eq!(
            map_line_pattern(Some(LineProperty::Dotted)),
            LinePattern::Dotted
        );
        assert_eq!(
            map_line_pattern(Some(LineProperty::Dashed)),
            LinePattern::Dashed
        );
        assert_eq!(
            map_line_pattern(Some(LineProperty::Center)),
            LinePattern::Center
        );
        assert_eq!(
            map_line_pattern(Some(LineProperty::Phantom)),
            LinePattern::Phantom
        );
        assert_eq!(
            map_line_pattern(Some(LineProperty::Erase)),
            LinePattern::Erase
        );
    }

    #[test]
    fn preserves_inline_feature_line_property() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SILK_SCREEN" side="TOP"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set>
            <Features>
              <Line startX="0" startY="0" endX="10" endY="0">
                <LineDesc lineWidth="0.1" lineEnd="ROUND" lineProperty="PHANTOM"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let layer = extract_layer_for_view(&ipc, "TOP", ArtworkScope::Board).unwrap();
        let path = &layer.arena.paths[layer.features[0].paths.start as usize];

        assert_eq!(path.stroke().unwrap().pattern, LinePattern::Phantom);
    }

    #[test]
    fn carries_spec_refs_fiducials_and_vcut_intent() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="Panel"/>
    <LayerRef name="TOP"/>
    <LayerRef name="VCUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="ANGLE">
          <Property value="90" unit="DEGREES"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Layer name="VCUT" layerFunction="V_CUT" side="ALL" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Step name="Panel" type="PALLET">
        <LayerFeature layerRef="TOP">
          <Set>
            <SpecRef id="VCut_1"/>
            <GlobalFiducial>
              <Location x="1" y="2"/>
              <Circle diameter="1"/>
              <PinRef componentRef="U1" pin="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="VCUT">
          <Set>
            <SpecRef id="VCut_1"/>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let top = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayFlattened).unwrap();
        assert_eq!(top.specs.len(), 1);
        assert_eq!(top.layers[0].spec_refs.count, 1);
        assert_eq!(top.feature_sets.len(), 1);
        assert_eq!(top.feature_sets[0].spec_refs.count, 1);
        assert_eq!(top.features[0].bucket, FeatureBucket::Fiducial);
        assert_eq!(top.features[0].intent.role, FeatureRole::Fiducial);
        assert_eq!(top.features[0].fiducial_kind, FiducialKind::Global);
        assert!(top.features[0].is_fiducial());
        assert_eq!(top.features[0].source_step_kind, LayoutStepKind::Panel);
        assert_eq!(
            top.features[0]
                .source_step_ref
                .map(|step| ipc.resolve(step)),
            Some("Panel")
        );
        assert_eq!(top.features[0].pin_refs.count, 1);
        assert_eq!(ipc.resolve(top.pin_refs[0].pin), "1");

        let vcut = extract_layer_for_view(&ipc, "VCUT", ArtworkScope::ArrayFlattened).unwrap();
        assert_eq!(vcut.layers[0].spec_refs.count, 1);
        assert_eq!(vcut.feature_sets[0].spec_refs.count, 1);
        assert_eq!(vcut.features[0].intent.domain, FeatureDomain::VCut);
        assert_eq!(vcut.features[0].intent.role, FeatureRole::ArraySeparation);
        assert!(vcut.features[0].is_vcut());
    }

    #[test]
    fn lowers_moire_as_rings_and_crosshair() {
        let mut doc = GeometryDocument::new();

        push_moire_path(
            &mut doc,
            Affine2::identity(),
            &ipc2581::types::Moire {
                diameter: 8.0,
                ring_width: 0.5,
                ring_gap: 1.0,
                ring_number: 3,
                line_width: Some(0.2),
                line_length: Some(10.0),
                line_angle: Some(0.0),
            },
        );

        assert_eq!(doc.arena.paths.len(), 5);
        assert_eq!(doc.arena.paths[0].fill_rule(), Some(FillRule::EvenOdd));
        assert_eq!(doc.arena.paths[0].contours.count, 2);
        assert_eq!(doc.arena.paths[1].contours.count, 2);
        assert_eq!(doc.arena.paths[2].contours.count, 2);
        assert_eq!(doc.arena.paths[3].fill_rule(), Some(FillRule::NonZero));
        assert_eq!(doc.arena.paths[4].fill_rule(), Some(FillRule::NonZero));
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-4.25, -4.25));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(4.25, 4.25));
        assert_eq!(doc.arena.paths[1].bbox.min, Point::new(-3.25, -3.25));
        assert_eq!(doc.arena.paths[1].bbox.max, Point::new(3.25, 3.25));
    }

    #[test]
    fn reads_standard_primitive_fill_properties() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let circle = ipc2581::types::StandardPrimitive::Circle(ipc2581::types::Styled {
            shape: ipc2581::types::Circle { diameter: 1.0 },
            fill_property: Some(FillProperty::Hollow),
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        });
        let rect = ipc2581::types::StandardPrimitive::RectCenter(ipc2581::types::Styled {
            shape: ipc2581::types::RectCenter {
                size: ipc2581::types::Size {
                    width: 1.0,
                    height: 1.0,
                },
            },
            fill_property: Some(FillProperty::Void),
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        });

        assert_eq!(primitive_paint(&context, &circle), PrimitivePaint::Hollow);
        assert_eq!(primitive_paint(&context, &rect), PrimitivePaint::Void);
    }

    #[test]
    fn zero_area_standard_primitive_emits_no_paths() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let mut doc = GeometryDocument::new();
        let primitive = ipc2581::types::StandardPrimitive::RectCenter(ipc2581::types::Styled {
            shape: ipc2581::types::RectCenter {
                size: ipc2581::types::Size {
                    width: 0.0,
                    height: 1.0,
                },
            },
            fill_property: None,
            line_desc: None,
            line_desc_ref: None,
            fill_desc: None,
            fill_desc_ref: None,
        });

        let paint =
            lower_standard_primitive(&context, &mut doc, &primitive, Affine2::identity()).unwrap();

        assert_eq!(paint, PrimitivePaint::Fill);
        assert!(doc.arena.paths.is_empty());
        assert!(doc.arena.contours.is_empty());
        assert!(doc.arena.cmds.is_empty());
    }

    #[test]
    fn lowers_trace_poly_step_curves_as_arcs() {
        let mut doc = GeometryDocument::new();
        let trace = ipc2581::types::Trace {
            line_desc_ref: None,
            points: vec![
                ipc2581::types::ecad::TracePoint { x: 1.0, y: 0.0 },
                ipc2581::types::ecad::TracePoint { x: 0.0, y: 1.0 },
            ],
            steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
                point: ipc2581::types::Point { x: 0.0, y: 1.0 },
                center: ipc2581::types::Point { x: 0.0, y: 0.0 },
                clockwise: false,
            })],
        };

        let feature = push_stroked_trace(
            &mut doc,
            StrokedFeatureStyle {
                net: None,
                polarity: GeometryPolarity::Dark,
                source: SourceRef::default(),
                width: 0.2,
                line_cap: LineCap::Round,
                line_pattern: LinePattern::Solid,
            },
            &trace,
        );

        assert_eq!(feature.paths.count, 1);
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.1, -0.1));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(1.1, 1.1));
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn lowers_feature_poly_step_curves_as_arcs() {
        let mut doc = GeometryDocument::new();
        let polyline = ipc2581::types::ecad::FeaturePolyline {
            begin: ipc2581::types::Point { x: 1.0, y: 0.0 },
            steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
                point: ipc2581::types::Point { x: 0.0, y: 1.0 },
                center: ipc2581::types::Point { x: 0.0, y: 0.0 },
                clockwise: false,
            })],
            line_desc_ref: None,
            line_width: 0.2,
            line_end: Some(LineEnd::Round),
            line_property: None,
        };

        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let feature = extract_feature_polyline(
            &ExtractContext {
                strings: ipc.interner(),
                padstacks: HashMap::new(),
                line_descs: HashMap::new(),
                fill_descs: HashMap::new(),
                standard_primitives: HashMap::new(),
                user_primitives: HashMap::new(),
            },
            None,
            GeometryPolarity::Dark,
            SourceRef::default(),
            &polyline,
            &mut doc,
        );

        assert_eq!(feature.paths.count, 1);
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.1, -0.1));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(1.1, 1.1));
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn lowers_hollow_user_circle_as_stroked_path() {
        let mut doc = GeometryDocument::new();
        let primitive = UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
            shapes: vec![ipc2581::types::UserShape {
                shape: UserShapeType::Circle(ipc2581::types::Circle { diameter: 1.4 }),
                line_desc: Some(ipc2581::types::LineDesc {
                    line_width: 0.1,
                    line_end: LineEnd::Round,
                    line_property: None,
                }),
                line_desc_ref: None,
                fill_desc: Some(ipc2581::types::FillDesc {
                    fill_property: FillProperty::Hollow,
                    line_width: None,
                    pitch1: None,
                    pitch2: None,
                    angle1: None,
                    angle2: None,
                    color: None,
                }),
                fill_desc_ref: None,
            }],
        });

        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let paint = lower_user_primitive(&context, &mut doc, &primitive, Affine2::identity());

        assert_eq!(paint, PrimitivePaint::Hollow);
        assert_eq!(doc.arena.paths.len(), 1);
        assert!(doc.arena.paths[0].is_stroked());
        assert!(!doc.arena.paths[0].is_filled());
        assert_eq!(doc.arena.paths[0].stroke().unwrap().width, 0.1);
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(-0.7, -0.7));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(0.7, 0.7));
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
        assert!(!doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::CubicTo));
    }

    #[test]
    fn lowers_user_special_lines_polylines_and_line_desc_refs() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="fine">
        <LineDesc lineWidth="0.15" lineEnd="NONE"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
</IPC-2581>"#,
        )
        .unwrap();
        let entry = ipc.content().dictionary_line_desc.entries[0].clone();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::from([(entry.id, entry.line_desc)]),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let mut doc = GeometryDocument::new();
        let primitive = UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
            shapes: vec![
                ipc2581::types::UserShape {
                    shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                        start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                        end: ipc2581::types::Point { x: 1.0, y: 0.0 },
                    }),
                    line_desc: None,
                    line_desc_ref: Some(entry.id),
                    fill_desc: None,
                    fill_desc_ref: None,
                },
                ipc2581::types::UserShape {
                    shape: UserShapeType::Polyline(ipc2581::types::Polyline {
                        begin: ipc2581::types::Point { x: 1.0, y: 0.0 },
                        steps: vec![PolyStep::Curve(ipc2581::types::PolyStepCurve {
                            point: ipc2581::types::Point { x: 0.0, y: 1.0 },
                            center: ipc2581::types::Point { x: 0.0, y: 0.0 },
                            clockwise: false,
                        })],
                    }),
                    line_desc: None,
                    line_desc_ref: Some(entry.id),
                    fill_desc: None,
                    fill_desc_ref: None,
                },
            ],
        });

        let paint = lower_user_primitive(&context, &mut doc, &primitive, Affine2::identity());

        assert_eq!(paint, PrimitivePaint::Fill);
        assert_eq!(doc.arena.paths.len(), 2);
        assert!(doc.arena.paths.iter().all(|path| path.is_stroked()));
        assert!(
            doc.arena
                .paths
                .iter()
                .all(|path| path.stroke().unwrap().width == 0.15)
        );
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn extracts_inline_stroked_user_primitive_as_trace_feature() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
            primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
                shapes: vec![ipc2581::types::UserShape {
                    shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                        start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                        end: ipc2581::types::Point { x: 1.0, y: 0.0 },
                    }),
                    line_desc: Some(ipc2581::types::LineDesc {
                        line_width: 0.2,
                        line_end: LineEnd::Round,
                        line_property: None,
                    }),
                    line_desc_ref: None,
                    fill_desc: None,
                    fill_desc_ref: None,
                }],
            }),
            x: 10.0,
            y: 20.0,
        };
        let mut doc = GeometryDocument::new();

        let features = extract_inline_user_primitive(
            &context,
            None,
            GeometryPolarity::Dark,
            SourceRef::default(),
            &primitive,
            &mut doc,
        )
        .unwrap();

        assert_eq!(features.len(), 1);
        let feature = &features[0];
        assert_eq!(feature.bucket, FeatureBucket::Trace);
        assert_eq!(feature.paths.count, 1);
        assert!(doc.arena.paths[feature.paths.start as usize].is_stroked());
    }

    #[test]
    fn lowers_inline_user_contour_as_compound_path_at_feature_location() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
            primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
                shapes: vec![
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Contour(ipc2581::types::Contour {
                            polygon: rect_polygon(0.0, 0.0, 2.0, 2.0),
                            cutouts: Vec::new(),
                        }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
                        fill_desc_ref: None,
                    },
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Contour(ipc2581::types::Contour {
                            polygon: rect_polygon(0.5, 0.5, 1.5, 1.5),
                            cutouts: Vec::new(),
                        }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
                        fill_desc_ref: None,
                    },
                ],
            }),
            x: 10.0,
            y: 20.0,
        };
        let mut doc = GeometryDocument::new();

        let features = extract_inline_user_primitive(
            &context,
            None,
            GeometryPolarity::Dark,
            SourceRef::default(),
            &primitive,
            &mut doc,
        )
        .unwrap();

        assert_eq!(features.len(), 1);
        let feature = &features[0];
        assert_eq!(feature.paths.count, 1);
        assert_eq!(doc.arena.paths[0].fill_rule(), Some(FillRule::EvenOdd));
        assert_eq!(doc.arena.paths[0].contours.count, 2);
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(10.0, 20.0));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(12.0, 22.0));
    }

    #[test]
    fn splits_mixed_inline_user_primitive_into_trace_and_fill_features() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            strings: ipc.interner(),
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
            fill_descs: HashMap::new(),
            standard_primitives: HashMap::new(),
            user_primitives: HashMap::new(),
        };
        let primitive = ipc2581::types::ecad::FeatureUserPrimitive {
            primitive: UserPrimitive::UserSpecial(ipc2581::types::UserSpecial {
                shapes: vec![
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Line(ipc2581::types::primitives::Line {
                            start: ipc2581::types::Point { x: 0.0, y: 0.0 },
                            end: ipc2581::types::Point { x: 2.0, y: 0.0 },
                        }),
                        line_desc: Some(ipc2581::types::LineDesc {
                            line_width: 0.2,
                            line_end: LineEnd::Round,
                            line_property: None,
                        }),
                        line_desc_ref: None,
                        fill_desc: None,
                        fill_desc_ref: None,
                    },
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Contour(ipc2581::types::Contour {
                            polygon: rect_polygon(0.0, 1.0, 2.0, 3.0),
                            cutouts: Vec::new(),
                        }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
                        fill_desc_ref: None,
                    },
                ],
            }),
            x: 10.0,
            y: 20.0,
        };
        let mut doc = GeometryDocument::new();

        let features = extract_inline_user_primitive(
            &context,
            None,
            GeometryPolarity::Dark,
            SourceRef::default(),
            &primitive,
            &mut doc,
        )
        .unwrap();

        assert_eq!(features.len(), 2);
        assert_eq!(features[0].bucket, FeatureBucket::Trace);
        assert_eq!(features[0].paths.count, 1);
        assert_eq!(features[1].bucket, FeatureBucket::Fill);
        assert_eq!(features[1].paths.count, 1);
        assert!(doc.arena.paths[features[0].paths.start as usize].is_stroked());
        assert!(doc.arena.paths[features[1].paths.start as usize].is_filled());
    }

    #[test]
    fn lowers_butterfly_with_removed_quadrants() {
        let mut doc = GeometryDocument::new();

        push_butterfly_path(
            &mut doc,
            Affine2::identity(),
            ipc2581::types::ButterflyShape::Square,
            4.0,
        );
        push_butterfly_path(
            &mut doc,
            Affine2::identity(),
            ipc2581::types::ButterflyShape::Round,
            4.0,
        );

        assert_eq!(doc.arena.paths.len(), 2);
        assert_eq!(doc.arena.paths[0].contours.count, 2);
        assert_eq!(doc.arena.paths[1].contours.count, 2);
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    fn rect_polygon(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ipc2581::types::Polygon {
        ipc2581::types::Polygon {
            begin: ipc2581::types::Point { x: min_x, y: min_y },
            steps: vec![
                PolyStep::Segment(ipc2581::types::PolyStepSegment {
                    point: ipc2581::types::Point { x: max_x, y: min_y },
                }),
                PolyStep::Segment(ipc2581::types::PolyStepSegment {
                    point: ipc2581::types::Point { x: max_x, y: max_y },
                }),
                PolyStep::Segment(ipc2581::types::PolyStepSegment {
                    point: ipc2581::types::Point { x: min_x, y: max_y },
                }),
                PolyStep::Segment(ipc2581::types::PolyStepSegment {
                    point: ipc2581::types::Point { x: min_x, y: min_y },
                }),
            ],
        }
    }

    #[test]
    fn lowers_thermal_as_spokes_without_redundant_ring() {
        let mut doc = GeometryDocument::new();

        push_thermal_path(&mut doc, Affine2::identity(), 10.0, 6.0, 2.0, 4, 0.0);

        assert_eq!(doc.arena.paths.len(), 4);
        assert!(doc.arena.paths.iter().all(|path| {
            path.fill_rule() == Some(FillRule::NonZero) && path.contours.count == 1
        }));
        assert_eq!(doc.arena.paths[0].bbox.min, Point::new(3.0, -1.0));
        assert_eq!(doc.arena.paths[0].bbox.max, Point::new(5.0, 1.0));
    }

    #[test]
    fn lowers_spokeless_thermal_as_donut() {
        let mut doc = GeometryDocument::new();

        push_thermal_path(&mut doc, Affine2::identity(), 10.0, 6.0, 2.0, 0, 0.0);

        assert_eq!(doc.arena.paths.len(), 1);
        assert_eq!(doc.arena.paths[0].fill_rule(), Some(FillRule::EvenOdd));
        assert_eq!(doc.arena.paths[0].contours.count, 2);
    }

    #[test]
    fn extracts_panel_and_repeated_layer_instances() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        let doc = extract_layer(&ipc, "TOP").expect("panel layer should extract");
        let layer = &doc.layers[0];
        let features = layer.features.slice(&doc.features);

        let (_, root_step) = root_step(&doc).unwrap();
        assert_eq!(root_step.kind, LayoutStepKind::Panel);
        assert_eq!(features.len(), 3);
        assert_eq!(features[0].center, Point::new(40.0, 5.0));
        assert_eq!(features[1].center, Point::new(12.0, 23.0));
        assert_eq!(features[2].center, Point::new(27.0, 23.0));
        assert_eq!(features[0].source.set_index, 0);
        assert_eq!(features[1].source.set_index, 1);
        assert_eq!(features[2].source.set_index, 2);
        assert_eq!(layer.bbox.min, Point::new(11.5, 4.5));
        assert_eq!(layer.bbox.max, Point::new(40.5, 23.5));
        assert_eq!(board_step_count(&doc), 1);
        assert_eq!(panel_step_count(&doc), 1);
        assert_eq!(board_instance_count(&doc), 2);
        let simple_array = simple_board_array_layout(&doc).unwrap();
        assert_eq!(simple_array.columns, 2);
        assert_eq!(simple_array.rows, 1);
        assert_eq!(simple_array.board_step, 1);
        assert_eq!(simple_array.board_width, 10.0);
        assert_eq!(simple_array.board_height, 5.0);
        assert_eq!(board_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
        assert_eq!(board_bbox(&doc).unwrap().max, Point::new(10.0, 5.0));
        assert_eq!(panel_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
        assert_eq!(panel_bbox(&doc).unwrap().max, Point::new(100.0, 80.0));
        assert_eq!(doc.layout.instances[0].bbox.min, Point::new(10.0, 20.0));
        assert_eq!(doc.layout.instances[0].bbox.max, Point::new(20.0, 25.0));
        assert_eq!(doc.layout.instances[1].bbox.min, Point::new(25.0, 20.0));
        assert_eq!(doc.layout.instances[1].bbox.max, Point::new(35.0, 25.0));
        assert_eq!(doc.layout.steps.len(), 2);
        assert_eq!(doc.layout.repeats.len(), 1);
        assert_eq!(doc.layout.instances.len(), 2);
        assert_eq!(doc.layout.root_step, Some(0));
        assert_eq!(doc.layout.steps[0].kind, LayoutStepKind::Panel);
        assert_eq!(doc.layout.steps[1].kind, LayoutStepKind::Board);
        assert_eq!(doc.layout.repeats[0].instances.start, 0);
        assert_eq!(doc.layout.repeats[0].instances.count, 2);
        assert_eq!(doc.layout.instances[0].repeat_index_x, 0);
        assert_eq!(doc.layout.instances[1].repeat_index_x, 1);
        assert_eq!(doc.layout.instances[1].transform.m02, 25.0);
    }

    #[test]
    fn imported_design_owns_strings_and_reuses_step_local_geometry() {
        let imported = {
            let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
                .expect("synthetic panel fixture should parse");
            import_design(&ipc).expect("complete design should import")
        };

        let top = imported.layer_id("TOP").unwrap();
        assert_eq!(
            imported.resolve(imported.layer_definitions[top.0 as usize].name),
            "TOP"
        );
        assert_eq!(imported.step_layers.len(), 2);
        assert_eq!(
            imported
                .step_layers
                .iter()
                .map(|step_layer| {
                    imported.geometry.layers[step_layer.document_layer as usize]
                        .features
                        .count
                })
                .sum::<u32>(),
            2,
            "the panel and board each retain one local feature definition"
        );

        let occurrences = imported
            .feature_occurrences(top, ArtworkScope::ArrayFlattened)
            .unwrap();
        assert_eq!(occurrences.len(), 3);
        assert_eq!(
            occurrences
                .iter()
                .map(|occurrence| occurrence.id.feature)
                .collect::<BTreeSet<_>>()
                .len(),
            2,
            "repeated boards create occurrences, not cloned definitions"
        );
        assert_eq!(
            occurrences
                .iter()
                .map(|occurrence| occurrence.id)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn imported_design_carries_global_bom_and_package_associations() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="board-a"/>
    <StepRef name="board-b"/>
    <BomRef name="bom-a"/>
    <BomRef name="bom-b"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="line"><LineDesc lineWidth="0" lineEnd="ROUND"/></EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <LogisticHeader>
    <Role id="owner" roleFunction="OWNER"/>
    <Enterprise id="maker" code="maker" name="Maker"/>
    <Person name="Engineer" enterpriseRef="maker" roleRef="owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="test" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="test">
      <SoftwarePackage name="test" vendor="test" revision="1"><Certification certificationStatus="SELFTEST"/></SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom-a">
    <BomHeader assembly="a" revision="1"><StepRef name="board-a"/></BomHeader>
    <BomItem OEMDesignNumberRef="part-a" quantity="1" category="ELECTRICAL">
      <RefDes name="U1" packageRef="pkg-a" populate="0" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Bom name="bom-b">
    <BomHeader assembly="b" revision="1"><StepRef name="board-b"/></BomHeader>
    <BomItem OEMDesignNumberRef="part-b" quantity="1" category="ELECTRICAL">
      <RefDes name="U3" packageRef="pkg-b" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Bom name="not-selected">
    <BomHeader assembly="other" revision="1"><StepRef name="board-a"/></BomHeader>
    <BomItem OEMDesignNumberRef="other" quantity="1" category="ELECTRICAL">
      <RefDes name="U4" packageRef="pkg-a" populate="1" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL"/>
    </BomItem>
  </Bom>
  <Ecad name="design">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board-a" type="BOARD">
        <Datum x="0" y="0"/>
        <Package name="pkg-a" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Package name="shared-package" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Component refDes="U1" packageRef="pkg-a" part="part-a" layerRef="TOP" mountType="SMT"><Location x="1" y="1"/></Component>
        <Component refDes="U4" packageRef="pkg-a" part="other" layerRef="TOP" mountType="SMT"><Location x="4" y="4"/></Component>
      </Step>
      <Step name="board-b" type="BOARD">
        <Datum x="0" y="0"/>
        <Package name="pkg-b" type="OTHER" pinOneOrientation="OTHER"><Outline><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="0" y="0"/></Polygon><LineDescRef id="line"/></Outline></Package>
        <Component refDes="U3" packageRef="pkg-b" part="part-b" layerRef="TOP" mountType="SMT"><Location x="2" y="2"/></Component>
        <Component refDes="U1" packageRef="pkg-b" part="part-b-u1" layerRef="TOP" mountType="SMT"><Location x="2.5" y="2.5"/></Component>
        <Component packageRef="shared-package" part="shared-part" layerRef="TOP" mountType="SMT"><Location x="3" y="3"/></Component>
      </Step>
    </CadData>
  </Ecad>
  <Avl name="parts">
    <AvlHeader title="parts" source="test" author="test" datetime="2026-01-01T00:00:00Z" version="1"/>
    <AvlItem OEMDesignNumber="part-a"/>
  </Avl>
</IPC-2581>"#;
        ipc2581::validate(xml).expect("association fixture conforms to IPC-2581C");
        let ipc = Ipc2581::parse(xml).unwrap();

        let imported = import_design(&ipc).unwrap();
        assert_eq!(imported.boms.len(), 3);
        assert!(imported.logistic_header.is_some());
        assert!(imported.avl.is_some());
        assert_eq!(imported.packages.len(), 3);
        assert_eq!(imported.components.len(), 5);

        let assembly = imported
            .assembly_document(crate::dialects::assembly::Scope::BoardArray)
            .unwrap();
        assert_eq!(assembly.primary_bom, Some(0));
        assert_eq!(assembly.boms.len(), 3);
        assert_eq!(assembly.packages.len(), 3);
        assert_eq!(assembly.components.len(), 5);
        assert_eq!(assembly.occurrences.len(), 2);
        assert_eq!(assembly.avl.as_ref().unwrap().items.len(), 1);
        let assembly_a = assembly
            .components
            .iter()
            .find(|component| component.part == "part-a")
            .unwrap();
        assert_eq!(assembly_a.side, crate::dialects::assembly::Side::Top);
        assert_eq!(assembly_a.mount, crate::dialects::assembly::Mount::Smt);
        assert_eq!(
            assembly_a.population,
            crate::dialects::assembly::Population::DoNotPopulate
        );
        let reference = assembly.preferred_bom_reference(assembly_a).unwrap();
        assert_eq!(assembly.bom_designator(reference).name, "U1");
        assert_eq!(
            assembly.bom_item(reference).category,
            Some(crate::dialects::assembly::BomCategory::Electrical)
        );

        let a = imported
            .components
            .iter()
            .find(|component| imported.resolve(component.source.part) == "part-a")
            .unwrap();
        assert_eq!(a.population, PopulationState::DoNotPopulate);
        assert_eq!(a.bom_references.len(), 1);
        let a_package = imported.package_definition(a.package.unwrap()).unwrap();
        assert_eq!(imported.resolve(a_package.source.name), "pkg-a");
        assert_eq!(
            imported
                .bom_reference(a.bom_references[0])
                .unwrap()
                .populate,
            Some(false)
        );
        assert_eq!(
            imported.resolve(
                imported
                    .bom_item(a.bom_references[0])
                    .unwrap()
                    .oem_design_number_ref
            ),
            "part-a"
        );

        let b = imported
            .components
            .iter()
            .find(|component| imported.resolve(component.source.part) == "part-b")
            .unwrap();
        assert_eq!(b.population, PopulationState::Unspecified);
        assert_eq!(b.bom_references.len(), 1);
        assert_eq!(
            imported
                .bom_reference(b.bom_references[0])
                .unwrap()
                .populate,
            None
        );

        let repeated_refdes = imported
            .components
            .iter()
            .find(|component| imported.resolve(component.source.part) == "part-b-u1")
            .unwrap();
        assert_eq!(repeated_refdes.population, PopulationState::Unspecified);
        assert!(repeated_refdes.bom_references.is_empty());

        let unselected = imported
            .components
            .iter()
            .find(|component| imported.resolve(component.source.part) == "other")
            .unwrap();
        assert_eq!(unselected.population, PopulationState::Populate);
        assert_eq!(unselected.bom_references.len(), 1);
        assert_eq!(
            imported.resolve(
                imported
                    .bom_item(unselected.bom_references[0])
                    .unwrap()
                    .oem_design_number_ref
            ),
            "other"
        );

        let shared = imported
            .components
            .iter()
            .find(|component| imported.resolve(component.source.part) == "shared-part")
            .unwrap();
        let shared_package = imported
            .package_definition(shared.package.unwrap())
            .unwrap();
        assert_eq!(
            imported.resolve(shared_package.source.name),
            "shared-package"
        );
        assert_ne!(shared.step, shared_package.step);
    }

    #[test]
    fn board_scope_rejects_an_unreachable_board_definition() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
      <Step name="unrelated-board" type="BOARD"/>
      <Step name="panel" type="PALLET"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = import_design(&ipc).unwrap();
        let top = imported.layer_id("TOP").unwrap();

        let error = imported
            .materialize_layer(top, ArtworkScope::Board)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("primary step 'panel' does not reference a board step"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn flattened_nested_panels_preserve_depth_first_paint_order() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="root"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="dot"><Circle diameter="2"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set polarity="NEGATIVE"><Features><Location x="0" y="0"/><StandardPrimitiveRef id="dot"/></Features></Set>
        </LayerFeature>
      </Step>
      <Step name="cell" type="PALLET">
        <StepRepeat stepRef="board" x="10" y="0" nx="1" ny="1" dx="0" dy="0"/>
        <LayerFeature layerRef="TOP">
          <Set><Features><Location x="0" y="0"/><StandardPrimitiveRef id="dot"/></Features></Set>
        </LayerFeature>
      </Step>
      <Step name="root" type="PALLET">
        <StepRepeat stepRef="cell" x="0" y="0" nx="2" ny="1" dx="10" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = import_design(&ipc).unwrap();
        let top = imported.layer_id("TOP").unwrap();
        let document = imported
            .materialize_layer(top, ArtworkScope::ArrayFlattened)
            .unwrap();

        assert_eq!(
            document
                .features
                .iter()
                .map(|feature| (feature.center.x, feature.polarity))
                .collect::<Vec<_>>(),
            vec![
                (0.0, GeometryPolarity::Dark),
                (10.0, GeometryPolarity::Clear),
                (10.0, GeometryPolarity::Dark),
                (20.0, GeometryPolarity::Clear),
            ]
        );
        let image = imported
            .composed_layer_image(top, ArtworkScope::ArrayFlattened)
            .unwrap();
        assert!(image.contains_point(Point::new(10.0, 0.0)));
    }

    #[test]
    fn component_occurrence_ids_survive_mirrored_board_repeats() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="panel"/></Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="COMPONENT_TOP" side="TOP"/>
      <Step name="board" type="BOARD">
        <Component refDes="U1" packageRef="pkg" part="part" layerRef="TOP" mountType="SMT">
          <Location x="1" y="2"/>
        </Component>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="20" dy="0" mirror="true"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let imported = import_design(&ipc).unwrap();

        let occurrences = imported
            .component_occurrences(ArtworkScope::ArrayFlattened)
            .unwrap();
        assert_eq!(occurrences.len(), 2);
        assert_ne!(occurrences[0].id, occurrences[1].id);
        assert_eq!(occurrences[0].id.component, occurrences[1].id.component);
        assert!((occurrences[0].root_from_component.m02 - 9.0).abs() < 1e-9);
        assert!((occurrences[1].root_from_component.m02 - 29.0).abs() < 1e-9);
        for occurrence in occurrences {
            let board_local = occurrence.board_from_component.unwrap();
            assert!((board_local.m02 - 1.0).abs() < 1e-9);
            assert!((board_local.m12 - 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn extracts_layer_for_geometry_view_board_or_board_array() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");

        let board = extract_layer_for_view(&ipc, "TOP", ArtworkScope::Board)
            .expect("board layer should extract");
        let board_layer = &board.layers[0];
        let board_features = board_layer.features.slice(&board.features);

        assert_eq!(board_features.len(), 1);
        assert_eq!(board_features[0].center, Point::new(2.0, 3.0));
        assert_eq!(board.layout.steps.len(), 1);
        assert_eq!(board.layout.root_step, Some(0));
        assert_eq!(board.layout.steps[0].kind, LayoutStepKind::Board);
        assert!(board.layout.instances.is_empty());
        assert_eq!(
            profile_occurrences_for(&board, ProfileSet::BoardOutlines).len(),
            1
        );

        let panel = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayFlattened)
            .expect("panel layer should extract");
        let panel_layer = &panel.layers[0];
        let panel_features = panel_layer.features.slice(&panel.features);

        assert_eq!(panel_features.len(), 3);
        assert_eq!(panel_features[0].center, Point::new(40.0, 5.0));
        assert_eq!(panel_features[1].center, Point::new(12.0, 23.0));
        assert_eq!(panel_features[2].center, Point::new(27.0, 23.0));
        assert_eq!(panel.layout.steps.len(), 2);
        assert_eq!(panel.layout.instances.len(), 2);
        assert_eq!(
            profile_occurrences_for(&panel, ProfileSet::FabricationOutlines).len(),
            3
        );
    }

    #[test]
    fn step_only_panel_extraction_omits_repeat_graph_expansion() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        let doc = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayLocal)
            .expect("panel layer should extract");
        let layer = &doc.layers[0];
        let features = layer.features.slice(&doc.features);

        assert_eq!(features.len(), 1);
        assert_eq!(doc.layout.steps.len(), 1);
        assert!(doc.layout.repeats.is_empty());
        assert!(doc.layout.instances.is_empty());
        assert_eq!(board_instance_count(&doc), 0);
        assert_eq!(panel_step_count(&doc), 1);
    }

    #[test]
    fn extract_layout_builds_sidecar_without_layer_features() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        let doc = extract_layout(&ipc).expect("layout should extract");

        assert!(doc.layers.is_empty());
        assert!(doc.features.is_empty());
        assert_eq!(doc.layout.steps.len(), 2);
        assert_eq!(doc.layout.repeats.len(), 1);
        assert_eq!(doc.layout.instances.len(), 2);
        assert_eq!(panel_step_count(&doc), 1);
        assert_eq!(board_instance_count(&doc), 2);
    }

    #[test]
    fn layout_expansion_bounds_large_repeats_and_skips_empty_repeats() {
        for (nx, ny) in [
            (u32::MAX, 1),
            (u32::MAX, u32::MAX),
            (0, u32::MAX),
            (u32::MAX, 0),
        ] {
            let xml = panel_layer_fixture()
                .replace("nx=\"2\" ny=\"1\"", &format!("nx=\"{nx}\" ny=\"{ny}\""));
            let ipc = Ipc2581::parse(&xml).unwrap();
            let result = extract_layout(&ipc);
            if nx == 0 || ny == 0 {
                let layout = result.unwrap();
                assert_eq!(layout.layout.repeats.len(), 1);
                assert!(layout.layout.instances.is_empty());
            } else {
                assert!(result.unwrap_err().to_string().contains("limit"));
            }
        }
    }

    #[test]
    fn nested_panel_layout_keeps_symbolic_parent_instances() {
        let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
            .expect("synthetic nested panel fixture should parse");
        let doc = extract_layout(&ipc).expect("layout should extract");
        let fabrication_profiles = profile_occurrences_for(&doc, ProfileSet::FabricationOutlines);
        let layout_boundaries = profile_occurrences_for(&doc, ProfileSet::LayoutBoundaries);

        assert_eq!(doc.profiles.len(), 3);
        assert_eq!(fabrication_profiles.len(), 5);
        assert_eq!(layout_boundaries.len(), 7);
        assert_eq!(
            fabrication_profiles
                .iter()
                .filter(|profile| profile.role == ProfileOccurrenceRole::RootPanel)
                .count(),
            1
        );
        assert_eq!(
            fabrication_profiles
                .iter()
                .filter(|profile| profile.role == ProfileOccurrenceRole::BoardInstance)
                .count(),
            4
        );
        assert!(
            fabrication_profiles
                .iter()
                .all(|profile| profile.role != ProfileOccurrenceRole::PanelInstance)
        );
        assert_eq!(
            layout_boundaries
                .iter()
                .filter(|profile| profile.role == ProfileOccurrenceRole::PanelInstance)
                .count(),
            2
        );
        assert_eq!(doc.layout.steps.len(), 3);
        assert_eq!(doc.layout.repeats.len(), 3);
        assert_eq!(doc.layout.instances.len(), 6);
        assert_eq!(board_instance_count(&doc), 4);
        assert_eq!(doc.layout.repeats[0].instances.start, 0);
        assert_eq!(doc.layout.repeats[0].instances.count, 2);
        assert_eq!(doc.layout.repeats[1].instances.start, 2);
        assert_eq!(doc.layout.repeats[1].instances.count, 2);
        assert_eq!(doc.layout.repeats[2].instances.start, 4);
        assert_eq!(doc.layout.repeats[2].instances.count, 2);
        assert_eq!(doc.layout.instances[0].parent_instance, None);
        assert_eq!(doc.layout.instances[1].parent_instance, None);
        assert_eq!(doc.layout.instances[2].parent_instance, Some(0));
        assert_eq!(doc.layout.instances[3].parent_instance, Some(0));
        assert_eq!(doc.layout.instances[4].parent_instance, Some(1));
        assert_eq!(doc.layout.instances[5].parent_instance, Some(1));
    }

    #[test]
    fn nested_panel_layer_extraction_materializes_descendant_board_features() {
        let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
            .expect("synthetic nested panel fixture should parse");
        let doc = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayFlattened)
            .expect("nested panel layer should extract");
        let layer = &doc.layers[0];
        let features = layer.features.slice(&doc.features);
        let centers = features
            .iter()
            .map(|feature| feature.center)
            .collect::<Vec<_>>();

        assert_eq!(
            centers,
            [
                Point::new(7.0, 8.0),
                Point::new(22.0, 8.0),
                Point::new(7.0, 28.0),
                Point::new(22.0, 28.0)
            ]
        );
        assert_eq!(board_instance_count(&doc), 4);
    }

    /// The reported fabrication-panel bug: a render of a nested panel has to
    /// carry every descendant board's copper, not just the root step's own
    /// support geometry.
    #[test]
    fn nested_panel_render_draws_every_descendant_board_instance() {
        let ipc = ipc2581::Ipc2581::parse(nested_panel_fixture())
            .expect("synthetic nested panel fixture should parse");
        let mut doc = extract_layer_for_view(&ipc, "TOP", ArtworkScope::ArrayFlattened)
            .expect("nested panel layer should extract");
        crate::dialects::ipc::process::normalize_for_artwork(&mut doc);

        let artwork = crate::dialects::ipc::lower_layer_to_artwork(
            &doc,
            0,
            crate::dialects::LayerRole::Copper,
            crate::dialects::Side::None,
        );
        let svg = crate::render::artwork_svg(&artwork, &crate::render::RenderOptions::default());

        // One drawn pad per board across both nested repeat levels.
        assert_eq!(svg.matches("<path d=").count(), 4, "{svg}");
    }

    #[test]
    fn nested_panel_instance_bbox_includes_child_repeats_without_profile() {
        let ipc = ipc2581::Ipc2581::parse(nested_panel_without_subpanel_profile_fixture())
            .expect("synthetic nested panel fixture should parse");
        let doc = extract_layout(&ipc).expect("layout should extract");

        assert_eq!(doc.layout.instances[0].bbox.min, Point::new(5.0, 5.0));
        assert_eq!(doc.layout.instances[0].bbox.max, Point::new(30.0, 10.0));
        assert_eq!(doc.layout.instances[1].bbox.min, Point::new(5.0, 25.0));
        assert_eq!(doc.layout.instances[1].bbox.max, Point::new(30.0, 30.0));
        assert_eq!(doc.layout.repeats[0].bbox.min, Point::new(5.0, 5.0));
        assert_eq!(doc.layout.repeats[0].bbox.max, Point::new(30.0, 30.0));
    }

    #[test]
    fn repeated_panel_traces_keep_distinct_source_sets_after_processing() {
        let ipc = ipc2581::Ipc2581::parse(panel_trace_fixture())
            .expect("synthetic panel fixture should parse");
        let imported = import_design(&ipc).expect("panel should import");
        let mut doc = imported
            .materialize_layer(
                imported.layer_id("TOP").unwrap(),
                ArtworkScope::ArrayFlattened,
            )
            .expect("panel layer should extract");
        crate::dialects::ipc::process::compose_for_rendering(&mut doc);

        let layer = &doc.layers[0];
        let traces = layer
            .features
            .slice(&doc.features)
            .iter()
            .filter(|feature| feature.bucket == FeatureBucket::Trace)
            .collect::<Vec<_>>();

        assert_eq!(traces.len(), 2);
        assert!(traces.iter().all(|feature| feature.paths.count > 0));
        assert_eq!(traces[0].source.set_index, 0);
        assert_eq!(traces[1].source.set_index, 1);
        assert_eq!(traces[0].source_instance, Some(0));
        assert_eq!(traces[1].source_instance, Some(1));
        let first = feature_occurrence_id(traces[0]).unwrap();
        let second = feature_occurrence_id(traces[1]).unwrap();
        assert_eq!(first.feature, second.feature);
        let definition = imported.feature_definition(first.feature).unwrap();
        assert_eq!(definition.source.set_index, 0);
        assert_eq!(
            definition.source.feature_index,
            traces[0].source.feature_index
        );
    }

    #[test]
    fn extracts_step_profile_and_cutouts_as_physical_board_profiles() {
        let ipc = ipc2581::Ipc2581::parse(profile_fixture())
            .expect("synthetic profile fixture should parse");
        let doc = extract_layer(&ipc, "TOP").expect("profile outline should extract");

        assert_eq!(doc.profiles.len(), 1);
        assert_eq!(doc.profile_cutouts.len(), 1);
        assert_eq!(board_step_count(&doc), 1);
        assert_eq!(panel_step_count(&doc), 0);
        assert_eq!(board_instance_count(&doc), 0);
        assert_eq!(doc.layout.steps[0].profiles.start, 0);
        assert_eq!(doc.layout.steps[0].profiles.count, 1);
        assert_eq!(board_bbox(&doc).unwrap().min, Point::new(0.0, 0.0));
        assert_eq!(board_bbox(&doc).unwrap().max, Point::new(20.0, 10.0));
        assert_eq!(doc.profiles[0].bbox.min, Point::new(0.0, 0.0));
        assert_eq!(doc.profiles[0].bbox.max, Point::new(20.0, 10.0));
        assert!(doc.layers[0].bbox.is_empty());
        assert!(doc.arena.paths.iter().all(|path| path.paint == Paint::None));
        assert!(doc.arena.cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn chamfered_rect_respects_corner_flags() {
        let mut doc = GeometryDocument::new();

        push_chamfered_rect_path(
            &mut doc,
            Affine2::identity(),
            10.0,
            6.0,
            1.0,
            [true, false, false, false],
        );

        let path = &doc.arena.paths[0];
        let contour = &doc.arena.contours[path.contours.start as usize];
        let cmds = contour.cmds.slice(&doc.arena.cmds);

        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(4.0, -3.0)));
        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(5.0, -2.0)));
        assert!(cmds.iter().any(|cmd| cmd.p0 == Point::new(5.0, 2.0)));
        assert!(cmds.iter().any(|cmd| cmd.p0 == Point::new(4.0, 3.0)));
        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-4.0, 3.0)));
        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-5.0, 2.0)));
        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-5.0, -2.0)));
        assert!(!cmds.iter().any(|cmd| cmd.p0 == Point::new(-4.0, -3.0)));
    }

    #[test]
    fn rounded_rect_preserves_arcs_when_transform_preserves_circles() {
        let mut doc = GeometryDocument::new();

        push_rounded_rect_path(&mut doc, Affine2::identity(), 10.0, 6.0, 1.0, [true; 4]);

        let path = &doc.arena.paths[0];
        let contour = &doc.arena.contours[path.contours.start as usize];
        let cmds = contour.cmds.slice(&doc.arena.cmds);

        assert_eq!(cmds.iter().filter(|cmd| cmd.op == PathOp::ArcTo).count(), 4);
        assert!(!cmds.iter().any(|cmd| cmd.op == PathOp::CubicTo));
    }

    #[test]
    fn rounded_rect_uses_cubics_when_transform_distorts_circles() {
        let mut doc = GeometryDocument::new();

        push_rounded_rect_path(
            &mut doc,
            Affine2 {
                m00: 2.0,
                m01: 0.0,
                m02: 0.0,
                m10: 0.0,
                m11: 1.0,
                m12: 0.0,
            },
            10.0,
            6.0,
            1.0,
            [true; 4],
        );

        let path = &doc.arena.paths[0];
        let contour = &doc.arena.contours[path.contours.start as usize];
        let cmds = contour.cmds.slice(&doc.arena.cmds);

        assert_eq!(
            cmds.iter().filter(|cmd| cmd.op == PathOp::CubicTo).count(),
            4
        );
        assert!(!cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn slot_cavity_span_controls_target_layers() {
        let mut interner = ipc2581::Interner::new();
        let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
        let l2 = test_layer(&mut interner, "L2", LayerFunction::Signal, None);
        let l3 = test_layer(&mut interner, "L3", LayerFunction::Signal, None);
        let route = test_layer(
            &mut interner,
            "ROUT",
            LayerFunction::Rout,
            Some(ipc2581::types::ecad::LayerSpan {
                from_layer: Some(l1.name),
                to_layer: Some(l2.name),
            }),
        );
        let layers = [l1.clone(), l2.clone(), l3.clone(), route.clone()];
        let slot = test_slot(false);

        assert!(slot_applies_to_layer(&route, &l1, &layers, &slot));
        assert!(slot_applies_to_layer(&route, &l2, &layers, &slot));
        assert!(!slot_applies_to_layer(&route, &l3, &layers, &slot));
        assert!(slot_applies_to_layer(&route, &route, &layers, &slot));
        assert!(layer_span_applies_to_layer(&route, &l1, &layers));
        assert!(layer_span_applies_to_layer(&route, &l2, &layers));
        assert!(!layer_span_applies_to_layer(&route, &l3, &layers));
        assert!(layer_span_applies_to_layer(&route, &route, &layers));
    }

    #[test]
    fn partial_depth_slot_cavity_does_not_default_to_through_board() {
        let mut interner = ipc2581::Interner::new();
        let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
        let route = test_layer(&mut interner, "ROUT", LayerFunction::Rout, None);
        let layers = [l1.clone(), route.clone()];
        let slot = test_slot(true);

        assert!(!slot_applies_to_layer(&route, &l1, &layers, &slot));
        assert!(slot_applies_to_layer(&route, &route, &layers, &slot));
    }

    #[test]
    fn unspanned_route_slot_stays_on_route_layer() {
        let mut interner = ipc2581::Interner::new();
        let l1 = test_layer(&mut interner, "L1", LayerFunction::Signal, None);
        let route = test_layer(&mut interner, "ROUT", LayerFunction::Rout, None);
        let layers = [l1.clone(), route.clone()];
        let slot = test_slot(false);

        assert!(!slot_applies_to_layer(&route, &l1, &layers, &slot));
        assert!(slot_applies_to_layer(&route, &route, &layers, &slot));
    }

    #[test]
    fn rotated_slot_cavity_xform_orients_route_slot() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Cu_B.Cu_1"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu_B.Cu_1" layerFunction="ROUT" side="ALL">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="F.Cu_B.Cu_1">
          <Set>
            <SlotCavity name="SLOT1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="10" y="20"/>
              <Xform rotation="90"/>
              <Oval width="1.70" height="0.60"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let doc = extract_layer(&ipc, "F.Cu_B.Cu_1").unwrap();
        assert_eq!(doc.features.len(), 1);

        let slot = &doc.features[0];
        assert_eq!(slot.kind, FeatureKind::Slot);
        assert!((slot.rotation_degrees - 90.0).abs() < 1e-9);
        assert!(
            slot.bbox.height() > slot.bbox.width(),
            "expected rotated slot to be vertical, got bbox {:?}",
            slot.bbox
        );
        assert!((slot.bbox.width() - 0.60).abs() < 1e-6);
        assert!((slot.bbox.height() - 1.70).abs() < 1e-6);
    }

    #[test]
    fn padstack_shape_offsets_do_not_reposition_pad_locations() {
        // KiCad exports a pad's final shape center in the Pad Location. The
        // PadstackPadDef offset describes the padstack but must not be applied
        // again when placing layer artwork.
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="square">
        <RectCenter width="8" height="8"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="offset_pad">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <Location x="-2.0" y="-2.0"/>
            <StandardPrimitiveRef id="square"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="offset_pad">
              <Location x="10" y="10"/>
              <StandardPrimitiveRef id="square"/>
            </Pad>
            <Pad padstackDefRef="offset_pad">
              <Xform rotation="270.0"/>
              <Location x="40" y="10"/>
              <StandardPrimitiveRef id="square"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let doc = extract_layer(&ipc, "TOP").unwrap();
        assert_eq!(doc.features.len(), 2);

        let unrotated = doc.features[0].bbox;
        assert!((unrotated.center().x - 10.0).abs() < 1e-9);
        assert!((unrotated.center().y - 10.0).abs() < 1e-9);

        let rotated = doc.features[1].bbox;
        assert!((rotated.center().x - 40.0).abs() < 1e-9);
        assert!((rotated.center().y - 10.0).abs() < 1e-9);
    }

    #[test]
    fn extracts_nonplated_padstack_artwork_on_soldermask_layers() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Mask"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="mask_opening">
        <Circle diameter="0.9906"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <PadStackDef name="npth_mask">
          <PadstackHoleDef name="npth" diameter="0.9906" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="0" y="0"/>
          <PadstackPadDef layerRef="F.Mask" padUse="REGULAR">
            <StandardPrimitiveRef id="mask_opening"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Mask">
          <Set>
            <Pad padstackDefRef="npth_mask">
              <Location x="117.065" y="-133.14"/>
              <PinRef componentRef="J3" pin="NPTH0"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let doc = extract_layer(&ipc, "F.Mask").unwrap();

        assert_eq!(doc.features.len(), 1);
        let feature = &doc.features[0];
        assert_eq!(feature.bucket, FeatureBucket::Pth);
        assert_eq!(feature.intent.domain, FeatureDomain::Soldermask);
        assert_eq!(feature.intent.plating, PlatingKind::NonPlated);
        assert_eq!(feature.pin_refs.count, 1);
        assert!((feature.bbox.width() - 0.9906).abs() < 1e-6);
        assert!((feature.bbox.height() - 0.9906).abs() < 1e-6);
    }

    fn test_layer(
        interner: &mut ipc2581::Interner,
        name: &str,
        layer_function: LayerFunction,
        span: Option<ipc2581::types::ecad::LayerSpan>,
    ) -> Layer {
        Layer {
            name: interner.intern(name),
            layer_function,
            side: None,
            polarity: None,
            span,
            spec_refs: Vec::new(),
            profile: None,
        }
    }

    fn test_slot(z_axis_dim: bool) -> ipc2581::types::Slot {
        ipc2581::types::Slot {
            name: None,
            shape: SlotShape::Primitive(StandardPrimitive::Circle(ipc2581::types::Styled {
                shape: ipc2581::types::Circle { diameter: 1.0 },
                fill_property: None,
                line_desc: None,
                line_desc_ref: None,
                fill_desc: None,
                fill_desc_ref: None,
            })),
            plating_status: PlatingStatus::NonPlated,
            z_axis_dim,
            xform: None,
            x: 0.0,
            y: 0.0,
        }
    }

    fn panel_layer_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <Circle diameter="1"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="100" y="0"/>
            <PolyStepSegment x="100" y="80"/>
            <PolyStepSegment x="0" y="80"/>
          </Polygon>
        </Profile>
        <PadStackDef name="panel_padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="panel_padstack">
              <Location x="40" y="5"/>
            </Pad>
          </Set>
        </LayerFeature>
        <StepRepeat stepRef="board" x="10" y="20" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn panel_trace_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="trace">
        <LineDesc lineWidth="1" lineEnd="ROUND"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Polyline lineDescRef="trace">
              <PolyBegin x="0" y="0"/>
              <PolyStepSegment x="10" y="0"/>
            </Polyline>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="20" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn nested_panel_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="subpanel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="30" y="0"/>
            <PolyStepSegment x="30" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="40"/>
            <PolyStepSegment x="0" y="40"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="subpanel" x="5" y="5" nx="1" ny="2" dx="0" dy="20"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn nested_panel_without_subpanel_profile_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="subpanel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="15" dy="0"/>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="40"/>
            <PolyStepSegment x="0" y="40"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="subpanel" x="5" y="5" nx="1" ny="2" dx="0" dy="20"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn profile_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="10"/>
            <PolyStepSegment x="0" y="10"/>
          </Polygon>
          <Cutout>
            <PolyBegin x="6" y="5"/>
            <PolyStepCurve x="4" y="5" centerX="5" centerY="5" clockwise="false"/>
            <PolyStepCurve x="6" y="5" centerX="5" centerY="5" clockwise="false"/>
          </Cutout>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}
