use std::collections::HashMap;
use std::hash::Hash;

use crate::dialects::ipc::*;
use crate::dialects::path as common_path;

type ContourPayload = common_path::PathPayload;
type PolygonContour = common_path::PolygonContour;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TraceGroupKey<S> {
    net: Option<S>,
    set_index: u32,
    polarity: GeometryPolarity,
    fill_rule: FillRule,
    intent: FeatureIntent<S>,
}

/// Run only structure-preserving cleanup passes.
///
/// This keeps source vector geometry, strokes, feature polarity, and layer
/// object ordering intact. Use this before targets that can still carry rich
/// vector artwork semantics.
pub fn normalize_preserving<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    normalize_bounds(doc);
    prune_unpainted_paths(doc);
    compose_feature_paths(doc);
    normalize_bounds(doc);
}

/// Resolve source geometry into composed fabrication artwork.
///
/// This is intentionally destructive: it outlines strokes, applies boolean
/// union/difference, resolves voids, and may convert arcs into polygon
/// contours. Use it only when a target needs final painted artwork.
pub fn compose_for_artwork_export<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    normalize_preserving(doc);
    outline_stroked_paths(doc);
    union_feature_filled_paths(doc);
    coalesce_related_trace_features(doc);
    resolve_set_voids(doc);
    resolve_negative_polarity(doc);
    subtract_layer_cutouts(doc);
    normalize_bounds(doc);
}

pub fn compose_for_rendering<S, L>(doc: &mut GeometryDocument<S, L>)
where
    S: Copy + Eq + Hash + Clone,
    L: Clone,
{
    compose_for_artwork_export(doc);
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

    // Step profiles are physical geometry, not painted layer features, and are
    // intentionally allowed to use unpainted paths.
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

        let contours = common_path::polygon_contours_to_payloads(common_path::union_contours(
            contours,
            FillRule::NonZero,
        ));
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
        doc.contours[contour_index].bbox = common_path::contour_bbox(
            &doc.path_cmds[doc.contours[contour_index].cmd_start as usize
                ..(doc.contours[contour_index].cmd_start + doc.contours[contour_index].cmd_count)
                    as usize],
        );
    }

    for path_index in 0..doc.paths.len() {
        doc.paths[path_index].bbox = path_bbox(doc, path_index);
    }

    for cutout_index in 0..doc.profile_cutouts.len() {
        let path = doc.profile_cutouts[cutout_index].path;
        doc.profile_cutouts[cutout_index].bbox = doc.paths[path as usize].bbox;
    }

    for profile_index in 0..doc.profiles.len() {
        let outer_path = doc.profiles[profile_index].outer_path;
        doc.profiles[profile_index].bbox = doc.paths[outer_path as usize].bbox;
    }

    for instance_index in 0..doc.layout.instances.len() {
        let step_index = doc.layout.instances[instance_index].child_step;
        let profile_start = doc.layout.steps[step_index as usize].profile_start;
        let profile_count = doc.layout.steps[step_index as usize].profile_count;
        let transform = doc.layout.instances[instance_index].transform;
        doc.layout.instances[instance_index].bbox =
            transformed_profiles_range_bbox(doc, profile_start, profile_count, transform);
    }

    for repeat_index in (0..doc.layout.repeats.len()).rev() {
        let instance_start = doc.layout.repeats[repeat_index].instance_start;
        let instance_count = doc.layout.repeats[repeat_index].instance_count;
        let bbox = layout_instances_range_bbox(doc, instance_start, instance_count);
        doc.layout.repeats[repeat_index].bbox = bbox;
        if let Some(parent_instance) = doc.layout.repeats[repeat_index].parent_instance {
            let instance_bbox = doc.layout.instances[parent_instance as usize].bbox;
            doc.layout.instances[parent_instance as usize].bbox = instance_bbox.union(bbox);
        }
    }

    for step_index in 0..doc.layout.steps.len() {
        let profile_start = doc.layout.steps[step_index].profile_start;
        let profile_count = doc.layout.steps[step_index].profile_count;
        let profile_bbox = profiles_range_bbox(doc, profile_start, profile_count);
        let repeat_bbox = layout_step_repeats_bbox(doc, step_index as u32);
        doc.layout.steps[step_index].bbox = if !profile_bbox.is_empty() {
            profile_bbox
        } else {
            repeat_bbox
        };
    }

    for feature_index in 0..doc.features.len() {
        doc.features[feature_index].bbox = feature_bbox(doc, feature_index);
    }

    for set_index in 0..doc.feature_sets.len() {
        doc.feature_sets[set_index].bbox = feature_set_bbox(doc, set_index);
    }

    for layer_index in 0..doc.layers.len() {
        doc.layers[layer_index].bbox = layer_bbox(doc, layer_index);
    }
}

fn profiles_range_bbox<S, L>(doc: &GeometryDocument<S, L>, start: u32, count: u32) -> BBox {
    doc.profiles[start as usize..(start + count) as usize]
        .iter()
        .map(|profile| profile.bbox)
        .fold(BBox::empty(), BBox::union)
}

fn transformed_profiles_range_bbox<S, L>(
    doc: &GeometryDocument<S, L>,
    start: u32,
    count: u32,
    transform: Affine2,
) -> BBox {
    doc.profiles[start as usize..(start + count) as usize]
        .iter()
        .map(|profile| transformed_path_bbox(doc, profile.outer_path, transform))
        .fold(BBox::empty(), BBox::union)
}

fn layout_instances_range_bbox<S, L>(doc: &GeometryDocument<S, L>, start: u32, count: u32) -> BBox {
    doc.layout.instances[start as usize..(start + count) as usize]
        .iter()
        .map(|instance| instance.bbox)
        .fold(BBox::empty(), BBox::union)
}

fn layout_step_repeats_bbox<S, L>(doc: &GeometryDocument<S, L>, step_index: u32) -> BBox {
    doc.layout
        .repeats
        .iter()
        .filter(|repeat| repeat.parent_step == step_index && repeat.parent_instance.is_none())
        .map(|repeat| repeat.bbox)
        .fold(BBox::empty(), BBox::union)
}

fn feature_set_bbox<S, L>(doc: &GeometryDocument<S, L>, set_index: usize) -> BBox {
    let set_id = set_index as u32;
    let linked_bbox = doc
        .features
        .iter()
        .filter(|feature| feature.set == Some(set_id))
        .map(|feature| feature.bbox)
        .fold(BBox::empty(), BBox::union);
    if !linked_bbox.is_empty() {
        return linked_bbox;
    }

    let set = &doc.feature_sets[set_index];
    let start = set.feature_start as usize;
    let end = (set.feature_start + set.feature_count).min(doc.features.len() as u32) as usize;
    if start >= end {
        return BBox::empty();
    }

    doc.features[start..end]
        .iter()
        .map(|feature| feature.bbox)
        .fold(BBox::empty(), BBox::union)
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
        if !is_copper_trace_feature(&feature) {
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
                if let Some(contours) = stroked_path_outline(doc, &path) {
                    doc.push_compound_path(
                        GeometryPath::filled(FillRule::NonZero, BBox::empty()),
                        contours,
                    );
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
        if !is_copper_trace_feature(&feature) {
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

        let contours = common_path::polygon_contours_to_payloads(common_path::union_contours(
            contours, fill_rule,
        ));
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
            if !is_copper_trace_feature(feature) || feature.polarity != GeometryPolarity::Positive {
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
                    intent: feature.intent,
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

            let contours = common_path::polygon_contours_to_payloads(common_path::union_contours(
                contours,
                key.fill_rule,
            ));
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

fn is_copper_trace_feature<S>(feature: &GeometryFeature<S>) -> bool {
    feature.bucket == FeatureBucket::Trace && feature.intent.domain == FeatureDomain::Copper
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
            cutters = common_path::simplify_polygon_contours(cutters, FillRule::NonZero);
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
        let cutouts = layer_cutout_images(doc, &layer);
        if cutouts.is_empty() {
            continue;
        }

        for feature_index in layer.feature_start..layer.feature_start + layer.feature_count {
            let feature = doc.features[feature_index as usize].clone();
            if feature.bucket == FeatureBucket::Cutout {
                continue;
            }

            let feature_bbox = paths_bbox(doc, feature.path_start, feature.path_count);
            if feature_bbox.is_empty() {
                continue;
            }

            let cutters = cutouts
                .iter()
                .filter(|cutout| feature_bbox.intersects(cutout.bbox))
                .flat_map(|cutout| cutout.contours.iter().cloned())
                .collect::<Vec<_>>();
            if cutters.is_empty() {
                continue;
            }

            subtract_contours_from_feature(doc, feature_index as usize, &cutters);
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

    let contours = common_path::polygon_contours_to_payloads(common_path::difference_contours(
        subject,
        cutters.to_vec(),
    ));
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
        && (!a.flags.filled || a.style.fill.rule == b.style.fill.rule)
        && (!a.flags.stroked || a.style.stroke == b.style.stroke)
}

fn feature_fill_rule(paths: &[GeometryPath]) -> Option<FillRule> {
    let fill_rule = paths.first()?.style.fill.rule;
    paths
        .iter()
        .all(|path| path.style.fill.rule == fill_rule)
        .then_some(fill_rule)
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
    feature.primitive_ref = None;
}

fn clear_feature_paths<S, L>(doc: &mut GeometryDocument<S, L>, feature_index: usize) {
    let feature = &mut doc.features[feature_index];
    feature.path_start = doc.paths.len() as u32;
    feature.path_count = 0;
    feature.primitive_ref = None;
}

fn path_contours<S, L>(doc: &GeometryDocument<S, L>, path: &GeometryPath) -> Vec<ContourPayload> {
    path_payloads(doc, path)
}

fn path_payloads<S, L>(
    doc: &GeometryDocument<S, L>,
    path: &GeometryPath,
) -> Vec<common_path::PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| common_path::PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

fn layer_cutout_images<S, L>(
    doc: &GeometryDocument<S, L>,
    layer: &GeometryLayer<S, L>,
) -> Vec<common_path::ContourImage> {
    doc.features[layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
        .iter()
        .filter(|feature| feature.bucket == FeatureBucket::Cutout)
        .filter_map(|feature| {
            let contours = feature_filled_contours(doc, feature);
            if contours.is_empty() {
                None
            } else {
                Some(common_path::ContourImage::new(contours))
            }
        })
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
                .entry(path.style.fill.rule)
                .or_default()
                .extend(path_polygon_contours(doc, path));
        }
    }

    let mut contours = groups
        .into_iter()
        .flat_map(|(fill_rule, contours)| {
            common_path::simplify_polygon_contours(contours, fill_rule)
        })
        .collect::<Vec<_>>();
    if contours.len() > 1 {
        contours = common_path::simplify_polygon_contours(contours, FillRule::NonZero);
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
) -> Option<Vec<ContourPayload>> {
    common_path::outline_stroke(
        &path_payloads(doc, path),
        path.style.stroke.width,
        path.style.stroke.line_cap,
        LineJoin::Round,
    )
}

fn path_polygon_contours<S, L>(
    doc: &GeometryDocument<S, L>,
    path: &GeometryPath,
) -> Vec<PolygonContour> {
    common_path::payloads_to_polygon_contours(&path_payloads(doc, path))
}

fn path_bbox<S, L>(doc: &GeometryDocument<S, L>, path_index: usize) -> BBox {
    let path = &doc.paths[path_index];
    let bbox = doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));

    if path.flags.stroked {
        bbox.expand(path.style.stroke.width / 2.0)
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
        let mut doc = TestDoc::new();
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
            ..copper_trace_feature()
        });

        compose_for_artwork_export(&mut doc);

        assert_eq!(doc.features[0].path_count, 1);
        let path = &doc.paths[doc.features[0].path_start as usize];
        assert!(path.flags.filled);
        assert!(!path.flags.stroked);
        assert_eq!(path.bbox.min, Point::new(-1.0, -1.0));
        assert_eq!(path.bbox.max, Point::new(11.0, 1.0));
    }

    #[test]
    fn process_prunes_unpainted_feature_paths_and_preserves_profile_paths() {
        let mut doc = TestDoc::new();
        doc.layers.push(GeometryLayer {
            name: "TOP".to_string(),
            source_layer_ref: 0,
            layer_function: (),
            spec_ref_start: 0,
            spec_ref_count: 0,
            set_start: 0,
            set_count: 0,
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

        let outer_profile_path = doc.push_path(
            GeometryPath::unpainted(BBox::empty()),
            [
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
            ],
        );
        let cutout_path = doc.push_path(
            GeometryPath::unpainted(BBox::empty()),
            [
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
            ],
        );
        doc.profile_cutouts.push(StepProfileCutout {
            path: cutout_path,
            bbox: BBox::empty(),
        });
        doc.profiles.push(StepProfile {
            outer_path: outer_profile_path,
            cutout_start: 0,
            cutout_count: 1,
            bbox: BBox::empty(),
        });

        compose_for_artwork_export(&mut doc);

        let feature_paths = &doc.paths[doc.features[0].path_start as usize
            ..(doc.features[0].path_start + doc.features[0].path_count) as usize];
        assert_eq!(feature_paths.len(), 1);
        assert!(feature_paths[0].flags.filled);

        assert_eq!(doc.profiles[0].outer_path, outer_profile_path);
        assert_eq!(doc.profile_cutouts[0].path, cutout_path);
        assert!(!doc.paths[outer_profile_path as usize].flags.filled);
        assert!(!doc.paths[outer_profile_path as usize].flags.stroked);
    }

    #[test]
    fn coalesces_related_trace_features_inside_one_source_set() {
        let mut doc = TestDoc::new();
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
            ..copper_trace_feature()
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
            ..copper_trace_feature()
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
            ..copper_trace_feature()
        });
        push_test_layer(&mut doc, 0, 3);

        compose_for_artwork_export(&mut doc);

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
        let mut doc = TestDoc::new();
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

        compose_for_artwork_export(&mut doc);

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
        let mut doc = TestDoc::new();
        doc.push_path(
            GeometryPath::stroked(1.0, LineCap::Round, BBox::empty()),
            [
                PathCmd::move_to(Point::new(0.0, 2.0)),
                PathCmd::line_to(Point::new(4.0, 2.0)),
            ],
        );
        doc.features.push(GeometryFeature {
            path_count: 1,
            ..copper_trace_feature()
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

        compose_for_artwork_export(&mut doc);

        let trace = &doc.features[0];
        let path = &doc.paths[trace.path_start as usize];
        assert!(path.flags.filled);
        assert!(path.contour_count >= 2);
        assert_eq!(path.bbox.min.x, -0.5);
        assert_eq!(path.bbox.max.x, 4.5);
    }

    #[test]
    fn flattens_processed_layer_features_to_single_mask() {
        let mut doc = TestDoc::new();
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
            ..copper_trace_feature()
        });
        push_test_layer(&mut doc, 0, 2);

        compose_for_artwork_export(&mut doc);
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
            spec_ref_start: 0,
            spec_ref_count: 0,
            set_start: 0,
            set_count: 0,
            feature_start,
            feature_count,
            bbox: BBox::empty(),
        });
    }

    fn copper_trace_feature() -> GeometryFeature<u32> {
        let mut feature = GeometryFeature::new(
            FeatureKind::Trace,
            FeatureBucket::Trace,
            GeometryPolarity::Positive,
        );
        feature.intent.domain = FeatureDomain::Copper;
        feature.intent.role = FeatureRole::Conductor;
        feature.intent.operation = FeatureOperation::AddMaterial;
        feature.intent.material = FeatureMaterial::Copper;
        feature
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
