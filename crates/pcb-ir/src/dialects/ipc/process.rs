//! Pass pipelines over IPC documents.
//!
//! Passes are plain functions that mutate a [`Document`] in place. Three
//! standard pipelines cover the common targets:
//!
//! - [`normalize_preserving`]: structure-preserving cleanup only.
//! - [`normalize_for_artwork`]: additionally resolves IPC paint semantics
//!   (set voids, negative polarity, layer cutouts) for artwork export.
//! - [`compose_for_rendering`]: destructive image composition (outlines
//!   strokes, unions fills) for final rendering targets.

use std::collections::HashMap;
use std::hash::Hash;

use crate::dialects::ipc::Document;
use crate::dialects::ipc::document::Layer;
use crate::dialects::ipc::feature::{Feature, FeatureBucket, FeatureIntent, FeatureKind};
use crate::geom::path::ContourBuf;
use crate::geom::region::{self, Ring};
use crate::geom::{BBox, ContourSet, FillRule, Paint, PaintKind, Path, Polarity, Span, tol};

/// Run only structure-preserving cleanup passes.
///
/// This keeps source vector geometry, strokes, feature polarity, and layer
/// object ordering intact. Use this before targets that can still carry rich
/// vector artwork semantics.
pub fn normalize_preserving<S, L>(doc: &mut Document<S, L>)
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    normalize_bounds(doc);
    prune_unpainted_paths(doc);
    compose_feature_paths(doc);
    normalize_bounds(doc);
}

/// Resolve IPC-specific paint semantics while preserving native artwork shapes.
///
/// IPC feature-set voids, negative polarity, and layer cutouts are semantic
/// operators on source features, not generic ordered artwork objects. Resolve
/// those before lowering to source-independent artwork, but do not outline
/// strokes or flatten unrelated positive features.
pub fn normalize_for_artwork<S, L>(doc: &mut Document<S, L>)
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    normalize_preserving(doc);
    resolve_set_voids(doc);
    resolve_negative_polarity(doc);
    subtract_layer_cutouts(doc);
    compact(doc);
    normalize_bounds(doc);
}

/// Resolve source geometry into a composed rendering image.
///
/// This is intentionally destructive: it outlines strokes, applies boolean
/// union/difference, resolves voids, and may convert arcs into polygon
/// contours. Use it only when a target needs a final painted image.
pub fn compose_for_rendering<S, L>(doc: &mut Document<S, L>)
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    normalize_preserving(doc);
    expand_stroked_paths_to_fills(doc);
    union_feature_filled_paths(doc);
    coalesce_related_trace_features(doc);
    resolve_set_voids(doc);
    resolve_negative_polarity(doc);
    subtract_layer_cutouts(doc);
    compact(doc);
    normalize_bounds(doc);
}

/// Drop unpainted paths from feature path spans.
///
/// Step profiles are physical geometry, not painted layer features, and are
/// intentionally allowed to keep unpainted paths.
pub fn prune_unpainted_paths<S, L>(doc: &mut Document<S, L>) {
    for feature_index in 0..doc.features.len() {
        let span = doc.features[feature_index].paths;
        if span
            .slice(&doc.arena.paths)
            .iter()
            .all(|path| path.paint.is_painted())
        {
            continue;
        }

        let painted = span
            .slice(&doc.arena.paths)
            .iter()
            .filter(|path| path.paint.is_painted())
            .copied()
            .collect::<Vec<_>>();
        let start = doc.arena.paths.len() as u32;
        for path in painted {
            copy_path(doc, path);
        }
        doc.features[feature_index].paths = Span::new(start, doc.arena.paths.len() as u32 - start);
    }
}

/// Recompute all cached bounds bottom-up.
pub fn normalize_bounds<S, L>(doc: &mut Document<S, L>) {
    doc.arena.recompute_bounds();

    for cutout_index in 0..doc.profile_cutouts.len() {
        let path = doc.profile_cutouts[cutout_index].path;
        doc.profile_cutouts[cutout_index].bbox = doc.arena.path(path).bbox;
    }

    for profile_index in 0..doc.profiles.len() {
        let outer_path = doc.profiles[profile_index].outer_path;
        doc.profiles[profile_index].bbox = doc.arena.path(outer_path).bbox;
    }

    for instance_index in 0..doc.layout.instances.len() {
        let step_index = doc.layout.instances[instance_index].child_step;
        let profiles = doc.layout.steps[step_index as usize].profiles;
        let transform = doc.layout.instances[instance_index].transform;
        doc.layout.instances[instance_index].bbox = profiles
            .slice(&doc.profiles)
            .iter()
            .map(|profile| doc.transformed_path_bbox(profile.outer_path, transform))
            .fold(BBox::empty(), BBox::union);
    }

    for repeat_index in (0..doc.layout.repeats.len()).rev() {
        let instances = doc.layout.repeats[repeat_index].instances;
        let bbox = instances
            .slice(&doc.layout.instances)
            .iter()
            .map(|instance| instance.bbox)
            .fold(BBox::empty(), BBox::union);
        doc.layout.repeats[repeat_index].bbox = bbox;
        if let Some(parent_instance) = doc.layout.repeats[repeat_index].parent_instance {
            let instance_bbox = doc.layout.instances[parent_instance as usize].bbox;
            doc.layout.instances[parent_instance as usize].bbox = instance_bbox.union(bbox);
        }
    }

    for step_index in 0..doc.layout.steps.len() {
        let profile_bbox = doc.layout.steps[step_index]
            .profiles
            .slice(&doc.profiles)
            .iter()
            .map(|profile| profile.bbox)
            .fold(BBox::empty(), BBox::union);
        let repeat_bbox = doc
            .layout
            .repeats
            .iter()
            .filter(|repeat| {
                repeat.parent_step == step_index as u32 && repeat.parent_instance.is_none()
            })
            .map(|repeat| repeat.bbox)
            .fold(BBox::empty(), BBox::union);
        doc.layout.steps[step_index].bbox = if !profile_bbox.is_empty() {
            profile_bbox
        } else {
            repeat_bbox
        };
    }

    for feature_index in 0..doc.features.len() {
        doc.features[feature_index].bbox = doc.arena.paths_bbox(doc.features[feature_index].paths);
    }

    for set_index in 0..doc.feature_sets.len() {
        doc.feature_sets[set_index].bbox = feature_set_bbox(doc, set_index);
    }

    for layer_index in 0..doc.layers.len() {
        doc.layers[layer_index].bbox = doc.layers[layer_index]
            .features
            .slice(&doc.features)
            .iter()
            .fold(BBox::empty(), |bbox, feature| bbox.union(feature.bbox));
    }
}

/// Drop paths no longer referenced by any feature, profile, or cutout.
///
/// Passes that rewrite feature geometry leave orphaned paths in the arena;
/// this reclaims them and remaps all stored path references.
pub fn compact<S, L>(doc: &mut Document<S, L>) {
    let mut live = vec![false; doc.arena.paths.len()];
    for feature in &doc.features {
        for index in feature.paths.indices() {
            live[index as usize] = true;
        }
    }
    for profile in &doc.profiles {
        live[profile.outer_path as usize] = true;
    }
    for cutout in &doc.profile_cutouts {
        live[cutout.path as usize] = true;
    }

    if live.iter().all(|&flag| flag) {
        return;
    }
    let mapping = doc.arena.compact(&live);

    for feature in &mut doc.features {
        feature.paths = remap_span(feature.paths, &mapping);
    }
    for profile in &mut doc.profiles {
        profile.outer_path = mapping[profile.outer_path as usize].expect("profile path is live");
    }
    for cutout in &mut doc.profile_cutouts {
        cutout.path = mapping[cutout.path as usize].expect("cutout path is live");
    }
}

fn remap_span(span: Span, mapping: &[Option<u32>]) -> Span {
    if span.is_empty() {
        return Span::EMPTY;
    }
    let start = mapping[span.start as usize].expect("span start is live");
    Span::new(start, span.count)
}

/// Flatten every layer's positive features into one unioned fill mask.
pub fn flatten_layers_to_masks<S, L>(doc: &mut Document<S, L>)
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer: Layer<S, L> = doc.layers[layer_index].clone();
        if layer.features.is_empty() {
            continue;
        }

        let feature_indices = layer.features.range().collect::<Vec<_>>();
        let rings = feature_indices
            .iter()
            .flat_map(|&feature_index| {
                let feature = &doc.features[feature_index];
                if feature.bucket == FeatureBucket::Cutout || feature.polarity != Polarity::Dark {
                    Vec::new()
                } else {
                    feature_filled_rings(doc, feature)
                }
            })
            .collect::<Vec<_>>();

        for &feature_index in &feature_indices {
            clear_feature_paths(doc, feature_index);
        }

        if rings.is_empty() {
            continue;
        }

        let contours = region::rings_to_contours(region::union_rings(rings, FillRule::NonZero));
        if contours.is_empty() {
            continue;
        }

        let mask_index = feature_indices[0];
        replace_feature_with_path(
            doc,
            mask_index,
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            contours,
        );
        let mask = &mut doc.features[mask_index];
        mask.kind = FeatureKind::FlattenedBucket;
        mask.bucket = FeatureBucket::Fill;
        mask.polarity = Polarity::Dark;
        mask.net = None;
    }

    compact(doc);
    normalize_bounds(doc);
}

/// Merge a feature's identically painted paths into one compound path.
pub fn compose_feature_paths<S, L>(doc: &mut Document<S, L>) {
    for feature_index in 0..doc.features.len() {
        let span = doc.features[feature_index].paths;
        if span.len() < 2 {
            continue;
        }

        let paths = span.slice(&doc.arena.paths);
        let paint = paths[0].paint;
        if !paths.iter().all(|path| path.paint == paint) {
            continue;
        }

        let contours = paths
            .iter()
            .flat_map(|path| doc.arena.path_contours(path))
            .collect::<Vec<_>>();
        replace_feature_with_path(doc, feature_index, paint, contours);
    }
}

/// Convert copper-trace strokes into filled outlines.
pub fn expand_stroked_paths_to_fills<S, L>(doc: &mut Document<S, L>) {
    for feature_index in 0..doc.features.len() {
        let feature = &doc.features[feature_index];
        if !is_copper_trace_feature(feature) {
            continue;
        }
        let span = feature.paths;
        if !span
            .slice(&doc.arena.paths)
            .iter()
            .any(|path| path.is_stroked())
        {
            continue;
        }

        let paths = span.slice(&doc.arena.paths).to_vec();
        let start = doc.arena.paths.len() as u32;
        for path in paths {
            match path.stroke() {
                Some(stroke) => {
                    if let Some(contours) = crate::geom::path::stroke_to_fill(
                        &doc.arena.path_contours(&path),
                        stroke.into(),
                    ) {
                        doc.arena.push_path(
                            Paint::Fill {
                                rule: FillRule::NonZero,
                            },
                            contours,
                        );
                    }
                }
                None => {
                    copy_path(doc, path);
                }
            }
        }
        doc.features[feature_index].paths = Span::new(start, doc.arena.paths.len() as u32 - start);
    }
}

/// Union a trace feature's filled paths into one region.
pub fn union_feature_filled_paths<S, L>(doc: &mut Document<S, L>) {
    for feature_index in 0..doc.features.len() {
        let feature = &doc.features[feature_index];
        if !is_copper_trace_feature(feature) {
            continue;
        }

        let paths = feature.paths.slice(&doc.arena.paths);
        if paths.is_empty() || !paths.iter().all(|path| path.is_filled()) {
            continue;
        }
        let Some(fill_rule) = common_fill_rule(paths) else {
            continue;
        };

        let rings = feature_rings(doc, &doc.features[feature_index]);
        if rings.len() < 2 {
            continue;
        }

        let contours = region::rings_to_contours(region::union_rings(rings, fill_rule));
        if contours.is_empty() {
            continue;
        }

        replace_feature_with_path(
            doc,
            feature_index,
            Paint::Fill { rule: fill_rule },
            contours,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TraceGroupKey<S> {
    net: Option<S>,
    set_index: u32,
    polarity: Polarity,
    fill_rule: FillRule,
    intent: FeatureIntent<S>,
}

/// Union filled trace features that share a net, source set, and intent.
pub fn coalesce_related_trace_features<S, L>(doc: &mut Document<S, L>)
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let mut groups: HashMap<TraceGroupKey<S>, Vec<usize>> = HashMap::new();

        for feature_index in layer.features.range() {
            let feature = &doc.features[feature_index];
            if !is_copper_trace_feature(feature) || feature.polarity != Polarity::Dark {
                continue;
            }

            let paths = feature.paths.slice(&doc.arena.paths);
            if paths.is_empty() || !paths.iter().all(|path| path.is_filled()) {
                continue;
            }

            let Some(fill_rule) = common_fill_rule(paths) else {
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

            let rings = group
                .iter()
                .flat_map(|&feature_index| feature_rings(doc, &doc.features[feature_index]))
                .collect::<Vec<_>>();
            if rings.len() < 2 {
                continue;
            }

            let contours = region::rings_to_contours(region::union_rings(rings, key.fill_rule));
            if contours.is_empty() {
                continue;
            }

            replace_feature_with_path(
                doc,
                group[0],
                Paint::Fill {
                    rule: key.fill_rule,
                },
                contours,
            );
            for &feature_index in &group[1..] {
                clear_feature_paths(doc, feature_index);
            }
        }
    }
}

/// Resolve IPC set-void semantics: a feature flagged `clears_previous_in_set`
/// subtracts its filled image from earlier positive features of the same set.
pub fn resolve_set_voids<S, L>(doc: &mut Document<S, L>)
where
    S: Clone,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        for mut feature_indices in layer_features_by_set(doc, &layer).into_values() {
            feature_indices.sort_by_key(|&index| doc.features[index].source.feature_index);
            let mut previous = Vec::new();

            for feature_index in feature_indices {
                let feature = &doc.features[feature_index];
                if feature.bucket == FeatureBucket::Cutout {
                    continue;
                }

                if feature.flags.clears_previous_in_set {
                    let cutters = feature_filled_rings(doc, &doc.features[feature_index]);
                    if !cutters.is_empty() {
                        for subject_index in previous.iter().copied() {
                            subtract_rings_from_feature(doc, subject_index, &cutters);
                        }
                    }
                    clear_feature_paths(doc, feature_index);
                    continue;
                }

                if doc.features[feature_index].polarity == Polarity::Dark {
                    previous.push(feature_index);
                }
            }
        }
    }
}

fn layer_features_by_set<S, L>(
    doc: &Document<S, L>,
    layer: &Layer<S, L>,
) -> HashMap<u32, Vec<usize>> {
    let mut features_by_set = HashMap::new();
    for feature_index in layer.features.range() {
        features_by_set
            .entry(doc.features[feature_index].source.set_index)
            .or_insert_with(Vec::new)
            .push(feature_index);
    }
    features_by_set
}

/// Resolve negative (clear) polarity as a layer-wide subtraction.
pub fn resolve_negative_polarity<S, L>(doc: &mut Document<S, L>)
where
    S: Clone,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let negative_features = layer
            .features
            .range()
            .filter(|&feature_index| {
                let feature = &doc.features[feature_index];
                feature.bucket != FeatureBucket::Cutout
                    && !feature.flags.clears_previous_in_set
                    && feature.polarity == Polarity::Clear
            })
            .collect::<Vec<_>>();
        let mut cutters = negative_features
            .iter()
            .flat_map(|&feature_index| feature_filled_rings(doc, &doc.features[feature_index]))
            .collect::<Vec<_>>();
        if cutters.is_empty() {
            continue;
        }
        if cutters.len() > 1 {
            cutters = region::simplify_rings(cutters, FillRule::NonZero);
        }

        for feature_index in layer.features.range() {
            let feature = &doc.features[feature_index];
            if feature.bucket != FeatureBucket::Cutout && feature.polarity == Polarity::Dark {
                subtract_rings_from_feature(doc, feature_index, &cutters);
            }
        }

        for feature_index in negative_features {
            clear_feature_paths(doc, feature_index);
        }
    }
}

/// Subtract cutout features from every other feature on their layer.
pub fn subtract_layer_cutouts<S, L>(doc: &mut Document<S, L>)
where
    S: Clone,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let cutouts = layer_cutout_sets(doc, &layer);
        if cutouts.is_empty() {
            continue;
        }

        for feature_index in layer.features.range() {
            let feature = &doc.features[feature_index];
            if feature.bucket == FeatureBucket::Cutout {
                continue;
            }

            let feature_bbox = doc.arena.paths_bbox(feature.paths);
            if feature_bbox.is_empty() {
                continue;
            }

            let cutters = cutouts
                .iter()
                .filter(|cutout| feature_bbox.intersects(cutout.bbox))
                .flat_map(|cutout| cutout.rings.iter().cloned())
                .collect::<Vec<_>>();
            if cutters.is_empty() {
                continue;
            }

            subtract_rings_from_feature(doc, feature_index, &cutters);
        }
    }
}

/// Split a lowered-primitive feature into per-paint-kind runs so each run can
/// become a homogeneous feature.
pub fn split_primitive_feature_path_runs<S: Clone, L>(
    doc: &Document<S, L>,
    feature: Feature<S>,
) -> Result<Vec<Feature<S>>, String> {
    if feature.paths.end() as usize > doc.arena.paths.len() {
        return Err(format!(
            "feature paths range {}..{} exceeds available length {}",
            feature.paths.start,
            feature.paths.end(),
            doc.arena.paths.len()
        ));
    }
    let mut features = Vec::new();
    let mut run_start = feature.paths.start;
    let mut run_kind = None;

    for path_index in feature.paths.indices() {
        let kind = doc.arena.paths[path_index as usize].paint.kind();
        if Some(kind) == run_kind {
            continue;
        }

        if let Some(kind) = run_kind {
            push_primitive_path_run(&mut features, doc, &feature, run_start, path_index, kind);
        }
        run_start = path_index;
        run_kind = Some(kind);
    }

    if let Some(kind) = run_kind {
        push_primitive_path_run(
            &mut features,
            doc,
            &feature,
            run_start,
            feature.paths.end(),
            kind,
        );
    }

    Ok(features)
}

fn push_primitive_path_run<S: Clone, L>(
    features: &mut Vec<Feature<S>>,
    doc: &Document<S, L>,
    feature: &Feature<S>,
    run_start: u32,
    run_end: u32,
    kind: PaintKind,
) {
    if run_start == run_end {
        return;
    }
    let Some(bucket) = FeatureBucket::for_primitive_paint(kind) else {
        return;
    };
    let span = Span::new(run_start, run_end - run_start);
    features.push(feature.with_path_span(bucket, span, doc.arena.paths_bbox(span)));
}

fn is_copper_trace_feature<S>(feature: &Feature<S>) -> bool {
    feature.bucket == FeatureBucket::Trace
        && feature.intent.domain == crate::dialects::ipc::feature::FeatureDomain::Copper
}

fn common_fill_rule(paths: &[Path]) -> Option<FillRule> {
    let fill_rule = paths.first()?.fill_rule()?;
    paths
        .iter()
        .all(|path| path.fill_rule() == Some(fill_rule))
        .then_some(fill_rule)
}

fn copy_path<S, L>(doc: &mut Document<S, L>, path: Path) -> u32 {
    let contours = doc.arena.path_contours(&path);
    doc.arena.push_path(path.paint, contours)
}

fn replace_feature_with_path<S, L>(
    doc: &mut Document<S, L>,
    feature_index: usize,
    paint: Paint,
    contours: Vec<ContourBuf>,
) {
    let path_id = doc.arena.push_path(paint, contours);
    let feature = &mut doc.features[feature_index];
    feature.paths = Span::single(path_id);
    feature.primitive_ref = None;
}

fn clear_feature_paths<S, L>(doc: &mut Document<S, L>, feature_index: usize) {
    let feature = &mut doc.features[feature_index];
    feature.paths = Span::EMPTY;
    feature.primitive_ref = None;
}

fn subtract_rings_from_feature<S, L>(
    doc: &mut Document<S, L>,
    feature_index: usize,
    cutters: &[Ring],
) {
    let subject = feature_filled_rings(doc, &doc.features[feature_index]);
    if subject.is_empty() {
        return;
    }

    let contours = region::rings_to_contours(region::difference_rings(subject, cutters.to_vec()));
    if contours.is_empty() {
        clear_feature_paths(doc, feature_index);
        return;
    }

    replace_feature_with_path(
        doc,
        feature_index,
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        contours,
    );
}

fn layer_cutout_sets<S, L>(doc: &Document<S, L>, layer: &Layer<S, L>) -> Vec<ContourSet> {
    layer
        .features
        .slice(&doc.features)
        .iter()
        .filter(|feature| feature.bucket == FeatureBucket::Cutout)
        .filter_map(|feature| {
            let rings = feature_filled_rings(doc, feature);
            if rings.is_empty() {
                None
            } else {
                Some(ContourSet::new(rings, FillRule::NonZero, tol::REGION_MM))
            }
        })
        .collect()
}

/// The regularized filled image of a feature's fill paths, grouped by fill
/// rule before the final union.
fn feature_filled_rings<S, L>(doc: &Document<S, L>, feature: &Feature<S>) -> Vec<Ring> {
    let mut groups: HashMap<FillRule, Vec<Ring>> = HashMap::new();
    for path in feature.paths.slice(&doc.arena.paths) {
        if let Some(rule) = path.fill_rule() {
            groups
                .entry(rule)
                .or_default()
                .extend(path_rings(doc, path));
        }
    }

    let mut rings = groups
        .into_iter()
        .flat_map(|(fill_rule, rings)| region::simplify_rings(rings, fill_rule))
        .collect::<Vec<_>>();
    if rings.len() > 1 {
        rings = region::simplify_rings(rings, FillRule::NonZero);
    }
    rings
}

fn feature_rings<S, L>(doc: &Document<S, L>, feature: &Feature<S>) -> Vec<Ring> {
    feature
        .paths
        .slice(&doc.arena.paths)
        .iter()
        .flat_map(|path| path_rings(doc, path))
        .collect()
}

fn path_rings<S, L>(doc: &Document<S, L>, path: &Path) -> Vec<Ring> {
    region::rings_from_contours(&doc.arena.path_contours(path))
}

fn feature_set_bbox<S, L>(doc: &Document<S, L>, set_index: usize) -> BBox {
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
    let start = set.features.start as usize;
    let end = (set.features.end()).min(doc.features.len() as u32) as usize;
    if start >= end {
        return BBox::empty();
    }

    doc.features[start..end]
        .iter()
        .map(|feature| feature.bbox)
        .fold(BBox::empty(), BBox::union)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::ipc::feature::{
        FeatureDomain, FeatureMaterial, FeatureOperation, FeatureRole, SourceRef,
    };
    use crate::dialects::ipc::validate::validate_artwork_ready;
    use crate::geom::path::PathCmd;
    use crate::geom::{LineCap, Point, StrokeStyle};

    type TestDoc = Document<u32, ()>;

    #[test]
    fn composes_compatible_stroked_feature_paths() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(2.0, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(5.0, 0.0)),
            ])],
        );
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(2.0, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(5.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 2),
            ..copper_trace_feature()
        });

        compose_for_rendering(&mut doc);

        assert_eq!(doc.features[0].paths.len(), 1);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert!(path.is_filled());
        assert_eq!(path.bbox.min, Point::new(-1.0, -1.0));
        assert_eq!(path.bbox.max, Point::new(11.0, 1.0));
    }

    #[test]
    fn process_prunes_unpainted_feature_paths_and_preserves_profile_paths() {
        let mut doc = TestDoc::new();

        let painted_feature_path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        doc.push_path(Paint::None, [rect_contour(2.0, 2.0, 3.0, 3.0)]);
        doc.features.push(Feature {
            paths: Span::new(painted_feature_path, 2),
            ..Feature::new(FeatureKind::Padstack, Polarity::Dark)
        });
        doc.layers.push(test_layer(Span::new(0, 1)));

        let outer_profile_path = doc.push_path(
            Paint::None,
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
            ])],
        );
        let cutout_path = doc.push_path(
            Paint::None,
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
            ])],
        );
        doc.profile_cutouts
            .push(crate::dialects::ipc::layout::StepProfileCutout {
                path: cutout_path,
                bbox: BBox::empty(),
            });
        doc.profiles
            .push(crate::dialects::ipc::layout::StepProfile {
                outer_path: outer_profile_path,
                cutouts: Span::new(0, 1),
                bbox: BBox::empty(),
            });

        compose_for_rendering(&mut doc);

        let feature_paths = doc.features[0].paths.slice(&doc.arena.paths);
        assert_eq!(feature_paths.len(), 1);
        assert!(feature_paths[0].is_filled());

        let outer = doc.arena.path(doc.profiles[0].outer_path);
        let cutout = doc.arena.path(doc.profile_cutouts[0].path);
        assert_eq!(outer.paint, Paint::None);
        assert_eq!(cutout.paint, Paint::None);
    }

    #[test]
    fn coalesces_related_trace_features_inside_one_source_set() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 2.0, 1.0)],
        );
        doc.features.push(Feature {
            net: Some(1),
            source: SourceRef {
                set_index: 7,
                feature_index: 0,
            },
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(1.0, 0.0, 3.0, 1.0)],
        );
        doc.features.push(Feature {
            net: Some(1),
            source: SourceRef {
                set_index: 7,
                feature_index: 1,
            },
            paths: Span::new(1, 1),
            ..copper_trace_feature()
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(10.0, 0.0, 11.0, 1.0)],
        );
        doc.features.push(Feature {
            net: Some(1),
            source: SourceRef {
                set_index: 8,
                feature_index: 0,
            },
            paths: Span::new(2, 1),
            ..copper_trace_feature()
        });
        doc.layers.push(test_layer(Span::new(0, 3)));

        compose_for_rendering(&mut doc);

        assert_eq!(doc.features[0].paths.len(), 1);
        assert_eq!(doc.features[1].paths.len(), 0);
        assert_eq!(doc.features[2].paths.len(), 1);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert_eq!(path.contours.len(), 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
    }

    #[test]
    fn resolves_negative_polarity_as_layer_subtraction() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 4.0, 4.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..Feature::new(FeatureKind::Polygon, Polarity::Dark)
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(1.0, 1.0, 3.0, 3.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..Feature::new(FeatureKind::Polygon, Polarity::Clear)
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc);

        let feature = &doc.features[0];
        let path = &doc.arena.paths[feature.paths.start as usize];
        assert_eq!(feature.paths.len(), 1);
        assert!(path.contours.len() > 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(4.0, 4.0));
        assert_eq!(doc.features[1].paths.len(), 0);
        assert!(doc.features[1].bbox.is_empty());
    }

    #[test]
    fn subtracts_cutouts_after_trace_union() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(1.0, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 2.0)),
                PathCmd::line_to(Point::new(4.0, 2.0)),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(1.5, 1.0, 2.5, 3.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..Feature::new(FeatureKind::Slot, Polarity::Dark)
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc);

        let trace = &doc.features[0];
        let path = &doc.arena.paths[trace.paths.start as usize];
        assert!(path.is_filled());
        assert!(path.contours.len() >= 2);
        assert_eq!(path.bbox.min.x, -0.5);
        assert_eq!(path.bbox.max.x, 4.5);
    }

    #[test]
    fn splits_primitive_path_runs_by_paint_kind() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.2, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(2.0, 0.0)),
                PathCmd::line_to(Point::new(3.0, 0.0)),
            ])],
        );
        let mut feature = Feature::new(FeatureKind::Primitive, Polarity::Dark);
        feature.paths = Span::new(0, 2);
        feature.flags.lowered_to_paths = true;

        let features = split_primitive_feature_path_runs(&doc, feature).unwrap();

        assert_eq!(features.len(), 2);
        assert_eq!(features[0].bucket, FeatureBucket::Fill);
        assert_eq!(features[0].paths, Span::new(0, 1));
        assert_eq!(features[1].bucket, FeatureBucket::Trace);
        assert_eq!(features[1].paths, Span::new(1, 1));
    }

    #[test]
    fn artwork_ready_validation_rejects_mixed_feature_paint_kinds() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.2, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(2.0, 0.0)),
                PathCmd::line_to(Point::new(3.0, 0.0)),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 2),
            ..Feature::new(FeatureKind::Primitive, Polarity::Dark)
        });

        let error = validate_artwork_ready(&doc).unwrap_err();

        assert!(error.to_string().contains("mixes Fill and Stroke paths"));
    }

    #[test]
    fn artwork_ready_validation_rejects_unresolved_negative_polarity() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..Feature::new(FeatureKind::Polygon, Polarity::Clear)
        });

        let error = validate_artwork_ready(&doc).unwrap_err();

        assert!(error.to_string().contains("unresolved negative polarity"));
    }

    #[test]
    fn artwork_ready_validation_rejects_non_circular_arcs() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.2, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::arc_to(Point::new(0.0, 2.0), Point::new(0.0, 0.0), false),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });

        let error = validate_artwork_ready(&doc).unwrap_err();

        assert!(error.to_string().contains("non-circular arc radii"));
    }

    #[test]
    fn artwork_ready_validation_accepts_source_precision_arc_radius_noise() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(0.2, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.250024, 0.0)),
                PathCmd::arc_to(Point::new(0.0, 0.249977), Point::new(0.0, 0.0), false),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });

        validate_artwork_ready(&doc).unwrap();
    }

    #[test]
    fn flattens_processed_layer_features_to_single_mask() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 2.0, 1.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..Feature::new(FeatureKind::Padstack, Polarity::Dark)
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(1.0, 0.0, 3.0, 1.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..copper_trace_feature()
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc);
        flatten_layers_to_masks(&mut doc);

        assert_eq!(doc.features[0].kind, FeatureKind::FlattenedBucket);
        assert_eq!(doc.features[0].bucket, FeatureBucket::Fill);
        assert_eq!(doc.features[0].paths.len(), 1);
        assert_eq!(doc.features[1].paths.len(), 0);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert_eq!(path.contours.len(), 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
        assert_eq!(doc.layers[0].bbox.min, Point::new(0.0, 0.0));
        assert_eq!(doc.layers[0].bbox.max, Point::new(3.0, 1.0));
    }

    #[test]
    fn compact_reclaims_orphaned_paths() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 4.0, 4.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..Feature::new(FeatureKind::Polygon, Polarity::Dark)
        });
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(1.0, 1.0, 3.0, 3.0)],
        );
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..Feature::new(FeatureKind::Polygon, Polarity::Clear)
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc);

        // The negative feature and the pre-subtraction positive path are gone.
        assert_eq!(doc.arena.paths.len(), 1);
        assert_eq!(doc.features[0].paths, Span::new(0, 1));
        doc.arena.validate("compacted").unwrap();
    }

    fn test_layer(features: Span) -> Layer<u32, ()> {
        Layer {
            name: "F.Cu".to_string(),
            source_layer_ref: 100,
            layer_function: (),
            spec_refs: Span::EMPTY,
            sets: Span::EMPTY,
            features,
            bbox: BBox::empty(),
        }
    }

    fn copper_trace_feature() -> Feature<u32> {
        let mut feature = Feature::new(FeatureKind::Trace, Polarity::Dark);
        feature.intent.domain = FeatureDomain::Copper;
        feature.intent.role = FeatureRole::Conductor;
        feature.intent.operation = FeatureOperation::AddMaterial;
        feature.intent.material = FeatureMaterial::Copper;
        feature
    }

    fn rect_contour(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(x0, y0)),
            PathCmd::line_to(Point::new(x1, y0)),
            PathCmd::line_to(Point::new(x1, y1)),
            PathCmd::line_to(Point::new(x0, y1)),
            PathCmd::close(),
        ])
    }
}
