use crate::dialects::gerber::*;
use crate::dialects::path as common_path;

pub fn process_document<A: Clone>(doc: &mut GeometryDocument<A>) {
    normalize_bounds(doc);
    outline_stroked_paths(doc);
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

pub fn outline_stroked_paths<A: Clone>(doc: &mut GeometryDocument<A>) {
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
                if let Some(contours) = common_path::outline_stroke(
                    &path_payloads(doc, &path),
                    path.stroke_width,
                    path.line_cap,
                    LineJoin::Round,
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
    let mut image = Vec::new();
    for feature in &doc.features {
        let feature_image = feature_image_contours(doc, feature);
        if feature_image.is_empty() {
            continue;
        }
        if feature.polarity == Polarity::Clear || feature.bucket == FeatureBucket::Cutout {
            image = common_path::difference_contours(image, feature_image);
        } else {
            image = common_path::union_contours(
                image.into_iter().chain(feature_image).collect(),
                FillRule::NonZero,
            );
        }
    }
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
    let mut image = Vec::new();
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
        if path.polarity == Polarity::Clear {
            image = common_path::difference_contours(
                image,
                common_path::union_contours(path_contours, FillRule::NonZero),
            );
        } else {
            image = common_path::union_contours(
                image.into_iter().chain(path_contours).collect(),
                FillRule::NonZero,
            );
        }
    }
    image
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
