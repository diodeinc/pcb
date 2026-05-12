use super::ir::*;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

type ContourPayload = (BBox, Vec<PathCmd>);
type PolygonContour = Vec<[f64; 2]>;

pub fn process_document(doc: &mut GeometryDocument) {
    normalize_bounds(doc);
    compose_feature_paths(doc);
    outline_stroked_paths(doc);
    union_feature_filled_paths(doc);
    subtract_layer_cutouts(doc);
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

    for layer_index in 0..doc.layers.len() {
        doc.layers[layer_index].bbox = layer_bbox(doc, layer_index);
    }
}

pub fn compose_feature_paths(doc: &mut GeometryDocument) {
    let feature_count = doc.features.len();
    for feature_index in 0..feature_count {
        let feature = doc.features[feature_index].clone();
        if feature.path_count < 2 {
            continue;
        }

        let paths = &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize];
        let Some(first) = paths.first() else {
            continue;
        };
        if !paths.iter().all(|path| compatible_paths(first, path)) {
            continue;
        }

        let mut contours = Vec::new();
        for path in paths {
            for contour in &doc.contours
                [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
            {
                let cmds = doc.path_cmds
                    [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                    .to_vec();
                contours.push((contour.bbox, cmds));
            }
        }

        let mut path = first.clone();
        path.bbox = BBox::empty();
        let path_id = doc.push_compound_path(path, contours);
        let feature = &mut doc.features[feature_index];
        feature.path_start = path_id;
        feature.path_count = 1;
    }
}

pub fn outline_stroked_paths(doc: &mut GeometryDocument) {
    let feature_count = doc.features.len();
    for feature_index in 0..feature_count {
        let feature = doc.features[feature_index].clone();
        if feature.bucket != FeatureBucket::Trace {
            continue;
        }

        let paths = &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize];
        if !paths.iter().any(|path| path.flags.stroked) {
            continue;
        }

        let paths = paths.to_vec();
        let path_start = doc.paths.len() as u32;
        for path in paths {
            if path.flags.stroked {
                if let Some((path, contours)) = stroked_path_outline(doc, &path) {
                    doc.push_compound_path(path, contours);
                }
            } else {
                copy_path(doc, &path);
            }
        }

        let feature = &mut doc.features[feature_index];
        feature.path_start = path_start;
        feature.path_count = doc.paths.len() as u32 - path_start;
    }
}

pub fn union_feature_filled_paths(doc: &mut GeometryDocument) {
    let feature_count = doc.features.len();
    for feature_index in 0..feature_count {
        let feature = doc.features[feature_index].clone();
        if feature.bucket != FeatureBucket::Trace {
            continue;
        }

        let paths = &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize];
        if paths.is_empty() || !paths.iter().all(|path| path.flags.filled) {
            continue;
        }

        let mut contours = Vec::new();
        for path in paths {
            path_to_polygon_contours(doc, path, &mut contours);
        }
        if contours.len() < 2 {
            continue;
        }

        let result = contours.simplify_shape(OverlayFillRule::NonZero);
        let contours = polygon_shapes_to_contours(result);
        if contours.is_empty() {
            continue;
        }

        let path_id = doc.push_compound_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            contours,
        );
        let feature = &mut doc.features[feature_index];
        feature.path_start = path_id;
        feature.path_count = 1;
    }
}

pub fn subtract_layer_cutouts(doc: &mut GeometryDocument) {
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let features = &doc.features
            [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize];
        let mut cutouts = Vec::new();
        for feature in features {
            if feature.bucket == FeatureBucket::Cutout {
                for path in &doc.paths[feature.path_start as usize
                    ..(feature.path_start + feature.path_count) as usize]
                {
                    path_to_polygon_contours(doc, path, &mut cutouts);
                }
            }
        }
        if cutouts.is_empty() {
            continue;
        }

        for feature_index in layer.feature_start..layer.feature_start + layer.feature_count {
            let feature = doc.features[feature_index as usize].clone();
            if feature.bucket == FeatureBucket::Cutout {
                continue;
            }

            let paths = &doc.paths
                [feature.path_start as usize..(feature.path_start + feature.path_count) as usize];
            if paths.is_empty() || !paths.iter().all(|path| path.flags.filled) {
                continue;
            }

            let mut subject = Vec::new();
            for path in paths {
                path_to_polygon_contours(doc, path, &mut subject);
            }
            if subject.is_empty() {
                continue;
            }

            let result =
                subject.overlay(&cutouts, OverlayRule::Difference, OverlayFillRule::NonZero);
            let contours = polygon_shapes_to_contours(result);
            if contours.is_empty() {
                let feature = &mut doc.features[feature_index as usize];
                feature.path_start = doc.paths.len() as u32;
                feature.path_count = 0;
                continue;
            }

            let path_id = doc.push_compound_path(
                GeometryPath::filled(FillRule::NonZero, BBox::empty()),
                contours,
            );
            let feature = &mut doc.features[feature_index as usize];
            feature.path_start = path_id;
            feature.path_count = 1;
        }
    }
}

fn compatible_paths(a: &GeometryPath, b: &GeometryPath) -> bool {
    a.flags.filled == b.flags.filled
        && a.flags.stroked == b.flags.stroked
        && (!a.flags.filled || a.fill_rule == b.fill_rule)
        && (!a.flags.stroked || (a.stroke_width == b.stroke_width && a.line_cap == b.line_cap))
}

fn copy_path(doc: &mut GeometryDocument, path: &GeometryPath) -> u32 {
    let contours = doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| {
            let cmds = doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec();
            (contour.bbox, cmds)
        })
        .collect::<Vec<_>>();
    doc.push_compound_path(path.clone(), contours)
}

fn stroked_path_outline(
    doc: &GeometryDocument,
    path: &GeometryPath,
) -> Option<(GeometryPath, Vec<ContourPayload>)> {
    let source = path_to_kurbo(doc, path);
    if source.elements().is_empty() {
        return None;
    }

    let stroke = Stroke::new(path.stroke_width)
        .with_join(Join::Round)
        .with_caps(kurbo_cap(path.line_cap));
    let outline = kurbo::stroke(source, &stroke, &StrokeOpts::default(), 0.01);
    let contours = kurbo_path_to_contours(&outline);
    if contours.is_empty() {
        return None;
    }

    Some((
        GeometryPath::filled(FillRule::NonZero, BBox::empty()),
        contours,
    ))
}

fn path_to_kurbo(doc: &GeometryDocument, path: &GeometryPath) -> BezPath {
    let mut out = BezPath::new();
    for contour in &doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
    {
        let mut current = Point::default();
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
                PathOp::CubicTo => {
                    current = cmd.p2;
                    out.curve_to(
                        kurbo_point(cmd.p0),
                        kurbo_point(cmd.p1),
                        kurbo_point(cmd.p2),
                    );
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
    let segment_count = (signed_sweep.abs() / std::f64::consts::FRAC_PI_2).ceil() as usize;
    let delta = signed_sweep / segment_count.max(1) as f64;
    let mut angle = start.angle_from(center);

    for _ in 0..segment_count.max(1) {
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

fn kurbo_path_to_contours(path: &BezPath) -> Vec<(BBox, Vec<PathCmd>)> {
    let mut contours = Vec::new();
    let mut cmds = Vec::new();
    let mut bbox = BBox::empty();
    let mut current = Point::default();

    for element in path.iter() {
        match element {
            PathEl::MoveTo(point) => {
                push_kurbo_contour(&mut contours, &mut bbox, &mut cmds);
                current = ir_point(point);
                bbox.include_point(current);
                cmds.push(PathCmd::move_to(current));
            }
            PathEl::LineTo(point) => {
                current = ir_point(point);
                bbox.include_point(current);
                cmds.push(PathCmd::line_to(current));
            }
            PathEl::QuadTo(p1, p2) => {
                let p1 = ir_point(p1);
                let p2 = ir_point(p2);
                let c1 = Point::new(
                    current.x + (p1.x - current.x) * 2.0 / 3.0,
                    current.y + (p1.y - current.y) * 2.0 / 3.0,
                );
                let c2 = Point::new(
                    p2.x + (p1.x - p2.x) * 2.0 / 3.0,
                    p2.y + (p1.y - p2.y) * 2.0 / 3.0,
                );
                bbox.include_point(c1);
                bbox.include_point(c2);
                bbox.include_point(p2);
                cmds.push(PathCmd::cubic_to(c1, c2, p2));
                current = p2;
            }
            PathEl::CurveTo(p1, p2, p3) => {
                let p1 = ir_point(p1);
                let p2 = ir_point(p2);
                let p3 = ir_point(p3);
                bbox.include_point(p1);
                bbox.include_point(p2);
                bbox.include_point(p3);
                cmds.push(PathCmd::cubic_to(p1, p2, p3));
                current = p3;
            }
            PathEl::ClosePath => cmds.push(PathCmd::close()),
        }
    }
    push_kurbo_contour(&mut contours, &mut bbox, &mut cmds);
    contours
}

fn push_kurbo_contour(
    contours: &mut Vec<(BBox, Vec<PathCmd>)>,
    bbox: &mut BBox,
    cmds: &mut Vec<PathCmd>,
) {
    if cmds.is_empty() {
        return;
    }
    contours.push((*bbox, std::mem::take(cmds)));
    *bbox = BBox::empty();
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

fn ir_point(point: kurbo::Point) -> Point {
    Point::new(point.x, point.y)
}

fn path_to_polygon_contours(
    doc: &GeometryDocument,
    path: &GeometryPath,
    out: &mut Vec<PolygonContour>,
) {
    let bez_path = path_to_kurbo(doc, path);
    let mut current = Vec::new();
    kurbo::flatten(bez_path, 0.005, |element| match element {
        PathEl::MoveTo(point) => {
            push_polygon_contour(out, &mut current);
            current.push([point.x, point.y]);
        }
        PathEl::LineTo(point) => current.push([point.x, point.y]),
        PathEl::ClosePath => push_polygon_contour(out, &mut current),
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => unreachable!("kurbo::flatten emits lines"),
    });
    push_polygon_contour(out, &mut current);
}

fn push_polygon_contour(out: &mut Vec<PolygonContour>, contour: &mut PolygonContour) {
    if contour.len() < 3 {
        contour.clear();
        return;
    }

    if contour.first() == contour.last() {
        contour.pop();
    }
    if contour.len() >= 3 {
        out.push(std::mem::take(contour));
    } else {
        contour.clear();
    }
}

fn polygon_shapes_to_contours(shapes: Vec<Vec<PolygonContour>>) -> Vec<ContourPayload> {
    let mut contours = Vec::new();
    for shape in shapes {
        for contour in shape {
            if contour.len() < 3 {
                continue;
            }

            let mut bbox = BBox::empty();
            let mut cmds = Vec::with_capacity(contour.len() + 2);
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
            contours.push((bbox, cmds));
        }
    }
    contours
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
                current = cmd.p0;
                bbox.include_point(cmd.p0);
            }
            PathOp::ArcTo => {
                bbox.include_circular_arc(current, cmd.p0, cmd.p1, cmd.clockwise);
                current = cmd.p0;
            }
            PathOp::CubicTo => {
                bbox.include_point(cmd.p0);
                bbox.include_point(cmd.p1);
                bbox.include_point(cmd.p2);
                current = cmd.p2;
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

fn layer_bbox(doc: &GeometryDocument, layer_index: usize) -> BBox {
    let layer = &doc.layers[layer_index];
    doc.features[layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, feature| bbox.union(feature.bbox))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_compatible_stroked_feature_paths() {
        let mut doc = GeometryDocument::new("test".to_string());
        let bbox = BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(10.0, 0.0),
        };
        doc.push_path(
            GeometryPath::stroked(2.0, LineCap::Round, bbox),
            [
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(5.0, 0.0)),
            ],
        );
        doc.push_path(
            GeometryPath::stroked(2.0, LineCap::Round, bbox),
            [
                PathCmd::move_to(Point::new(5.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 2,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });

        process_document(&mut doc);

        assert_eq!(doc.features[0].path_count, 1);
        let path = &doc.paths[doc.features[0].path_start as usize];
        assert!(path.flags.filled);
        assert!(!path.flags.stroked);
        assert_eq!(path.bbox.min, Point::new(-1.0, -1.0));
        assert_eq!(path.bbox.max, Point::new(11.0, 1.0));
    }

    #[test]
    fn leaves_incompatible_feature_paths_separate() {
        let mut doc = GeometryDocument::new("test".to_string());
        let bbox = BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(1.0, 1.0),
        };
        doc.push_path(
            GeometryPath::filled(FillRule::EvenOdd, bbox),
            [PathCmd::move_to(Point::new(0.0, 0.0)), PathCmd::close()],
        );
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, bbox),
            [PathCmd::move_to(Point::new(1.0, 1.0)), PathCmd::close()],
        );
        doc.features.push(GeometryFeature {
            path_count: 2,
            ..GeometryFeature::new(
                FeatureKind::Padstack,
                FeatureBucket::Thermal,
                GeometryPolarity::Positive,
            )
        });

        process_document(&mut doc);

        assert_eq!(doc.features[0].path_start, 0);
        assert_eq!(doc.features[0].path_count, 2);
    }

    #[test]
    fn unions_filled_trace_geometry_inside_one_feature() {
        let mut doc = GeometryDocument::new("test".to_string());
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 2.0, 1.0),
        );
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.0, 0.0, 3.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            path_count: 2,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });

        process_document(&mut doc);

        assert_eq!(doc.features[0].path_count, 1);
        let path = &doc.paths[doc.features[0].path_start as usize];
        assert!(path.flags.filled);
        assert_eq!(path.contour_count, 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
    }

    #[test]
    fn subtracts_cutouts_from_filled_layer_geometry() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 4.0, 4.0),
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.0, 1.0, 3.0, 3.0),
        );
        doc.features.push(GeometryFeature {
            path_start: 1,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Slot,
                FeatureBucket::Cutout,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 2,
            bbox: BBox::empty(),
        });

        process_document(&mut doc);

        let feature = &doc.features[0];
        let path = &doc.paths[feature.path_start as usize];
        assert_eq!(feature.path_count, 1);
        assert!(path.contour_count > 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(4.0, 4.0));
    }

    #[test]
    fn subtracts_cutouts_after_trace_union() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        doc.push_path(
            GeometryPath::stroked(1.0, LineCap::Round, BBox::empty()),
            [
                PathCmd::move_to(Point::new(0.0, 2.0)),
                PathCmd::line_to(Point::new(4.0, 2.0)),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.5, 1.0, 2.5, 3.0),
        );
        doc.features.push(GeometryFeature {
            path_start: 1,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Slot,
                FeatureBucket::Cutout,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 2,
            bbox: BBox::empty(),
        });

        process_document(&mut doc);

        let trace = &doc.features[0];
        let path = &doc.paths[trace.path_start as usize];
        assert!(path.flags.filled);
        assert!(path.contour_count >= 2);
        assert_eq!(path.bbox.min.x, -0.5);
        assert_eq!(path.bbox.max.x, 4.5);
    }

    #[test]
    fn removes_features_fully_inside_cutouts() {
        let mut interner = ipc2581::Interner::new();
        let mut doc = GeometryDocument::new("test".to_string());
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.0, 1.0, 2.0, 2.0),
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 3.0, 3.0),
        );
        doc.features.push(GeometryFeature {
            path_start: 1,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Slot,
                FeatureBucket::Cutout,
                GeometryPolarity::Positive,
            )
        });
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: interner.intern("F.Cu"),
            feature_start: 0,
            feature_count: 2,
            bbox: BBox::empty(),
        });

        process_document(&mut doc);

        assert_eq!(doc.features[0].path_count, 0);
        assert!(doc.features[0].bbox.is_empty());
    }

    fn rect_cmds(x0: f64, y0: f64, x1: f64, y1: f64) -> [PathCmd; 5] {
        [
            PathCmd::move_to(Point::new(x0, y0)),
            PathCmd::line_to(Point::new(x1, y0)),
            PathCmd::line_to(Point::new(x1, y1)),
            PathCmd::line_to(Point::new(x0, y1)),
            PathCmd::close(),
        ]
    }
}
