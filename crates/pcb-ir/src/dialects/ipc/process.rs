use std::collections::HashMap;
use std::hash::Hash;

use crate::common::*;
use crate::dialects::ipc::*;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

type ContourPayload = (BBox, Vec<PathCmd>);
type PolygonContour = Vec<[f64; 2]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TraceGroupKey<S> {
    net: Option<S>,
    set_index: u32,
    polarity: GeometryPolarity,
    fill_rule: FillRule,
}

pub fn process_document<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    normalize_bounds(doc);
    prune_unpainted_paths(doc);
    compose_feature_paths(doc);
    outline_stroked_paths(doc);
    union_feature_filled_paths(doc);
    coalesce_related_trace_features(doc);
    resolve_set_voids(doc);
    resolve_negative_polarity(doc);
    subtract_layer_cutouts(doc);
    normalize_bounds(doc);
}

pub fn prune_unpainted_paths<S, L>(doc: &mut GeometryDocument<S, L>) {
    for feature_index in 0..doc.features.len() {
        let feature = &doc.features[feature_index];
        let path_start = feature.path_start;
        let path_count = feature.path_count;
        let (path_start, path_count) = prune_unpainted_path_range(doc, path_start, path_count);
        let feature = &mut doc.features[feature_index];
        feature.path_start = path_start;
        feature.path_count = path_count;
    }

    for outline_index in 0..doc.board_outlines.len() {
        let outline = &doc.board_outlines[outline_index];
        let path_start = outline.path_start;
        let path_count = outline.path_count;
        let (path_start, path_count) = prune_unpainted_path_range(doc, path_start, path_count);
        let outline = &mut doc.board_outlines[outline_index];
        outline.path_start = path_start;
        outline.path_count = path_count;
    }
}

fn prune_unpainted_path_range<S, L>(
    doc: &mut GeometryDocument<S, L>,
    path_start: u32,
    path_count: u32,
) -> (u32, u32) {
    let paths = &doc.paths[path_start as usize..(path_start + path_count) as usize];
    if paths
        .iter()
        .all(|path| path.flags.filled || path.flags.stroked)
    {
        return (path_start, path_count);
    }

    let painted_paths = paths
        .iter()
        .filter(|path| path.flags.filled || path.flags.stroked)
        .cloned()
        .collect::<Vec<_>>();
    let new_path_start = doc.paths.len() as u32;
    for path in painted_paths {
        copy_path(doc, &path);
    }
    (new_path_start, doc.paths.len() as u32 - new_path_start)
}

pub fn flatten_layers_to_masks<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        if layer.feature_count == 0 {
            continue;
        }

        let feature_indices = layer_features(&layer).collect::<Vec<_>>();
        let contours = feature_indices
            .iter()
            .flat_map(|&feature_index| {
                let feature = &doc.features[feature_index];
                if feature.bucket == FeatureBucket::Cutout
                    || feature.polarity != GeometryPolarity::Positive
                {
                    Vec::new()
                } else {
                    feature_filled_contours(doc, feature)
                }
            })
            .collect::<Vec<_>>();

        for &feature_index in &feature_indices {
            clear_feature_paths(doc, feature_index);
        }

        if contours.is_empty() {
            continue;
        }

        let contours =
            polygon_shapes_to_contours(contours.simplify_shape(OverlayFillRule::NonZero));
        if contours.is_empty() {
            continue;
        }

        let mask_index = feature_indices[0];
        replace_feature_with_compound_path(
            doc,
            mask_index,
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            contours,
        );
        let mask = &mut doc.features[mask_index];
        mask.kind = FeatureKind::FlattenedBucket;
        mask.bucket = FeatureBucket::Fill;
        mask.polarity = GeometryPolarity::Positive;
        mask.net = None;
    }

    normalize_bounds(doc);
}

pub fn normalize_bounds<S, L>(doc: &mut GeometryDocument<S, L>) {
    for contour_index in 0..doc.contours.len() {
        doc.contours[contour_index].bbox = contour_bbox(doc, contour_index);
    }

    for path_index in 0..doc.paths.len() {
        doc.paths[path_index].bbox = path_bbox(doc, path_index);
    }

    for outline_index in 0..doc.board_outlines.len() {
        let outline = &doc.board_outlines[outline_index];
        doc.board_outlines[outline_index].bbox =
            paths_bbox(doc, outline.path_start, outline.path_count);
    }

    for feature_index in 0..doc.features.len() {
        doc.features[feature_index].bbox = feature_bbox(doc, feature_index);
    }

    for layer_index in 0..doc.layers.len() {
        doc.layers[layer_index].bbox = layer_bbox(doc, layer_index);
    }
}

pub fn compose_feature_paths<S: Clone, L>(doc: &mut GeometryDocument<S, L>) {
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

        let contours = paths
            .iter()
            .flat_map(|path| path_contours(doc, path))
            .collect::<Vec<_>>();

        let mut path = first.clone();
        path.bbox = BBox::empty();
        replace_feature_with_compound_path(doc, feature_index, path, contours);
    }
}

pub fn outline_stroked_paths<S: Clone, L>(doc: &mut GeometryDocument<S, L>) {
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
        let new_path_start = doc.paths.len() as u32;
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
        feature.path_start = new_path_start;
        feature.path_count = doc.paths.len() as u32 - new_path_start;
    }
}

pub fn union_feature_filled_paths<S: Clone, L>(doc: &mut GeometryDocument<S, L>) {
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
        let Some(fill_rule) = feature_fill_rule(paths) else {
            continue;
        };

        let contours = feature_polygon_contours(doc, &feature);
        if contours.len() < 2 {
            continue;
        }

        let result = contours.simplify_shape(overlay_fill_rule(fill_rule));
        let contours = polygon_shapes_to_contours(result);
        if contours.is_empty() {
            continue;
        }

        replace_feature_with_compound_path(
            doc,
            feature_index,
            GeometryPath::filled(fill_rule, BBox::empty()),
            contours,
        );
    }
}

pub fn coalesce_related_trace_features<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let mut groups: HashMap<TraceGroupKey<S>, Vec<usize>> = HashMap::new();

        for feature_index in layer.feature_start..layer.feature_start + layer.feature_count {
            let feature_index = feature_index as usize;
            let feature = &doc.features[feature_index];
            if feature.bucket != FeatureBucket::Trace
                || feature.polarity != GeometryPolarity::Positive
            {
                continue;
            }

            let paths = &doc.paths
                [feature.path_start as usize..(feature.path_start + feature.path_count) as usize];
            if paths.is_empty() || !paths.iter().all(|path| path.flags.filled) {
                continue;
            }

            let Some(fill_rule) = feature_fill_rule(paths) else {
                continue;
            };
            groups
                .entry(TraceGroupKey {
                    net: feature.net,
                    set_index: feature.source.set_index,
                    polarity: feature.polarity,
                    fill_rule,
                })
                .or_default()
                .push(feature_index);
        }

        for (key, group) in groups {
            if group.len() < 2 {
                continue;
            }

            let contours = group
                .iter()
                .flat_map(|&feature_index| {
                    feature_polygon_contours(doc, &doc.features[feature_index])
                })
                .collect::<Vec<_>>();
            if contours.len() < 2 {
                continue;
            }

            let result = contours.simplify_shape(overlay_fill_rule(key.fill_rule));
            let contours = polygon_shapes_to_contours(result);
            if contours.is_empty() {
                continue;
            }

            replace_feature_with_compound_path(
                doc,
                group[0],
                GeometryPath::filled(key.fill_rule, BBox::empty()),
                contours,
            );
            for &feature_index in &group[1..] {
                clear_feature_paths(doc, feature_index);
            }
        }
    }
}

pub fn resolve_set_voids<S: Clone, L: Clone>(doc: &mut GeometryDocument<S, L>) {
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        for mut feature_indices in layer_features_by_set(doc, &layer).into_values() {
            feature_indices.sort_by_key(|&index| doc.features[index].source.feature_index);
            let mut previous = Vec::new();

            for feature_index in feature_indices {
                let feature = doc.features[feature_index].clone();
                if feature.bucket == FeatureBucket::Cutout {
                    continue;
                }

                if feature.flags.clears_previous_in_set {
                    let cutters = feature_filled_contours(doc, &feature);
                    if !cutters.is_empty() {
                        for subject_index in previous.iter().copied() {
                            subtract_contours_from_feature(doc, subject_index, &cutters);
                        }
                    }
                    clear_feature_paths(doc, feature_index);
                    continue;
                }

                if feature.polarity == GeometryPolarity::Positive {
                    previous.push(feature_index);
                }
            }
        }
    }
}

fn layer_features_by_set<S, L>(
    doc: &GeometryDocument<S, L>,
    layer: &GeometryLayer<S, L>,
) -> HashMap<u32, Vec<usize>> {
    let mut features_by_set = HashMap::new();
    for feature_index in layer_features(layer) {
        features_by_set
            .entry(doc.features[feature_index].source.set_index)
            .or_insert_with(Vec::new)
            .push(feature_index);
    }
    features_by_set
}

pub fn resolve_negative_polarity<S: Clone, L: Clone>(doc: &mut GeometryDocument<S, L>) {
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let negative_features = layer_features(&layer)
            .filter(|&feature_index| {
                let feature = &doc.features[feature_index];
                feature.bucket != FeatureBucket::Cutout
                    && !feature.flags.clears_previous_in_set
                    && feature.polarity == GeometryPolarity::Negative
            })
            .collect::<Vec<_>>();
        let mut cutters = negative_features
            .iter()
            .flat_map(|&feature_index| feature_filled_contours(doc, &doc.features[feature_index]))
            .collect::<Vec<_>>();
        if cutters.is_empty() {
            continue;
        }
        if cutters.len() > 1 {
            cutters = simplify_polygon_contours(cutters, FillRule::NonZero);
        }

        for feature_index in layer_features(&layer).collect::<Vec<_>>() {
            let feature = &doc.features[feature_index];
            if feature.bucket != FeatureBucket::Cutout
                && feature.polarity == GeometryPolarity::Positive
            {
                subtract_contours_from_feature(doc, feature_index, &cutters);
            }
        }

        for feature_index in negative_features {
            clear_feature_paths(doc, feature_index);
        }
    }
}

pub fn subtract_layer_cutouts<S: Clone, L: Clone>(doc: &mut GeometryDocument<S, L>) {
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let cutouts = layer_cutout_contours(doc, &layer);
        if cutouts.is_empty() {
            continue;
        }

        for feature_index in layer.feature_start..layer.feature_start + layer.feature_count {
            let feature = doc.features[feature_index as usize].clone();
            if feature.bucket == FeatureBucket::Cutout {
                continue;
            }

            subtract_contours_from_feature(doc, feature_index as usize, &cutouts);
        }
    }
}

fn subtract_contours_from_feature<S, L>(
    doc: &mut GeometryDocument<S, L>,
    feature_index: usize,
    cutters: &[PolygonContour],
) {
    let subject = feature_filled_contours(doc, &doc.features[feature_index]);
    if subject.is_empty() {
        return;
    }

    let cutters = cutters.to_vec();
    let result = subject.overlay(&cutters, OverlayRule::Difference, OverlayFillRule::NonZero);
    let contours = polygon_shapes_to_contours(result);
    if contours.is_empty() {
        clear_feature_paths(doc, feature_index);
        return;
    }

    replace_feature_with_compound_path(
        doc,
        feature_index,
        GeometryPath::filled(FillRule::NonZero, BBox::empty()),
        contours,
    );
}

fn layer_features<S, L>(layer: &GeometryLayer<S, L>) -> impl Iterator<Item = usize> + '_ {
    (layer.feature_start..layer.feature_start + layer.feature_count).map(|index| index as usize)
}

fn compatible_paths(a: &GeometryPath, b: &GeometryPath) -> bool {
    a.flags.filled == b.flags.filled
        && a.flags.stroked == b.flags.stroked
        && (!a.flags.filled || a.fill_rule == b.fill_rule)
        && (!a.flags.stroked || (a.stroke_width == b.stroke_width && a.line_cap == b.line_cap))
}

fn feature_fill_rule(paths: &[GeometryPath]) -> Option<FillRule> {
    let fill_rule = paths.first()?.fill_rule;
    paths
        .iter()
        .all(|path| path.fill_rule == fill_rule)
        .then_some(fill_rule)
}

fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

fn copy_path<S, L>(doc: &mut GeometryDocument<S, L>, path: &GeometryPath) -> u32 {
    doc.push_compound_path(path.clone(), path_contours(doc, path))
}

fn replace_feature_with_compound_path<S, L>(
    doc: &mut GeometryDocument<S, L>,
    feature_index: usize,
    path: GeometryPath,
    contours: Vec<ContourPayload>,
) {
    let path_id = doc.push_compound_path(path, contours);
    let feature = &mut doc.features[feature_index];
    feature.path_start = path_id;
    feature.path_count = 1;
}

fn clear_feature_paths<S, L>(doc: &mut GeometryDocument<S, L>, feature_index: usize) {
    let feature = &mut doc.features[feature_index];
    feature.path_start = doc.paths.len() as u32;
    feature.path_count = 0;
}

fn path_contours<S, L>(doc: &GeometryDocument<S, L>, path: &GeometryPath) -> Vec<ContourPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| {
            let cmds = doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec();
            (contour.bbox, cmds)
        })
        .collect()
}

fn layer_cutout_contours<S, L>(
    doc: &GeometryDocument<S, L>,
    layer: &GeometryLayer<S, L>,
) -> Vec<PolygonContour> {
    doc.features[layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .filter(|feature| feature.bucket == FeatureBucket::Cutout)
        .flat_map(|feature| feature_filled_contours(doc, feature))
        .collect()
}

fn feature_filled_contours<S, L>(
    doc: &GeometryDocument<S, L>,
    feature: &GeometryFeature<S>,
) -> Vec<PolygonContour> {
    let mut groups: HashMap<FillRule, Vec<PolygonContour>> = HashMap::new();
    for path in
        &doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
    {
        if path.flags.filled {
            groups
                .entry(path.fill_rule)
                .or_default()
                .extend(path_polygon_contours(doc, path));
        }
    }

    let mut contours = groups
        .into_iter()
        .flat_map(|(fill_rule, contours)| simplify_polygon_contours(contours, fill_rule))
        .collect::<Vec<_>>();
    if contours.len() > 1 {
        contours = simplify_polygon_contours(contours, FillRule::NonZero);
    }
    contours
}

fn feature_polygon_contours<S, L>(
    doc: &GeometryDocument<S, L>,
    feature: &GeometryFeature<S>,
) -> Vec<PolygonContour> {
    doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        .iter()
        .flat_map(|path| path_polygon_contours(doc, path))
        .collect()
}

fn stroked_path_outline<S, L>(
    doc: &GeometryDocument<S, L>,
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

fn path_to_kurbo<S, L>(doc: &GeometryDocument<S, L>, path: &GeometryPath) -> BezPath {
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

fn path_to_polygon_contours<S, L>(
    doc: &GeometryDocument<S, L>,
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

fn path_polygon_contours<S, L>(
    doc: &GeometryDocument<S, L>,
    path: &GeometryPath,
) -> Vec<PolygonContour> {
    let mut contours = Vec::new();
    path_to_polygon_contours(doc, path, &mut contours);
    contours
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

fn simplify_polygon_contours(
    contours: Vec<PolygonContour>,
    fill_rule: FillRule,
) -> Vec<PolygonContour> {
    polygon_shapes_to_polygon_contours(contours.simplify_shape(overlay_fill_rule(fill_rule)))
}

fn polygon_shapes_to_polygon_contours(shapes: Vec<Vec<PolygonContour>>) -> Vec<PolygonContour> {
    shapes.into_iter().flatten().collect()
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

fn contour_bbox<S, L>(doc: &GeometryDocument<S, L>, contour_index: usize) -> BBox {
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

fn path_bbox<S, L>(doc: &GeometryDocument<S, L>, path_index: usize) -> BBox {
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

fn feature_bbox<S, L>(doc: &GeometryDocument<S, L>, feature_index: usize) -> BBox {
    let feature = &doc.features[feature_index];
    paths_bbox(doc, feature.path_start, feature.path_count)
}

fn paths_bbox<S, L>(doc: &GeometryDocument<S, L>, path_start: u32, path_count: u32) -> BBox {
    doc.paths[path_start as usize..(path_start + path_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
}

fn layer_bbox<S, L>(doc: &GeometryDocument<S, L>, layer_index: usize) -> BBox {
    let layer = &doc.layers[layer_index];
    doc.features[layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, feature| bbox.union(feature.bbox))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestDoc = GeometryDocument<u32, ()>;

    #[test]
    fn composes_compatible_stroked_feature_paths() {
        let mut doc = TestDoc::new("test".to_string());
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
    fn process_prunes_unpainted_feature_and_outline_paths() {
        let mut doc = TestDoc::new("test".to_string());
        doc.layers.push(GeometryLayer {
            name: "TOP".to_string(),
            source_layer_ref: 0,
            layer_function: (),
            feature_start: 0,
            feature_count: 1,
            bbox: BBox::empty(),
        });

        let painted_feature_path = doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 1.0, 1.0),
        );
        let mut unpainted = GeometryPath::filled(FillRule::NonZero, BBox::empty());
        unpainted.flags.filled = false;
        let _unpainted_feature_path = doc.push_path(unpainted, rect_cmds(2.0, 2.0, 3.0, 3.0));
        doc.features.push(GeometryFeature {
            path_start: painted_feature_path,
            path_count: 2,
            ..GeometryFeature::new(
                FeatureKind::Padstack,
                FeatureBucket::Smd,
                GeometryPolarity::Positive,
            )
        });

        let painted_outline_path = doc.push_path(
            GeometryPath::stroked(0.1, LineCap::Round, BBox::empty()),
            [
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
            ],
        );
        let mut unpainted = GeometryPath::stroked(0.1, LineCap::Round, BBox::empty());
        unpainted.flags.stroked = false;
        let _unpainted_outline_path = doc.push_path(
            unpainted,
            [
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
            ],
        );
        doc.board_outlines.push(BoardOutline {
            path_start: painted_outline_path,
            path_count: 2,
            bbox: BBox::empty(),
        });

        process_document(&mut doc);

        let feature_paths = &doc.paths[doc.features[0].path_start as usize
            ..(doc.features[0].path_start + doc.features[0].path_count) as usize];
        assert_eq!(feature_paths.len(), 1);
        assert!(feature_paths[0].flags.filled);

        let outline_paths = &doc.paths[doc.board_outlines[0].path_start as usize
            ..(doc.board_outlines[0].path_start + doc.board_outlines[0].path_count) as usize];
        assert_eq!(outline_paths.len(), 1);
        assert!(outline_paths[0].flags.stroked);
    }

    #[test]
    fn coalesces_related_trace_features_inside_one_source_set() {
        let mut doc = TestDoc::new("test".to_string());
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 2.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            net: Some(1),
            source: SourceRef {
                set_index: 7,
                feature_index: 0,
            },
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.0, 0.0, 3.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            net: Some(1),
            source: SourceRef {
                set_index: 7,
                feature_index: 1,
            },
            path_start: 1,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(10.0, 0.0, 11.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            net: Some(1),
            source: SourceRef {
                set_index: 8,
                feature_index: 0,
            },
            path_start: 2,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });
        push_test_layer(&mut doc, 0, 3);

        process_document(&mut doc);

        assert_eq!(doc.features[0].path_count, 1);
        assert_eq!(doc.features[1].path_count, 0);
        assert_eq!(doc.features[2].path_count, 1);
        let path = &doc.paths[doc.features[0].path_start as usize];
        assert_eq!(path.contour_count, 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
    }

    #[test]
    fn resolves_negative_polarity_as_layer_subtraction() {
        let mut doc = TestDoc::new("test".to_string());
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
                FeatureKind::Polygon,
                FeatureBucket::Fill,
                GeometryPolarity::Negative,
            )
        });
        push_test_layer(&mut doc, 0, 2);

        process_document(&mut doc);

        let feature = &doc.features[0];
        let path = &doc.paths[feature.path_start as usize];
        assert_eq!(feature.path_count, 1);
        assert!(path.contour_count > 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(4.0, 4.0));
        assert_eq!(doc.features[1].path_count, 0);
        assert!(doc.features[1].bbox.is_empty());
    }

    #[test]
    fn subtracts_cutouts_after_trace_union() {
        let mut doc = TestDoc::new("test".to_string());
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
        push_test_layer(&mut doc, 0, 2);

        process_document(&mut doc);

        let trace = &doc.features[0];
        let path = &doc.paths[trace.path_start as usize];
        assert!(path.flags.filled);
        assert!(path.contour_count >= 2);
        assert_eq!(path.bbox.min.x, -0.5);
        assert_eq!(path.bbox.max.x, 4.5);
    }

    #[test]
    fn flattens_processed_layer_features_to_single_mask() {
        let mut doc = TestDoc::new("test".to_string());
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(0.0, 0.0, 2.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Padstack,
                FeatureBucket::Smd,
                GeometryPolarity::Positive,
            )
        });
        doc.push_path(
            GeometryPath::filled(FillRule::NonZero, BBox::empty()),
            rect_cmds(1.0, 0.0, 3.0, 1.0),
        );
        doc.features.push(GeometryFeature {
            path_start: 1,
            path_count: 1,
            ..GeometryFeature::new(
                FeatureKind::Trace,
                FeatureBucket::Trace,
                GeometryPolarity::Positive,
            )
        });
        push_test_layer(&mut doc, 0, 2);

        process_document(&mut doc);
        flatten_layers_to_masks(&mut doc);

        assert_eq!(doc.features[0].kind, FeatureKind::FlattenedBucket);
        assert_eq!(doc.features[0].bucket, FeatureBucket::Fill);
        assert_eq!(doc.features[0].path_count, 1);
        assert_eq!(doc.features[1].path_count, 0);
        let path = &doc.paths[doc.features[0].path_start as usize];
        assert_eq!(path.contour_count, 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
        assert_eq!(doc.layers[0].bbox.min, Point::new(0.0, 0.0));
        assert_eq!(doc.layers[0].bbox.max, Point::new(3.0, 1.0));
    }

    fn push_test_layer(doc: &mut TestDoc, feature_start: u32, feature_count: u32) {
        doc.layers.push(GeometryLayer {
            name: "F.Cu".to_string(),
            source_layer_ref: 100,
            layer_function: (),
            feature_start,
            feature_count,
            bbox: BBox::empty(),
        });
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
