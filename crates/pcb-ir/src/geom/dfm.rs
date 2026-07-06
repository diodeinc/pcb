//! Morphological manufacturability checks over filled regions.
//!
//! A feature narrower than the fabrication minimum disappears under a
//! morphological opening (erode then dilate) with a disk of that width;
//! a gap narrower than the minimum disappears under the closing. The
//! difference between the region and its opening/closing is therefore
//! exactly the sub-minimum material ("slivers") and sub-minimum clearance,
//! reported piece by piece.

use crate::geom::bbox::BBox;
use crate::geom::point::Point;
use crate::geom::region::{ContourSet, Ring, ring_signed_area, rings_bbox};
use crate::geom::tol;

/// One contiguous sub-minimum piece of material or clearance.
#[derive(Debug, Clone)]
pub struct ThinPiece {
    pub bbox: BBox,
    pub area_mm2: f64,
    /// Estimated width (2·area/perimeter — exact for long ribbons).
    pub width_mm: f64,
    /// Estimated length (half the perimeter — exact for long ribbons).
    pub length_mm: f64,
}

/// Filled material narrower than `min_width_mm`.
pub fn thin_features(region: &ContourSet, min_width_mm: f64) -> Vec<ThinPiece> {
    if region.is_empty() {
        return Vec::new();
    }
    let radius = min_width_mm / 2.0;
    let opened = erode(region, radius).disk_dilate(radius);
    pieces(&region.difference(&opened), min_width_mm)
}

/// Gaps in the material narrower than `min_gap_mm`, including boundary
/// notches.
pub fn thin_gaps(region: &ContourSet, min_gap_mm: f64) -> Vec<ThinPiece> {
    if region.is_empty() {
        return Vec::new();
    }
    let radius = min_gap_mm / 2.0;
    let closed = erode(&region.disk_dilate(radius), radius);
    pieces(&closed.difference(region), min_gap_mm)
}

/// Erosion by complement: pad a working universe around the region, dilate
/// the complement, and take what survives.
fn erode(region: &ContourSet, radius: f64) -> ContourSet {
    let pad = 2.0 * radius;
    let universe = ContourSet::rectangle(
        BBox {
            min: Point::new(region.bbox.min.x - pad, region.bbox.min.y - pad),
            max: Point::new(region.bbox.max.x + pad, region.bbox.max.y + pad),
        },
        region.tolerance,
    );
    universe.difference(&universe.difference(region).disk_dilate(radius))
}

/// Extract reportable pieces from a residue region, dropping numeric noise:
/// flattening-scale ribbons along long edges and the corner bites that any
/// right angle sheds under a disk opening.
fn pieces(residue: &ContourSet, min_width_mm: f64) -> Vec<ThinPiece> {
    let noise_width = 2.0 * tol::FLATTEN_MM;
    let min_length = 2.0 * min_width_mm;
    let min_area = 0.25 * min_width_mm * min_width_mm;

    let mut pieces: Vec<ThinPiece> = residue
        .rings
        .iter()
        .filter_map(|ring| {
            let area = ring_signed_area(ring);
            if area <= 0.0 {
                return None; // holes of residue pieces
            }
            let perimeter = ring_perimeter(ring);
            if perimeter <= 0.0 {
                return None;
            }
            let piece = ThinPiece {
                bbox: rings_bbox(std::slice::from_ref(ring)),
                area_mm2: area,
                width_mm: 2.0 * area / perimeter,
                length_mm: perimeter / 2.0,
            };
            let long_side = piece.bbox.width().max(piece.bbox.height());
            (piece.width_mm >= noise_width && long_side >= min_length && area >= min_area)
                .then_some(piece)
        })
        .collect();
    pieces.sort_by(|a, b| b.area_mm2.total_cmp(&a.area_mm2));
    pieces
}

fn ring_perimeter(ring: &Ring) -> f64 {
    (0..ring.len())
        .map(|i| {
            let [x0, y0] = ring[i];
            let [x1, y1] = ring[(i + 1) % ring.len()];
            (x1 - x0).hypot(y1 - y0)
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::{ContourBuf, PathCmd};
    use crate::geom::{FillRule, shapes};

    fn rect_at(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    #[test]
    fn clean_shapes_report_nothing() {
        let region = ContourSet::from_contours(
            &[
                shapes::rect(10.0, 6.0).unwrap(),
                shapes::circle(3.0).unwrap(),
            ],
            FillRule::NonZero,
            tol::REGION_MM,
        );

        assert!(thin_features(&region, 0.1).is_empty());
        assert!(thin_gaps(&region, 0.1).is_empty());
    }

    #[test]
    fn thin_spur_is_reported_with_its_size() {
        // A healthy plate with a 0.05 x 2.0 mm spur sticking out.
        let region = ContourSet::from_filled_contours(
            &[
                rect_at(0.0, 0.0, 10.0, 10.0),
                rect_at(10.0, 5.0, 12.0, 5.05),
            ],
            tol::REGION_MM,
        );

        let findings = thin_features(&region, 0.1);

        assert_eq!(findings.len(), 1);
        let piece = &findings[0];
        assert!(
            (piece.width_mm - 0.05).abs() < 0.02,
            "width {}",
            piece.width_mm
        );
        assert!(piece.length_mm > 1.5, "length {}", piece.length_mm);
    }

    #[test]
    fn narrow_gap_between_plates_is_reported() {
        let region = ContourSet::from_filled_contours(
            &[
                rect_at(0.0, 0.0, 10.0, 10.0),
                rect_at(10.06, 0.0, 20.0, 10.0),
            ],
            tol::REGION_MM,
        );

        let gaps = thin_gaps(&region, 0.1);

        assert_eq!(gaps.len(), 1);
        assert!(
            (gaps[0].width_mm - 0.06).abs() < 0.02,
            "width {}",
            gaps[0].width_mm
        );
        assert!(thin_features(&region, 0.1).is_empty());
    }

    #[test]
    fn wide_features_and_gaps_pass() {
        let region = ContourSet::from_filled_contours(
            &[
                rect_at(0.0, 0.0, 10.0, 10.0),
                rect_at(10.5, 0.0, 20.0, 10.0),
            ],
            tol::REGION_MM,
        );

        assert!(thin_features(&region, 0.1).is_empty());
        assert!(thin_gaps(&region, 0.1).is_empty());
    }
}
