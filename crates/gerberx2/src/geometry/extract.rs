use std::collections::HashMap;

use super::ir::*;
use crate::GerberX2;
use crate::types as gerber;

const SWEEP_SAMPLE_MM: f64 = 0.025;

pub fn extract_document(gerber: &GerberX2) -> GeometryDocument {
    let mut doc = GeometryDocument::new(file_function(gerber));
    let apertures = gerber
        .aperture_definitions()
        .iter()
        .map(|aperture| (aperture.code, aperture))
        .collect::<HashMap<_, _>>();

    for (object_index, object) in gerber.objects().iter().enumerate() {
        match &object.kind {
            gerber::ObjectKind::Flash { at, aperture } => {
                let Some(definition) = apertures.get(aperture) else {
                    doc.warn(format!("flash references undefined aperture D{aperture}"));
                    continue;
                };
                let Some(geometry) = &definition.geometry else {
                    doc.warn(format!(
                        "flash aperture D{aperture} has no lowered geometry"
                    ));
                    continue;
                };
                let transform = Affine2::placement(
                    point(*at),
                    object.rotation_degrees,
                    object.mirroring,
                    object.scaling,
                );
                let mut feature = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Flash,
                    classify_bucket(object, FeatureKind::Flash),
                );
                feature.aperture = Some(*aperture);
                doc.push_feature(feature, aperture_paths(geometry, transform));
            }
            gerber::ObjectKind::Draw {
                start,
                end,
                aperture,
            } => {
                let mut feature = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Draw,
                    classify_bucket(object, FeatureKind::Draw),
                );
                feature.aperture = Some(*aperture);
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    doc.push_feature(feature, vec![line_path(point(*start), point(*end), width)]);
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    doc.push_feature(
                        feature,
                        sampled_line_sweep(point(*start), point(*end), object, geometry),
                    );
                } else {
                    doc.warn(format!("D{aperture} draw aperture has no lowered geometry"));
                }
            }
            gerber::ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                aperture,
            } => {
                let mut feature = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Arc,
                    classify_bucket(object, FeatureKind::Arc),
                );
                feature.aperture = Some(*aperture);
                let start = point(*start);
                let center = Point::new(start.x + center_offset.x, start.y + center_offset.y);
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    doc.push_feature(
                        feature,
                        vec![arc_path(start, point(*end), center, *clockwise, width)],
                    );
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    doc.push_feature(
                        feature,
                        sampled_arc_sweep(start, point(*end), center, *clockwise, object, geometry),
                    );
                } else {
                    doc.warn(format!("D{aperture} arc aperture has no lowered geometry"));
                }
            }
            gerber::ObjectKind::Region { contours } => {
                let feature = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Region,
                    classify_bucket(object, FeatureKind::Region),
                );
                doc.push_feature(feature, vec![region_path(contours)]);
            }
        }
    }

    super::process::normalize_bounds(&mut doc);
    doc
}

fn aperture_geometry<'a>(
    apertures: &'a HashMap<i32, &gerber::ApertureDefinition>,
    code: i32,
) -> Option<&'a gerber::ApertureGeometry> {
    apertures.get(&code)?.geometry.as_ref()
}

fn file_function(gerber: &GerberX2) -> Vec<String> {
    gerber
        .file_attributes()
        .iter()
        .find(|attr| gerber.resolve(attr.name) == ".FileFunction")
        .map(|attr| {
            attr.fields
                .iter()
                .map(|field| gerber.resolve(*field).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn feature_from_object(
    object: &gerber::GraphicalObject,
    object_index: usize,
    kind: FeatureKind,
    bucket: FeatureBucket,
) -> GeometryFeature {
    let mut feature = GeometryFeature::new(kind, bucket, object.polarity);
    feature.object_index = object_index as u32;
    feature.aperture_attributes = object.aperture_attributes.clone();
    feature.object_attributes = object.object_attributes.clone();
    feature.mirroring = object.mirroring;
    feature.rotation_degrees = object.rotation_degrees;
    feature.scaling = object.scaling;
    feature
}

fn classify_bucket(object: &gerber::GraphicalObject, kind: FeatureKind) -> FeatureBucket {
    if object.polarity == gerber::Polarity::Clear {
        return FeatureBucket::Cutout;
    }
    match kind {
        FeatureKind::Region => FeatureBucket::Fill,
        FeatureKind::Draw | FeatureKind::Arc => FeatureBucket::Trace,
        FeatureKind::Flash => FeatureBucket::Pad,
        FeatureKind::Composite => FeatureBucket::Unknown,
    }
}

fn circular_aperture_diameter(
    apertures: &HashMap<i32, &gerber::ApertureDefinition>,
    code: i32,
) -> Option<f64> {
    match apertures.get(&code)?.template {
        gerber::ApertureTemplate::Circle { diameter, .. } => Some(diameter),
        _ => None,
    }
}

fn aperture_paths(geometry: &gerber::ApertureGeometry, transform: Affine2) -> Vec<PathPayload> {
    geometry
        .paths
        .iter()
        .map(|path| {
            let contours = path
                .contours
                .iter()
                .map(|contour| transform_contour(&contour.commands, transform))
                .collect();
            PathPayload {
                path: GeometryPath::filled_with_polarity(FillRule::NonZero, path.polarity),
                contours,
            }
        })
        .collect()
}

fn transform_contour(commands: &[gerber::PathCommand], transform: Affine2) -> ContourPayload {
    let mut bbox = BBox::empty();
    let mut cmds = Vec::new();
    for command in commands {
        let cmd = match *command {
            gerber::PathCommand::MoveTo(p) => PathCmd::move_to(transform.transform_point(point(p))),
            gerber::PathCommand::LineTo(p) => PathCmd::line_to(transform.transform_point(point(p))),
            gerber::PathCommand::ArcTo {
                end,
                center,
                clockwise,
            } => PathCmd::arc_to(
                transform.transform_point(point(end)),
                transform.transform_point(point(center)),
                clockwise,
            ),
            gerber::PathCommand::Close => PathCmd::close(),
        };
        include_cmd_bbox(&mut bbox, cmds.last().copied(), cmd);
        cmds.push(cmd);
    }
    ContourPayload { bbox, cmds }
}

fn line_path(start: Point, end: Point, width: f64) -> PathPayload {
    let bbox = BBox::from_point(start)
        .union(BBox::from_point(end))
        .expand(width / 2.0);
    PathPayload {
        path: GeometryPath::stroked(width, LineCap::Round),
        contours: vec![ContourPayload {
            bbox,
            cmds: vec![PathCmd::move_to(start), PathCmd::line_to(end)],
        }],
    }
}

fn arc_path(start: Point, end: Point, center: Point, clockwise: bool, width: f64) -> PathPayload {
    let mut bbox = BBox::empty();
    bbox.include_circular_arc(start, end, center, clockwise);
    bbox = bbox.expand(width / 2.0);
    PathPayload {
        path: GeometryPath::stroked(width, LineCap::Round),
        contours: vec![ContourPayload {
            bbox,
            cmds: vec![
                PathCmd::move_to(start),
                PathCmd::arc_to(end, center, clockwise),
            ],
        }],
    }
}

fn sampled_line_sweep(
    start: Point,
    end: Point,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
) -> Vec<PathPayload> {
    let length = start.distance_to(end);
    let steps = sample_steps(length);
    (0..=steps)
        .flat_map(|index| {
            let t = index as f64 / steps.max(1) as f64;
            let at = Point::new(
                start.x + (end.x - start.x) * t,
                start.y + (end.y - start.y) * t,
            );
            aperture_paths(geometry, object_transform(object, at))
        })
        .collect()
}

fn sampled_arc_sweep(
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
    object: &gerber::GraphicalObject,
    geometry: &gerber::ApertureGeometry,
) -> Vec<PathPayload> {
    let radius = start.distance_to(center);
    let sweep = arc_sweep_radians(start, end, center, clockwise);
    let steps = sample_steps(radius * sweep);
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let start_angle = start.angle_from(center);
    (0..=steps)
        .flat_map(|index| {
            let t = index as f64 / steps.max(1) as f64;
            let angle = start_angle + signed_sweep * t;
            let at = Point::new(
                center.x + radius * angle.cos(),
                center.y + radius * angle.sin(),
            );
            aperture_paths(geometry, object_transform(object, at))
        })
        .collect()
}

fn object_transform(object: &gerber::GraphicalObject, at: Point) -> Affine2 {
    Affine2::placement(
        at,
        object.rotation_degrees,
        object.mirroring,
        object.scaling,
    )
}

fn sample_steps(length: f64) -> usize {
    (length / SWEEP_SAMPLE_MM).ceil().max(1.0) as usize
}

fn region_path(contours: &[gerber::Contour]) -> PathPayload {
    let contours = contours.iter().map(region_contour).collect();
    PathPayload {
        path: GeometryPath::filled(FillRule::NonZero),
        contours,
    }
}

fn region_contour(contour: &gerber::Contour) -> ContourPayload {
    let mut bbox = BBox::empty();
    let mut cmds = Vec::new();
    if let Some(first) = contour.segments.first() {
        let start = match *first {
            gerber::ContourSegment::Line { start, .. }
            | gerber::ContourSegment::Arc { start, .. } => point(start),
        };
        cmds.push(PathCmd::move_to(start));
        bbox.include_point(start);
    }
    for segment in &contour.segments {
        let cmd = match *segment {
            gerber::ContourSegment::Line { end, .. } => PathCmd::line_to(point(end)),
            gerber::ContourSegment::Arc {
                start,
                end,
                center_offset,
                clockwise,
            } => {
                let start = point(start);
                PathCmd::arc_to(
                    point(end),
                    Point::new(start.x + center_offset.x, start.y + center_offset.y),
                    clockwise,
                )
            }
        };
        include_cmd_bbox(&mut bbox, cmds.last().copied(), cmd);
        cmds.push(cmd);
    }
    cmds.push(PathCmd::close());
    ContourPayload { bbox, cmds }
}

fn include_cmd_bbox(bbox: &mut BBox, previous: Option<PathCmd>, cmd: PathCmd) {
    match cmd.op {
        PathOp::MoveTo | PathOp::LineTo => bbox.include_point(cmd.p0),
        PathOp::ArcTo => {
            if let Some(previous) = previous {
                bbox.include_circular_arc(previous.p0, cmd.p0, cmd.p1, cmd.clockwise);
            } else {
                bbox.include_point(cmd.p0);
                bbox.include_point(cmd.p1);
            }
        }
        PathOp::Close => {}
    }
}

fn point(p: gerber::Point) -> Point {
    Point::new(p.x, p.y)
}
