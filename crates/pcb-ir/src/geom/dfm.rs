//! Geometry measurements shared by manufacturability checks.

use std::collections::HashMap;

use crate::geom::bbox::BBox;
use crate::geom::dist;
use crate::geom::point::Point;
use crate::geom::region::{ContourSet, Ring, TwoSidedResidualComponent, ring_edges};
use crate::geom::tol;

/// The shortest separation between two pieces of geometry, with the points
/// that realize it. Distances are expressed in the IR's canonical millimeters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClearanceMeasurement {
    pub distance_mm: f64,
    pub first: Point,
    pub second: Point,
}

/// The minimum radial material enclosure around a circular cutout whose
/// center lies in the material.
///
/// `enclosure_mm` is signed. A positive value is material outside the cutout;
/// a negative value is the amount by which the cutout breaches the material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircularEnclosureMeasurement {
    pub enclosure_mm: f64,
    pub cutout_boundary: Point,
    pub material_boundary: Point,
}

/// Uniform-grid index over a region's boundary segments.
///
/// DFM rules ask whether thousands of points and segments lie within a small
/// clearance of the same composed layer boundary. This keeps those queries
/// local instead of walking every polygon edge for every query.
#[derive(Debug, Clone)]
pub struct RegionBoundaryIndex {
    cell_size_mm: f64,
    segments: Vec<(Point, Point)>,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

/// Grid pitch bounds. Queries stay correct for any pitch; these only keep the
/// cell count proportionate when a caller's search radius is extreme.
const MIN_CELL_MM: f64 = 0.5;
const MAX_CELL_MM: f64 = 50.0;

impl RegionBoundaryIndex {
    /// Index a region's boundary for queries out to about `search_radius_mm`.
    ///
    /// The radius is a pitch hint, not a limit: queries may pass any distance.
    pub fn new(region: &ContourSet, search_radius_mm: f64) -> Self {
        let cell_size_mm = search_radius_mm.clamp(MIN_CELL_MM, MAX_CELL_MM);
        let segments = region.rings.iter().flat_map(ring_edges).collect::<Vec<_>>();
        let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (index, &(start, end)) in segments.iter().enumerate() {
            corridor_cells(start, end, 0.0, cell_size_mm, |cell| {
                cells.entry(cell).or_default().push(index as u32);
            });
        }
        Self {
            cell_size_mm,
            segments,
            cells,
        }
    }

    /// Nearest boundary point no farther than `max_distance_mm` from `point`.
    pub fn nearest_within(
        &self,
        point: Point,
        max_distance_mm: f64,
    ) -> Option<ClearanceMeasurement> {
        let min_x = grid_cell(point.x - max_distance_mm, self.cell_size_mm);
        let max_x = grid_cell(point.x + max_distance_mm, self.cell_size_mm);
        let min_y = grid_cell(point.y - max_distance_mm, self.cell_size_mm);
        let max_y = grid_cell(point.y + max_distance_mm, self.cell_size_mm);
        let mut nearest: Option<ClearanceMeasurement> = None;
        for x in min_x..=max_x {
            for y in min_y..=max_y {
                for &index in self.cells.get(&(x, y)).into_iter().flatten() {
                    let (start, end) = self.segments[index as usize];
                    let (distance_mm, boundary) = dist::point_segment(point, start, end);
                    if distance_mm <= max_distance_mm + tol::EPSILON_MM
                        && nearest.is_none_or(|current| distance_mm < current.distance_mm)
                    {
                        nearest = Some(ClearanceMeasurement {
                            distance_mm,
                            first: point,
                            second: boundary,
                        });
                    }
                }
            }
        }
        nearest
    }

    /// Nearest boundary point no farther than `max_distance_mm` from the
    /// query segment. A segment crossing the boundary measures zero.
    pub fn segment_nearest_within(
        &self,
        start: Point,
        end: Point,
        max_distance_mm: f64,
    ) -> Option<ClearanceMeasurement> {
        let mut nearest: Option<ClearanceMeasurement> = None;
        corridor_cells(start, end, max_distance_mm, self.cell_size_mm, |cell| {
            for &index in self.cells.get(&cell).into_iter().flatten() {
                let (edge_start, edge_end) = self.segments[index as usize];
                let (distance_mm, on_query, on_boundary) =
                    dist::segments(start, end, edge_start, edge_end);
                if distance_mm <= max_distance_mm + tol::EPSILON_MM
                    && nearest.is_none_or(|current| distance_mm < current.distance_mm)
                {
                    nearest = Some(ClearanceMeasurement {
                        distance_mm,
                        first: on_query,
                        second: on_boundary,
                    });
                }
            }
        });
        nearest
    }

    /// The radial material enclosure of a circular cutout whose center lies
    /// in the material, searched out to `max_enclosure_mm`.
    ///
    /// The largest disk centered at `center` inside the material has radius
    /// `d(center, boundary)`, so the enclosure is that distance minus the
    /// cutout radius. `None` means the enclosure exceeds `max_enclosure_mm`;
    /// comparing the returned measurement against a limit is the caller's
    /// policy.
    pub fn circular_enclosure(
        &self,
        center: Point,
        cutout_radius_mm: f64,
        max_enclosure_mm: f64,
    ) -> Option<CircularEnclosureMeasurement> {
        let search = (cutout_radius_mm + max_enclosure_mm).max(0.0);
        let nearest = self.nearest_within(center, search)?;
        let direction = nearest.second - center;
        let direction = if direction.length() <= f64::EPSILON {
            Point::new(1.0, 0.0)
        } else {
            direction / direction.length()
        };
        Some(CircularEnclosureMeasurement {
            enclosure_mm: nearest.distance_mm - cutout_radius_mm,
            cutout_boundary: center + direction * cutout_radius_mm,
            material_boundary: nearest.second,
        })
    }
}

fn grid_cell(value: f64, cell_size_mm: f64) -> i64 {
    (value / cell_size_mm).floor() as i64
}

/// Visit every grid cell within `radius_mm` of the segment, column by
/// column, so long diagonal segments touch a linear number of cells instead
/// of their whole bounding box.
fn corridor_cells(
    start: Point,
    end: Point,
    radius_mm: f64,
    cell_size_mm: f64,
    mut visit: impl FnMut((i64, i64)),
) {
    let min_column = grid_cell(start.x.min(end.x) - radius_mm, cell_size_mm);
    let max_column = grid_cell(start.x.max(end.x) + radius_mm, cell_size_mm);
    let delta = end - start;
    for column in min_column..=max_column {
        let column_min = column as f64 * cell_size_mm - radius_mm;
        let column_max = (column + 1) as f64 * cell_size_mm + radius_mm;
        let (t_min, t_max) = if delta.x.abs() <= f64::EPSILON {
            (0.0, 1.0)
        } else {
            let enter = (column_min - start.x) / delta.x;
            let exit = (column_max - start.x) / delta.x;
            (
                enter.min(exit).clamp(0.0, 1.0),
                enter.max(exit).clamp(0.0, 1.0),
            )
        };
        let y_at_min = start.y + delta.y * t_min;
        let y_at_max = start.y + delta.y * t_max;
        let min_row = grid_cell(y_at_min.min(y_at_max) - radius_mm, cell_size_mm);
        let max_row = grid_cell(y_at_min.max(y_at_max) + radius_mm, cell_size_mm);
        for row in min_row..=max_row {
            visit((column, row));
        }
    }
}

/// Set distance between two closed disks: `max(0, ‖c₁ − c₂‖ − r₁ − r₂)`,
/// with the boundary witness points along the center line.
pub fn disk_clearance(
    first_center: Point,
    first_radius_mm: f64,
    second_center: Point,
    second_radius_mm: f64,
) -> ClearanceMeasurement {
    let delta = second_center - first_center;
    let length = delta.length();
    if length <= f64::EPSILON {
        return ClearanceMeasurement {
            distance_mm: 0.0,
            first: first_center,
            second: second_center,
        };
    }
    let direction = delta / length;
    ClearanceMeasurement {
        distance_mm: (length - first_radius_mm - second_radius_mm).max(0.0),
        first: first_center + direction * first_radius_mm,
        second: second_center - direction * second_radius_mm,
    }
}

/// Euclidean separation of two axis-aligned bounds. Because each enclosed
/// region is a subset of its bounds, this is a conservative lower bound on
/// region-to-region clearance: a value meeting a minimum proves the exact
/// regions meet it too.
pub fn bbox_clearance_lower_bound(first: BBox, second: BBox) -> f64 {
    let x = (second.min.x - first.max.x)
        .max(first.min.x - second.max.x)
        .max(0.0);
    let y = (second.min.y - first.max.y)
        .max(first.min.y - second.max.y)
        .max(0.0);
    x.hypot(y)
}

/// Shortest boundary-to-boundary clearance between two filled regions.
///
/// Overlapping, contained, or touching regions have zero clearance. `None`
/// means at least one input is empty.
pub fn region_clearance(first: &ContourSet, second: &ContourSet) -> Option<ClearanceMeasurement> {
    if first.is_empty() || second.is_empty() {
        return None;
    }

    if let Some(point) = contained_vertex(first, second).or_else(|| contained_vertex(second, first))
    {
        return Some(ClearanceMeasurement {
            distance_mm: 0.0,
            first: point,
            second: point,
        });
    }

    closest_ring_edges(&first.rings, &second.rings)
}

/// A vertex of `subject` inside `container`, batched in one winding sweep.
fn contained_vertex(subject: &ContourSet, container: &ContourSet) -> Option<Point> {
    let vertices = subject
        .rings
        .iter()
        .flat_map(|ring| ring.iter())
        .map(|&[x, y]| Point::new(x, y))
        .collect::<Vec<_>>();
    container
        .contains_points_batch(&vertices)
        .into_iter()
        .zip(vertices)
        .find_map(|(inside, vertex)| inside.then_some(vertex))
}

fn closest_ring_edges(first: &[Ring], second: &[Ring]) -> Option<ClearanceMeasurement> {
    first
        .iter()
        .flat_map(ring_edges)
        .flat_map(|(first_start, first_end)| {
            second.iter().flat_map(move |ring| {
                ring_edges(ring).map(move |(second_start, second_end)| {
                    let (distance_mm, on_first, on_second) =
                        dist::segments(first_start, first_end, second_start, second_end);
                    ClearanceMeasurement {
                        distance_mm,
                        first: on_first,
                        second: on_second,
                    }
                })
            })
        })
        .min_by(|left, right| left.distance_mm.total_cmp(&right.distance_mm))
}

/// One contiguous sub-minimum piece of material or clearance.
///
/// A feature narrower than the fabrication minimum disappears under a
/// morphological opening (erode then dilate) with a disk of that width; a
/// gap narrower than the minimum disappears under the closing. The
/// difference between the region and its opening/closing is exactly the
/// sub-minimum material ("slivers") and sub-minimum clearance, piece by
/// piece.
#[derive(Debug, Clone)]
pub struct ThinPiece {
    pub bbox: BBox,
    pub area_mm2: f64,
    /// Exact separation of the two opposing source-boundary branches in the
    /// flattened polygon representation.
    pub width_mm: f64,
    /// Approximate longitudinal extent (half the residue perimeter).
    pub length_mm: f64,
    pub first: Point,
    pub second: Point,
}

/// Opening and closing use tessellated round offsets. Widening the candidate
/// threshold by the full two-offset plus source-flattening error makes the
/// morphological stage conservative; exact source-boundary distance below
/// then decides the verdict and discards the extra candidates.
const MORPHOLOGY_CANDIDATE_GUARD_MM: f64 = 2.0 * tol::STROKE_OUTLINE_MM + tol::FLATTEN_MM;

/// Two flattened source boundaries determine a local width. Each may lie up
/// to one chord tolerance inside its source curve, so a result within this
/// band of the limit is geometrically indeterminate rather than a finding.
const TWO_BOUNDARY_UNCERTAINTY_MM: f64 = 2.0 * tol::FLATTEN_MM + tol::EPSILON_MM;

/// Filled material narrower than `min_width_mm`. Only two-sided residue is
/// reported, so the bite an isolated convex arc sheds under the opening is
/// not a thin feature.
pub fn thin_features(region: &ContourSet, min_width_mm: f64) -> Vec<ThinPiece> {
    pieces(
        region.disk_feature_violation_components(
            (min_width_mm + MORPHOLOGY_CANDIDATE_GUARD_MM) / 2.0,
        ),
        min_width_mm,
    )
}

/// Gaps in the material narrower than `min_gap_mm`, including boundary
/// notches. Only two-sided residue is reported, so the bite an isolated
/// concave corner sheds under the closing is not clearance.
pub fn thin_gaps(region: &ContourSet, min_gap_mm: f64) -> Vec<ThinPiece> {
    pieces(
        region.disk_gap_violation_components((min_gap_mm + MORPHOLOGY_CANDIDATE_GUARD_MM) / 2.0),
        min_gap_mm,
    )
}

/// Convert the conservative opening/closing candidates into authoritative
/// measurements. A candidate is reportable only when the exact distance
/// between its opposing source-boundary branches violates the requested
/// minimum; this is what prevents offset tessellation from creating findings.
fn pieces(components: Vec<TwoSidedResidualComponent>, minimum_mm: f64) -> Vec<ThinPiece> {
    let mut pieces = components
        .into_iter()
        .filter(|component| component.distance_mm + TWO_BOUNDARY_UNCERTAINTY_MM < minimum_mm)
        .filter_map(|component| {
            let perimeter = component
                .region
                .rings
                .iter()
                .flat_map(ring_edges)
                .map(|(start, end)| start.distance_to(end))
                .sum::<f64>();
            (perimeter > 0.0).then(|| ThinPiece {
                bbox: component.region.bbox,
                area_mm2: component.region.area(),
                width_mm: component.distance_mm,
                length_mm: perimeter / 2.0,
                first: component.first,
                second: component.second,
            })
        })
        .collect::<Vec<_>>();
    pieces.sort_by(|a, b| b.area_mm2.total_cmp(&a.area_mm2));
    pieces
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

    fn rect_region(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourSet {
        ContourSet::from_contours(
            &[rect_at(min_x, min_y, max_x, max_y)],
            FillRule::NonZero,
            tol::REGION_MM,
        )
    }

    #[test]
    fn clearance_reports_distance_and_witness_points() {
        let left = rect_region(0.0, 0.0, 2.0, 2.0);
        let right = rect_region(3.5, 0.5, 5.0, 1.5);

        let between_regions = region_clearance(&left, &right).unwrap();
        assert!((between_regions.distance_mm - 1.5).abs() < 1e-9);
        assert!((between_regions.first.x - 2.0).abs() < 1e-9);
        assert!((between_regions.second.x - 3.5).abs() < 1e-9);

        let index = RegionBoundaryIndex::new(&left, 1.5);
        let from_segment = index
            .segment_nearest_within(Point::new(-1.0, 3.0), Point::new(3.0, 3.0), 1.5)
            .unwrap();
        assert!((from_segment.distance_mm - 1.0).abs() < 1e-9);

        let crossing = index
            .segment_nearest_within(Point::new(-1.0, 1.0), Point::new(3.0, 1.0), 0.5)
            .unwrap();
        assert_eq!(crossing.distance_mm, 0.0);
    }

    #[test]
    fn region_clearance_handles_diagonal_separation_crossing_and_containment() {
        let origin = rect_region(0.0, 0.0, 1.0, 1.0);
        let diagonal = rect_region(2.0, 3.0, 3.0, 4.0);
        let clearance = region_clearance(&origin, &diagonal).unwrap();
        assert!((clearance.distance_mm - 5.0_f64.sqrt()).abs() < 1e-9);
        assert!(
            (bbox_clearance_lower_bound(origin.bbox, diagonal.bbox) - 5.0_f64.sqrt()).abs() < 1e-9
        );

        let horizontal = rect_region(-2.0, -0.25, 2.0, 0.25);
        let vertical = rect_region(-0.25, -2.0, 0.25, 2.0);
        assert_eq!(
            region_clearance(&horizontal, &vertical)
                .unwrap()
                .distance_mm,
            0.0,
            "crossing regions overlap even when neither contains the other"
        );

        let container = rect_region(-2.0, -2.0, 2.0, 2.0);
        let contained = rect_region(-0.5, -0.5, 0.5, 0.5);
        assert_eq!(
            region_clearance(&container, &contained)
                .unwrap()
                .distance_mm,
            0.0
        );
    }

    #[test]
    fn corridor_indexing_covers_long_diagonal_segments() {
        let diagonal = ContourSet::from_contours(
            &[ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(80.0, 80.0)),
                PathCmd::line_to(Point::new(80.0, 80.2)),
                PathCmd::line_to(Point::new(0.0, 0.2)),
                PathCmd::close(),
            ])],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let index = RegionBoundaryIndex::new(&diagonal, 0.5);
        let near_middle = index
            .nearest_within(Point::new(40.3, 40.0), 0.5)
            .expect("mid-segment boundary within reach");
        assert!((near_middle.distance_mm - 0.3 / 2.0_f64.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn circular_enclosure_is_signed_and_reports_witnesses() {
        let copper = ContourSet::from_contours(
            &[shapes::circle(5.0).unwrap()],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let index = RegionBoundaryIndex::new(&copper, 1.475);

        assert_eq!(
            index.circular_enclosure(Point::default(), 1.35, 0.125 - tol::FLATTEN_MM),
            None,
            "a satisfied enclosure exceeds the search bound"
        );

        let measurement = index
            .circular_enclosure(Point::new(1.3, 0.0), 1.35, 0.125)
            .expect("hole extending beyond copper must measure");
        assert!((measurement.enclosure_mm + 0.15).abs() < tol::FLATTEN_MM);
        assert!((measurement.cutout_boundary.x - 2.65).abs() < tol::FLATTEN_MM);
        assert!((measurement.material_boundary.x - 2.5).abs() < tol::FLATTEN_MM);
    }

    #[test]
    fn circular_enclosure_handles_noncircular_lands() {
        let copper = rect_region(-3.0, -1.5, 3.0, 1.5);
        let index = RegionBoundaryIndex::new(&copper, 1.6);

        let measurement = index
            .circular_enclosure(Point::default(), 1.0, 0.6)
            .expect("the short side of a rectangular land must set the enclosure");
        assert!((measurement.enclosure_mm - 0.5).abs() < 1e-9);
        assert!((measurement.material_boundary.y.abs() - 1.5).abs() < 1e-9);
    }

    #[test]
    fn isolated_convex_corners_are_not_thin_features() {
        // The mirror of the concave case: a shallow convex bulge sheds a
        // long thin opening residue with only one boundary side.
        let bulge = ContourSet::from_contours(
            &[ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(20.0, 0.0)),
                PathCmd::line_to(Point::new(20.0, 2.8)),
                PathCmd::line_to(Point::new(10.0, 4.0)),
                PathCmd::line_to(Point::new(0.0, 2.8)),
                PathCmd::close(),
            ])],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let raw_residue = bulge.difference(&bulge.disk_open(0.5));
        assert!(
            !raw_residue.is_empty(),
            "the opening must shed residue here"
        );
        assert!(thin_features(&bulge, 1.0).is_empty());
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
    fn isolated_concave_corners_are_not_gaps() {
        // A shallow concave kink sheds a long thin closing residue with only
        // one boundary side; that is corner geometry, not clearance.
        let chevron = ContourSet::from_contours(
            &[ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 1.2)),
                PathCmd::line_to(Point::new(20.0, 0.0)),
                PathCmd::line_to(Point::new(20.0, 4.0)),
                PathCmd::line_to(Point::new(0.0, 4.0)),
                PathCmd::close(),
            ])],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let raw_residue = chevron.disk_close(0.5).difference(&chevron);
        assert!(
            !raw_residue.is_empty(),
            "the closing must shed residue here"
        );
        assert!(thin_gaps(&chevron, 1.0).is_empty());
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
    fn small_islands_are_not_filtered_out_of_feature_width() {
        let island = rect_region(0.0, 0.0, 0.05, 0.05);

        let findings = thin_features(&island, 0.1);

        assert_eq!(findings.len(), 1);
        assert!((findings[0].width_mm - 0.05).abs() < 1e-9);
        assert!((findings[0].first.distance_to(findings[0].second) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn short_gaps_are_not_filtered_out_of_clearance() {
        let left = rect_region(0.0, 0.0, 0.05, 0.05);
        let right = rect_region(0.11, 0.0, 0.16, 0.05);
        let region = left.union(&right);

        let findings = thin_gaps(&region, 0.1);

        assert_eq!(findings.len(), 1);
        assert!((findings[0].width_mm - 0.06).abs() < 1e-9);
    }

    #[test]
    fn exact_minimums_are_not_morphology_false_positives() {
        let feature = rect_region(0.0, 0.0, 1.0, 0.1);
        let disk = ContourSet::from_filled_contours(
            &[shapes::circle(0.1).expect("valid circle")],
            tol::REGION_MM,
        );
        let undersized_disk = ContourSet::from_filled_contours(
            &[shapes::circle(0.08).expect("valid circle")],
            tol::REGION_MM,
        );
        let left = rect_region(2.0, 0.0, 3.0, 1.0);
        let right = rect_region(3.1, 0.0, 4.1, 1.0);

        assert!(thin_features(&feature, 0.1).is_empty());
        let disk_findings = thin_features(&disk, 0.1);
        assert!(disk_findings.is_empty(), "findings: {disk_findings:?}");
        assert_eq!(thin_features(&undersized_disk, 0.1).len(), 1);
        assert!(thin_gaps(&left.union(&right), 0.1).is_empty());
    }

    #[test]
    fn widths_are_invariant_under_quarter_turns() {
        let left = rect_region(0.0, 0.0, 4.0, 2.0);
        let right = rect_region(4.06, 0.0, 8.0, 2.0);
        let region = left.union(&right);
        let rotated = ContourSet::new(
            region
                .rings
                .iter()
                .map(|ring| ring.iter().map(|[x, y]| [-y, *x]).collect())
                .collect(),
            FillRule::NonZero,
            tol::REGION_MM,
        );

        let original = thin_gaps(&region, 0.1);
        let turned = thin_gaps(&rotated, 0.1);

        assert_eq!(original.len(), 1);
        assert_eq!(turned.len(), 1);
        assert!((original[0].width_mm - turned[0].width_mm).abs() < 1e-9);
    }
}
