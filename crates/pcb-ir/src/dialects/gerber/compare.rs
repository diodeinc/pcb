use crate::common::*;
use crate::dialects::artwork::{self, ArtworkDocument};
use crate::dialects::mask::MaskDocument;
use crate::dialects::path as common_path;

/// Tolerances for comparing two Gerber layer images.
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
    pub difference: GeometryDifferenceSummary,
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
    pub object_count: usize,
    pub path_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryDifferenceSummary {
    pub reference_only: DirectionalDifferenceSummary,
    pub candidate_only: DirectionalDifferenceSummary,
    pub symmetric_area_mm2: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectionalDifferenceSummary {
    pub area_mm2: f64,
    pub components: Vec<DifferenceComponentSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DifferenceComponentSummary {
    pub bbox: BBox,
    pub area_mm2: f64,
}

/// Compare two layer artworks using manufacturing-relevant final-image metrics.
///
/// The object stream is composed using Gerber dark/clear paint semantics before
/// comparison. Object counts and command streams are reported for diagnostics,
/// but image bounds, area, and symmetric difference determine the match.
pub fn compare_documents<A, B>(
    reference: &ArtworkDocument<Vec<String>, A>,
    candidate: &ArtworkDocument<Vec<String>, B>,
    tolerance: GeometryCompareTolerance,
) -> GeometryCompareReport
where
    A: Clone,
    B: Clone,
{
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

    let difference = difference_summary(reference, candidate);
    if difference.symmetric_area_mm2 > tolerance.area_mm2 {
        mismatches.push(format!(
            "symmetric difference area is {:.6} mm², tolerance={:.6}",
            difference.symmetric_area_mm2, tolerance.area_mm2
        ));
    }

    GeometryCompareReport {
        reference: reference_summary,
        candidate: candidate_summary,
        difference,
        mismatches,
    }
}

pub fn summarize<A: Clone>(doc: &ArtworkDocument<Vec<String>, A>) -> GeometrySummary {
    let mask = artwork::compose_to_mask(doc);
    GeometrySummary {
        file_function: doc
            .layers
            .first()
            .map(|layer| layer.meta.clone())
            .unwrap_or_default(),
        bbox: mask
            .layers
            .first()
            .map(|layer| layer.bbox)
            .unwrap_or_else(BBox::empty),
        area_mm2: polygon_area(&document_image_contours(&mask)),
        object_count: doc.objects.len(),
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

fn difference_summary<A, B>(
    reference: &ArtworkDocument<Vec<String>, A>,
    candidate: &ArtworkDocument<Vec<String>, B>,
) -> GeometryDifferenceSummary
where
    A: Clone,
    B: Clone,
{
    let reference = document_image_contours(&artwork::compose_to_mask(reference));
    let candidate = document_image_contours(&artwork::compose_to_mask(candidate));
    let reference_only = directional_difference_summary(reference.clone(), candidate.clone());
    let candidate_only = directional_difference_summary(candidate, reference);
    let symmetric_area_mm2 = reference_only.area_mm2 + candidate_only.area_mm2;
    GeometryDifferenceSummary {
        reference_only,
        candidate_only,
        symmetric_area_mm2,
    }
}

fn directional_difference_summary(
    subject: Vec<common_path::PolygonContour>,
    cutters: Vec<common_path::PolygonContour>,
) -> DirectionalDifferenceSummary {
    let mut components = common_path::difference_contour_shapes(subject, cutters)
        .into_iter()
        .filter_map(difference_component_summary)
        .collect::<Vec<_>>();
    components.sort_by(|left, right| right.area_mm2.total_cmp(&left.area_mm2));
    let area_mm2 = components
        .iter()
        .map(|component| component.area_mm2)
        .sum::<f64>();
    DirectionalDifferenceSummary {
        area_mm2,
        components,
    }
}

fn difference_component_summary(
    shape: common_path::PolygonShape,
) -> Option<DifferenceComponentSummary> {
    if shape.is_empty() {
        return None;
    }
    let area_mm2 = shape_area(&shape);
    if area_mm2 <= 1e-9 {
        return None;
    }
    Some(DifferenceComponentSummary {
        bbox: common_path::polygon_contours_bbox(&shape),
        area_mm2,
    })
}

fn document_image_contours<LayerMeta>(
    mask: &MaskDocument<LayerMeta>,
) -> Vec<common_path::PolygonContour> {
    let mut contours = Vec::new();
    let Some(layer) = mask.layers.first() else {
        return contours;
    };
    for shape in
        &mask.shapes[layer.shape_start as usize..(layer.shape_start + layer.shape_count) as usize]
    {
        let payloads = mask.contours
            [shape.contour_start as usize..(shape.contour_start + shape.contour_count) as usize]
            .iter()
            .map(|contour| common_path::PathPayload {
                bbox: contour.bbox,
                cmds: mask.path_cmds
                    [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                    .to_vec(),
            })
            .collect::<Vec<_>>();
        contours.extend(common_path::payloads_to_polygon_contours(&payloads));
    }
    common_path::union_contours(contours, FillRule::NonZero)
}

fn polygon_area(contours: &[common_path::PolygonContour]) -> f64 {
    shape_area(contours)
}

fn shape_area(contours: &[common_path::PolygonContour]) -> f64 {
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
    use crate::dialects::artwork::{ArtworkGeometry, ArtworkLayer, ArtworkObject, ArtworkPath};
    use crate::dialects::path::PathCmd;

    #[test]
    fn compares_processed_geometry_summaries_with_tolerance() {
        let reference = triangle_doc("Top");
        let mut candidate = triangle_doc("Top");
        candidate.layers[0].bbox.max.x += 0.005;

        let report = compare_documents(
            &reference,
            &candidate,
            GeometryCompareTolerance {
                bbox_mm: 0.01,
                area_mm2: 0.001,
            },
        );
        assert!(report.is_match(), "{:#?}", report.mismatches);

        let mut reference = reference;
        reference.layers[0].meta = vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()];
        candidate.layers[0].meta = vec!["Copper".to_string(), "L2".to_string(), "Inr".to_string()];
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
        artwork::normalize_bounds(&mut candidate);

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

    #[test]
    fn dark_flash_does_not_reduce_self_cut_region_area() {
        let reference = self_cut_even_odd_doc(false);
        let candidate = self_cut_even_odd_doc(true);

        let reference_area = summarize(&reference).area_mm2;
        let candidate_area = summarize(&candidate).area_mm2;

        assert!(
            candidate_area >= reference_area,
            "adding a dark flash reduced area: reference={reference_area}, candidate={candidate_area}"
        );
    }

    fn triangle_doc(side: &str) -> ArtworkDocument<Vec<String>, ()> {
        let mut doc = ArtworkDocument::new(Unit::Millimeter);
        let layer = doc.push_layer(ArtworkLayer {
            name: "Copper".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: vec!["Copper".to_string(), "L1".to_string(), side.to_string()],
        });
        let path = doc.push_path(
            ArtworkPath::filled(FillRule::NonZero),
            vec![common_path::PathPayload {
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
        doc.push_object(
            layer,
            ArtworkObject::new(PaintPolarity::Dark, ArtworkGeometry::Region { path }),
        );
        artwork::normalize_bounds(&mut doc);
        doc
    }

    fn self_cut_even_odd_doc(with_flash: bool) -> ArtworkDocument<Vec<String>, ()> {
        let mut doc = ArtworkDocument::new(Unit::Millimeter);
        let layer = doc.push_layer(ArtworkLayer {
            name: "Copper".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()],
        });
        let path = doc.push_path(
            ArtworkPath::filled(FillRule::EvenOdd),
            vec![common_path::PathPayload {
                bbox: BBox {
                    min: Point::new(0.0, 0.0),
                    max: Point::new(4.0, 4.0),
                },
                cmds: vec![
                    PathCmd::move_to(Point::new(0.0, 0.0)),
                    PathCmd::line_to(Point::new(4.0, 0.0)),
                    PathCmd::line_to(Point::new(4.0, 4.0)),
                    PathCmd::line_to(Point::new(0.0, 4.0)),
                    PathCmd::line_to(Point::new(0.0, 0.0)),
                    PathCmd::line_to(Point::new(1.0, 1.0)),
                    PathCmd::line_to(Point::new(3.0, 1.0)),
                    PathCmd::line_to(Point::new(3.0, 3.0)),
                    PathCmd::line_to(Point::new(1.0, 3.0)),
                    PathCmd::line_to(Point::new(1.0, 1.0)),
                    PathCmd::line_to(Point::new(0.0, 0.0)),
                    PathCmd::close(),
                ],
            }],
        );
        doc.push_object(
            layer,
            ArtworkObject::new(PaintPolarity::Dark, ArtworkGeometry::Region { path }),
        );
        if with_flash {
            doc.push_object(
                layer,
                ArtworkObject::new(
                    PaintPolarity::Dark,
                    ArtworkGeometry::CircleFlash {
                        at: Point::new(2.0, 2.0),
                        diameter: 0.5,
                    },
                ),
            );
        }
        artwork::normalize_bounds(&mut doc);
        doc
    }

    fn cubic_doc(c1: Point, c2: Point) -> ArtworkDocument<Vec<String>, ()> {
        let mut doc = ArtworkDocument::new(Unit::Millimeter);
        let layer = doc.push_layer(ArtworkLayer {
            name: "Copper".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            object_start: 0,
            object_count: 0,
            bbox: BBox::empty(),
            meta: vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()],
        });
        let path = doc.push_path(
            ArtworkPath::filled(FillRule::NonZero),
            vec![common_path::PathPayload {
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
        doc.push_object(
            layer,
            ArtworkObject::new(PaintPolarity::Dark, ArtworkGeometry::Region { path }),
        );
        artwork::normalize_bounds(&mut doc);
        doc
    }
}
