//! Manufacturing-relevant comparison of two layer artworks.
//!
//! The object streams are composed into final images before comparison, so
//! two different export paths that describe the same layer image compare
//! equal even when their object streams differ. Object counts and command
//! streams are reported for diagnostics only.

use crate::dialects::artwork::{self, Document};
use crate::dialects::mask;
use crate::geom::region::{self, Ring, Shape};
use crate::geom::{BBox, FillRule};

/// Tolerances for comparing two layer images.
///
/// Intended for smoke tests where two export paths should describe the same
/// image but are not expected to be bytewise identical.
#[derive(Debug, Clone, Copy)]
pub struct CompareTolerance {
    pub bbox_mm: f64,
    pub area_mm2: f64,
}

impl Default for CompareTolerance {
    fn default() -> Self {
        Self {
            bbox_mm: 0.01,
            area_mm2: 0.01,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompareReport {
    pub reference: Summary,
    pub candidate: Summary,
    pub difference: DifferenceSummary,
    pub mismatches: Vec<String>,
}

impl CompareReport {
    pub fn is_match(&self) -> bool {
        self.mismatches.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub file_function: Vec<String>,
    pub bbox: BBox,
    pub area_mm2: f64,
    pub object_count: usize,
    pub path_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DifferenceSummary {
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

/// Compare two layer artworks using final-image metrics: bounds, filled
/// area, and symmetric difference.
pub fn compare_documents<A, B>(
    reference: &Document<Vec<String>, A>,
    candidate: &Document<Vec<String>, B>,
    tolerance: CompareTolerance,
) -> CompareReport
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

    CompareReport {
        reference: reference_summary,
        candidate: candidate_summary,
        difference,
        mismatches,
    }
}

pub fn summarize<A: Clone>(doc: &Document<Vec<String>, A>) -> Summary {
    let mask = artwork::compose_to_mask(doc);
    Summary {
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
        area_mm2: region::rings_area(&document_image_rings(&mask)),
        object_count: doc.objects.len(),
        path_count: doc.arena.paths.len(),
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
    reference: &Document<Vec<String>, A>,
    candidate: &Document<Vec<String>, B>,
) -> DifferenceSummary
where
    A: Clone,
    B: Clone,
{
    let reference = document_image_rings(&artwork::compose_to_mask(reference));
    let candidate = document_image_rings(&artwork::compose_to_mask(candidate));
    let reference_only = directional_difference_summary(reference.clone(), candidate.clone());
    let candidate_only = directional_difference_summary(candidate, reference);
    let symmetric_area_mm2 = reference_only.area_mm2 + candidate_only.area_mm2;
    DifferenceSummary {
        reference_only,
        candidate_only,
        symmetric_area_mm2,
    }
}

fn directional_difference_summary(
    subject: Vec<Ring>,
    cutters: Vec<Ring>,
) -> DirectionalDifferenceSummary {
    let mut components = region::difference_shapes(subject, cutters)
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

fn difference_component_summary(shape: Shape) -> Option<DifferenceComponentSummary> {
    if shape.is_empty() {
        return None;
    }
    let area_mm2 = region::rings_area(&shape);
    if area_mm2 <= 1e-9 {
        return None;
    }
    Some(DifferenceComponentSummary {
        bbox: region::rings_bbox(&shape),
        area_mm2,
    })
}

fn document_image_rings<LayerMeta>(mask: &mask::Document<LayerMeta>) -> Vec<Ring> {
    let mut rings = Vec::new();
    let Some(layer) = mask.layers.first() else {
        return rings;
    };
    for shape in mask.shapes(layer) {
        rings.extend(region::rings_from_contours(
            &mask.arena.path_contours(shape),
        ));
    }
    region::union_rings(rings, FillRule::NonZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::artwork::{Geometry, Layer, Object};
    use crate::dialects::{LayerRole, Side};
    use crate::geom::path::{ContourBuf, PathCmd};
    use crate::geom::{Affine2, Mirror, Paint, Point, Polarity, Span};

    #[test]
    fn compares_processed_geometry_summaries_with_tolerance() {
        let reference = triangle_doc("Top");
        let mut candidate = triangle_doc("Top");
        candidate.layers[0].bbox.max.x += 0.005;

        let report = compare_documents(
            &reference,
            &candidate,
            CompareTolerance {
                bbox_mm: 0.01,
                area_mm2: 0.001,
            },
        );
        assert!(report.is_match(), "{:#?}", report.mismatches);

        let mut reference = reference;
        reference.layers[0].meta = vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()];
        candidate.layers[0].meta = vec!["Copper".to_string(), "L2".to_string(), "Inr".to_string()];
        let report = compare_documents(&reference, &candidate, CompareTolerance::default());
        assert!(!report.is_match());
        assert!(report.mismatches[0].contains("file function differs"));
    }

    #[test]
    fn detects_symmetric_difference_with_same_area_geometry() {
        let reference = triangle_doc("Top");
        let mut candidate = triangle_doc("Top");
        for cmd in &mut candidate.arena.cmds {
            cmd.p0.x += 0.25;
            cmd.p1.x += 0.25;
        }
        artwork::normalize_bounds(&mut candidate);

        let report = compare_documents(
            &reference,
            &candidate,
            CompareTolerance {
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
            CompareTolerance {
                bbox_mm: 1.0,
                area_mm2: 0.001,
            },
        );

        assert!(!report.is_match());
        assert!(
            report
                .mismatches
                .iter()
                .any(|message| message.contains("area")
                    || message.contains("symmetric difference")),
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

    fn gerber_layer(side: &str) -> Layer<Vec<String>> {
        Layer {
            name: "Copper".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: vec!["Copper".to_string(), "L1".to_string(), side.to_string()],
        }
    }

    fn triangle_doc(side: &str) -> Document<Vec<String>, ()> {
        let mut doc = Document::new();
        let layer = doc.push_layer(gerber_layer(side));
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(0.0, 1.0)),
                PathCmd::close(),
            ])],
        );
        doc.push_object(
            layer,
            Object::new(Polarity::Dark, Geometry::Region { path }),
        );
        artwork::normalize_bounds(&mut doc);
        doc
    }

    fn self_cut_even_odd_doc(with_flash: bool) -> Document<Vec<String>, ()> {
        let mut doc = Document::new();
        let layer = doc.push_layer(gerber_layer("Top"));
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            vec![ContourBuf::new(vec![
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
            ])],
        );
        doc.push_object(
            layer,
            Object::new(Polarity::Dark, Geometry::Region { path }),
        );
        if with_flash {
            let aperture = doc.push_aperture(artwork::Aperture::circle(0.5));
            doc.push_object(
                layer,
                Object::new(
                    Polarity::Dark,
                    Geometry::Flash {
                        aperture,
                        transform: Affine2::placement(Point::new(2.0, 2.0), 0.0, Mirror::NONE, 1.0),
                    },
                ),
            );
        }
        artwork::normalize_bounds(&mut doc);
        doc
    }

    fn cubic_doc(c1: Point, c2: Point) -> Document<Vec<String>, ()> {
        let mut doc = Document::new();
        let layer = doc.push_layer(gerber_layer("Top"));
        let path = doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            vec![ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::cubic_to(c1, c2, Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(0.0, 1.0)),
                PathCmd::close(),
            ])],
        );
        doc.push_object(
            layer,
            Object::new(Polarity::Dark, Geometry::Region { path }),
        );
        artwork::normalize_bounds(&mut doc);
        doc
    }
}
