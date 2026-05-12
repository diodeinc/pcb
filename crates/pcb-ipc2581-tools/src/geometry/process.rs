use super::ir::*;

pub fn process_document(doc: &mut GeometryDocument) {
    normalize_bounds(doc);
    compose_feature_paths(doc);
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

fn compatible_paths(a: &GeometryPath, b: &GeometryPath) -> bool {
    a.flags.filled == b.flags.filled
        && a.flags.stroked == b.flags.stroked
        && (!a.flags.filled || a.fill_rule == b.fill_rule)
        && (!a.flags.stroked || (a.stroke_width == b.stroke_width && a.line_cap == b.line_cap))
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
        assert_eq!(path.contour_count, 2);
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
}
