//! Geometry measurements shared by manufacturability checks.
//!
//! Every query here answers with a [`Distance`]: a signed length between
//! two witness points, carrying the uncertainty of the flattened boundaries
//! it was measured against. Deciding whether a distance violates a limit is
//! the caller's policy, via [`Distance::certainly_below`].

use std::collections::HashMap;

use crate::geom::bbox::BBox;
use crate::geom::dist;
use crate::geom::point::Point;
use crate::geom::region::{ContourSet, Ring, TwoSidedResidualComponent, ring_edges};
use crate::geom::tol;

pub use crate::geom::dist::Distance;

/// Uniform-grid index over a region's boundary segments.
///
/// DFM rules ask whether thousands of points and segments lie within a small
/// clearance of the same composed layer boundary. This keeps those queries
/// local instead of walking every polygon edge for every query. Every
/// answer counts the region boundary as one flattened input.
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
            for cell in corridor_cells(start, end, 0.0, cell_size_mm) {
                cells.entry(cell).or_default().push(index as u32);
            }
        }
        Self {
            cell_size_mm,
            segments,
            cells,
        }
    }

    /// Nearest boundary point no farther than `max_distance_mm` from `point`.
    pub fn nearest_within(&self, point: Point, max_distance_mm: f64) -> Option<Distance> {
        let cells = corridor_cells(point, point, max_distance_mm, self.cell_size_mm);
        self.segments_in(cells)
            .map(|&(start, end)| {
                let (mm, boundary) = dist::point_segment(point, start, end);
                Distance::flattened(mm, point, boundary, 1)
            })
            .filter(|distance| distance.mm <= max_distance_mm + tol::EPSILON_MM)
            .min_by(|left, right| left.mm.total_cmp(&right.mm))
    }

    /// Nearest boundary point no farther than `max_distance_mm` from the
    /// query segment. A segment crossing the boundary measures zero.
    pub fn segment_nearest_within(
        &self,
        start: Point,
        end: Point,
        max_distance_mm: f64,
    ) -> Option<Distance> {
        let cells = corridor_cells(start, end, max_distance_mm, self.cell_size_mm);
        self.segments_in(cells)
            .map(|&(edge_start, edge_end)| {
                let (mm, on_query, on_boundary) = dist::segments(start, end, edge_start, edge_end);
                Distance::flattened(mm, on_query, on_boundary, 1)
            })
            .filter(|distance| distance.mm <= max_distance_mm + tol::EPSILON_MM)
            .min_by(|left, right| left.mm.total_cmp(&right.mm))
    }

    /// The radial material enclosure of a circular cutout whose center lies
    /// in the material, searched out to `max_enclosure_mm`.
    ///
    /// The largest disk centered at `center` inside the material has radius
    /// `d(center, boundary)`, so the enclosure is that distance minus the
    /// cutout radius: signed, negative by the depth the cutout breaches the
    /// material. The witnesses are the cutout boundary and the material
    /// boundary along the nearest direction. `None` means the enclosure
    /// exceeds `max_enclosure_mm`.
    pub fn circular_enclosure(
        &self,
        center: Point,
        cutout_radius_mm: f64,
        max_enclosure_mm: f64,
    ) -> Option<Distance> {
        let search = (cutout_radius_mm + max_enclosure_mm).max(0.0);
        let nearest = self.nearest_within(center, search)?;
        let direction = nearest.second - center;
        let direction = if direction.length() <= f64::EPSILON {
            Point::new(1.0, 0.0)
        } else {
            direction / direction.length()
        };
        Some(Distance::flattened(
            nearest.mm - cutout_radius_mm,
            center + direction * cutout_radius_mm,
            nearest.second,
            1,
        ))
    }

    /// Every indexed segment registered in any of `cells`. A segment spanning
    /// several cells is yielded once per cell; every consumer takes a
    /// minimum, for which repeats are harmless.
    fn segments_in(
        &self,
        cells: impl Iterator<Item = (i64, i64)>,
    ) -> impl Iterator<Item = &(Point, Point)> {
        cells
            .filter_map(|cell| self.cells.get(&cell))
            .flatten()
            .map(|&index| &self.segments[index as usize])
    }
}

fn grid_cell(value: f64, cell_size_mm: f64) -> i64 {
    (value / cell_size_mm).floor() as i64
}

/// Every grid cell within `radius_mm` of the segment, column by column, so
/// long diagonal segments touch a linear number of cells instead of their
/// whole bounding box. A zero-length segment yields the cells around a point.
fn corridor_cells(
    start: Point,
    end: Point,
    radius_mm: f64,
    cell_size_mm: f64,
) -> impl Iterator<Item = (i64, i64)> {
    let min_column = grid_cell(start.x.min(end.x) - radius_mm, cell_size_mm);
    let max_column = grid_cell(start.x.max(end.x) + radius_mm, cell_size_mm);
    let delta = end - start;
    (min_column..=max_column).flat_map(move |column| {
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
        (min_row..=max_row).map(move |row| (column, row))
    })
}

/// Set distance between two closed disks: `max(0, ‖c₁ − c₂‖ − r₁ − r₂)`,
/// with the boundary witness points along the center line. Exact.
pub fn disk_clearance(
    first_center: Point,
    first_radius_mm: f64,
    second_center: Point,
    second_radius_mm: f64,
) -> Distance {
    let delta = second_center - first_center;
    let length = delta.length();
    if length <= f64::EPSILON {
        return Distance::exact(0.0, first_center, second_center);
    }
    let direction = delta / length;
    Distance::exact(
        (length - first_radius_mm - second_radius_mm).max(0.0),
        first_center + direction * first_radius_mm,
        second_center - direction * second_radius_mm,
    )
}

/// Shortest boundary-to-boundary clearance between two filled regions.
///
/// Overlapping, contained, or touching regions have zero clearance. Both
/// boundaries count as flattened inputs. `None` means at least one input is
/// empty: an empty set has no nearest point.
pub fn region_clearance(first: &ContourSet, second: &ContourSet) -> Option<Distance> {
    if first.is_empty() || second.is_empty() {
        return None;
    }

    if let Some(point) = contained_vertex(first, second).or_else(|| contained_vertex(second, first))
    {
        return Some(Distance::flattened(0.0, point, point, 2));
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

fn closest_ring_edges(first: &[Ring], second: &[Ring]) -> Option<Distance> {
    first
        .iter()
        .flat_map(ring_edges)
        .flat_map(|(first_start, first_end)| {
            second.iter().flat_map(move |ring| {
                ring_edges(ring).map(move |(second_start, second_end)| {
                    let (mm, on_first, on_second) =
                        dist::segments(first_start, first_end, second_start, second_end);
                    Distance::flattened(mm, on_first, on_second, 2)
                })
            })
        })
        .min_by(|left, right| left.mm.total_cmp(&right.mm))
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
    /// flattened polygon representation; both branches count as flattened.
    pub width: Distance,
    /// Approximate longitudinal extent (half the residue perimeter).
    pub length_mm: f64,
}

/// Opening and closing use tessellated round offsets. Widening the candidate
/// threshold by the full two-offset plus source-flattening error makes the
/// morphological stage conservative; exact source-boundary distance below
/// then decides the verdict and discards the extra candidates.
const MORPHOLOGY_CANDIDATE_GUARD_MM: f64 = 2.0 * tol::STROKE_OUTLINE_MM + tol::FLATTEN_MM;

/// Filled material certainly narrower than `min_width_mm`. Only two-sided
/// residue is reported, so the bite an isolated convex arc sheds under the
/// opening is not a thin feature. Largest piece first.
pub fn thin_features(region: &ContourSet, min_width_mm: f64) -> Vec<ThinPiece> {
    pieces(
        region.disk_feature_violation_components(
            (min_width_mm + MORPHOLOGY_CANDIDATE_GUARD_MM) / 2.0,
        ),
        min_width_mm,
    )
}

/// Gaps in the material certainly narrower than `min_gap_mm`, including
/// boundary notches. Only two-sided residue is reported, so the bite an
/// isolated concave corner sheds under the closing is not clearance.
/// Largest piece first.
pub fn thin_gaps(region: &ContourSet, min_gap_mm: f64) -> Vec<ThinPiece> {
    pieces(
        region.disk_gap_violation_components((min_gap_mm + MORPHOLOGY_CANDIDATE_GUARD_MM) / 2.0),
        min_gap_mm,
    )
}

/// The narrowest local width of a filled region: the least separation of
/// any two facing boundary branches. An opening wide enough to erase the
/// whole region makes every piece of it a candidate. `None` when no two
/// branches face each other (an empty region, or a single point).
pub fn min_width(region: &ContourSet) -> Option<Distance> {
    let erase_all = 2.0 * region.bbox.width().max(region.bbox.height());
    thin_features(region, erase_all)
        .into_iter()
        .map(|piece| piece.width)
        .min_by(|left, right| left.mm.total_cmp(&right.mm))
}

/// Convert the conservative opening/closing candidates into authoritative
/// measurements. A candidate is reportable only when the exact distance
/// between its opposing source-boundary branches is certainly below the
/// requested minimum; this is what prevents offset tessellation from
/// creating findings.
fn pieces(components: Vec<TwoSidedResidualComponent>, minimum_mm: f64) -> Vec<ThinPiece> {
    let mut pieces = components
        .into_iter()
        .filter(|component| component.width.certainly_below(minimum_mm))
        .map(|component| ThinPiece {
            bbox: component.region.bbox,
            area_mm2: component.region.area(),
            width: component.width,
            length_mm: region_perimeter(&component.region) / 2.0,
        })
        .collect::<Vec<_>>();
    pieces.sort_by(|a, b| b.area_mm2.total_cmp(&a.area_mm2));
    pieces
}

fn region_perimeter(region: &ContourSet) -> f64 {
    region
        .rings
        .iter()
        .flat_map(ring_edges)
        .map(|(start, end)| start.distance_to(end))
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
        assert!((between_regions.mm - 1.5).abs() < 1e-9);
        assert!((between_regions.first.x - 2.0).abs() < 1e-9);
        assert!((between_regions.second.x - 3.5).abs() < 1e-9);

        let index = RegionBoundaryIndex::new(&left, 1.5);
        let from_segment = index
            .segment_nearest_within(Point::new(-1.0, 3.0), Point::new(3.0, 3.0), 1.5)
            .unwrap();
        assert!((from_segment.mm - 1.0).abs() < 1e-9);

        let crossing = index
            .segment_nearest_within(Point::new(-1.0, 1.0), Point::new(3.0, 1.0), 0.5)
            .unwrap();
        assert_eq!(crossing.mm, 0.0);
    }

    #[test]
    fn region_clearance_handles_diagonal_separation_crossing_and_containment() {
        let origin = rect_region(0.0, 0.0, 1.0, 1.0);
        let diagonal = rect_region(2.0, 3.0, 3.0, 4.0);
        let clearance = region_clearance(&origin, &diagonal).unwrap();
        assert!((clearance.mm - 5.0_f64.sqrt()).abs() < 1e-9);
        assert!((origin.bbox.distance_to(diagonal.bbox) - 5.0_f64.sqrt()).abs() < 1e-9);

        let horizontal = rect_region(-2.0, -0.25, 2.0, 0.25);
        let vertical = rect_region(-0.25, -2.0, 0.25, 2.0);
        assert_eq!(
            region_clearance(&horizontal, &vertical).unwrap().mm,
            0.0,
            "crossing regions overlap even when neither contains the other"
        );

        let container = rect_region(-2.0, -2.0, 2.0, 2.0);
        let contained = rect_region(-0.5, -0.5, 0.5, 0.5);
        assert_eq!(region_clearance(&container, &contained).unwrap().mm, 0.0);
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
        assert!((near_middle.mm - 0.3 / 2.0_f64.sqrt()).abs() < 1e-6);
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
        assert!((measurement.mm + 0.15).abs() < tol::FLATTEN_MM);
        assert_eq!(measurement.uncertainty_mm, tol::FLATTEN_MM);
        assert!((measurement.first.x - 2.65).abs() < tol::FLATTEN_MM);
        assert!((measurement.second.x - 2.5).abs() < tol::FLATTEN_MM);
    }

    #[test]
    fn circular_enclosure_handles_noncircular_lands() {
        let copper = rect_region(-3.0, -1.5, 3.0, 1.5);
        let index = RegionBoundaryIndex::new(&copper, 1.6);

        let measurement = index
            .circular_enclosure(Point::default(), 1.0, 0.6)
            .expect("the short side of a rectangular land must set the enclosure");
        assert!((measurement.mm - 0.5).abs() < 1e-9);
        assert!((measurement.second.y.abs() - 1.5).abs() < 1e-9);
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
            (piece.width.mm - 0.05).abs() < 0.02,
            "width {}",
            piece.width.mm
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
            (gaps[0].width.mm - 0.06).abs() < 0.02,
            "width {}",
            gaps[0].width.mm
        );
        assert!(thin_features(&region, 0.1).is_empty());
    }

    #[test]
    fn small_islands_are_not_filtered_out_of_feature_width() {
        let island = rect_region(0.0, 0.0, 0.05, 0.05);

        let findings = thin_features(&island, 0.1);

        assert_eq!(findings.len(), 1);
        assert!((findings[0].width.mm - 0.05).abs() < 1e-9);
        assert!(
            (findings[0]
                .width
                .first
                .distance_to(findings[0].width.second)
                - 0.05)
                .abs()
                < 1e-9
        );
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
    fn min_width_is_limit_free() {
        let stadium = ContourSet::from_contours(
            &[shapes::obround(1.8, 0.6, true).expect("valid obround")],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let width = min_width(&stadium).expect("a stadium has facing walls");
        // The flattened arc's inscribed disk is its apothem, short of the
        // true radius by at most one flattening tolerance per side.
        assert!(
            (0.0..=width.uncertainty_mm).contains(&(0.6 - width.mm)),
            "width {}",
            width.mm
        );
        assert_eq!(width.uncertainty_mm, 2.0 * tol::FLATTEN_MM);

        let plate = rect_region(0.0, 0.0, 10.0, 3.0);
        assert!((min_width(&plate).unwrap().mm - 3.0).abs() < 1e-9);
        assert!(min_width(&ContourSet::empty(tol::REGION_MM)).is_none());
    }

    #[test]
    fn tapered_tip_is_measured_at_its_tip() {
        // A plate with a trapezoidal spur narrowing from 0.4 mm to 0.05 mm:
        // the tip is the width, though its walls are far from parallel.
        let region = ContourSet::from_contours(
            &[ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 4.8)),
                PathCmd::line_to(Point::new(12.0, 4.975)),
                PathCmd::line_to(Point::new(12.0, 5.025)),
                PathCmd::line_to(Point::new(10.0, 5.2)),
                PathCmd::line_to(Point::new(10.0, 10.0)),
                PathCmd::line_to(Point::new(0.0, 10.0)),
                PathCmd::close(),
            ])],
            FillRule::NonZero,
            tol::REGION_MM,
        );

        let findings = thin_features(&region, 0.1);

        assert_eq!(findings.len(), 1);
        let width = findings[0].width;
        assert!((width.mm - 0.05).abs() < 0.01, "width {}", width.mm);
        assert!(
            width.first.x > 11.5 && width.second.x > 11.5,
            "witnesses at the tip"
        );
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
        assert!((original[0].width.mm - turned[0].width.mm).abs() < 1e-9);
    }
}
