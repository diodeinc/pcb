use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::View;
use pcb_ir::dialects::nc;

use crate::geometry;
use crate::manufacturing::{ManufacturingFile, ManufacturingFileKind};
use crate::xnc::{XncAttribute, XncBuilder, XncUnit, write_xnc};

pub fn build_xnc_drill_files(ipc: &Ipc2581, view: View) -> Result<Vec<ManufacturingFile>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = copper_layer_refs(&ecad.cad_data.layers);
    let mut nc = nc::Document::new();

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
        pcb_ir::dialects::ipc::lower_to_nc(&doc, &mut nc).map_err(anyhow::Error::msg)?;
    }

    xnc_files_from_nc(ipc, &nc, &copper_layers)
}

fn copper_layer_refs(layers: &[ipc2581::types::ecad::Layer]) -> Vec<ipc2581::Symbol> {
    layers
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
        .map(|layer| layer.name)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct XncGroupKey {
    plating: nc::Plating,
    span: XncSpanKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum XncSpanKey {
    ThroughBoard,
    FromTo { from: usize, to: usize },
}

fn xnc_files_from_nc(
    ipc: &Ipc2581,
    nc: &nc::Document<ipc2581::Symbol>,
    copper_layers: &[ipc2581::Symbol],
) -> Result<Vec<ManufacturingFile>> {
    let mut groups = std::collections::BTreeMap::<XncGroupKey, XncBuilder>::new();
    for object in &nc.objects {
        let key = XncGroupKey {
            plating: object.plating,
            span: xnc_span_key(copper_layers, &object.span),
        };
        let file_function = xnc_file_function(&key, copper_layers);
        let builder = groups
            .entry(key)
            .or_insert_with(|| XncBuilder::new(XncUnit::Metric, vec![file_function]));
        let tool_attributes = xnc_tool_attributes(object);
        let object_attributes = xnc_object_attributes(ipc, object);
        match &object.geometry {
            nc::Geometry::Drill { at, diameter } => {
                builder.add_drill(*diameter, *at, tool_attributes, object_attributes)?;
            }
            nc::Geometry::Slot {
                start,
                end,
                diameter,
            } => {
                builder.add_slot(*diameter, *start, *end, tool_attributes, object_attributes)?;
            }
            nc::Geometry::Route {
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
                            nc::RouteSegment::Line { to } => {
                                crate::xnc::XncRouteSegment::Line { to }
                            }
                            nc::RouteSegment::ClockwiseArc { to, radius } => {
                                crate::xnc::XncRouteSegment::ClockwiseArc { to, radius }
                            }
                            nc::RouteSegment::CounterClockwiseArc { to, radius } => {
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
        nc::Plating::Plated => ("Plated", xnc_span_suffix(copper_layers, key.span)),
        nc::Plating::NonPlated => ("NonPlated", "NPTH".to_string()),
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

fn xnc_tool_attributes(object: &nc::Object<ipc2581::Symbol>) -> Vec<XncAttribute> {
    let drill_function = match object.function {
        nc::Function::Via => "ViaDrill",
        nc::Function::Component => "ComponentDrill",
    };
    let fields = match object.plating {
        nc::Plating::Plated => vec!["Plated", "PTH", drill_function],
        nc::Plating::NonPlated => vec!["NonPlated", "NPTH", drill_function],
    };
    vec![XncAttribute::tool("AperFunction", fields)]
}

fn xnc_object_attributes(ipc: &Ipc2581, object: &nc::Object<ipc2581::Symbol>) -> Vec<XncAttribute> {
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

fn xnc_span_key(
    copper_layers: &[ipc2581::Symbol],
    span: &nc::DrillSpan<ipc2581::Symbol>,
) -> XncSpanKey {
    match span {
        nc::DrillSpan::FromTo { from, to } => {
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
        nc::DrillSpan::ThroughBoard => XncSpanKey::ThroughBoard,
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
        nc::Plating::Plated => "PTH",
        nc::Plating::NonPlated => "NPTH",
    };
    if matches!(key.span, XncSpanKey::ThroughBoard) {
        return format!("{base}.drl");
    }
    let (from, to) = key.span.layer_numbers(1);
    format!("{base}_L{from}_L{to}.drl")
}
