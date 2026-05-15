use super::ir::*;
use crate::types::Polarity;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

type PolygonContour = Vec<[f64; 2]>;

pub fn process_document(doc: &mut GeometryDocument) {
    normalize_bounds(doc);
    outline_stroked_paths(doc);
    resolve_polarity_and_cutouts(doc);
    normalize_bounds(doc);
}

pub fn normalize_bounds(doc: &mut GeometryDocument) {
    for contour_index in 0..doc.contours.len() {
        doc.contours[contour_index].bbox = contour_bbox(doc, contour_index);
    }
    for path_index in 0..doc.paths.len() {
        doc.paths[path_index].bbox = path_bbox(doc, path_index);
    }
    for feature_index in 0..doc.features.len() {
        doc.features[feature_index].bbox = feature_bbox(doc, feature_index);
    }
    doc.bbox = doc
        .features
        .iter()
        .filter(|feature| feature.path_count > 0)
        .fold(BBox::empty(), |bbox, feature| bbox.union(feature.bbox));
}

pub fn outline_stroked_paths(doc: &mut GeometryDocument) {
    let feature_count = doc.features.len();
    for feature_index in 0..feature_count {
        let feature = doc.features[feature_index].clone();
        let paths = doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
            .to_vec();
        if !paths.iter().any(|path| path.flags.stroked) {
            continue;
        }

        let path_start = doc.paths.len() as u32;
        for path in paths {
            if path.flags.stroked {
                if let Some(payload) = stroked_path_outline(doc, &path) {
                    doc.push_path(payload.path, payload.contours);
                }
            } else {
                let contours = path_contours(doc, &path);
                doc.push_path(path, contours);
            }
        }
        let feature = &mut doc.features[feature_index];
        feature.path_start = path_start;
        feature.path_count = doc.paths.len() as u32 - path_start;
    }
}

pub fn resolve_polarity_and_cutouts(doc: &mut GeometryDocument) {
    let mut image = Vec::new();
    for feature in &doc.features {
        let feature_image = feature_image_contours(doc, feature);
        if feature_image.is_empty() {
            continue;
        }
        if feature.polarity == Polarity::Clear || feature.bucket == FeatureBucket::Cutout {
            image = difference_contours(image, feature_image);
        } else {
            image = union_contours(image.into_iter().chain(feature_image).collect());
        }
    }
    if image.is_empty() {
        clear_all_features(doc);
        return;
    }

    let path_id = doc.push_path(
        GeometryPath::filled(FillRule::NonZero),
        polygon_contours_to_contours(image),
    );
    let mut composite =
        GeometryFeature::new(FeatureKind::Composite, FeatureBucket::Fill, Polarity::Dark);
    composite.path_start = path_id;
    composite.path_count = 1;
    composite.object_index = doc.features.len() as u32;
    clear_all_features(doc);
    doc.features.push(composite);
}

fn stroked_path_outline(doc: &GeometryDocument, path: &GeometryPath) -> Option<PathPayload> {
    if path.stroke_width <= 0.0 {
        return None;
    }
    let source = path_to_kurbo(doc, path);
    if source.elements().is_empty() {
        return None;
    }
    let stroke = Stroke::new(path.stroke_width)
        .with_join(Join::Round)
        .with_caps(kurbo_cap(path.line_cap));
    let outline = kurbo::stroke(source, &stroke, &StrokeOpts::default(), 0.01);
    let contours = kurbo_path_to_line_contours(&outline);
    (!contours.is_empty()).then_some(PathPayload {
        path: GeometryPath::filled(FillRule::NonZero),
        contours,
    })
}

fn path_to_kurbo(doc: &GeometryDocument, path: &GeometryPath) -> BezPath {
    let mut out = BezPath::new();
    let mut current = Point::default();
    for contour in &doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
    {
        for cmd in &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
        {
            match cmd.op {
                PathOp::MoveTo => {
                    current = cmd.p0;
                    out.move_to(kurbo_point(cmd.p0));
                }
                PathOp::LineTo => {
                    current = cmd.p0;
                    out.line_to(kurbo_point(cmd.p0));
                }
                PathOp::ArcTo => {
                    append_arc_to_kurbo(&mut out, current, cmd.p0, cmd.p1, cmd.clockwise);
                    current = cmd.p0;
                }
                PathOp::Close => out.close_path(),
            }
        }
    }
    out
}

fn append_arc_to_kurbo(
    out: &mut BezPath,
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
) {
    let radius = start.distance_to(center);
    if radius == 0.0 {
        out.line_to(kurbo_point(end));
        return;
    }
    let sweep = arc_sweep_radians(start, end, center, clockwise);
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let segment_count = (signed_sweep.abs() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    let delta = signed_sweep / segment_count as f64;
    let mut angle = start.angle_from(center);
    for _ in 0..segment_count {
        let next_angle = angle + delta;
        let k = 4.0 / 3.0 * (delta / 4.0).tan();
        let p0 = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
        let p3 = Point::new(
            center.x + radius * next_angle.cos(),
            center.y + radius * next_angle.sin(),
        );
        let c1 = Point::new(
            p0.x - radius * angle.sin() * k,
            p0.y + radius * angle.cos() * k,
        );
        let c2 = Point::new(
            p3.x + radius * next_angle.sin() * k,
            p3.y - radius * next_angle.cos() * k,
        );
        out.curve_to(kurbo_point(c1), kurbo_point(c2), kurbo_point(p3));
        angle = next_angle;
    }
}

fn kurbo_path_to_line_contours(path: &BezPath) -> Vec<ContourPayload> {
    let mut contours = Vec::new();
    let mut current = Vec::new();
    kurbo::flatten(path.clone(), 0.005, |element| match element {
        PathEl::MoveTo(point) => {
            push_line_contour(&mut contours, &mut current);
            current.push(Point::new(point.x, point.y));
        }
        PathEl::LineTo(point) => current.push(Point::new(point.x, point.y)),
        PathEl::ClosePath => push_line_contour(&mut contours, &mut current),
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => unreachable!("kurbo::flatten emits lines"),
    });
    push_line_contour(&mut contours, &mut current);
    contours
}

fn push_line_contour(contours: &mut Vec<ContourPayload>, points: &mut Vec<Point>) {
    if points.len() < 2 {
        points.clear();
        return;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(points.len() + 1);
    for (index, point) in points.drain(..).enumerate() {
        bbox.include_point(point);
        if index == 0 {
            cmds.push(PathCmd::move_to(point));
        } else {
            cmds.push(PathCmd::line_to(point));
        }
    }
    cmds.push(PathCmd::close());
    contours.push(ContourPayload { bbox, cmds });
}

fn feature_image_contours(
    doc: &GeometryDocument,
    feature: &GeometryFeature,
) -> Vec<PolygonContour> {
    let mut image = Vec::new();
    for path in
        &doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
    {
        if !path.flags.filled {
            continue;
        }
        let path_contours = path_polygon_contours(doc, path);
        if path_contours.is_empty() {
            continue;
        }
        if path.polarity == Polarity::Clear {
            image = difference_contours(image, union_contours(path_contours));
        } else {
            image = union_contours(image.into_iter().chain(path_contours).collect());
        }
    }
    image
}

fn path_polygon_contours(doc: &GeometryDocument, path: &GeometryPath) -> Vec<PolygonContour> {
    let mut contours = Vec::new();
    let bez_path = path_to_kurbo(doc, path);
    let mut current = Vec::new();
    kurbo::flatten(bez_path, 0.005, |element| match element {
        PathEl::MoveTo(point) => {
            push_polygon_contour(&mut contours, &mut current);
            current.push([point.x, point.y]);
        }
        PathEl::LineTo(point) => current.push([point.x, point.y]),
        PathEl::ClosePath => push_polygon_contour(&mut contours, &mut current),
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => unreachable!("kurbo::flatten emits lines"),
    });
    push_polygon_contour(&mut contours, &mut current);
    contours
}

fn push_polygon_contour(out: &mut Vec<PolygonContour>, contour: &mut PolygonContour) {
    if contour.first() == contour.last() {
        contour.pop();
    }
    if contour.len() >= 3 {
        out.push(std::mem::take(contour));
    } else {
        contour.clear();
    }
}

fn union_contours(contours: Vec<PolygonContour>) -> Vec<PolygonContour> {
    polygon_shapes_to_polygon_contours(contours.simplify_shape(OverlayFillRule::NonZero))
}

fn difference_contours(
    subject: Vec<PolygonContour>,
    cutters: Vec<PolygonContour>,
) -> Vec<PolygonContour> {
    if subject.is_empty() || cutters.is_empty() {
        return subject;
    }
    polygon_shapes_to_polygon_contours(subject.overlay(
        &cutters,
        OverlayRule::Difference,
        OverlayFillRule::NonZero,
    ))
}

fn polygon_shapes_to_polygon_contours(shapes: Vec<Vec<PolygonContour>>) -> Vec<PolygonContour> {
    shapes.into_iter().flatten().collect()
}

fn polygon_contours_to_contours(contours: Vec<PolygonContour>) -> Vec<ContourPayload> {
    contours
        .into_iter()
        .filter(|contour| contour.len() >= 3)
        .map(|contour| {
            let mut bbox = BBox::empty();
            let mut cmds = Vec::with_capacity(contour.len() + 1);
            for (index, [x, y]) in contour.into_iter().enumerate() {
                let point = Point::new(x, y);
                bbox.include_point(point);
                if index == 0 {
                    cmds.push(PathCmd::move_to(point));
                } else {
                    cmds.push(PathCmd::line_to(point));
                }
            }
            cmds.push(PathCmd::close());
            ContourPayload { bbox, cmds }
        })
        .collect()
}

fn clear_all_features(doc: &mut GeometryDocument) {
    let path_start = doc.paths.len() as u32;
    for feature in &mut doc.features {
        feature.path_count = 0;
        feature.path_start = path_start;
    }
}

fn path_contours(doc: &GeometryDocument, path: &GeometryPath) -> Vec<ContourPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| ContourPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

fn contour_bbox(doc: &GeometryDocument, contour_index: usize) -> BBox {
    let contour = &doc.contours[contour_index];
    let mut bbox = BBox::empty();
    let mut current = Point::default();
    for cmd in
        &doc.path_cmds[contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
    {
        match cmd.op {
            PathOp::MoveTo | PathOp::LineTo => {
                bbox.include_point(cmd.p0);
                current = cmd.p0;
            }
            PathOp::ArcTo => {
                bbox.include_circular_arc(current, cmd.p0, cmd.p1, cmd.clockwise);
                current = cmd.p0;
            }
            PathOp::Close => {}
        }
    }
    bbox
}

fn path_bbox(doc: &GeometryDocument, path_index: usize) -> BBox {
    let path = &doc.paths[path_index];
    let bbox = doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));
    if path.flags.stroked {
        bbox.expand(path.stroke_width / 2.0)
    } else {
        bbox
    }
}

fn feature_bbox(doc: &GeometryDocument, feature_index: usize) -> BBox {
    let feature = &doc.features[feature_index];
    doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
}

fn kurbo_cap(line_cap: LineCap) -> Cap {
    match line_cap {
        LineCap::Round => Cap::Round,
        LineCap::Square => Cap::Square,
        LineCap::Butt => Cap::Butt,
    }
}

fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(point.x, point.y)
}
