use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::types::LayerFunction;
use pcb_ir::common::{Point, Unit};
use pcb_ir::dialects::ipc::{
    FeatureKind, FeatureOperation, FeatureSpan, GeometryFeature, GeometryView, LayoutStepKind,
    PlatingKind,
};
use pcb_ir::dialects::nc::{
    NcDocument, NcFunction, NcGeometry, NcObject, NcPlating, NcRouteSegment, NcSpan,
};

use crate::geometry;
use crate::manufacturing::{ManufacturingFile, ManufacturingFileKind};
use crate::xnc::{XncAttribute, XncBuilder, XncUnit, write_xnc};

type IpcGeometryDocument = pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>;

pub fn build_xnc_drill_files(ipc: &Ipc2581, view: GeometryView) -> Result<Vec<ManufacturingFile>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = copper_layer_refs(&ecad.cad_data.layers);
    let mut nc = NcDocument::new(Unit::Millimeter);

    for layer in &ecad.cad_data.layers {
        if !matches!(
            layer.layer_function,
            LayerFunction::Drill | LayerFunction::Rout
        ) {
            continue;
        }
        let layer_name = ipc.resolve(layer.name);
        let doc = geometry::extract_layer_for_view(ipc, layer_name, view).with_context(|| {
            format!("failed to extract IPC-2581 drill/rout layer '{layer_name}'")
        })?;
        collect_nc_features(&doc, &mut nc)?;
    }

    xnc_files_from_nc(ipc, &nc, &copper_layers)
}

fn copper_layer_refs(layers: &[ipc2581::types::ecad::Layer]) -> Vec<ipc2581::Symbol> {
    layers
        .iter()
        .filter(|layer| {
            matches!(
                layer.layer_function,
                LayerFunction::Conductor
                    | LayerFunction::CondFilm
                    | LayerFunction::CondFoil
                    | LayerFunction::Plane
                    | LayerFunction::Signal
                    | LayerFunction::Mixed
            )
        })
        .map(|layer| layer.name)
        .collect()
}

fn collect_nc_features(
    doc: &IpcGeometryDocument,
    nc: &mut NcDocument<ipc2581::Symbol>,
) -> Result<()> {
    for layer in &doc.layers {
        for feature in &doc.features
            [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        {
            match feature.kind {
                FeatureKind::Hole if feature.outer_diameter > 0.0 => {
                    nc.objects.push(nc_object_from_feature(
                        doc,
                        feature,
                        NcGeometry::Drill {
                            at: feature.center,
                            diameter: feature.outer_diameter,
                        },
                    )?);
                }
                FeatureKind::Slot => {
                    if feature.intent.operation == FeatureOperation::Route
                        && feature.source_step_kind != LayoutStepKind::Board
                    {
                        continue;
                    }
                    let Some(slot) = nc_linear_slot(feature) else {
                        if feature.intent.operation == FeatureOperation::Route {
                            continue;
                        }
                        bail!(
                            "cannot export slot on layer '{}' to XNC because it is not a simple oval slot",
                            layer.name
                        );
                    };
                    let geometry = NcGeometry::Slot {
                        diameter: slot.diameter,
                        start: slot.start,
                        end: slot.end,
                    };
                    nc.objects
                        .push(nc_object_from_feature(doc, feature, geometry)?);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn nc_object_from_feature(
    doc: &IpcGeometryDocument,
    feature: &GeometryFeature<ipc2581::Symbol>,
    geometry: NcGeometry,
) -> Result<NcObject<ipc2581::Symbol>> {
    let plating = match feature.intent.plating {
        PlatingKind::Via | PlatingKind::ViaCapped | PlatingKind::Plated => NcPlating::Plated,
        PlatingKind::NonPlated | PlatingKind::None => NcPlating::NonPlated,
        PlatingKind::Unknown => {
            bail!("cannot export drill/rout feature to XNC with unknown plating")
        }
    };
    let function = if matches!(
        feature.intent.plating,
        PlatingKind::Via | PlatingKind::ViaCapped
    ) {
        NcFunction::Via
    } else {
        NcFunction::Component
    };
    let span = match feature.intent.span {
        FeatureSpan::ThroughBoard | FeatureSpan::Unknown => NcSpan::ThroughBoard,
        FeatureSpan::Layer(layer) => NcSpan::FromTo {
            from: Some(layer),
            to: Some(layer),
        },
        FeatureSpan::FromTo { from, to } => NcSpan::FromTo { from, to },
    };

    let pin_ref = (feature.pin_ref_count > 0)
        .then(|| doc.pin_refs.get(feature.pin_ref_start as usize))
        .flatten();
    Ok(NcObject {
        geometry,
        plating,
        span,
        function,
        net: feature.net,
        component: pin_ref.and_then(|pin_ref| pin_ref.component_ref),
        pin: pin_ref.map(|pin_ref| pin_ref.pin),
    })
}

#[derive(Debug, Clone, Copy)]
struct NcLinearSlot {
    diameter: f64,
    start: Point,
    end: Point,
}

fn nc_linear_slot(feature: &GeometryFeature<ipc2581::Symbol>) -> Option<NcLinearSlot> {
    if feature.width <= 0.0 || feature.height <= 0.0 || feature.scale <= 0.0 {
        return None;
    }
    let diameter = feature.width.min(feature.height) * feature.scale;
    if diameter <= NC_GEOMETRY_EPSILON {
        return None;
    }
    let long = feature.width.max(feature.height);
    let short = feature.width.min(feature.height);
    let centerline = (long - short).max(0.0) / 2.0;
    if centerline <= NC_GEOMETRY_EPSILON {
        return None;
    }
    let (start, end) = if feature.width >= feature.height {
        (Point::new(-centerline, 0.0), Point::new(centerline, 0.0))
    } else {
        (Point::new(0.0, -centerline), Point::new(0.0, centerline))
    };
    Some(NcLinearSlot {
        diameter,
        start: feature.transform.transform_point(start),
        end: feature.transform.transform_point(end),
    })
}

const NC_GEOMETRY_EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct XncGroupKey {
    plating: NcPlating,
    span: XncSpanKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum XncSpanKey {
    ThroughBoard,
    FromTo { from: usize, to: usize },
}

fn xnc_files_from_nc(
    ipc: &Ipc2581,
    nc: &NcDocument<ipc2581::Symbol>,
    copper_layers: &[ipc2581::Symbol],
) -> Result<Vec<ManufacturingFile>> {
    let mut groups = std::collections::BTreeMap::<XncGroupKey, XncBuilder>::new();
    for object in &nc.objects {
        let key = XncGroupKey {
            plating: object.plating,
            span: xnc_span_key(copper_layers, &object.span),
        };
        let unit = match nc.unit {
            Unit::Millimeter => XncUnit::Metric,
            Unit::Inch => XncUnit::Inch,
        };
        let file_function = xnc_file_function(&key, copper_layers);
        let builder = groups
            .entry(key)
            .or_insert_with(|| XncBuilder::new(unit, vec![file_function]));
        let tool_attributes = xnc_tool_attributes(object);
        let object_attributes = xnc_object_attributes(ipc, object);
        match &object.geometry {
            NcGeometry::Drill { at, diameter } => {
                builder.add_drill(*diameter, *at, tool_attributes, object_attributes)?;
            }
            NcGeometry::Slot {
                start,
                end,
                diameter,
            } => {
                builder.add_slot(*diameter, *start, *end, tool_attributes, object_attributes)?;
            }
            NcGeometry::Route {
                start,
                diameter,
                segments,
            } => {
                builder.add_route(
                    *diameter,
                    *start,
                    segments
                        .iter()
                        .map(|segment| match *segment {
                            NcRouteSegment::Line { to } => crate::xnc::XncRouteSegment::Line { to },
                            NcRouteSegment::ClockwiseArc { to, radius } => {
                                crate::xnc::XncRouteSegment::ClockwiseArc { to, radius }
                            }
                            NcRouteSegment::CounterClockwiseArc { to, radius } => {
                                crate::xnc::XncRouteSegment::CounterClockwiseArc { to, radius }
                            }
                        })
                        .collect(),
                    tool_attributes,
                    object_attributes,
                )?;
            }
        }
    }

    groups
        .into_iter()
        .filter_map(|(key, builder)| {
            let document = builder.finish();
            (!document.is_empty()).then_some((key, document))
        })
        .map(|(key, document)| {
            Ok(ManufacturingFile {
                filename: xnc_filename(&key),
                kind: ManufacturingFileKind::Xnc,
                contents: write_xnc(&document)?,
            })
        })
        .collect()
}

fn xnc_file_function(key: &XncGroupKey, copper_layers: &[ipc2581::Symbol]) -> XncAttribute {
    let (plating, suffix) = match key.plating {
        NcPlating::Plated => ("Plated", xnc_span_suffix(copper_layers, key.span)),
        NcPlating::NonPlated => ("NonPlated", "NPTH".to_string()),
    };
    let (from, to) = key.span.layer_numbers(copper_layers.len().max(1));
    XncAttribute::file(
        "FileFunction",
        [
            plating.to_string(),
            from.to_string(),
            to.to_string(),
            suffix,
        ],
    )
}

fn xnc_tool_attributes(object: &NcObject<ipc2581::Symbol>) -> Vec<XncAttribute> {
    let drill_function = match object.function {
        NcFunction::Via => "ViaDrill",
        NcFunction::Component => "ComponentDrill",
    };
    let fields = match object.plating {
        NcPlating::Plated => vec!["Plated", "PTH", drill_function],
        NcPlating::NonPlated => vec!["NonPlated", "NPTH", drill_function],
    };
    vec![XncAttribute::tool("AperFunction", fields)]
}

fn xnc_object_attributes(ipc: &Ipc2581, object: &NcObject<ipc2581::Symbol>) -> Vec<XncAttribute> {
    let mut attributes = Vec::new();
    if let Some(net) = object.net {
        attributes.push(XncAttribute::object("N", [ipc.resolve(net)]));
    }
    if let Some(component) = object.component {
        attributes.push(XncAttribute::object("C", [ipc.resolve(component)]));
        if let Some(pin) = object.pin {
            attributes.push(XncAttribute::object(
                "P",
                [ipc.resolve(component), ipc.resolve(pin)],
            ));
        }
    }
    attributes
}

fn xnc_span_key(copper_layers: &[ipc2581::Symbol], span: &NcSpan<ipc2581::Symbol>) -> XncSpanKey {
    match span {
        NcSpan::FromTo { from, to } => {
            let Some(from) = from.and_then(|layer| copper_layer_index(copper_layers, layer)) else {
                return XncSpanKey::ThroughBoard;
            };
            let Some(to) = to.and_then(|layer| copper_layer_index(copper_layers, layer)) else {
                return XncSpanKey::ThroughBoard;
            };
            let (from, to) = (from.min(to), from.max(to));
            if from == 1 && to == copper_layers.len().max(1) {
                XncSpanKey::ThroughBoard
            } else {
                XncSpanKey::FromTo { from, to }
            }
        }
        NcSpan::ThroughBoard => XncSpanKey::ThroughBoard,
    }
}

fn xnc_span_suffix(copper_layers: &[ipc2581::Symbol], span: XncSpanKey) -> String {
    match span {
        XncSpanKey::ThroughBoard => "PTH".to_string(),
        XncSpanKey::FromTo { from, to } if from == to => "PTH".to_string(),
        XncSpanKey::FromTo { from, to } => {
            let last = copper_layers.len().max(1);
            if from == 1 || to == 1 || from == last || to == last {
                "Blind".to_string()
            } else {
                "Buried".to_string()
            }
        }
    }
}

impl XncSpanKey {
    fn layer_numbers(self, total_copper_layers: usize) -> (usize, usize) {
        match self {
            XncSpanKey::ThroughBoard => (1, total_copper_layers),
            XncSpanKey::FromTo { from, to } => (from, to),
        }
    }
}

fn copper_layer_index(copper_layers: &[ipc2581::Symbol], layer: ipc2581::Symbol) -> Option<usize> {
    copper_layers
        .iter()
        .position(|candidate| *candidate == layer)
        .map(|index| index + 1)
}

fn xnc_filename(key: &XncGroupKey) -> String {
    let base = match key.plating {
        NcPlating::Plated => "PTH",
        NcPlating::NonPlated => "NPTH",
    };
    if matches!(key.span, XncSpanKey::ThroughBoard) {
        return format!("{base}.drl");
    }
    let (from, to) = key.span.layer_numbers(1);
    format!("{base}_L{from}_L{to}.drl")
}
