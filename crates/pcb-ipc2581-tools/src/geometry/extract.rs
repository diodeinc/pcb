use std::collections::HashMap;

use anyhow::{Context, Result};
use ipc2581::types::{
    FillProperty, LayerFunction, LineEnd, PadUse, PlatingStatus, Polarity, PolyStep, SlotShape,
    StandardPrimitive, ecad::Layer,
};
use ipc2581::{Ipc2581, Symbol};

use super::ir::*;

struct ExtractContext<'a> {
    ipc: &'a Ipc2581,
    padstacks: HashMap<Symbol, &'a ipc2581::types::PadStackDef>,
    line_descs: HashMap<Symbol, ipc2581::types::LineDesc>,
    standard_primitives: HashMap<Symbol, &'a StandardPrimitive>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrimitivePaint {
    Fill,
    Hollow,
    Void,
}

pub fn extract_layer(ipc: &Ipc2581, layer_name: &str) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step = ecad
        .cad_data
        .steps
        .first()
        .context("IPC-2581 ECAD section has no Step")?;
    let layer = ecad
        .cad_data
        .layers
        .iter()
        .find(|layer| ipc.resolve(layer.name) == layer_name)
        .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))?;

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
    };

    let mut doc = GeometryDocument::new(ipc.resolve(step.name).to_string());
    let feature_start = doc.features.len() as u32;
    let mut layer_bbox = BBox::empty();
    let layer_polarity = map_polarity(layer.polarity.unwrap_or(Polarity::Positive));

    for layer_feature in step
        .layer_features
        .iter()
        .filter(|feature| feature.layer_ref == layer.name)
    {
        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            let polarity = set.polarity.map(map_polarity).unwrap_or(layer_polarity);

            for (feature_index, pad) in set.pads.iter().enumerate() {
                if let Some(feature) = extract_pad(
                    &context,
                    layer.name,
                    set.net,
                    polarity,
                    SourceRef {
                        set_index: set_index as u32,
                        feature_index: feature_index as u32,
                    },
                    pad,
                    &mut doc,
                )? {
                    layer_bbox = layer_bbox.union(feature.bbox);
                    doc.features.push(feature);
                }
            }

            for (feature_index, trace) in set.traces.iter().enumerate() {
                if let Some(feature) = extract_trace(
                    &context,
                    set.net,
                    polarity,
                    SourceRef {
                        set_index: set_index as u32,
                        feature_index: feature_index as u32,
                    },
                    trace,
                    &mut doc,
                ) {
                    layer_bbox = layer_bbox.union(feature.bbox);
                    doc.features.push(feature);
                }
            }

            for (feature_index, polygon) in set.polygons.iter().enumerate() {
                let feature = extract_polygon(
                    set.net,
                    polarity,
                    SourceRef {
                        set_index: set_index as u32,
                        feature_index: feature_index as u32,
                    },
                    polygon,
                    &mut doc,
                );
                layer_bbox = layer_bbox.union(feature.bbox);
                doc.features.push(feature);
            }

            for (feature_index, line) in set.lines.iter().enumerate() {
                let feature = extract_line(
                    set.net,
                    polarity,
                    SourceRef {
                        set_index: set_index as u32,
                        feature_index: feature_index as u32,
                    },
                    line,
                    &mut doc,
                );
                layer_bbox = layer_bbox.union(feature.bbox);
                doc.features.push(feature);
            }
        }
    }

    for layer_feature in &step.layer_features {
        let Some(source_layer) = ecad
            .cad_data
            .layers
            .iter()
            .find(|candidate| candidate.name == layer_feature.layer_ref)
        else {
            continue;
        };
        let is_drill_layer = source_layer.layer_function == LayerFunction::Drill;
        let is_routing_layer = source_layer.layer_function == LayerFunction::Rout;

        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            if is_drill_layer
                && layer_span_applies_to_layer(source_layer, layer, &ecad.cad_data.layers)
            {
                for (feature_index, hole) in set.holes.iter().enumerate() {
                    let feature = extract_hole(
                        SourceRef {
                            set_index: set_index as u32,
                            feature_index: feature_index as u32,
                        },
                        hole,
                        &mut doc,
                    );
                    layer_bbox = layer_bbox.union(feature.bbox);
                    doc.features.push(feature);
                }
            }

            if is_drill_layer || is_routing_layer {
                for (feature_index, slot) in set.slots.iter().enumerate() {
                    if slot_applies_to_layer(source_layer, layer, &ecad.cad_data.layers, slot) {
                        let feature = extract_slot(
                            &context,
                            SourceRef {
                                set_index: set_index as u32,
                                feature_index: feature_index as u32,
                            },
                            slot,
                            &mut doc,
                        )?;
                        layer_bbox = layer_bbox.union(feature.bbox);
                        doc.features.push(feature);
                    }
                }
            }
        }
    }

    let feature_count = doc.features.len() as u32 - feature_start;
    doc.layers.push(GeometryLayer {
        name: layer_name.to_string(),
        source_layer_ref: layer.name,
        feature_start,
        feature_count,
        bbox: layer_bbox,
    });

    Ok(doc)
}

fn slot_applies_to_layer(
    source_layer: &Layer,
    target_layer: &Layer,
    layers: &[Layer],
    slot: &ipc2581::types::Slot,
) -> bool {
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
    let Some(span) = source_layer.span else {
        return true;
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

    let bucket = match padstack.hole_def.as_ref().map(|hole| hole.plating_status) {
        Some(PlatingStatus::Via) => FeatureBucket::Via,
        Some(PlatingStatus::Plated) => FeatureBucket::Pth,
        Some(PlatingStatus::NonPlated) => return Ok(None),
        None => FeatureBucket::Smd,
    };

    let xform = pad.xform.unwrap_or_default();
    let offset = Affine2::transform_vector(
        xform.rotation,
        xform.mirror,
        xform.scale,
        Point::new(xform.x_offset, xform.y_offset),
    );
    let center = Point::new(x + offset.x, y + offset.y);
    let transform = Affine2::placement(center, xform.rotation, xform.mirror, xform.scale);

    let primitive_ref = pad
        .standard_primitive_ref
        .or_else(|| find_pad_primitive_ref(padstack, layer_ref));
    let Some(primitive_ref) = primitive_ref else {
        doc.warn(format!(
            "Skipping padstack '{}' because it has no regular primitive for layer '{}'",
            context.ipc.resolve(padstack.name),
            context.ipc.resolve(layer_ref)
        ));
        return Ok(None);
    };
    let Some(primitive) = context.standard_primitives.get(&primitive_ref).copied() else {
        doc.warn(format!(
            "Skipping padstack '{}' because primitive '{}' is missing",
            context.ipc.resolve(padstack.name),
            context.ipc.resolve(primitive_ref)
        ));
        return Ok(None);
    };

    let path_start = doc.paths.len() as u32;
    let paint = lower_standard_primitive(context, doc, primitive, transform, bucket)?;
    let path_count = doc.paths.len() as u32 - path_start;
    if path_count == 0 {
        return Ok(None);
    }
    let bbox = paths_bbox(doc, path_start, path_count);

    let mut feature = GeometryFeature::new(
        FeatureKind::Padstack,
        bucket,
        if paint == PrimitivePaint::Void {
            GeometryPolarity::Negative
        } else {
            polarity
        },
    );
    feature.net = net;
    feature.source = source;
    feature.transform = transform;
    feature.bbox = bbox;
    feature.path_start = path_start;
    feature.path_count = path_count;
    feature.center = center;
    feature.rotation_degrees = xform.rotation;
    feature.scale = xform.scale;
    feature.padstack_ref = Some(padstack_ref);
    feature.primitive_ref = Some(primitive_ref);
    feature.flags.expanded_padstack = true;
    feature.flags.lowered_to_paths = true;

    Ok(Some(feature))
}

fn find_pad_primitive_ref(
    padstack: &ipc2581::types::PadStackDef,
    layer_ref: Symbol,
) -> Option<Symbol> {
    padstack
        .pad_defs
        .iter()
        .find(|pad_def| pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Regular)
        .or_else(|| {
            padstack.pad_defs.iter().find(|pad_def| {
                pad_def.layer_ref == layer_ref && pad_def.pad_use == PadUse::Thermal
            })
        })
        .and_then(|pad_def| pad_def.standard_primitive_ref)
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

    let points: Vec<Point> = trace
        .points
        .iter()
        .map(|point| Point::new(point.x, point.y))
        .collect();
    Some(push_stroked_polyline(
        doc,
        net,
        polarity,
        source,
        points,
        line_desc.line_width,
        map_line_cap(line_desc.line_end),
    ))
}

fn extract_line(
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    line: &ipc2581::types::ecad::Line,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    push_stroked_polyline(
        doc,
        net,
        polarity,
        source,
        vec![
            Point::new(line.start_x, line.start_y),
            Point::new(line.end_x, line.end_y),
        ],
        line.line_width,
        line.line_end.map(map_line_cap).unwrap_or(LineCap::Round),
    )
}

fn extract_polygon(
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    polygon: &ipc2581::types::Polygon,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let path_start = doc.paths.len() as u32;
    push_polygon_path(doc, polygon, Affine2::identity(), FillRule::NonZero);
    let path_count = doc.paths.len() as u32 - path_start;

    let mut feature = GeometryFeature::new(FeatureKind::Polygon, FeatureBucket::Fill, polarity);
    feature.net = net;
    feature.source = source;
    feature.bbox = paths_bbox(doc, path_start, path_count);
    feature.path_start = path_start;
    feature.path_count = path_count;
    feature.flags.lowered_to_paths = true;
    feature
}

fn push_stroked_polyline(
    doc: &mut GeometryDocument,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    points: Vec<Point>,
    width: f64,
    line_cap: LineCap,
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
    bbox = bbox.expand(width / 2.0);

    let path_start = doc.paths.len() as u32;
    doc.push_path(GeometryPath::stroked(width, line_cap, bbox), cmds);

    let mut feature = GeometryFeature::new(FeatureKind::Trace, FeatureBucket::Trace, polarity);
    feature.net = net;
    feature.source = source;
    feature.bbox = bbox;
    feature.path_start = path_start;
    feature.path_count = 1;
    feature.stroke_width = width;
    feature.line_cap = line_cap;
    feature.flags.lowered_to_paths = true;
    feature
}

fn extract_hole(
    source: SourceRef,
    hole: &ipc2581::types::Hole,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let path_start = doc.paths.len() as u32;
    let center = Point::new(hole.x, hole.y);
    push_ellipse_path(
        doc,
        Affine2::placement(center, 0.0, false, 1.0),
        hole.diameter,
        hole.diameter,
    );
    let path_count = doc.paths.len() as u32 - path_start;

    let mut feature = GeometryFeature::new(
        FeatureKind::Hole,
        FeatureBucket::Cutout,
        GeometryPolarity::Positive,
    );
    feature.source = source;
    feature.bbox = paths_bbox(doc, path_start, path_count);
    feature.path_start = path_start;
    feature.path_count = path_count;
    feature.center = center;
    feature.outer_diameter = hole.diameter;
    feature.flags.lowered_to_paths = true;
    feature
}

fn extract_slot(
    context: &ExtractContext<'_>,
    source: SourceRef,
    slot: &ipc2581::types::Slot,
    doc: &mut GeometryDocument,
) -> Result<GeometryFeature> {
    let transform = Affine2::placement(Point::new(slot.x, slot.y), 0.0, false, 1.0);
    let path_start = doc.paths.len() as u32;

    match &slot.shape {
        SlotShape::Outline(polygon) => {
            push_polygon_path(doc, polygon, transform, FillRule::NonZero);
        }
        SlotShape::Primitive(primitive) => {
            let _ = lower_standard_primitive(
                context,
                doc,
                primitive,
                transform,
                FeatureBucket::Cutout,
            )?;
        }
    }

    let path_count = doc.paths.len() as u32 - path_start;
    let mut feature = GeometryFeature::new(
        FeatureKind::Slot,
        FeatureBucket::Cutout,
        GeometryPolarity::Positive,
    );
    feature.source = source;
    feature.transform = transform;
    feature.bbox = paths_bbox(doc, path_start, path_count);
    feature.path_start = path_start;
    feature.path_count = path_count;
    feature.center = Point::new(slot.x, slot.y);
    feature.flags.lowered_to_paths = true;
    Ok(feature)
}

fn lower_standard_primitive(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    primitive: &StandardPrimitive,
    transform: Affine2,
    bucket: FeatureBucket,
) -> Result<PrimitivePaint> {
    let path_start = doc.paths.len() as u32;
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
            let mut contours = Vec::with_capacity(1 + contour.cutouts.len());
            contours.push(polygon_contour(&contour.polygon, transform));
            for cutout in &contour.cutouts {
                contours.push(polygon_contour(cutout, transform));
            }
            doc.push_compound_path(
                GeometryPath::filled(FillRule::EvenOdd, BBox::empty()),
                contours,
            );
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

    if matches!(bucket, FeatureBucket::Thermal) {
        // Thermal pads are already represented by their primitive. This branch is
        // just here to make the bucket dependency explicit for future styling.
    }

    let paint = primitive_paint(primitive);
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
            );
        }
        PrimitivePaint::Void => {}
    }

    Ok(paint)
}

fn push_polygon_path(
    doc: &mut GeometryDocument,
    polygon: &ipc2581::types::Polygon,
    transform: Affine2,
    fill_rule: FillRule,
) {
    let (bbox, cmds) = polygon_contour(polygon, transform);
    doc.push_path(GeometryPath::filled(fill_rule, bbox), cmds);
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

fn make_paths_stroked(doc: &mut GeometryDocument, path_start: u32, width: f64, line_cap: LineCap) {
    for path in &mut doc.paths[path_start as usize..] {
        path.flags.filled = false;
        path.flags.stroked = true;
        path.fill_rule = FillRule::NonZero;
        path.stroke_width = width;
        path.line_cap = line_cap;
    }
}

fn make_paths_unpainted(doc: &mut GeometryDocument, path_start: u32) {
    for path in &mut doc.paths[path_start as usize..] {
        path.flags.filled = false;
        path.flags.stroked = false;
    }
}

fn polygon_contour(polygon: &ipc2581::types::Polygon, transform: Affine2) -> (BBox, Vec<PathCmd>) {
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
                bbox.include_circular_arc(start, end, center, clockwise);
                cmds.push(PathCmd::arc_to(end, center, clockwise));
                current = Point::new(curve.point.x, curve.point.y);
            }
        }
    }
    cmds.push(PathCmd::close());
    (bbox, cmds)
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
    doc.push_path(GeometryPath::filled(fill_rule, bbox), cmds);
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
        cmds.push(PathCmd::cubic_to(
            Point::new(hw - r + k * r, -hh),
            Point::new(hw, -hh + r - k * r),
            Point::new(hw, -hh + r),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        hw,
        hh - if upper_right { r } else { 0.0 },
    )));
    if upper_right {
        cmds.push(PathCmd::cubic_to(
            Point::new(hw, hh - r + k * r),
            Point::new(hw - r + k * r, hh),
            Point::new(hw - r, hh),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw + if upper_left { r } else { 0.0 },
        hh,
    )));
    if upper_left {
        cmds.push(PathCmd::cubic_to(
            Point::new(-hw + r - k * r, hh),
            Point::new(-hw, hh - r + k * r),
            Point::new(-hw, hh - r),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw,
        -hh + if lower_left { r } else { 0.0 },
    )));
    if lower_left {
        cmds.push(PathCmd::cubic_to(
            Point::new(-hw, -hh + r - k * r),
            Point::new(-hw + r - k * r, -hh),
            Point::new(-hw + r, -hh),
        ));
    }
    cmds.push(PathCmd::close());

    let mut bbox = BBox::empty();
    let cmds = cmds
        .into_iter()
        .map(|cmd| transform_path_cmd(cmd, transform, &mut bbox))
        .collect::<Vec<_>>();
    doc.push_path(GeometryPath::filled(FillRule::NonZero, bbox), cmds);
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
    let (bbox, cmds) = ellipse_contour(transform, width, height);
    doc.push_path(GeometryPath::filled(FillRule::NonZero, bbox), cmds);
}

fn ellipse_contour(transform: Affine2, width: f64, height: f64) -> (BBox, Vec<PathCmd>) {
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
    (bbox, cmds)
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

    let mut bbox = BBox::empty();
    let cmds = local_cmds
        .into_iter()
        .map(|cmd| transform_path_cmd(cmd, transform, &mut bbox))
        .collect::<Vec<_>>();
    doc.push_path(GeometryPath::filled(FillRule::NonZero, bbox), cmds);
}

fn transform_path_cmd(cmd: PathCmd, transform: Affine2, bbox: &mut BBox) -> PathCmd {
    let mut transformed = cmd;
    transformed.p0 = transform.transform_point(cmd.p0);
    transformed.p1 = transform.transform_point(cmd.p1);
    if cmd.op != PathOp::ArcTo {
        transformed.p2 = transform.transform_point(cmd.p2);
    } else if transform.determinant() < 0.0 {
        transformed.clockwise = !cmd.clockwise;
    }
    transformed.p3 = transform.transform_point(cmd.p3);

    match cmd.op {
        PathOp::MoveTo | PathOp::LineTo => bbox.include_point(transformed.p0),
        PathOp::ArcTo => {
            bbox.include_point(transformed.p0);
            bbox.include_point(transformed.p1);
        }
        PathOp::CubicTo => {
            bbox.include_point(transformed.p0);
            bbox.include_point(transformed.p1);
            bbox.include_point(transformed.p2);
        }
        PathOp::Close => {}
    }

    transformed
}

fn push_donut_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    outer_diameter: f64,
    inner_diameter: f64,
) {
    doc.push_compound_path(
        GeometryPath::filled(FillRule::EvenOdd, BBox::empty()),
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
        ipc2581::types::ButterflyShape::Round => doc.push_compound_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            [
                circular_sector_contour(transform, radius, 90.0, 180.0),
                circular_sector_contour(transform, radius, 270.0, 360.0),
            ],
        ),
        ipc2581::types::ButterflyShape::Square => doc.push_compound_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
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
            transform.concat(Affine2::placement(Point::default(), angle, false, 1.0)),
            length,
            width,
        );
        push_rect_path(
            doc,
            transform.concat(Affine2::placement(
                Point::default(),
                angle + 90.0,
                false,
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
        let spoke_transform =
            transform.concat(Affine2::placement(center, angle.to_degrees(), false, 1.0));
        push_rect_path(doc, spoke_transform, length, spoke_width);
    }
}

fn circular_sector_contour(
    transform: Affine2,
    radius: f64,
    start_degrees: f64,
    end_degrees: f64,
) -> (BBox, Vec<PathCmd>) {
    let start_angle = start_degrees.to_radians();
    let end_angle = end_degrees.to_radians();
    let start = Point::new(radius * start_angle.cos(), radius * start_angle.sin());
    let end = Point::new(radius * end_angle.cos(), radius * end_angle.sin());
    let mut bbox = BBox::empty();
    let cmds = [
        PathCmd::move_to(Point::default()),
        PathCmd::line_to(start),
        PathCmd::arc_to(end, Point::default(), false),
        PathCmd::close(),
    ]
    .into_iter()
    .map(|cmd| transform_path_cmd(cmd, transform, &mut bbox))
    .collect();
    (bbox, cmds)
}

fn rect_contour(transform: Affine2, x0: f64, y0: f64, x1: f64, y1: f64) -> (BBox, Vec<PathCmd>) {
    let mut bbox = BBox::empty();
    let cmds = [
        PathCmd::move_to(Point::new(x0, y0)),
        PathCmd::line_to(Point::new(x1, y0)),
        PathCmd::line_to(Point::new(x1, y1)),
        PathCmd::line_to(Point::new(x0, y1)),
        PathCmd::close(),
    ]
    .into_iter()
    .map(|cmd| transform_path_cmd(cmd, transform, &mut bbox))
    .collect();
    (bbox, cmds)
}

fn paths_bbox(doc: &GeometryDocument, path_start: u32, path_count: u32) -> BBox {
    doc.paths[path_start as usize..(path_start + path_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
}

fn map_polarity(polarity: Polarity) -> GeometryPolarity {
    match polarity {
        Polarity::Positive => GeometryPolarity::Positive,
        Polarity::Negative => GeometryPolarity::Negative,
    }
}

fn map_line_cap(line_end: LineEnd) -> LineCap {
    match line_end {
        LineEnd::Round => LineCap::Round,
        LineEnd::Square => LineCap::Square,
        LineEnd::Flat => LineCap::Butt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_moire_as_rings_and_crosshair() {
        let mut doc = GeometryDocument::new("test".to_string());

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

        assert_eq!(doc.paths.len(), 5);
        assert_eq!(doc.paths[0].fill_rule, FillRule::EvenOdd);
        assert_eq!(doc.paths[0].contour_count, 2);
        assert_eq!(doc.paths[1].contour_count, 2);
        assert_eq!(doc.paths[2].contour_count, 2);
        assert_eq!(doc.paths[3].fill_rule, FillRule::NonZero);
        assert_eq!(doc.paths[4].fill_rule, FillRule::NonZero);
        assert_eq!(doc.paths[0].bbox.min, Point::new(-4.25, -4.25));
        assert_eq!(doc.paths[0].bbox.max, Point::new(4.25, 4.25));
        assert_eq!(doc.paths[1].bbox.min, Point::new(-3.25, -3.25));
        assert_eq!(doc.paths[1].bbox.max, Point::new(3.25, 3.25));
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
    fn lowers_butterfly_with_removed_quadrants() {
        let mut doc = GeometryDocument::new("test".to_string());

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

        assert_eq!(doc.paths.len(), 2);
        assert_eq!(doc.paths[0].contour_count, 2);
        assert_eq!(doc.paths[1].contour_count, 2);
        assert!(doc.path_cmds.iter().any(|cmd| cmd.op == PathOp::ArcTo));
    }

    #[test]
    fn lowers_thermal_as_spokes_without_redundant_ring() {
        let mut doc = GeometryDocument::new("test".to_string());

        push_thermal_path(&mut doc, Affine2::identity(), 10.0, 6.0, 2.0, 4, 0.0);

        assert_eq!(doc.paths.len(), 4);
        assert!(
            doc.paths
                .iter()
                .all(|path| path.fill_rule == FillRule::NonZero && path.contour_count == 1)
        );
        assert_eq!(doc.paths[0].bbox.min, Point::new(3.0, -1.0));
        assert_eq!(doc.paths[0].bbox.max, Point::new(5.0, 1.0));
    }

    #[test]
    fn lowers_spokeless_thermal_as_donut() {
        let mut doc = GeometryDocument::new("test".to_string());

        push_thermal_path(&mut doc, Affine2::identity(), 10.0, 6.0, 2.0, 0, 0.0);

        assert_eq!(doc.paths.len(), 1);
        assert_eq!(doc.paths[0].fill_rule, FillRule::EvenOdd);
        assert_eq!(doc.paths[0].contour_count, 2);
    }

    #[test]
    fn chamfered_rect_respects_corner_flags() {
        let mut doc = GeometryDocument::new("test".to_string());

        push_chamfered_rect_path(
            &mut doc,
            Affine2::identity(),
            10.0,
            6.0,
            1.0,
            [true, false, false, false],
        );

        let path = &doc.paths[0];
        let contour = &doc.contours[path.contour_start as usize];
        let cmds = &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize];

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
        assert!(layer_span_applies_to_layer(&route, &l1, &layers));
        assert!(layer_span_applies_to_layer(&route, &l2, &layers));
        assert!(!layer_span_applies_to_layer(&route, &l3, &layers));
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
            x: 0.0,
            y: 0.0,
        }
    }
}
