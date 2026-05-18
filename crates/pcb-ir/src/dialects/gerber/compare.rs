use crate::dialects::gerber::*;
use crate::dialects::path as common_path;

/// Tolerances for comparing two processed Gerber geometry documents.
///
/// The comparison is intended for smoke tests where two different export paths
/// should describe the same layer image but are not expected to produce bytewise
/// identical Gerber.
#[derive(Debug, Clone, Copy)]
pub struct GeometryCompareTolerance {
    pub bbox_mm: f64,
    pub area_mm2: f64,
}

impl Default for GeometryCompareTolerance {
    fn default() -> Self {
        Self {
            bbox_mm: 0.01,
            area_mm2: 0.01,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryCompareReport {
    pub reference: GeometrySummary,
    pub candidate: GeometrySummary,
    pub mismatches: Vec<String>,
}

impl GeometryCompareReport {
    pub fn is_match(&self) -> bool {
        self.mismatches.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometrySummary {
    pub file_function: Vec<String>,
    pub bbox: BBox,
    pub area_mm2: f64,
    pub feature_count: usize,
    pub path_count: usize,
}

/// Compare two processed layer geometries using manufacturing-relevant summary
/// metrics.
///
/// Call `process::process_document` on both inputs first. This helper assumes
/// strokes and clear polarity have already been resolved into the final layer
/// image, then compares the final image bounds and filled area. It intentionally
/// does not compare object counts or command streams, because idiomatic exports
/// may use different apertures/regions while preserving geometry.
pub fn compare_documents<A, B>(
    reference: &GeometryDocument<A>,
    candidate: &GeometryDocument<B>,
    tolerance: GeometryCompareTolerance,
) -> GeometryCompareReport {
    let reference_summary = summarize(reference);
    let candidate_summary = summarize(candidate);
    let mut mismatches = Vec::new();

    if reference_summary.file_function != candidate_summary.file_function {
        mismatches.push(format!(
            "file function differs: reference={:?}, candidate={:?}",
            reference_summary.file_function, candidate_summary.file_function
        ));
    }

    compare_bbox(
        "bbox",
        reference_summary.bbox,
        candidate_summary.bbox,
        tolerance.bbox_mm,
        &mut mismatches,
    );

    let area_delta = (reference_summary.area_mm2 - candidate_summary.area_mm2).abs();
    if area_delta > tolerance.area_mm2 {
        mismatches.push(format!(
            "filled area differs by {area_delta:.6} mm²: reference={:.6}, candidate={:.6}, tolerance={:.6}",
            reference_summary.area_mm2, candidate_summary.area_mm2, tolerance.area_mm2
        ));
    }

    let symmetric_difference_area = symmetric_difference_area(reference, candidate);
    if symmetric_difference_area > tolerance.area_mm2 {
        mismatches.push(format!(
            "symmetric difference area is {symmetric_difference_area:.6} mm², tolerance={:.6}",
            tolerance.area_mm2
        ));
    }

    GeometryCompareReport {
        reference: reference_summary,
        candidate: candidate_summary,
        mismatches,
    }
}

pub fn summarize<A>(doc: &GeometryDocument<A>) -> GeometrySummary {
    GeometrySummary {
        file_function: doc.file_function.clone(),
        bbox: doc.bbox,
        area_mm2: filled_area(doc),
        feature_count: doc.features.len(),
        path_count: doc.paths.len(),
    }
}

fn compare_bbox(
    label: &str,
    reference: BBox,
    candidate: BBox,
    tolerance: f64,
    mismatches: &mut Vec<String>,
) {
    if reference.is_empty() || candidate.is_empty() {
        if reference.is_empty() != candidate.is_empty() {
            mismatches.push(format!(
                "{label} emptiness differs: reference_empty={}, candidate_empty={}",
                reference.is_empty(),
                candidate.is_empty()
            ));
        }
        return;
    }

    for (name, reference, candidate) in [
        ("min.x", reference.min.x, candidate.min.x),
        ("min.y", reference.min.y, candidate.min.y),
        ("max.x", reference.max.x, candidate.max.x),
        ("max.y", reference.max.y, candidate.max.y),
    ] {
        let delta = (reference - candidate).abs();
        if delta > tolerance {
            mismatches.push(format!(
                "{label}.{name} differs by {delta:.6} mm: reference={reference:.6}, candidate={candidate:.6}, tolerance={tolerance:.6}"
            ));
        }
    }
}

fn filled_area<A>(doc: &GeometryDocument<A>) -> f64 {
    polygon_area(&document_image_contours(doc))
}

fn symmetric_difference_area<A, B>(
    reference: &GeometryDocument<A>,
    candidate: &GeometryDocument<B>,
) -> f64 {
    let reference = document_image_contours(reference);
    let candidate = document_image_contours(candidate);
    polygon_area(&common_path::difference_contours(
        reference.clone(),
        candidate.clone(),
    )) + polygon_area(&common_path::difference_contours(candidate, reference))
}

fn document_image_contours<A>(doc: &GeometryDocument<A>) -> Vec<common_path::PolygonContour> {
    let mut contours = Vec::new();
    for feature in &doc.features {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            if !path.flags.filled {
                continue;
            }
            for contour in &doc.contours
                [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
            {
                contours.extend(common_path::payloads_to_polygon_contours(&[
                    common_path::PathPayload {
                        bbox: contour.bbox,
                        cmds: doc.path_cmds[contour.cmd_start as usize
                            ..(contour.cmd_start + contour.cmd_count) as usize]
                            .to_vec(),
                    },
                ]));
            }
        }
    }
    common_path::union_contours(contours, FillRule::NonZero)
}

fn polygon_area(contours: &[common_path::PolygonContour]) -> f64 {
    contours
        .iter()
        .map(|contour| {
            let points = contour
                .iter()
                .map(|[x, y]| Point::new(*x, *y))
                .collect::<Vec<_>>();
            signed_area(&points)
        })
        .sum::<f64>()
        .abs()
}

fn signed_area(points: &[Point]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for (a, b) in points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
    {
        area += a.x * b.y - b.x * a.y;
    }
    area * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_processed_geometry_summaries_with_tolerance() {
        let mut reference = triangle_doc("Top");
        let mut candidate = triangle_doc("Top");
        candidate.bbox.max.x += 0.005;

        let report = compare_documents(
            &reference,
            &candidate,
            GeometryCompareTolerance {
                bbox_mm: 0.01,
                area_mm2: 0.001,
            },
        );
        assert!(report.is_match(), "{:#?}", report.mismatches);

        reference.file_function = vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()];
        candidate.file_function = vec!["Copper".to_string(), "L2".to_string(), "Inr".to_string()];
        let report = compare_documents(&reference, &candidate, GeometryCompareTolerance::default());
        assert!(!report.is_match());
        assert!(report.mismatches[0].contains("file function differs"));
    }

    #[test]
    fn detects_symmetric_difference_with_same_area_geometry() {
        let reference = triangle_doc("Top");
        let mut candidate = triangle_doc("Top");
        for cmd in &mut candidate.path_cmds {
            cmd.p0.x += 0.25;
            cmd.p1.x += 0.25;
        }
        super::process::normalize_bounds(&mut candidate);

        let report = compare_documents(
            &reference,
            &candidate,
            GeometryCompareTolerance {
                bbox_mm: 1.0,
                area_mm2: 0.001,
            },
        );

        assert!(!report.is_match());
        assert!(
            report
                .mismatches
                .iter()
                .any(|message| message.contains("symmetric difference")),
            "{:#?}",
            report.mismatches
        );
    }

    #[test]
    fn compares_cubic_curve_shape_not_just_endpoint() {
        let reference = cubic_doc(Point::new(0.25, 1.0), Point::new(0.75, 1.0));
        let candidate = cubic_doc(Point::new(0.25, 0.0), Point::new(0.75, 0.0));

        let report = compare_documents(
            &reference,
            &candidate,
            GeometryCompareTolerance {
                bbox_mm: 1.0,
                area_mm2: 0.001,
            },
        );

        assert!(!report.is_match());
        assert!(
            report
                .mismatches
                .iter()
                .any(|message| message.contains("area") || message.contains("symmetric difference")),
            "{:#?}",
            report.mismatches
        );
    }

    fn triangle_doc(side: &str) -> GeometryDocument {
        let mut doc = GeometryDocument::new(vec![
            "Copper".to_string(),
            "L1".to_string(),
            side.to_string(),
        ]);
        let path = doc.push_path(
            GeometryPath::filled(FillRule::NonZero),
            vec![ContourPayload {
                bbox: BBox {
                    min: Point::new(0.0, 0.0),
                    max: Point::new(1.0, 1.0),
                },
                cmds: vec![
                    PathCmd::move_to(Point::new(0.0, 0.0)),
                    PathCmd::line_to(Point::new(1.0, 0.0)),
                    PathCmd::line_to(Point::new(0.0, 1.0)),
                    PathCmd::close(),
                ],
            }],
        );
        let mut feature =
            GeometryFeature::new(FeatureKind::Composite, FeatureBucket::Fill, Polarity::Dark);
        feature.path_start = path;
        feature.path_count = 1;
        doc.features.push(feature);
        super::process::normalize_bounds(&mut doc);
        doc
    }

    fn cubic_doc(c1: Point, c2: Point) -> GeometryDocument {
        let mut doc = GeometryDocument::new(vec![
            "Copper".to_string(),
            "L1".to_string(),
            "Top".to_string(),
        ]);
        let path = doc.push_path(
            GeometryPath::filled(FillRule::NonZero),
            vec![ContourPayload {
                bbox: BBox {
                    min: Point::new(0.0, 0.0),
                    max: Point::new(1.0, 1.0),
                },
                cmds: vec![
                    PathCmd::move_to(Point::new(0.0, 0.0)),
                    PathCmd::cubic_to(c1, c2, Point::new(1.0, 0.0)),
                    PathCmd::line_to(Point::new(0.0, 1.0)),
                    PathCmd::close(),
                ],
            }],
        );
        let mut feature =
            GeometryFeature::new(FeatureKind::Composite, FeatureBucket::Fill, Polarity::Dark);
        feature.path_start = path;
        feature.path_count = 1;
        doc.features.push(feature);
        super::process::normalize_bounds(&mut doc);
        doc
    }
}
