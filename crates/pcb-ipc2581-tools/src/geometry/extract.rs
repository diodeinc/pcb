use std::collections::HashMap;

use anyhow::{Context, Result};
use ipc2581::types::{
    FillProperty, LayerFunction, LineEnd, PadUse, PlatingStatus, Polarity, PolyStep, SlotShape,
    StandardPrimitive, Styled,
};
use ipc2581::{Ipc2581, Symbol};

use super::ir::*;

struct ExtractContext<'a> {
    ipc: &'a Ipc2581,
    padstacks: HashMap<Symbol, &'a ipc2581::types::PadStackDef>,
    line_descs: HashMap<Symbol, ipc2581::types::LineDesc>,
    standard_primitives: HashMap<Symbol, &'a StandardPrimitive>,
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
        let is_drill_layer = ecad.cad_data.layers.iter().any(|candidate| {
            candidate.name == layer_feature.layer_ref
                && candidate.layer_function == LayerFunction::Drill
        });

        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            if is_drill_layer {
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

            for (feature_index, slot) in set.slots.iter().enumerate() {
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
    lower_standard_primitive(context, doc, primitive, transform, bucket)?;
    let path_count = doc.paths.len() as u32 - path_start;
    if path_count == 0 {
        return Ok(None);
    }
    let bbox = paths_bbox(doc, path_start, path_count);

    let mut feature = GeometryFeature::new(FeatureKind::Padstack, bucket, polarity);
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
        FeatureKind::Trace,
        FeatureBucket::Trace,
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
        FeatureKind::Trace,
        FeatureBucket::Trace,
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
    kind: FeatureKind,
    bucket: FeatureBucket,
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

    let mut feature = GeometryFeature::new(kind, bucket, polarity);
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
            lower_standard_primitive(context, doc, primitive, transform, FeatureBucket::Cutout)?;
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
) -> Result<()> {
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
                .unwrap_or(thermal.shape.outer_diameter * 0.15);
            push_thermal_path(
                doc,
                transform,
                thermal.shape.outer_diameter,
                thermal.shape.inner_diameter,
                spoke_width,
                thermal.shape.spoke_count.max(1),
            );
        }
        StandardPrimitive::Contour(contour) => {
            push_polygon_path(doc, &contour.polygon, transform, FillRule::EvenOdd);
            for cutout in &contour.cutouts {
                push_polygon_path(doc, cutout, transform, FillRule::EvenOdd);
            }
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
            let hw = rect.shape.size.width / 2.0;
            let hh = rect.shape.size.height / 2.0;
            let c = rect.shape.chamfer.min(hw).min(hh);
            push_closed_points_path(
                doc,
                transform,
                vec![
                    Point::new(-hw + c, -hh),
                    Point::new(hw - c, -hh),
                    Point::new(hw, -hh + c),
                    Point::new(hw, hh - c),
                    Point::new(hw - c, hh),
                    Point::new(-hw + c, hh),
                    Point::new(-hw, hh - c),
                    Point::new(-hw, -hh + c),
                ],
                FillRule::NonZero,
            );
        }
        StandardPrimitive::Butterfly(butterfly) => {
            push_ellipse_path(doc, transform, butterfly.shape.size, butterfly.shape.size);
        }
        StandardPrimitive::Moire(moire) => {
            push_ellipse_path(doc, transform, moire.diameter, moire.diameter);
        }
    }

    if matches!(bucket, FeatureBucket::Thermal) {
        // Thermal pads are already represented by their primitive. This branch is
        // just here to make the bucket dependency explicit for future styling.
    }

    let _ = context;
    Ok(())
}

fn push_polygon_path(
    doc: &mut GeometryDocument,
    polygon: &ipc2581::types::Polygon,
    transform: Affine2,
    fill_rule: FillRule,
) {
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
    doc.push_path(GeometryPath::filled(fill_rule, bbox), cmds);
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
        -hw + if upper_left { r } else { 0.0 },
        -hh,
    )));

    cmds.push(PathCmd::line_to(Point::new(
        hw - if upper_right { r } else { 0.0 },
        -hh,
    )));
    if upper_right {
        cmds.push(PathCmd::cubic_to(
            Point::new(hw - r + k * r, -hh),
            Point::new(hw, -hh + r - k * r),
            Point::new(hw, -hh + r),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        hw,
        hh - if lower_right { r } else { 0.0 },
    )));
    if lower_right {
        cmds.push(PathCmd::cubic_to(
            Point::new(hw, hh - r + k * r),
            Point::new(hw - r + k * r, hh),
            Point::new(hw - r, hh),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw + if lower_left { r } else { 0.0 },
        hh,
    )));
    if lower_left {
        cmds.push(PathCmd::cubic_to(
            Point::new(-hw + r - k * r, hh),
            Point::new(-hw, hh - r + k * r),
            Point::new(-hw, hh - r),
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw,
        -hh + if upper_left { r } else { 0.0 },
    )));
    if upper_left {
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
    doc.push_path(GeometryPath::filled(FillRule::NonZero, bbox), cmds);
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
    let path_start = doc.paths.len();
    push_ellipse_path(doc, transform, outer_diameter, outer_diameter);
    push_ellipse_path(doc, transform, inner_diameter, inner_diameter);
    for path in &mut doc.paths[path_start..] {
        path.fill_rule = FillRule::EvenOdd;
    }
}

fn push_thermal_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    outer_diameter: f64,
    inner_diameter: f64,
    spoke_width: f64,
    spoke_count: u32,
) {
    push_donut_path(doc, transform, outer_diameter, inner_diameter);
    let outer_radius = outer_diameter / 2.0;
    let inner_radius = inner_diameter / 2.0;
    let length = (outer_radius - inner_radius).max(0.0);
    for index in 0..spoke_count {
        let angle = index as f64 * std::f64::consts::TAU / spoke_count as f64;
        let center_radius = inner_radius + length / 2.0;
        let center = Point::new(center_radius * angle.cos(), center_radius * angle.sin());
        let spoke_transform = Affine2::placement(
            transform.transform_point(center),
            angle.to_degrees(),
            false,
            1.0,
        );
        push_rect_path(doc, spoke_transform, length, spoke_width);
    }
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

fn _styled_is_filled<T>(styled: &Styled<T>) -> bool {
    !matches!(
        styled.fill_property,
        Some(FillProperty::Hollow | FillProperty::Void)
    )
}
