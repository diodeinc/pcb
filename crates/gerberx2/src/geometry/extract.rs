use std::collections::HashMap;

use crate::GerberX2;
use crate::types as gerber;
use pcb_ir::common::*;
use pcb_ir::dialects::artwork::{
    self, ArtworkDocument, ArtworkGeometry, ArtworkLayer, ArtworkObject, ArtworkPath,
};
use pcb_ir::dialects::gerber::{self as gerber_ir, FeatureBucket, FeatureKind, Polarity};
use pcb_ir::dialects::path::{self as common_path, PathCmd, PathOp};

const SWEEP_SAMPLE_MM: f64 = 0.025;

pub type GerberArtworkDocument = ArtworkDocument<Vec<String>, GerberObjectMeta>;

#[derive(Debug, Clone, PartialEq)]
pub struct GerberObjectMeta {
    pub kind: FeatureKind,
    pub bucket: FeatureBucket,
    pub polarity: Polarity,
    pub aperture: Option<i32>,
    pub object_index: u32,
    pub aperture_attributes: Vec<gerber::Attribute>,
    pub object_attributes: Vec<gerber::Attribute>,
    pub mirroring: gerber_ir::Mirroring,
    pub rotation_degrees: f64,
    pub scaling: f64,
}

pub fn extract_document(gerber: &GerberX2) -> GerberArtworkDocument {
    let file_function = file_function(gerber);
    let mut doc = ArtworkDocument::<Vec<String>, GerberObjectMeta>::new(Unit::Millimeter);
    let layer = doc.push_layer(ArtworkLayer {
        name: file_function.join(", "),
        role: gerber_ir::layer_role(&file_function),
        side: gerber_ir::layer_side(&file_function),
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: file_function,
    });
    let apertures = gerber
        .aperture_definitions()
        .iter()
        .map(|aperture| (aperture.code, aperture))
        .collect::<HashMap<_, _>>();

    for (object_index, object) in gerber.objects().iter().enumerate() {
        match &object.kind {
            gerber::ObjectKind::Flash { at, aperture } => {
                let Some(definition) = apertures.get(aperture) else {
                    warn(
                        &mut doc,
                        format!("flash references undefined aperture D{aperture}"),
                    );
                    continue;
                };
                let Some(geometry) = &definition.geometry else {
                    warn(
                        &mut doc,
                        format!("flash aperture D{aperture} has no lowered geometry"),
                    );
                    continue;
                };
                let transform = Affine2::placement(
                    point(*at),
                    object.rotation_degrees,
                    object.mirroring,
                    object.scaling,
                );
                let mut meta = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Flash,
                    classify_bucket(object, FeatureKind::Flash),
                );
                meta.aperture = Some(*aperture);
                push_feature_paths(&mut doc, layer, meta, aperture_paths(geometry, transform));
            }
            gerber::ObjectKind::Draw {
                start,
                end,
                aperture,
            } => {
                let mut meta = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Draw,
                    classify_bucket(object, FeatureKind::Draw),
                );
                meta.aperture = Some(*aperture);
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    push_feature_paths(
                        &mut doc,
                        layer,
                        meta,
                        vec![line_path(
                            point(*start),
                            point(*end),
                            width * object.scaling.abs(),
                        )],
                    );
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    push_feature_paths(
                        &mut doc,
                        layer,
                        meta,
                        sampled_line_sweep(point(*start), point(*end), object, geometry),
                    );
                } else {
                    warn(
                        &mut doc,
                        format!("D{aperture} draw aperture has no lowered geometry"),
                    );
                }
            }
            gerber::ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                aperture,
            } => {
                let mut meta = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Arc,
                    classify_bucket(object, FeatureKind::Arc),
                );
                meta.aperture = Some(*aperture);
                let start = point(*start);
                let center = Point::new(start.x + center_offset.x, start.y + center_offset.y);
                if let Some(width) = circular_aperture_diameter(&apertures, *aperture) {
                    push_feature_paths(
                        &mut doc,
                        layer,
                        meta,
                        vec![arc_path(
                            start,
                            point(*end),
                            center,
                            *clockwise,
                            width * object.scaling.abs(),
                        )],
                    );
                } else if let Some(geometry) = aperture_geometry(&apertures, *aperture) {
                    push_feature_paths(
                        &mut doc,
                        layer,
                        meta,
                        sampled_arc_sweep(start, point(*end), center, *clockwise, object, geometry),
                    );
                } else {
                    warn(
                        &mut doc,
                        format!("D{aperture} arc aperture has no lowered geometry"),
                    );
                }
            }
            gerber::ObjectKind::Region { contours } => {
                let meta = feature_from_object(
                    object,
                    object_index,
                    FeatureKind::Region,
                    classify_bucket(object, FeatureKind::Region),
                );
                push_feature_paths(&mut doc, layer, meta, vec![region_path(contours)]);
            }
        }
    }

    artwork::normalize_bounds(&mut doc);
    doc
}

fn aperture_geometry<'a>(
    apertures: &'a HashMap<i32, &gerber::ApertureDefinition>,
    code: i32,
) -> Option<&'a gerber::ApertureGeometry> {
    apertures.get(&code)?.geometry.as_ref()
}

fn warn(doc: &mut GerberArtworkDocument, message: impl Into<String>) {
    doc.diagnostics.push(GeometryDiagnostic {
        severity: DiagnosticSeverity::Warning,
        message: message.into(),
    });
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
) -> GerberObjectMeta {
    GerberObjectMeta {
        kind,
        bucket,
        polarity: object.polarity,
        aperture: None,
        object_index: object_index as u32,
        aperture_attributes: object.aperture_attributes.clone(),
        object_attributes: object.object_attributes.clone(),
        mirroring: object.mirroring,
        rotation_degrees: object.rotation_degrees,
        scaling: object.scaling,
    }
}

fn classify_bucket(object: &gerber::GraphicalObject, kind: FeatureKind) -> FeatureBucket {
    if object.polarity == Polarity::Clear {
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

#[derive(Debug, Clone)]
struct ExtractedPath {
    paint: PaintPolarity,
    path: ArtworkPath,
    contours: Vec<common_path::PathPayload>,
}

fn push_feature_paths(
    doc: &mut GerberArtworkDocument,
    layer: u32,
    meta: GerberObjectMeta,
    paths: Vec<ExtractedPath>,
) {
    if paths.is_empty() {
        return;
    }

    if paths.len() == 1 && paths[0].paint == PaintPolarity::Dark {
        let extracted = paths.into_iter().next().unwrap();
        let is_stroked = extracted.path.flags.stroked;
        let path = doc.push_path(extracted.path, extracted.contours);
        doc.push_object(
            layer,
            ArtworkObject {
                paint: gerber_ir::paint_polarity(meta.polarity),
                order: Default::default(),
                geometry: if is_stroked {
                    ArtworkGeometry::Stroke { path }
                } else {
                    ArtworkGeometry::Region { path }
                },
                net: None,
                bbox: doc.paths[path as usize].bbox,
                meta,
            },
        );
        return;
    }

    let mut composer = common_path::PaintComposer::default();
    for extracted in paths {
        let contours = common_path::payloads_to_polygon_contours(&extracted.contours);
        if contours.is_empty() {
            continue;
        }
        let op = match extracted.paint {
            PaintPolarity::Dark => common_path::PaintOp::Dark,
            PaintPolarity::Clear => common_path::PaintOp::Clear,
        };
        composer.push(op, contours);
    }
    let contours = common_path::polygon_contours_to_payloads(composer.finish());
    if contours.is_empty() {
        return;
    }

    let path = doc.push_path(ArtworkPath::filled(FillRule::NonZero), contours);
    doc.push_object(
        layer,
        ArtworkObject {
            paint: gerber_ir::paint_polarity(meta.polarity),
            order: Default::default(),
            geometry: ArtworkGeometry::Region { path },
            net: None,
            bbox: doc.paths[path as usize].bbox,
            meta,
        },
    );
}

fn aperture_paths(geometry: &gerber::ApertureGeometry, transform: Affine2) -> Vec<ExtractedPath> {
    geometry
        .paths
        .iter()
        .map(|path| {
            let contours = path
                .contours
                .iter()
                .map(|contour| transform_contour(&contour.commands, transform))
                .collect();
            ExtractedPath {
                paint: gerber_ir::paint_polarity(path.polarity),
                path: ArtworkPath::filled(FillRule::NonZero),
                contours,
            }
        })
        .collect()
}

fn transform_contour(
    commands: &[gerber::PathCommand],
    transform: Affine2,
) -> common_path::PathPayload {
    let mut bbox = BBox::empty();
    let mut cmds = Vec::new();
    let flips_orientation = transform.determinant() < 0.0;
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
                clockwise ^ flips_orientation,
            ),
            gerber::PathCommand::Close => PathCmd::close(),
        };
        include_cmd_bbox(&mut bbox, cmds.last().copied(), cmd);
        cmds.push(cmd);
    }
    common_path::PathPayload { bbox, cmds }
}

fn line_path(start: Point, end: Point, width: f64) -> ExtractedPath {
    let bbox = BBox::from_point(start)
        .union(BBox::from_point(end))
        .expand(width / 2.0);
    ExtractedPath {
        paint: PaintPolarity::Dark,
        path: ArtworkPath::stroked(width, LineCap::Round, LineJoin::Round),
        contours: vec![common_path::PathPayload {
            bbox,
            cmds: vec![PathCmd::move_to(start), PathCmd::line_to(end)],
        }],
    }
}

fn arc_path(start: Point, end: Point, center: Point, clockwise: bool, width: f64) -> ExtractedPath {
    let mut bbox = BBox::empty();
    bbox.include_circular_arc(start, end, center, clockwise);
    bbox = bbox.expand(width / 2.0);
    ExtractedPath {
        paint: PaintPolarity::Dark,
        path: ArtworkPath::stroked(width, LineCap::Round, LineJoin::Round),
        contours: vec![common_path::PathPayload {
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
) -> Vec<ExtractedPath> {
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
) -> Vec<ExtractedPath> {
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

fn region_path(contours: &[gerber::Contour]) -> ExtractedPath {
    let contours = contours.iter().map(region_contour).collect();
    ExtractedPath {
        paint: PaintPolarity::Dark,
        path: ArtworkPath::filled(FillRule::NonZero),
        contours,
    }
}

fn region_contour(contour: &gerber::Contour) -> common_path::PathPayload {
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
    common_path::PathPayload { bbox, cmds }
}

fn include_cmd_bbox(bbox: &mut BBox, previous: Option<PathCmd>, cmd: PathCmd) {
    match cmd.op {
        PathOp::MoveTo | PathOp::LineTo => bbox.include_point(cmd.p0),
        PathOp::ArcTo => {
            if let Some(start) = previous.and_then(PathCmd::end_point) {
                bbox.include_circular_arc(start, cmd.p0, cmd.p1, cmd.clockwise);
            } else {
                bbox.include_point(cmd.p0);
                bbox.include_point(cmd.p1);
            }
        }
        PathOp::CubicTo => {
            bbox.include_point(cmd.p0);
            bbox.include_point(cmd.p1);
            bbox.include_point(cmd.p2);
        }
        PathOp::Close => {}
    }
}

fn point(p: gerber::Point) -> Point {
    Point::new(p.x, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_bbox_after_cubic_uses_cubic_endpoint() {
        let mut bbox = BBox::empty();
        include_cmd_bbox(
            &mut bbox,
            Some(PathCmd::cubic_to(
                Point::new(100.0, 100.0),
                Point::new(100.0, 100.0),
                Point::new(1.0, 0.0),
            )),
            PathCmd::arc_to(Point::new(0.0, 1.0), Point::new(0.0, 0.0), false),
        );

        assert!(bbox.max.x <= 1.0);
        assert!(bbox.max.y <= 1.0);
    }
}
