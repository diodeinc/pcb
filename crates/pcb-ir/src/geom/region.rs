//! Regularized planar regions and boolean composition.
//!
//! The flattened polygon form used for boolean set operations is a list of
//! [`Ring`]s (closed polygon boundaries). [`ContourSet`] is the regularized
//! region type built on top: union, difference, intersection, and disk
//! dilation over filled point sets, shared by every dialect so IPC, Gerber,
//! SVG, and comparison all use the same geometry semantics.

mod booleans;
mod construction;
mod decomposition;
mod flattening;
mod gaps;
mod offsets;
mod query;
mod simplification;
#[cfg(test)]
mod tests;
mod widths;

pub use booleans::PaintComposer;
pub(crate) use booleans::difference_shapes;
pub use construction::flatten_within;
pub use decomposition::decompose_on_grid;
pub use flattening::rings_to_contours;
pub use gaps::{DiskGapRegularization, GapRegularizationError};
pub(crate) use gaps::{TwoSidedResidualComponent, gap_reach_mm};
pub use query::PreparedRegion;
pub use simplification::simplify_shapes;
pub(crate) use simplification::{decimate_rings_inward, simplify_rings};

use i_overlay::core::fill_rule::FillRule as OverlayFillRule;

use crate::geom::bbox::BBox;
use crate::geom::dist;
use crate::geom::point::Point;
use crate::geom::style::FillRule;
use crate::geom::tol;
use crate::geom::{AccuracyError, GeometryAccuracy, Resolution};

pub type Ring = Vec<[f64; 2]>;

/// One connected polygon: an outer ring plus hole rings.
pub type Shape = Vec<Ring>;

/// Parameter intervals of `start..end` the region covers, in query direction.
///
/// The region boundary is covered; point-only contacts are omitted. Boundary
/// crossings supply the split stations in the flattened representation and
/// midpoint containment decides each interval, including cutouts. Stations
/// and intervals within the region tolerance of each other are one.
pub(crate) fn segment_inside_intervals(
    region: &ContourSet,
    start: Point,
    end: Point,
) -> Vec<(f64, f64)> {
    let delta = end - start;
    let length = delta.length();
    if region.is_empty() || !start.is_finite() || !end.is_finite() || length == 0.0 {
        return Vec::new();
    }
    let epsilon = region.tolerance().max(tol::EPSILON_MM);
    let slack = (epsilon / length).min(1.0);
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let station = |t: f64| {
        (-slack..=1.0 + slack)
            .contains(&t)
            .then_some(t.clamp(0.0, 1.0))
    };
    let mut stations = vec![0.0, 1.0];
    for (a, b) in region.edges() {
        let edge = b - a;
        let offset = a - start;
        let denominator = cross(delta, edge);
        if denominator.abs() <= f64::EPSILON * length * edge.length() * 8.0 {
            // A coincident edge contributes both ends of its overlap, and
            // midpoint containment then includes that stretch of boundary.
            if cross(delta, offset).abs() <= epsilon * length {
                stations.extend([a, b].into_iter().filter_map(|point| {
                    let relative = point - start;
                    station((relative.x * delta.x + relative.y * delta.y) / (length * length))
                }));
            }
        } else {
            let along_edge = cross(offset, delta) / denominator;
            let edge_slack = epsilon / edge.length();
            if (-edge_slack..=1.0 + edge_slack).contains(&along_edge) {
                stations.extend(station(cross(offset, edge) / denominator));
            }
        }
    }
    stations.sort_by(f64::total_cmp);
    stations.dedup_by(|next, kept| *next - *kept <= slack);
    // Whatever survived nearest the end stands for the end itself.
    *stations.last_mut().expect("both ends are stations") = 1.0;
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for pair in stations.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if !region.contains_point(start + delta * from.midpoint(to)) {
            continue;
        }
        match intervals.last_mut() {
            Some(previous) if previous.1 == from => previous.1 = to,
            _ => intervals.push((from, to)),
        }
    }
    intervals
}

pub(crate) fn rings_bbox(rings: &[Ring]) -> BBox {
    rings
        .iter()
        .flat_map(|ring| ring.iter())
        .fold(BBox::empty(), |mut bbox, &[x, y]| {
            bbox.include_point(Point::new(x, y));
            bbox
        })
}

/// The closed edge cycle of one ring, as start/end point pairs.
pub fn ring_edges(ring: &Ring) -> impl Iterator<Item = (Point, Point)> + '_ {
    edges_of(ring)
}

/// Signed area of one ring (positive when counter-clockwise).
pub fn ring_signed_area(ring: &Ring) -> f64 {
    signed_area_of(ring)
}

/// [`ring_edges`] of vertices however they are stored.
fn edges_of(ring: &[[f64; 2]]) -> impl Iterator<Item = (Point, Point)> + '_ {
    let point = |[x, y]: [f64; 2]| Point::new(x, y);
    ring.iter()
        .copied()
        .zip(ring.iter().skip(1).chain(ring.first()).copied())
        .map(move |(start, end)| (point(start), point(end)))
}

/// [`ring_signed_area`] of vertices however they are stored.
fn signed_area_of(ring: &[[f64; 2]]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let [origin_x, origin_y] = ring[0];
    let mut area = 0.0;
    for edge in ring[1..].windows(2) {
        let [x0, y0] = edge[0];
        let [x1, y1] = edge[1];
        area += (x0 - origin_x) * (y1 - origin_y) - (x1 - origin_x) * (y0 - origin_y);
    }
    area / 2.0
}

/// Net enclosed area of a regularized ring set (holes are wound opposite the
/// outer boundary, so summing signed areas subtracts them).
pub fn rings_area(rings: &[Ring]) -> f64 {
    rings.iter().map(ring_signed_area).sum::<f64>().abs()
}

/// Regularized filled planar point set.
///
/// A `ContourSet` is always in canonical form: rings are regularized
/// (non-overlapping, holes wound opposite their outer boundary). The winding/fill rule of
/// the *source* geometry matters only at construction; every subsequent
/// operation is a regularized set operation.
#[derive(Debug, Clone)]
pub struct ContourSet {
    /// Bounds of `rings`, fixed at construction like the rings themselves:
    /// a region is built through its constructors, never edited in place.
    pub bbox: BBox,
    pub rings: Vec<Ring>,
    /// Bounds of each ring, indexed like `rings` and fixed with them.
    pub(crate) ring_bounds: Vec<BBox>,
    /// The significance tolerance and approximation budget this region was
    /// prepared at. Every operation that approximates spends from the budget
    /// and every derived region inherits it.
    pub resolution: Resolution,
    /// Accumulated boundary approximation in mm; infinity means unbounded.
    /// Mutating public rings invalidates this history.
    pub uncertainty_mm: f64,
}

impl ContourSet {
    /// The region's own budget applied to its accumulated approximation.
    fn checked(self) -> Result<Self, AccuracyError> {
        self.resolution.accuracy.check(self.uncertainty_mm)?;
        Ok(self)
    }

    /// Significance tolerance and containment slack, in millimetres.
    pub fn tolerance(&self) -> f64 {
        self.resolution.tolerance_mm
    }

    /// The approximation budget every derived region may spend within.
    pub fn budget(&self) -> GeometryAccuracy {
        self.resolution.accuracy
    }

    /// The same region under a different budget. Loosening always succeeds;
    /// tightening succeeds only when the accumulated approximation fits.
    pub fn rebudget(mut self, accuracy: GeometryAccuracy) -> Result<Self, AccuracyError> {
        accuracy.check(self.uncertainty_mm)?;
        self.resolution.accuracy = accuracy;
        Ok(self)
    }

    pub fn is_empty(&self) -> bool {
        self.ring_count() == 0
    }

    pub fn ring_count(&self) -> usize {
        self.rings.len()
    }

    /// The vertices of one ring.
    pub fn ring(&self, index: usize) -> &[[f64; 2]] {
        &self.rings[index]
    }

    /// The vertices of every ring, in ring order.
    pub fn rings(&self) -> impl ExactSizeIterator<Item = &[[f64; 2]]> + Clone {
        self.rings.iter().map(Vec::as_slice)
    }

    /// Every boundary edge, ring after ring.
    pub fn edges(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.rings().flat_map(edges_of)
    }

    /// The rings as owned polygons, for interfaces that take them so.
    pub fn to_rings(&self) -> Vec<Ring> {
        self.rings().map(<[_]>::to_vec).collect()
    }

    pub fn into_rings(self) -> Vec<Ring> {
        self.rings
    }

    pub(crate) fn ring_bounds(&self, index: usize) -> BBox {
        self.ring_bounds[index]
    }

    /// Every ring with its bounds.
    pub(crate) fn bounded_rings(&self) -> impl Iterator<Item = (&[[f64; 2]], BBox)> {
        self.rings().zip(self.ring_bounds.iter().copied())
    }

    /// The rings as the overlay reads them.
    fn overlay_source(&self) -> &[Ring] {
        &self.rings
    }

    pub fn bbox(&self) -> BBox {
        self.bbox
    }

    /// Net enclosed area.
    pub fn area(&self) -> f64 {
        self.rings().map(signed_area_of).sum::<f64>().abs()
    }

    /// Portions of `start..end` the region covers, in query direction. The
    /// region boundary is covered; point-only contacts are omitted.
    pub fn segment_spans(&self, start: Point, end: Point) -> Vec<(Point, Point)> {
        let delta = end - start;
        segment_inside_intervals(self, start, end)
            .into_iter()
            .map(|(from, to)| (start + delta * from, start + delta * to))
            .collect()
    }

    /// Test many points against the same region in one sweep.
    ///
    /// Testing points one at a time walks every edge per point. This sweeps
    /// them instead: sort by height, and at each height solve the edge
    /// crossings once and share them across every point on that line. Unlike
    /// [`Self::contains_point`] it tests the strict interior by winding
    /// number: points on or within tolerance of the boundary may land on
    /// either side. A caller that keeps a [`PreparedRegion`] asks its
    /// [`PreparedRegion::winding`] instead, which agrees point for point.
    pub fn contains_points_batch(&self, points: &[Point]) -> Vec<bool> {
        let mut inside = vec![false; points.len()];
        let mut by_height = (0..points.len()).collect::<Vec<_>>();
        by_height.sort_by(|&left, &right| {
            let (left, right) = (points[left], points[right]);
            left.y.total_cmp(&right.y).then(left.x.total_cmp(&right.x))
        });
        for line in
            by_height.chunk_by(|&left, &right| points[left].y.total_cmp(&points[right].y).is_eq())
        {
            let (first, last) = (points[line[0]], points[line[line.len() - 1]]);
            let y = first.y;
            // Only a ring whose bounds reach the line crosses it, and a ring
            // wholly to one side of every point on the line winds around
            // none of them.
            let mut crossings = self
                .bounded_rings()
                .filter(|(_, bounds)| {
                    bounds.min.y <= y
                        && y <= bounds.max.y
                        && bounds.min.x <= last.x
                        && first.x <= bounds.max.x
                })
                .flat_map(|(ring, _)| edges_of(ring))
                .filter_map(|(start, end)| horizontal_crossing(start, end, y))
                .collect::<Vec<_>>();
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            let mut crossing = 0;
            let mut winding = 0;
            for &point in line {
                while crossing < crossings.len() && crossings[crossing].0 <= points[point].x {
                    winding += crossings[crossing].1;
                    crossing += 1;
                }
                inside[point] = winding != 0;
            }
        }
        inside
    }

    /// What fraction of each cell of a regular grid over `bounds` the region
    /// covers, row-major from the bottom-left cell.
    ///
    /// Measured as an area rather than sampled. A periodic fill — a hatch, a
    /// thieving lattice — beats against any sampling pitch and comes back as a
    /// moire pattern that is an artefact of the sampling and not of the copper.
    ///
    /// The area a cell shares with the region is `∮ clamp(x - left, 0, width)
    /// dy` along the part of the boundary within the cell's row. An edge is
    /// cut where it crosses grid lines, and a piece inside one cell adds the
    /// trapezoid between itself and the cell's left side there and its whole
    /// rise times the cell width to every cell left of it, which one running
    /// sum per row hands out at the end. Holes are wound against their outer
    /// ring, so their sign takes them out. A rise is the difference of the
    /// heights a piece ends at, grid lines and vertices exactly, so the rises
    /// of a boundary that passes a cell by cancel to nothing, not nearly.
    pub fn grid_coverage(&self, bounds: BBox, columns: usize, rows: usize) -> Vec<f64> {
        assert!(columns > 0 && rows > 0, "a grid needs at least one cell");
        let width = bounds.width() / columns as f64;
        let height = bounds.height() / rows as f64;
        // Grid lines strictly between two coordinates, by position.
        let lines_between = |from: f64, to: f64, origin: f64, span: f64, count: usize| {
            let first = ((from.min(to) - origin) / span).floor() + 1.0;
            let last = ((from.max(to) - origin) / span).ceil() - 1.0;
            (first.max(0.0) as usize..(last.min(count as f64) + 1.0).max(0.0) as usize)
                .map(move |line| origin + line as f64 * span)
        };
        // Column `columns` stands for everything right of the grid, whose
        // rise still reaches every cell of its row.
        let stride = columns + 1;
        let mut trapezoids = vec![0.0; columns * rows];
        let mut rises = vec![0.0; stride * rows];
        let mut cuts = Vec::new();
        for (ring, _) in self
            .bounded_rings()
            .filter(|(_, ring_bounds)| ring_bounds.intersects(bounds))
        {
            for (start, end) in edges_of(ring).filter(|(start, end)| start.y != end.y) {
                let delta = end - start;
                cuts.clear();
                cuts.extend([(0.0, start), (1.0, end)]);
                cuts.extend(
                    lines_between(start.y, end.y, bounds.min.y, height, rows).map(|y| {
                        let along = (y - start.y) / delta.y;
                        (along, Point::new(start.x + along * delta.x, y))
                    }),
                );
                cuts.extend(
                    lines_between(start.x, end.x, bounds.min.x, width, columns).map(|x| {
                        let along = (x - start.x) / delta.x;
                        (along, Point::new(x, start.y + along * delta.y))
                    }),
                );
                cuts.sort_by(|left, right| left.0.total_cmp(&right.0));
                for piece in cuts.windows(2) {
                    let (from, to) = (piece[0].1, piece[1].1);
                    let middle = from.midpoint(to);
                    let row = ((middle.y - bounds.min.y) / height).floor();
                    let column = ((middle.x - bounds.min.x) / width).floor();
                    if !(0.0..rows as f64).contains(&row) || column < 0.0 {
                        continue;
                    }
                    let (row, column) = (row as usize, (column as usize).min(columns));
                    let rise = to.y - from.y;
                    rises[row * stride + column] += rise;
                    if column < columns {
                        trapezoids[row * columns + column] +=
                            rise * (middle.x - (bounds.min.x + column as f64 * width));
                    }
                }
            }
        }
        let mut coverage = trapezoids;
        for (row, rises) in coverage.chunks_mut(columns).zip(rises.chunks(stride)) {
            let mut right = rises[columns];
            for (area, rise) in row.iter_mut().zip(rises).rev() {
                *area = (*area + width * right) / (width * height);
                right += rise;
            }
        }
        coverage
    }

    /// Whether the regularized region contains the point, including its boundary.
    pub fn contains_point(&self, point: Point) -> bool {
        if self.is_empty() || !self.bbox.contains_point(point) {
            return false;
        }

        let epsilon = self.tolerance().max(tol::EPSILON_MM);
        let rings_near = |reach: f64| {
            self.bounded_rings()
                .filter(move |(_, bounds)| bounds.expand(reach).contains_point(point))
                .map(|(ring, _)| ring)
        };
        if rings_near(epsilon).any(|ring| ring_boundary_distance(ring, point) <= epsilon) {
            return true;
        }

        // A ring whose bounds miss the point cannot wind around it.
        rings_near(tol::EPSILON_MM)
            .map(|ring| ring_winding(ring, point))
            .sum::<i32>()
            != 0
    }
}

pub(crate) fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

/// Where an edge crosses the horizontal line at `y`, with its direction.
pub(crate) fn horizontal_crossing(start: Point, end: Point, y: f64) -> Option<(f64, i32)> {
    // Half-open in y so a vertex shared by two edges is counted once, which
    // keeps the winding number honest.
    let direction = if start.y <= y && y < end.y {
        1
    } else if end.y <= y && y < start.y {
        -1
    } else {
        return None;
    };
    Some((
        start.x + (y - start.y) * (end.x - start.x) / (end.y - start.y),
        direction,
    ))
}

/// The winding number of one ring around a point.
fn ring_winding(ring: &[[f64; 2]], point: Point) -> i32 {
    edges_of(ring)
        .filter_map(|(start, end)| horizontal_crossing(start, end, point.y))
        .filter_map(|(x, direction)| (x <= point.x).then_some(direction))
        .sum()
}

fn flatten_shapes(shapes: Vec<Shape>) -> Vec<Ring> {
    shapes.into_iter().flatten().collect()
}

fn ring_boundary_distance(ring: &[[f64; 2]], point: Point) -> f64 {
    edges_of(ring)
        .map(|(start, end)| dist::point_segment(point, start, end).0)
        .fold(f64::INFINITY, f64::min)
}
