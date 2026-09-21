//! Copying a definition layer into a target document under one occurrence.

use super::*;

pub(super) fn source_layer_set_span(source: &GeometryDocument, layer_index: usize) -> Result<u32> {
    let layer = &source.layers[layer_index];
    let mut span = 0;
    for set in layer.sets.slice(&source.feature_sets) {
        let set_end = set
            .source_set_index
            .checked_add(1)
            .context("Source feature set index overflow")?;
        span = span.max(set_end);
    }
    Ok(span)
}

pub(super) fn append_span<T: Clone>(target: &mut Vec<T>, source: &[T], span: Span) -> Span {
    let start = target.len() as u32;
    target.extend(span.slice(source).iter().cloned());
    Span::new(start, span.count)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_transformed_layer(
    target: &mut GeometryDocument,
    source: &GeometryDocument,
    layer_index: usize,
    transform: Affine2,
    source_set_offset: u32,
    source_instance: Option<u32>,
    target_layer: u32,
) -> Result<BBox> {
    let layer = &source.layers[layer_index];
    let mut layer_bbox = BBox::empty();
    let mut placement_groups = HashMap::<u32, u32>::new();

    for source_set_index in layer.sets.indices() {
        let source_set = &source.feature_sets[source_set_index as usize];
        let spec_refs = append_span(
            &mut target.spec_refs,
            &source.spec_refs,
            source_set.spec_refs,
        );
        let target_set = target.feature_sets.len() as u32;
        target.feature_sets.push(FeatureSet {
            layer: target_layer,
            source_set_index: source_set
                .source_set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?,
            spec_refs,
            features: Span::new(target.features.len() as u32, 0),
            bbox: BBox::empty(),
            ..source_set.clone()
        });

        for feature in source_set.features.slice(&source.features) {
            let spec_refs =
                append_span(&mut target.spec_refs, &source.spec_refs, feature.spec_refs);
            let target_placement_group = if let Some(source_group_id) = feature.placement_group {
                if let Some(&target_group_id) = placement_groups.get(&source_group_id) {
                    Some(target_group_id)
                } else {
                    let source_group = &source.feature_placement_groups[source_group_id as usize];
                    let placement_start = target.feature_placements.len() as u32;
                    target.feature_placements.extend(
                        source_group
                            .placements
                            .slice(&source.feature_placements)
                            .iter()
                            .map(|&placement| transform.concat(placement)),
                    );
                    let target_group_id = target.feature_placement_groups.len() as u32;
                    target.feature_placement_groups.push(FeaturePlacementGroup {
                        placements: Span::new(
                            placement_start,
                            target.feature_placements.len() as u32 - placement_start,
                        ),
                        features: Span::new(
                            target.features.len() as u32,
                            source_group.features.count,
                        ),
                    });
                    placement_groups.insert(source_group_id, target_group_id);
                    Some(target_group_id)
                }
            } else {
                None
            };
            // Grouped geometry stays local; its placements carry the transform.
            let paths = target.arena.append_paths_from(
                &source.arena,
                feature.paths,
                if target_placement_group.is_some() {
                    Affine2::IDENTITY
                } else {
                    transform
                },
            );
            let bbox = if target_placement_group.is_some() {
                feature.bbox.transformed(transform)
            } else {
                target.arena.paths_bbox(paths)
            };

            let mut feature = feature.clone();
            feature.spec_refs = spec_refs;
            if target_placement_group.is_none() {
                feature.transform = transform.concat(feature.transform);
                feature.center = transform.transform_point(feature.center);
            }
            feature.bbox = bbox;
            feature.paths = paths;
            feature.placement_group = target_placement_group;
            feature.source_instance = source_instance;
            feature.source.set_index = feature
                .source
                .set_index
                .checked_add(source_set_offset)
                .context("Panel source feature set index overflow")?;
            feature.pin_refs =
                append_span(&mut target.pin_refs, &source.pin_refs, feature.pin_refs);
            target.push_feature(target_set, feature);
            layer_bbox = layer_bbox.union(bbox);
        }
    }

    Ok(layer_bbox)
}
