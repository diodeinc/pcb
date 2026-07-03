use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    FillDesc, FillProperty, LayerFunction, LineEnd, LineProperty, PadUse, PlatingStatus, Polarity,
    PolyStep, SlotShape, StandardPrimitive, UserPrimitive, UserShapeType, Xform,
    ecad::{Layer, SetFeature, Step, StepRepeat, StepType},
};
use ipc2581::{Ipc2581, Symbol};

use crate::steps;
use pcb_ir::dialects::ipc::*;
use pcb_ir::geom::Polarity as GeometryPolarity;
use pcb_ir::geom::path::transform_cmds;
use pcb_ir::geom::*;

type GeometryDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;
type GeometryLayer = pcb_ir::dialects::ipc::Layer<Symbol, LayerFunction>;
type GeometryFeature = pcb_ir::dialects::ipc::Feature<Symbol>;

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
    ipc: &'a Ipc2581,
    padstacks: HashMap<Symbol, &'a ipc2581::types::PadStackDef>,
    line_descs: HashMap<Symbol, ipc2581::types::LineDesc>,
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
enum PadPrimitiveRef {
    Standard(Symbol),
    User(Symbol),
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
        net: set.net,
        polarity,
        spec_refs,
        features: Span::new(doc.features.len() as u32, 0),
        bbox: BBox::empty(),
    });
    set_id
}

fn push_extracted_feature(
    doc: &mut GeometryDocument,
    set_id: u32,
    source_layer_ref: Symbol,
    mut feature: GeometryFeature,
    layer_bbox: &mut BBox,
) {
    feature.source_layer_ref = Some(source_layer_ref);
    feature.set = Some(set_id);
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
    if feature.intent.side == pcb_ir::dialects::Side::None {
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
    if crate::layers::is_copper(function) {
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

fn side_for_layer(side: Option<ipc2581::types::ecad::Side>) -> pcb_ir::dialects::Side {
    match side {
        Some(ipc2581::types::ecad::Side::Top) => pcb_ir::dialects::Side::Top,
        Some(ipc2581::types::ecad::Side::Bottom) => pcb_ir::dialects::Side::Bottom,
        Some(ipc2581::types::ecad::Side::Internal) => pcb_ir::dialects::Side::Inner,
        _ => pcb_ir::dialects::Side::None,
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
    }
}

pub fn extract_layer(ipc: &Ipc2581, layer_name: &str) -> Result<GeometryDocument> {
    extract_layer_for_view(ipc, layer_name, View::ArrayFlattened)
}

pub fn extract_layer_for_view(
    ipc: &Ipc2581,
    layer_name: &str,
    view: View,
) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let layer = ecad
        .cad_data
        .layers
        .iter()
        .find(|layer| ipc.resolve(layer.name) == layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;
    let primary_step = steps::primary_step(ipc, &ecad.cad_data.steps)
        .context("IPC-2581 ECAD section has no Step")?;

    let step = match view {
        View::Board => canonical_board_step(ipc, &ecad.cad_data.steps, primary_step)?,
        View::ArrayLocal | View::ArraySupport | View::ArrayFlattened | View::LayoutSymbolic => {
            primary_step
        }
    };

    let mut doc = match view {
        View::Board => extract_step_layer(ipc, step, &ecad.cad_data.layers, layer, layer_name)?,
        View::ArraySupport if is_panel_step(step) => extract_panel_layer(
            ipc,
            &ecad.cad_data.steps,
            &ecad.cad_data.layers,
            step,
            layer,
            layer_name,
            PanelLayerMode::SupportOnly,
        )?,
        View::ArrayFlattened if is_panel_step(step) => extract_panel_layer(
            ipc,
            &ecad.cad_data.steps,
            &ecad.cad_data.layers,
            step,
            layer,
            layer_name,
            PanelLayerMode::Flattened,
        )?,
        View::ArrayLocal | View::ArraySupport | View::ArrayFlattened | View::LayoutSymbolic => {
            extract_step_layer(ipc, step, &ecad.cad_data.layers, layer, layer_name)?
        }
    };

    match view {
        View::Board | View::ArrayLocal | View::ArraySupport => {
            append_step_only_layout_geometry(&mut doc, step)
        }
        View::ArrayFlattened | View::LayoutSymbolic => {
            append_layout_geometry(&mut doc, ipc, &ecad.cad_data.steps, step)?
        }
    }
    populate_ipc_specs(&mut doc, ipc);
    pcb_ir::dialects::ipc::process::normalize_bounds(&mut doc);
    Ok(doc)
}

fn canonical_board_step<'a>(
    ipc: &Ipc2581,
    steps: &'a [Step],
    primary_step: &'a Step,
) -> Result<&'a Step> {
    if is_board_step(primary_step) {
        return Ok(primary_step);
    }

    let mut stack = vec![primary_step.name];
    if let Some(step) = first_reachable_board_step(ipc, steps, primary_step, &mut stack)? {
        return Ok(step);
    }

    bail!(
        "IPC-2581 primary step '{}' does not reference a board step",
        ipc.resolve(primary_step.name)
    )
}

fn first_reachable_board_step<'a>(
    ipc: &Ipc2581,
    steps: &'a [Step],
    parent_step: &Step,
    stack: &mut Vec<Symbol>,
) -> Result<Option<&'a Step>> {
    for repeat in &parent_step.step_repeats {
        let source_step = steps
            .iter()
            .find(|step| step.name == repeat.step_ref)
            .with_context(|| {
                format!(
                    "StepRepeat references unknown Step '{}'",
                    ipc.resolve(repeat.step_ref)
                )
            })?;

        if is_board_step(source_step) {
            return Ok(Some(source_step));
        }
        if !is_panel_step(source_step) {
            continue;
        }
        if stack.contains(&source_step.name) {
            bail!(
                "StepRepeat cycle references Step '{}'",
                ipc.resolve(source_step.name)
            );
        }

        stack.push(source_step.name);
        let board_step = first_reachable_board_step(ipc, steps, source_step, stack)?;
        stack.pop();
        if board_step.is_some() {
            return Ok(board_step);
        }
    }

    Ok(None)
}

pub fn extract_layout(ipc: &Ipc2581) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step = steps::primary_step(ipc, &ecad.cad_data.steps)
        .context("IPC-2581 ECAD section has no Step")?;
    let mut doc = GeometryDocument::new();
    append_layout_geometry(&mut doc, ipc, &ecad.cad_data.steps, step)?;
    populate_ipc_specs(&mut doc, ipc);
    pcb_ir::dialects::ipc::process::normalize_bounds(&mut doc);
    Ok(doc)
}

fn extract_panel_layer(
    ipc: &Ipc2581,
    steps: &[Step],
    layers: &[Layer],
    panel: &Step,
    layer: &Layer,
    layer_name: &str,
    mode: PanelLayerMode,
) -> Result<GeometryDocument> {
    let mut doc = GeometryDocument::new();
    let feature_start = doc.features.len() as u32;
    let set_start = doc.feature_sets.len() as u32;
    let mut layer_bbox = BBox::empty();
    let mut append_state = LayerAppendState::default();
    let mut stack = vec![panel.name];

    layer_bbox = layer_bbox.union(append_step_layer_tree(
        &mut doc,
        &mut append_state,
        LayerMaterializeContext {
            ipc,
            steps,
            layers,
            layer,
            layer_name,
        },
        panel,
        Affine2::identity(),
        mode,
        &mut stack,
    )?);

    let feature_count = doc.features.len() as u32 - feature_start;
    let set_count = doc.feature_sets.len() as u32 - set_start;
    let spec_refs = push_spec_refs(&mut doc, &layer.spec_refs);
    doc.layers.push(GeometryLayer {
        name: layer_name.to_string(),
        source_layer_ref: layer.name,
        layer_function: layer.layer_function,
        spec_refs,
        sets: Span::new(set_start, set_count),
        features: Span::new(feature_start, feature_count),
        bbox: layer_bbox,
    });

    Ok(doc)
}

#[derive(Debug, Clone, Copy)]
struct LayerMaterializeContext<'a> {
    ipc: &'a Ipc2581,
    steps: &'a [Step],
    layers: &'a [Layer],
    layer: &'a Layer,
    layer_name: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelLayerMode {
    SupportOnly,
    Flattened,
}

fn append_step_layer_tree(
    doc: &mut GeometryDocument,
    append_state: &mut LayerAppendState,
    context: LayerMaterializeContext<'_>,
    step: &Step,
    transform: Affine2,
    mode: PanelLayerMode,
    stack: &mut Vec<Symbol>,
) -> Result<BBox> {
    if mode == PanelLayerMode::SupportOnly && is_board_step(step) {
        return Ok(BBox::empty());
    }

    let step_doc = extract_step_layer(
        context.ipc,
        step,
        context.layers,
        context.layer,
        context.layer_name,
    )?;
    doc.diagnostics.extend(step_doc.diagnostics.iter().cloned());
    let mut bbox = append_state.append_layer(doc, &step_doc, 0, transform)?;

    for repeat in &step.step_repeats {
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

        stack.push(source_step.name);
        for iy in 0..repeat.ny {
            for ix in 0..repeat.nx {
                let instance_transform = transform.concat(step_repeat_transform(repeat, ix, iy));
                bbox = bbox.union(append_step_layer_tree(
                    doc,
                    append_state,
                    context,
                    source_step,
                    instance_transform,
                    mode,
                    stack,
                )?);
            }
        }
        stack.pop();
    }

    Ok(bbox)
}

fn extract_step_layer(
    ipc: &Ipc2581,
    step: &Step,
    layers: &[Layer],
    layer: &Layer,
    layer_name: &str,
) -> Result<GeometryDocument> {
    let content = ipc.content();
    let context = ExtractContext {
        ipc,
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
            let set_id =
                push_feature_set_record(&mut doc, layer_index, set_index as u32, set, polarity);

            for (feature_index, set_feature) in set.features.iter().enumerate() {
                let source = SourceRef {
                    set_index: set_index as u32,
                    feature_index: feature_index as u32,
                };
                let features = match set_feature {
                    SetFeature::Pad(pad) => extract_pad(
                        &context, layer.name, set.net, polarity, source, pad, &mut doc,
                    )?
                    .into_iter()
                    .collect(),
                    SetFeature::Fiducial(fiducial) => {
                        extract_fiducial(&context, set.net, polarity, source, fiducial, &mut doc)?
                            .into_iter()
                            .collect()
                    }
                    SetFeature::Trace(trace) => {
                        extract_trace(&context, set.net, polarity, source, trace, &mut doc)
                            .into_iter()
                            .collect()
                    }
                    SetFeature::UserPrimitive(primitive) => extract_inline_user_primitive(
                        &context, set.net, polarity, source, primitive, &mut doc,
                    )?,
                    SetFeature::Polygon(polygon) => vec![extract_polygon(
                        set.net, polarity, source, polygon, &mut doc,
                    )],
                    SetFeature::Line(line) => vec![extract_line(
                        &context, set.net, polarity, source, line, &mut doc,
                    )],
                    SetFeature::Arc(arc) => vec![extract_arc(
                        &context, set.net, polarity, source, arc, &mut doc,
                    )],
                    SetFeature::Polyline(polyline) => vec![extract_feature_polyline(
                        &context, set.net, polarity, source, polyline, &mut doc,
                    )],
                    SetFeature::StandardPrimitiveRef(primitive_ref) => extract_feature_primitive(
                        &context,
                        set.net,
                        polarity,
                        source,
                        primitive_ref,
                        FeaturePrimitiveKind::Standard,
                        &mut doc,
                    )?,
                    SetFeature::UserPrimitiveRef(primitive_ref) => extract_feature_primitive(
                        &context,
                        set.net,
                        polarity,
                        source,
                        primitive_ref,
                        FeaturePrimitiveKind::User,
                        &mut doc,
                    )?,
                    SetFeature::Hole(_) | SetFeature::Slot(_) => Vec::new(),
                };

                for mut feature in features {
                    feature.source_step_ref = Some(step.name);
                    feature.source_step_kind = source_step_kind;
                    complete_feature_intent(layer, &mut feature);
                    push_extracted_feature(
                        &mut doc,
                        set_id,
                        layer_feature.layer_ref,
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
            let mut emitted = Vec::new();

            if is_drill_layer && source_layer.name == layer.name {
                for (feature_index, set_feature) in set.features.iter().enumerate() {
                    if let SetFeature::Hole(hole) = set_feature {
                        let feature = extract_hole(
                            SourceRef {
                                set_index: set_index as u32,
                                feature_index: feature_index as u32,
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

fn step_repeat_transform(repeat: &StepRepeat, ix: u32, iy: u32) -> Affine2 {
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

#[derive(Debug, Default)]
struct LayerAppendState {
    next_source_set_index: u32,
}

impl LayerAppendState {
    fn append_layer(
        &mut self,
        target: &mut GeometryDocument,
        source: &GeometryDocument,
        layer_index: usize,
        transform: Affine2,
    ) -> Result<BBox> {
        let source_set_offset = self.next_source_set_index;
        let source_set_span = source_layer_set_span(source, layer_index)?;
        let bbox =
            append_transformed_layer(target, source, layer_index, transform, source_set_offset)?;
        self.next_source_set_index = self
            .next_source_set_index
            .checked_add(source_set_span)
            .context("Panel contains too many repeated source feature sets")?;
        Ok(bbox)
    }
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

fn append_transformed_layer(
    target: &mut GeometryDocument,
    source: &GeometryDocument,
    layer_index: usize,
    transform: Affine2,
    source_set_offset: u32,
) -> Result<BBox> {
    let layer = &source.layers[layer_index];
    let mut layer_bbox = BBox::empty();

    for source_set_index in layer.sets.indices() {
        let source_set = &source.feature_sets[source_set_index as usize];
        let spec_ref_start = target.spec_refs.len() as u32;
        target.spec_refs.extend(
            source_set
                .spec_refs
                .slice(&source.spec_refs)
                .iter()
                .cloned(),
        );
        let target_set = target.feature_sets.len() as u32;
        target.feature_sets.push(FeatureSet {
            layer: 0,
            source_set_index: source_set
                .source_set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?,
            source_geometry_ref: source_set.source_geometry_ref,
            net: source_set.net,
            polarity: source_set.polarity,
            spec_refs: Span::new(
                spec_ref_start,
                target.spec_refs.len() as u32 - spec_ref_start,
            ),
            features: Span::new(target.features.len() as u32, 0),
            bbox: BBox::empty(),
        });

        for feature in source_set.features.slice(&source.features) {
            let path_start = target.arena.paths.len() as u32;
            for path_index in feature.paths.indices() {
                target
                    .arena
                    .append_path_from(&source.arena, path_index, transform);
            }
            let path_count = target.arena.paths.len() as u32 - path_start;
            let paths = Span::new(path_start, path_count);
            let bbox = target.arena.paths_bbox(paths);

            let mut feature = feature.clone();
            feature.transform = transform.concat(feature.transform);
            feature.bbox = bbox;
            feature.paths = paths;
            feature.center = transform.transform_point(feature.center);
            feature.set = Some(target_set);
            feature.source.set_index = feature
                .source
                .set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?;
            if !feature.pin_refs.is_empty() {
                let pin_ref_start = target.pin_refs.len() as u32;
                target
                    .pin_refs
                    .extend(feature.pin_refs.slice(&source.pin_refs).iter().cloned());
                feature.pin_refs = Span::new(pin_ref_start, feature.pin_refs.count);
            }
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

fn append_step_only_layout_geometry(doc: &mut GeometryDocument, primary_step: &Step) {
    if is_panel_step(primary_step) {
        let profiles = append_step_profile(doc, primary_step);
        let step = push_or_update_layout_step(doc, primary_step, profiles);
        doc.layout.root_step = Some(step);
    } else if is_board_step(primary_step) {
        doc.layout.root_step = Some(ensure_layout_step_for_step(doc, primary_step));
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

fn is_panel_step(step: &Step) -> bool {
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
    let Some(padstack) = context.padstacks.get(&padstack_ref).copied() else {
        doc.warn(format!(
            "Skipping pad referencing missing padstack '{}'",
            context.ipc.resolve(padstack_ref)
        ));
        return Ok(None);
    };

    let role = match padstack.hole_def.as_ref().map(|hole| hole.plating_status) {
        Some(PlatingStatus::Via) => FeatureRole::Via,
        _ => FeatureRole::Pad,
    };

    let placement = ipc_placement(Point::new(x, y), pad.xform);

    let primitive_ref = pad_primitive_ref(pad, padstack, layer_ref);
    let Some(primitive_ref) = primitive_ref else {
        doc.warn(format!(
            "Skipping padstack '{}' because it has no regular primitive for layer '{}'",
            context.ipc.resolve(padstack.name),
            context.ipc.resolve(layer_ref)
        ));
        return Ok(None);
    };

    let path_start = doc.arena.paths.len() as u32;
    let paint = match primitive_ref {
        PadPrimitiveRef::Standard(primitive_ref) => {
            let Some(primitive) = context.standard_primitives.get(&primitive_ref).copied() else {
                doc.warn(format!(
                    "Skipping padstack '{}' because primitive '{}' is missing",
                    context.ipc.resolve(padstack.name),
                    context.ipc.resolve(primitive_ref)
                ));
                return Ok(None);
            };
            lower_standard_primitive(context, doc, primitive, placement.transform)?
        }
        PadPrimitiveRef::User(primitive_ref) => {
            let Some(primitive) = context.user_primitives.get(&primitive_ref).copied() else {
                doc.warn(format!(
                    "Skipping padstack '{}' because user primitive '{}' is missing",
                    context.ipc.resolve(padstack.name),
                    context.ipc.resolve(primitive_ref)
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
    feature.primitive_ref = match primitive_ref {
        PadPrimitiveRef::Standard(primitive_ref) | PadPrimitiveRef::User(primitive_ref) => {
            Some(primitive_ref)
        }
    };
    feature.intent.plating = padstack
        .hole_def
        .as_ref()
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
    padstack: &ipc2581::types::PadStackDef,
    layer_ref: Symbol,
) -> Option<PadPrimitiveRef> {
    pad.standard_primitive_ref
        .map(PadPrimitiveRef::Standard)
        .or_else(|| pad.user_primitive_ref.map(PadPrimitiveRef::User))
        .or_else(|| find_pad_primitive_ref(padstack, layer_ref))
}

fn find_pad_primitive_ref(
    padstack: &ipc2581::types::PadStackDef,
    layer_ref: Symbol,
) -> Option<PadPrimitiveRef> {
    padstack
        .pad_defs
        .iter()
        .find(|pad_def| pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Regular)
        .or_else(|| {
            padstack.pad_defs.iter().find(|pad_def| {
                pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Thermal
            })
        })
        .and_then(|pad_def| {
            pad_def
                .standard_primitive_ref
                .map(PadPrimitiveRef::Standard)
                .or_else(|| pad_def.user_primitive_ref.map(PadPrimitiveRef::User))
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
    let paint = match primitive_kind {
        FeaturePrimitiveKind::Standard => {
            let Some(primitive) = context.standard_primitives.get(&primitive_ref.id).copied()
            else {
                doc.warn(format!(
                    "Skipping feature because standard primitive '{}' is missing",
                    context.ipc.resolve(primitive_ref.id)
                ));
                return Ok(Vec::new());
            };
            lower_standard_primitive(context, doc, primitive, transform)?
        }
        FeaturePrimitiveKind::User => {
            let Some(primitive) = context.user_primitives.get(&primitive_ref.id).copied() else {
                doc.warn(format!(
                    "Skipping feature because user primitive '{}' is missing",
                    context.ipc.resolve(primitive_ref.id)
                ));
                return Ok(Vec::new());
            };
            lower_user_primitive(context, doc, primitive, transform)
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
            Some(primitive_ref.id),
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
    primitive_ref: Option<Symbol>,
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
                    context.ipc.resolve(*primitive_ref)
                ));
                return Ok(None);
            };
            (
                lower_standard_primitive(context, doc, primitive, placement.transform)?,
                Some(*primitive_ref),
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
            context.ipc.resolve(line_desc_ref)
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
    let path_start = doc.arena.paths.len() as u32;
    let center = Point::new(hole.x, hole.y);
    push_ellipse_path(
        doc,
        Affine2::placement(center, 0.0, Mirror::NONE, 1.0),
        hole.diameter,
        hole.diameter,
    );
    let paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);

    let mut feature = GeometryFeature::new(FeatureKind::Hole, GeometryPolarity::Dark);
    feature.source = source;
    feature.bbox = doc.arena.paths_bbox(paths);
    feature.paths = paths;
    feature.center = center;
    feature.outer_diameter = hole.diameter;
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
    let paint = primitive_paint(primitive);
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

fn primitive_paint(primitive: &StandardPrimitive) -> PrimitivePaint {
    match primitive_fill_property(primitive) {
        Some(FillProperty::Hollow) => PrimitivePaint::Hollow,
        Some(FillProperty::Void) => PrimitivePaint::Void,
        _ => PrimitivePaint::Fill,
    }
}

fn primitive_fill_property(primitive: &StandardPrimitive) -> Option<FillProperty> {
    match primitive {
        StandardPrimitive::Circle(styled) => styled.fill_property,
        StandardPrimitive::RectCenter(styled) => styled.fill_property,
        StandardPrimitive::RectRound(styled) => styled.fill_property,
        StandardPrimitive::RectCham(styled) => styled.fill_property,
        StandardPrimitive::RectCorner(styled) => styled.fill_property,
        StandardPrimitive::Oval(styled) => styled.fill_property,
        StandardPrimitive::Butterfly(styled) => styled.fill_property,
        StandardPrimitive::Diamond(styled) => styled.fill_property,
        StandardPrimitive::Donut(styled) => styled.fill_property,
        StandardPrimitive::Ellipse(styled) => styled.fill_property,
        StandardPrimitive::Hexagon(styled) => styled.fill_property,
        StandardPrimitive::Octagon(styled) => styled.fill_property,
        StandardPrimitive::Thermal(styled) => styled.fill_property,
        StandardPrimitive::Triangle(styled) => styled.fill_property,
        StandardPrimitive::Moire(_) | StandardPrimitive::Contour(_) => None,
    }
}

fn primitive_line_desc(
    context: &ExtractContext<'_>,
    primitive: &StandardPrimitive,
) -> Option<ipc2581::types::LineDesc> {
    let line_desc_ref = match primitive {
        StandardPrimitive::Circle(styled) => styled.line_desc_ref,
        StandardPrimitive::RectCenter(styled) => styled.line_desc_ref,
        StandardPrimitive::RectRound(styled) => styled.line_desc_ref,
        StandardPrimitive::RectCham(styled) => styled.line_desc_ref,
        StandardPrimitive::RectCorner(styled) => styled.line_desc_ref,
        StandardPrimitive::Oval(styled) => styled.line_desc_ref,
        StandardPrimitive::Butterfly(styled) => styled.line_desc_ref,
        StandardPrimitive::Diamond(styled) => styled.line_desc_ref,
        StandardPrimitive::Donut(styled) => styled.line_desc_ref,
        StandardPrimitive::Ellipse(styled) => styled.line_desc_ref,
        StandardPrimitive::Hexagon(styled) => styled.line_desc_ref,
        StandardPrimitive::Octagon(styled) => styled.line_desc_ref,
        StandardPrimitive::Thermal(styled) => styled.line_desc_ref,
        StandardPrimitive::Triangle(styled) => styled.line_desc_ref,
        StandardPrimitive::Moire(_) | StandardPrimitive::Contour(_) => None,
    }?;
    context.line_descs.get(&line_desc_ref).copied()
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
        LineEnd::Round => LineCap::Round,
        LineEnd::Square => LineCap::Square,
        LineEnd::Flat => LineCap::Butt,
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

        let layer = extract_layer_for_view(&ipc, "TOP", View::Board).unwrap();
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

        let top = extract_layer_for_view(&ipc, "TOP", View::ArrayFlattened).unwrap();
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

        let vcut = extract_layer_for_view(&ipc, "VCUT", View::ArrayFlattened).unwrap();
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
        let circle = ipc2581::types::StandardPrimitive::Circle(ipc2581::types::Styled {
            shape: ipc2581::types::Circle { diameter: 1.0 },
            fill_property: Some(FillProperty::Hollow),
            line_desc_ref: None,
        });
        let rect = ipc2581::types::StandardPrimitive::RectCenter(ipc2581::types::Styled {
            shape: ipc2581::types::RectCenter {
                size: ipc2581::types::Size {
                    width: 1.0,
                    height: 1.0,
                },
            },
            fill_property: Some(FillProperty::Void),
            line_desc_ref: None,
        });

        assert_eq!(primitive_paint(&circle), PrimitivePaint::Hollow);
        assert_eq!(primitive_paint(&rect), PrimitivePaint::Void);
    }

    #[test]
    fn zero_area_standard_primitive_emits_no_paths() {
        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
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
            line_desc_ref: None,
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

        let feature = extract_feature_polyline(
            &ExtractContext {
                ipc: &Ipc2581::parse(
                    r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
                )
                .unwrap(),
                padstacks: HashMap::new(),
                line_descs: HashMap::new(),
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
                    angle1: None,
                    angle2: None,
                }),
            }],
        });

        let ipc = Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="Owner"><FunctionMode mode="FABRICATION"/></Content></IPC-2581>"#,
        )
        .unwrap();
        let context = ExtractContext {
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
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
        <LineDesc lineWidth="0.15" lineEnd="FLAT"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
</IPC-2581>"#,
        )
        .unwrap();
        let entry = ipc.content().dictionary_line_desc.entries[0].clone();
        let context = ExtractContext {
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::from([(entry.id, entry.line_desc)]),
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
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
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
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
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
                    },
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Contour(ipc2581::types::Contour {
                            polygon: rect_polygon(0.5, 0.5, 1.5, 1.5),
                            cutouts: Vec::new(),
                        }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
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
            ipc: &ipc,
            padstacks: HashMap::new(),
            line_descs: HashMap::new(),
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
                    },
                    ipc2581::types::UserShape {
                        shape: UserShapeType::Contour(ipc2581::types::Contour {
                            polygon: rect_polygon(0.0, 1.0, 2.0, 3.0),
                            cutouts: Vec::new(),
                        }),
                        line_desc: None,
                        line_desc_ref: None,
                        fill_desc: None,
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
    fn extracts_layer_for_geometry_view_board_or_board_array() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");

        let board =
            extract_layer_for_view(&ipc, "TOP", View::Board).expect("board layer should extract");
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

        let panel = extract_layer_for_view(&ipc, "TOP", View::ArrayFlattened)
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
    fn symbolic_panel_extraction_carries_repeats_without_child_features() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        let doc = extract_layer_for_view(&ipc, "TOP", View::LayoutSymbolic)
            .expect("panel layer should extract");
        let layer = &doc.layers[0];
        let features = layer.features.slice(&doc.features);

        assert_eq!(features.len(), 1);
        assert_eq!(features[0].center, Point::new(40.0, 5.0));
        assert_eq!(doc.layout.steps.len(), 2);
        assert_eq!(doc.layout.repeats.len(), 1);
        assert_eq!(doc.layout.instances.len(), 2);
        assert_eq!(board_instance_count(&doc), 2);
    }

    #[test]
    fn step_only_panel_extraction_omits_repeat_graph_expansion() {
        let ipc = ipc2581::Ipc2581::parse(panel_layer_fixture())
            .expect("synthetic panel fixture should parse");
        let doc = extract_layer_for_view(&ipc, "TOP", View::ArrayLocal)
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
        let doc = extract_layer_for_view(&ipc, "TOP", View::ArrayFlattened)
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
        let mut doc = extract_layer(&ipc, "TOP").expect("panel layer should extract");
        pcb_ir::dialects::ipc::process::compose_for_rendering(&mut doc);

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
                line_desc_ref: None,
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
