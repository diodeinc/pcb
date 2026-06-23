use crate::dialects::gerber::*;
use crate::dialects::path as common_path;

/// Resolve Gerber paint operations into a single composed image.
///
/// This is destructive and polygonizes strokes/regions. Use it for rendering,
/// comparison, and mask extraction, not for preserving original Gerber objects.
pub fn compose_for_rendering<A: Clone>(doc: &mut GeometryDocument<A>) {
    normalize_bounds(doc);
    expand_stroked_paths_to_fills(doc);
    resolve_polarity_and_cutouts(doc);
    normalize_bounds(doc);
}

pub fn normalize_bounds<A>(doc: &mut GeometryDocument<A>) {
    for contour_index in 0..doc.contours.len() {
        let contour = &doc.contours[contour_index];
        doc.contours[contour_index].bbox = common_path::contour_bbox(
            &doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize],
        );
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

pub fn expand_stroked_paths_to_fills<A: Clone>(doc: &mut GeometryDocument<A>) {
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
                if let Some(contours) = common_path::stroke_to_fill(
                    &path_payloads(doc, &path),
                    common_path::StrokeToFillStyle::new(
                        path.stroke_width,
                        path.line_cap,
                        LineJoin::Round,
                    ),
                ) {
                    let contours = common_path::polygon_contours_to_payloads(
                        common_path::payloads_to_polygon_contours(&contours),
                    );
                    doc.push_path(GeometryPath::filled(FillRule::NonZero), contours);
                }
            } else {
                doc.push_path(path.clone(), path_contours(doc, &path));
            }
        }
        let feature = &mut doc.features[feature_index];
        feature.path_start = path_start;
        feature.path_count = doc.paths.len() as u32 - path_start;
    }
}

pub fn resolve_polarity_and_cutouts<A>(doc: &mut GeometryDocument<A>) {
    let mut composer = common_path::PaintComposer::default();

    for feature in &doc.features {
        let feature_image = feature_image_contours(doc, feature);
        if feature_image.is_empty() {
            continue;
        }

        let op = if feature.polarity == Polarity::Clear || feature.bucket == FeatureBucket::Cutout {
            common_path::PaintOp::Clear
        } else {
            common_path::PaintOp::Dark
        };
        composer.push(op, feature_image);
    }
    let image = composer.finish();

    if image.is_empty() {
        clear_all_features(doc);
        return;
    }

    let path_id = doc.push_path(
        GeometryPath::filled(FillRule::NonZero),
        common_path::polygon_contours_to_payloads(image),
    );
    let mut composite =
        GeometryFeature::<A>::new(FeatureKind::Composite, FeatureBucket::Fill, Polarity::Dark);
    composite.path_start = path_id;
    composite.path_count = 1;
    composite.object_index = doc.features.len() as u32;
    clear_all_features(doc);
    doc.features.push(composite);
}

fn feature_image_contours<A>(
    doc: &GeometryDocument<A>,
    feature: &GeometryFeature<A>,
) -> Vec<common_path::PolygonContour> {
    let mut composer = common_path::PaintComposer::default();

    for path in
        &doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
    {
        if !path.flags.filled {
            continue;
        }
        let path_contours = common_path::payloads_to_polygon_contours(&path_payloads(doc, path));
        if path_contours.is_empty() {
            continue;
        }

        let op = if path.polarity == Polarity::Clear {
            common_path::PaintOp::Clear
        } else {
            common_path::PaintOp::Dark
        };
        composer.push(op, path_contours);
    }

    composer.finish()
}

fn clear_all_features<A>(doc: &mut GeometryDocument<A>) {
    let path_start = doc.paths.len() as u32;
    for feature in &mut doc.features {
        feature.path_count = 0;
        feature.path_start = path_start;
    }
}

fn path_contours<A>(doc: &GeometryDocument<A>, path: &GeometryPath) -> Vec<ContourPayload> {
    path_payloads(doc, path)
}

fn path_payloads<A>(
    doc: &GeometryDocument<A>,
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

fn path_bbox<A>(doc: &GeometryDocument<A>, path_index: usize) -> BBox {
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

fn feature_bbox<A>(doc: &GeometryDocument<A>, feature_index: usize) -> BBox {
    let feature = &doc.features[feature_index];
    doc.paths[feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        .iter()
        .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
}
