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

use crate::geom::{AccuracyError, GeometryAccuracy};
use std::collections::HashMap;
use std::hash::Hash;

use crate::dialects::ipc::Document;
use crate::dialects::ipc::document::Layer;
use crate::dialects::ipc::feature::{Feature, FeatureBucket, FeatureIntent, FeatureKind};
use crate::geom::path::ContourBuf;
use crate::geom::region::{self};
use crate::geom::{
    Affine2, BBox, ContourSet, FillRule, Paint, PaintKind, Path, Polarity, Span, tol,
};

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
/// IPC feature-set voids and layer cutouts are semantic operators on source
/// features, not generic ordered artwork objects. Resolve those before
/// lowering to source-independent artwork, but do not outline strokes or
/// flatten unrelated positive features. Negative-polarity features stay
/// native: ordered artwork carries per-object polarity with exactly IPC's
/// sequential paint semantics, so resolving them here would only flatten
/// repeated clear instances into unshareable boundary geometry.
pub fn normalize_for_artwork<S: Copy + Eq + Hash, L: Clone>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    normalize_preserving(doc);
    resolve_set_voids(doc, accuracy)?;
    subtract_layer_cutouts(doc, accuracy)?;
    compact(doc);
    normalize_bounds(doc);
    for contour in &doc.arena.contours {
        accuracy.check(contour.uncertainty_mm)?;
    }
    Ok(())
}

/// Resolve source geometry into a composed rendering image.
///
/// This is intentionally destructive: it outlines strokes, applies boolean
/// union/difference, resolves voids, and may convert arcs into polygon
/// contours. Negative polarity stays native — mask composition paints
/// polarity runs sequentially. Use it only when a target needs a final
/// painted image.
pub fn compose_for_rendering<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError>
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    expand_feature_placement_groups(doc, accuracy)?;
    normalize_preserving(doc);
    expand_stroked_paths_to_fills(doc, accuracy)?;
    union_feature_filled_paths(doc, accuracy)?;
    coalesce_related_trace_features(doc, accuracy)?;
    resolve_set_voids(doc, accuracy)?;
    subtract_layer_cutouts(doc, accuracy)?;
    compact(doc);
    normalize_bounds(doc);

    Ok(())
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
        doc.features[feature_index].bbox = doc.placed_paths_bbox(&doc.features[feature_index]);
    }

    // One pass over the features, not one scan per set: flattened panels
    // carry tens of thousands of sets and a per-set scan is quadratic.
    let mut linked_bboxes = vec![BBox::empty(); doc.feature_sets.len()];
    for feature in &doc.features {
        if let Some(set_id) = feature.set {
            let linked = &mut linked_bboxes[set_id as usize];
            *linked = linked.union(feature.bbox);
        }
    }
    for (set_index, linked_bbox) in linked_bboxes.into_iter().enumerate() {
        doc.feature_sets[set_index].bbox = if linked_bbox.is_empty() {
            feature_set_span_bbox(doc, set_index)
        } else {
            linked_bbox
        };
    }

    for layer_index in 0..doc.layers.len() {
        doc.layers[layer_index].bbox = doc.layers[layer_index]
            .features
            .slice(&doc.features)
            .iter()
            .fold(BBox::empty(), |bbox, feature| bbox.union(feature.bbox));
    }
}

/// Retain selected feature definitions while keeping every feature span and
/// placement-group reference valid.
///
/// Placement groups are preserved when the predicate keeps or drops the
/// complete local definition. A partially retained group is materialized
/// first because its remaining members no longer represent the source's
/// repeated ordered group.
pub fn retain_features<S: Clone, L>(
    doc: &mut Document<S, L>,
    mut retain: impl FnMut(&Feature<S>) -> bool,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    let mut retained = doc.features.iter().map(&mut retain).collect::<Vec<_>>();
    let splits_group = doc.feature_placement_groups.iter().any(|group| {
        let kept = group
            .features
            .indices()
            .filter(|&index| retained[index as usize])
            .count();
        kept != 0 && kept != group.features.len()
    });
    if splits_group {
        expand_feature_placement_groups(doc, accuracy)?;
        retained = doc.features.iter().map(&mut retain).collect();
    }
    if retained.iter().all(|&keep| keep) {
        return Ok(());
    }

    let mut retained_prefix = Vec::with_capacity(retained.len() + 1);
    retained_prefix.push(0_u32);
    for &keep in &retained {
        retained_prefix.push(retained_prefix.last().copied().unwrap() + u32::from(keep));
    }
    let remap_span = |span: Span| {
        let start = retained_prefix[span.start as usize];
        let end = retained_prefix[span.end() as usize];
        Span::new(start, end - start)
    };

    for layer in &mut doc.layers {
        layer.features = remap_span(layer.features);
    }
    for set in &mut doc.feature_sets {
        set.features = remap_span(set.features);
    }

    let old_groups = std::mem::take(&mut doc.feature_placement_groups);
    let old_placements = std::mem::take(&mut doc.feature_placements);
    let mut group_mapping = vec![None; old_groups.len()];
    for (old_id, group) in old_groups.into_iter().enumerate() {
        if group.features.is_empty() || !retained[group.features.start as usize] {
            continue;
        }
        debug_assert!(
            group
                .features
                .indices()
                .all(|index| retained[index as usize]),
            "partially retained placement groups must be materialized"
        );
        let placement_start = doc.feature_placements.len() as u32;
        doc.feature_placements
            .extend_from_slice(group.placements.slice(&old_placements));
        let new_id = doc.feature_placement_groups.len() as u32;
        group_mapping[old_id] = Some(new_id);
        doc.feature_placement_groups
            .push(crate::dialects::ipc::feature::FeaturePlacementGroup {
                placements: Span::new(
                    placement_start,
                    doc.feature_placements.len() as u32 - placement_start,
                ),
                features: remap_span(group.features),
            });
    }

    doc.features = std::mem::take(&mut doc.features)
        .into_iter()
        .zip(retained)
        .filter_map(|(mut feature, keep)| {
            keep.then(|| {
                feature.placement_group = feature
                    .placement_group
                    .map(|group| group_mapping[group as usize].expect("retained group is mapped"));
                feature
            })
        })
        .collect();
    compact(doc);
    normalize_bounds(doc);

    Ok(())
}

/// Materialize shared IPC feature placement groups only for passes that need
/// one independent feature image per occurrence. Structure-preserving
/// pipelines and artwork lowering keep the groups compact.
pub fn expand_feature_placement_groups<S: Clone, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    if doc.feature_placement_groups.is_empty() {
        return Ok(());
    }

    let mut old_features = std::mem::take(&mut doc.features)
        .into_iter()
        .map(Some)
        .collect::<Vec<_>>();
    let mut expanded = Vec::with_capacity(old_features.len());
    let mut mapping = vec![Span::EMPTY; old_features.len()];

    for feature_index in 0..old_features.len() {
        let Some(feature) = old_features[feature_index].take() else {
            continue;
        };
        let start = expanded.len() as u32;
        if let Some(group_id) = feature.placement_group {
            let group = doc.feature_placement_groups[group_id as usize];
            debug_assert_eq!(feature_index as u32, group.features.start);
            let mut members = Vec::with_capacity(group.features.len());
            members.push(feature);
            for member_index in group.features.indices().skip(1) {
                members.push(
                    old_features[member_index as usize]
                        .take()
                        .expect("placement group member is consumed once"),
                );
            }
            for (instances, placement_index) in
                std::iter::repeat_n(members, group.placements.len()).zip(group.placements.indices())
            {
                let placement = doc.feature_placements[placement_index as usize];
                for mut instance in instances {
                    instance.source_placement = Some(
                        instance
                            .source_placement
                            .map(|source_start| {
                                source_start + placement_index - group.placements.start
                            })
                            .unwrap_or(placement_index),
                    );
                    materialize_feature_placement(doc, &mut instance, placement, accuracy)?;
                    expanded.push(instance);
                }
            }
            let span = Span::new(start, expanded.len() as u32 - start);
            for member_index in group.features.indices() {
                mapping[member_index as usize] = span;
            }
        } else {
            expanded.push(feature);
            mapping[feature_index] = Span::single(start);
        }
    }

    for layer in &mut doc.layers {
        layer.features = expanded_span(layer.features, &mapping);
    }
    for set in &mut doc.feature_sets {
        set.features = expanded_span(set.features, &mapping);
    }
    doc.features = expanded;
    doc.feature_placement_groups.clear();
    doc.feature_placements.clear();

    Ok(())
}

fn materialize_feature_placement<S, L>(
    doc: &mut Document<S, L>,
    feature: &mut Feature<S>,
    placement: Affine2,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    let scale = placement.m00.hypot(placement.m10);
    let path_start = doc.arena.paths.len() as u32;
    for path_index in feature.paths.indices() {
        let path = doc.arena.paths[path_index as usize];
        let contours = doc
            .arena
            .transformed_contour_bufs(path.contours, placement, accuracy)?;
        doc.arena.push_path(path.paint.scaled(scale), contours);
    }
    let paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);
    feature.placement_group = None;
    feature.transform = placement.concat(feature.transform);
    feature.center = placement.transform_point(feature.center);
    feature.paths = paths;
    // Absolute image sizes follow the placement; width/height/radius stay in
    // the local frame consumers map through `transform`.
    feature.stroke_width *= scale;
    feature.outer_diameter *= scale;
    feature.inner_diameter *= scale;
    feature.bbox = doc.arena.paths_bbox(paths);

    Ok(())
}

fn expanded_span(span: Span, mapping: &[Span]) -> Span {
    if span.is_empty() {
        return Span::EMPTY;
    }
    let first = mapping[span.start as usize];
    let last = mapping[span.end() as usize - 1];
    Span::new(first.start, last.end() - first.start)
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

/// Flatten every layer's ordered paint into one unioned fill mask.
///
/// Each layer is lowered to artwork and composed with the same machinery as
/// rendering and Gerber export, so strokes, flashes, and polarity sequencing
/// flatten exactly as they manufacture instead of through a second
/// composition implementation.
pub fn flatten_layers_to_masks<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError>
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    for layer_index in 0..doc.layers.len() {
        let features = doc.layers[layer_index].features;
        if features.is_empty() {
            continue;
        }

        let artwork = super::lower_layer_to_artwork(
            doc,
            layer_index,
            crate::dialects::LayerRole::Other,
            crate::dialects::Side::None,
            accuracy,
        )?;
        let mask = crate::dialects::artwork::compose_to_mask(&artwork, accuracy)?;
        let contours = mask
            .layers
            .first()
            .map(|mask_layer| {
                mask.shapes(mask_layer)
                    .iter()
                    .flat_map(|shape| mask.arena.path_contours(shape))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for feature_index in features.range() {
            clear_feature_paths(doc, feature_index);
        }

        if contours.is_empty() {
            continue;
        }

        let mask_index = features.start as usize;
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

    // Composed masks are already in layer coordinates, so their source
    // placement groups must not be applied again by later consumers.
    for feature in &mut doc.features {
        feature.placement_group = None;
    }
    doc.feature_placement_groups.clear();
    doc.feature_placements.clear();

    compact(doc);
    normalize_bounds(doc);

    Ok(())
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
pub fn expand_stroked_paths_to_fills<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    let _: () = for feature_index in 0..doc.features.len() {
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
                        accuracy,
                    )? {
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
    };
    Ok(())
}

/// Union a trace feature's filled paths into one region.
pub fn union_feature_filled_paths<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    let _: () = for feature_index in 0..doc.features.len() {
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

        let image = feature_filled_region(doc, &doc.features[feature_index], 0.0, accuracy)?;
        let contours = image.to_contours();
        if contours.is_empty() {
            continue;
        }

        replace_feature_with_path(
            doc,
            feature_index,
            Paint::Fill { rule: fill_rule },
            contours,
        );
    };
    Ok(())
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
pub fn coalesce_related_trace_features<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError>
where
    S: Copy + Eq + Hash,
    L: Clone,
{
    let _: () = for layer_index in 0..doc.layers.len() {
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

            let mut composer = region::PaintComposer::default();
            for &feature_index in &group {
                composer.push(
                    Polarity::Dark,
                    feature_filled_region(doc, &doc.features[feature_index], 0.0, accuracy)?,
                );
            }
            let contours = composer.finish(0.0).to_contours();
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
    };
    Ok(())
}

/// Resolve IPC set-void semantics: a feature flagged `clears_previous_in_set`
/// subtracts its filled image from earlier positive features of the same set.
pub fn resolve_set_voids<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError>
where
    S: Clone,
    L: Clone,
{
    // Void subtraction images features in layer coordinates, so shared
    // placement groups must be materialized before any void can cut.
    if !doc.feature_placement_groups.is_empty()
        && doc
            .features
            .iter()
            .any(|feature| feature.flags.clears_previous_in_set)
    {
        expand_feature_placement_groups(doc, accuracy)?;
    }
    let _: () = for layer_index in 0..doc.layers.len() {
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
                    let cutters =
                        feature_filled_region(doc, &doc.features[feature_index], 0.0, accuracy)?;
                    if !cutters.is_empty() {
                        for subject_index in previous.iter().copied() {
                            subtract_region_from_feature(doc, subject_index, &cutters, accuracy)?;
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
    };
    Ok(())
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

/// Subtract cutout features from every other feature on their layer.
pub fn subtract_layer_cutouts<S, L>(
    doc: &mut Document<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError>
where
    S: Clone,
    L: Clone,
{
    // Cutout subtraction images features in layer coordinates, so shared
    // placement groups must be materialized before any cutout can cut.
    if !doc.feature_placement_groups.is_empty()
        && doc
            .features
            .iter()
            .any(|feature| feature.bucket == FeatureBucket::Cutout)
    {
        expand_feature_placement_groups(doc, accuracy)?;
    }
    let _: () = for layer_index in 0..doc.layers.len() {
        let layer = doc.layers[layer_index].clone();
        let cutouts = layer_cutout_sets(doc, &layer, accuracy)?;
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

            let mut composer = region::PaintComposer::default();
            for cutout in cutouts
                .iter()
                .filter(|cutout| feature_bbox.intersects(cutout.bbox))
            {
                composer.push(Polarity::Dark, cutout.clone());
            }
            let cutters = composer.finish(0.0);
            if cutters.is_empty() {
                continue;
            }
            subtract_region_from_feature(doc, feature_index, &cutters, accuracy)?;
        }
    };
    Ok(())
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

    // A fragment of a dictionary entry is not the entry: only a feature
    // carrying the entry's entire geometry keeps its primitive identity.
    if features.len() != 1 || features[0].paths != feature.paths {
        for fragment in &mut features {
            fragment.primitive_ref = None;
        }
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

fn subtract_region_from_feature<S, L>(
    doc: &mut Document<S, L>,
    feature_index: usize,
    cutters: &ContourSet,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    let subject = feature_filled_region(doc, &doc.features[feature_index], 0.0, accuracy)?;
    if subject.is_empty() {
        return Ok(());
    }

    // Only cutters that can reach this feature participate; most features on
    // a layer are nowhere near any of them, and dense generated cutter sets
    // (balance void lattices) would otherwise make every subtraction sweep
    // the whole set.
    let near = cutters
        .rings
        .iter()
        .zip(&cutters.ring_bounds)
        .filter(|(_, bounds)| bounds.intersects(subject.bbox))
        .map(|(ring, _)| ring.clone())
        .collect::<Vec<_>>();
    if near.is_empty() {
        return Ok(());
    }

    let near = ContourSet::from_regularized(near, 0.0, cutters.uncertainty_mm);
    let contours = subject.difference(&near).to_contours();
    if contours.is_empty() {
        clear_feature_paths(doc, feature_index);
        return Ok(());
    }

    replace_feature_with_path(
        doc,
        feature_index,
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        contours,
    );

    Ok(())
}

fn layer_cutout_sets<S, L>(
    doc: &Document<S, L>,
    layer: &Layer<S, L>,
    accuracy: GeometryAccuracy,
) -> Result<Vec<ContourSet>, AccuracyError> {
    Ok(layer
        .features
        .slice(&doc.features)
        .iter()
        .filter(|feature| feature.bucket == FeatureBucket::Cutout)
        .map(|feature| feature_filled_region(doc, feature, tol::REGION_MM, accuracy))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|region| !region.is_empty())
        .collect())
}

/// The regularized filled image of a feature's fill paths, grouped by fill
/// rule before the final union.
fn feature_filled_region<S, L>(
    doc: &Document<S, L>,
    feature: &Feature<S>,
    tolerance: f64,
    accuracy: GeometryAccuracy,
) -> Result<ContourSet, AccuracyError> {
    let mut groups: HashMap<FillRule, Vec<ContourBuf>> = HashMap::new();
    for path in feature.paths.slice(&doc.arena.paths) {
        if let Some(rule) = path.fill_rule() {
            groups
                .entry(rule)
                .or_default()
                .extend(doc.arena.path_contours(path));
        }
    }

    let mut composer = region::PaintComposer::default();
    for (fill_rule, contours) in groups {
        composer.push(
            Polarity::Dark,
            ContourSet::from_contours(&contours, fill_rule, 0.0, accuracy)?,
        );
    }
    Ok(composer.finish(tolerance))
}

/// Bounds of a set's own feature span, for sets with no linked features.
fn feature_set_span_bbox<S, L>(doc: &Document<S, L>, set_index: usize) -> BBox {
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
        FeatureDomain, FeatureMaterial, FeatureOperation, FeaturePlacementGroup, FeatureRole,
        FeatureSet, PrimitiveRef, SourceRef,
    };
    use crate::dialects::ipc::validate::validate_artwork_ready;
    use crate::geom::path::PathCmd;
    use crate::geom::{LineCap, Point, StrokeStyle};

    type TestDoc = Document<u32, ()>;

    #[test]
    fn composes_compatible_stroked_feature_paths() {
        let accuracy = GeometryAccuracy::default();

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

        compose_for_rendering(&mut doc, accuracy).unwrap();

        assert_eq!(doc.features[0].paths.len(), 1);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert!(path.is_filled());
        assert_eq!(path.bbox.min, Point::new(-1.0, -1.0));
        assert_eq!(path.bbox.max, Point::new(11.0, 1.0));
    }

    #[test]
    fn process_prunes_unpainted_feature_paths_and_preserves_profile_paths() {
        let accuracy = GeometryAccuracy::default();

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

        compose_for_rendering(&mut doc, accuracy).unwrap();

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
        let accuracy = GeometryAccuracy::default();

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
                definition: None,
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
                definition: None,
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
                definition: None,
            },
            paths: Span::new(2, 1),
            ..copper_trace_feature()
        });
        doc.layers.push(test_layer(Span::new(0, 3)));

        compose_for_rendering(&mut doc, accuracy).unwrap();

        assert_eq!(doc.features[0].paths.len(), 1);
        assert_eq!(doc.features[1].paths.len(), 0);
        assert_eq!(doc.features[2].paths.len(), 1);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert_eq!(path.contours.len(), 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(3.0, 1.0));
    }

    #[test]
    fn compose_keeps_clear_polarity_native() {
        let accuracy = GeometryAccuracy::default();

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

        compose_for_rendering(&mut doc, accuracy).unwrap();

        // Both features keep their native geometry and polarity; the
        // sequential paint fold applies the subtraction at composition.
        assert_eq!(doc.features[0].paths.len(), 1);
        assert_eq!(doc.features[0].polarity, Polarity::Dark);
        assert_eq!(doc.features[1].paths.len(), 1);
        assert_eq!(doc.features[1].polarity, Polarity::Clear);

        let artwork = crate::dialects::ipc::lower_layer_to_artwork(
            &doc,
            0,
            crate::dialects::LayerRole::Copper,
            crate::dialects::Side::Top,
            accuracy,
        )
        .unwrap();
        let mask = crate::dialects::artwork::compose_to_mask(&artwork, accuracy).unwrap();
        let shape = mask.layers[0].shapes.slice(&mask.arena.paths)[0];
        let image = crate::geom::region::ContourSet::from_contours(
            &mask.arena.path_contours(&shape),
            FillRule::NonZero,
            crate::geom::tol::REGION_MM,
            accuracy,
        )
        .unwrap();
        assert!((image.area() - 12.0).abs() < 1e-6);
        assert!(!image.contains_point(Point::new(2.0, 2.0)));
    }

    #[test]
    fn subtracts_cutouts_after_trace_union() {
        let accuracy = GeometryAccuracy::default();

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

        compose_for_rendering(&mut doc, accuracy).unwrap();

        let trace = &doc.features[0];
        let path = &doc.arena.paths[trace.paths.start as usize];
        assert!(path.is_filled());
        assert!(path.contours.len() >= 2);
        assert_eq!(path.bbox.min.x, -0.5);
        assert_eq!(path.bbox.max.x, 4.5);
    }

    #[test]
    fn splits_primitive_path_runs_by_paint_kind() {
        let _accuracy = GeometryAccuracy::default();

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
        let _accuracy = GeometryAccuracy::default();

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
    fn artwork_ready_validation_accepts_clear_polarity() {
        let _accuracy = GeometryAccuracy::default();

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

        validate_artwork_ready(&doc).unwrap();
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
        let accuracy = GeometryAccuracy::default();

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

        compose_for_rendering(&mut doc, accuracy).unwrap();
        flatten_layers_to_masks(&mut doc, accuracy).unwrap();

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
    fn flattening_expands_strokes_that_composition_left_unexpanded() {
        let accuracy = GeometryAccuracy::default();

        // Only copper-trace features expand strokes during composition;
        // primitive strokes reach the flattener as strokes and must still
        // contribute their swept copper to the mask.
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Stroke(StrokeStyle::new(1.0, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(5.0, 0.0)),
            ])],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..Feature::new(FeatureKind::Primitive, Polarity::Dark)
        });
        doc.layers.push(test_layer(Span::new(0, 1)));

        compose_for_rendering(&mut doc, accuracy).unwrap();
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert!(
            path.stroke().is_some(),
            "precondition: stroke survives composition"
        );

        flatten_layers_to_masks(&mut doc, accuracy).unwrap();

        assert_eq!(doc.features[0].kind, FeatureKind::FlattenedBucket);
        assert_eq!(doc.features[0].paths.len(), 1);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        assert!(path.is_filled());
        assert_eq!(path.bbox.min, Point::new(-0.5, -0.5));
        assert_eq!(path.bbox.max, Point::new(5.5, 0.5));
    }

    #[test]
    fn flattening_keeps_layer_cutouts_clear() {
        let accuracy = GeometryAccuracy::default();

        // Cutout features keep their dark-drawn geometry after composition;
        // flattening must subtract it, not union it back over the clearance.
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
        let mut cutout = Feature::new(FeatureKind::Polygon, Polarity::Dark);
        cutout.bucket = FeatureBucket::Cutout;
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..cutout
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc, accuracy).unwrap();
        flatten_layers_to_masks(&mut doc, accuracy).unwrap();

        assert_eq!(doc.features[0].kind, FeatureKind::FlattenedBucket);
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        let image = ContourSet::from_contours(
            &doc.arena.path_contours(path),
            FillRule::NonZero,
            tol::REGION_MM,
            accuracy,
        )
        .unwrap();
        assert!(image.contains_point(Point::new(0.5, 0.5)));
        assert!(!image.contains_point(Point::new(2.0, 2.0)));
    }

    #[test]
    fn flattening_consumes_feature_placement_groups() {
        let accuracy = GeometryAccuracy::default();

        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        let mut feature = Feature::new(FeatureKind::Polygon, Polarity::Dark);
        feature.paths = Span::single(0);
        feature.placement_group = Some(0);
        doc.features.push(feature);
        doc.feature_placements.extend([
            Affine2::translation(Point::new(10.0, 0.0)),
            Affine2::translation(Point::new(20.0, 0.0)),
        ]);
        doc.feature_placement_groups.push(FeaturePlacementGroup {
            placements: Span::new(0, 2),
            features: Span::single(0),
        });
        doc.layers.push(test_layer(Span::single(0)));

        flatten_layers_to_masks(&mut doc, accuracy).unwrap();

        assert!(doc.feature_placement_groups.is_empty());
        assert!(doc.feature_placements.is_empty());
        assert_eq!(doc.features[0].placement_group, None);
        assert_eq!(doc.layers[0].bbox.min, Point::new(10.0, 0.0));
        assert_eq!(doc.layers[0].bbox.max, Point::new(21.0, 1.0));
        let path = &doc.arena.paths[doc.features[0].paths.start as usize];
        let image = ContourSet::from_contours(
            &doc.arena.path_contours(path),
            FillRule::NonZero,
            tol::REGION_MM,
            accuracy,
        )
        .unwrap();
        assert!(image.contains_point(Point::new(10.5, 0.5)));
        assert!(image.contains_point(Point::new(20.5, 0.5)));
        assert!(!image.contains_point(Point::new(30.5, 0.5)));
    }

    #[test]
    fn compact_reclaims_orphaned_paths() {
        let accuracy = GeometryAccuracy::default();

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
            flags: crate::dialects::ipc::FeatureFlags {
                clears_previous_in_set: true,
                ..Default::default()
            },
            ..Feature::new(FeatureKind::Polygon, Polarity::Clear)
        });
        doc.layers.push(test_layer(Span::new(0, 2)));

        compose_for_rendering(&mut doc, accuracy).unwrap();

        // The set void and the pre-subtraction positive path are gone.
        assert_eq!(doc.arena.paths.len(), 1);
        assert_eq!(doc.features[0].paths, Span::new(0, 1));
        doc.arena.validate("compacted").unwrap();
    }

    #[test]
    fn retaining_features_remaps_placement_and_owner_spans() {
        let accuracy = GeometryAccuracy::default();

        let mut doc = TestDoc::new();
        for (x0, x1) in [(0.0, 1.0), (0.0, 1.0), (2.0, 3.0)] {
            doc.push_path(
                Paint::Fill {
                    rule: FillRule::NonZero,
                },
                [rect_contour(x0, 0.0, x1, 1.0)],
            );
        }
        let mut borrowed = Feature::new(FeatureKind::Polygon, Polarity::Dark);
        borrowed.paths = Span::single(0);
        borrowed.source_layer_ref = Some(200);
        doc.features.push(borrowed);
        for path in [1, 2] {
            let mut member = Feature::new(FeatureKind::Polygon, Polarity::Dark);
            member.paths = Span::single(path);
            member.source_layer_ref = Some(100);
            member.placement_group = Some(0);
            doc.features.push(member);
        }
        doc.feature_placements.extend([
            Affine2::translation(Point::new(10.0, 0.0)),
            Affine2::translation(Point::new(20.0, 0.0)),
        ]);
        doc.feature_placement_groups.push(FeaturePlacementGroup {
            placements: Span::new(0, 2),
            features: Span::new(1, 2),
        });
        doc.feature_sets.push(FeatureSet {
            layer: 0,
            source_set_index: 0,
            source_geometry_ref: None,
            component_ref: None,
            geometry_usage: None,
            net: None,
            polarity: Polarity::Dark,
            spec_refs: Span::EMPTY,
            features: Span::single(0),
            bbox: BBox::empty(),
        });
        doc.feature_sets.push(FeatureSet {
            layer: 0,
            source_set_index: 1,
            source_geometry_ref: None,
            component_ref: None,
            geometry_usage: None,
            net: None,
            polarity: Polarity::Dark,
            spec_refs: Span::EMPTY,
            features: Span::new(1, 2),
            bbox: BBox::empty(),
        });
        doc.layers.push(test_layer(Span::new(0, 3)));

        retain_features(
            &mut doc,
            |feature| feature.source_layer_ref == Some(100),
            accuracy,
        )
        .unwrap();

        assert_eq!(doc.features.len(), 2);
        assert_eq!(doc.layers[0].features, Span::new(0, 2));
        assert_eq!(doc.feature_sets[0].features, Span::EMPTY);
        assert_eq!(doc.feature_sets[1].features, Span::new(0, 2));
        assert_eq!(doc.feature_placement_groups.len(), 1);
        assert_eq!(doc.feature_placement_groups[0].features, Span::new(0, 2));
        assert!(
            doc.features
                .iter()
                .all(|feature| feature.placement_group == Some(0))
        );

        expand_feature_placement_groups(&mut doc, accuracy).unwrap();

        assert_eq!(doc.features.len(), 4);
        assert_eq!(doc.layers[0].features, Span::new(0, 4));
        assert_eq!(doc.feature_sets[1].features, Span::new(0, 4));
        assert!(doc.feature_placement_groups.is_empty());
        assert_eq!(
            doc.features
                .iter()
                .map(|feature| feature.bbox.min.x)
                .collect::<Vec<_>>(),
            [10.0, 12.0, 20.0, 22.0]
        );
    }

    #[test]
    fn normalization_refines_curves_for_intersecting_cutouts() {
        let mut doc = TestDoc::new();
        let paint = Paint::Fill {
            rule: FillRule::NonZero,
        };
        doc.push_path(paint, [crate::geom::shapes::circle(2.0).unwrap()]);
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });
        doc.push_path(paint, [rect_contour(0.0, -3.0, 3.0, 3.0)]);
        doc.features.push(Feature {
            paths: Span::new(1, 1),
            ..Feature::new(FeatureKind::Slot, Polarity::Dark)
        });
        doc.layers.push(test_layer(Span::new(0, 2)));
        normalize_for_artwork(&mut doc, crate::geom::GeometryAccuracy::new(0.001).unwrap())
            .unwrap();
        let image = feature_filled_region(
            &doc,
            &doc.features[0],
            0.0,
            GeometryAccuracy::new(0.001).unwrap(),
        )
        .unwrap();
        assert!(!image.is_empty());
        assert!(image.uncertainty_mm <= 0.001);
        assert!(image.contains_point(Point::new(-0.5, 0.0)));
        assert!(!image.contains_point(Point::new(0.5, 0.0)));
    }

    #[test]
    fn normalization_accepts_inherited_error_within_the_total_budget() {
        let mut doc = TestDoc::new();
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0).with_uncertainty(0.008)],
        );
        doc.features.push(Feature {
            paths: Span::new(0, 1),
            ..copper_trace_feature()
        });
        doc.layers.push(test_layer(Span::new(0, 1)));
        normalize_for_artwork(&mut doc, crate::geom::GeometryAccuracy::new(0.01).unwrap()).unwrap();
        assert!(!doc.arena.contours.is_empty());
        assert!(
            doc.arena
                .contours
                .iter()
                .all(|contour| (0.008..=0.01).contains(&contour.uncertainty_mm))
        );
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

    #[test]
    fn split_path_runs_keep_primitive_identity_only_for_whole_entries() {
        let _accuracy = GeometryAccuracy::default();

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
                PathCmd::move_to(Point::new(0.0, 2.0)),
                PathCmd::line_to(Point::new(1.0, 2.0)),
            ])],
        );

        let mut feature = Feature::new(FeatureKind::Primitive, Polarity::Dark);
        feature.primitive_ref = Some(PrimitiveRef::User(7));
        feature.paths = Span::new(0, 2);

        let fragments = split_primitive_feature_path_runs(&doc, feature.clone()).unwrap();
        assert_eq!(fragments.len(), 2);
        assert!(
            fragments
                .iter()
                .all(|fragment| fragment.primitive_ref.is_none()),
            "fragments must not claim the dictionary entry's identity"
        );

        feature.paths = Span::new(0, 1);
        let whole = split_primitive_feature_path_runs(&doc, feature).unwrap();
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].primitive_ref, Some(PrimitiveRef::User(7)));
    }
}
