//! Regularized planar regions and boolean composition.
//!
//! The flattened polygon form used for boolean set operations is a list of
//! [`Ring`]s (closed polygon boundaries). [`ContourSet`] is the regularized
//! region type built on top: union, difference, intersection, and disk
//! dilation over filled point sets, shared by every dialect so IPC, Gerber,
//! SVG, and comparison all use the same geometry semantics.

mod booleans;
mod construction;
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
pub use flattening::rings_to_contours;
pub(crate) use gaps::TwoSidedResidualComponent;
pub use gaps::{DiskGapRegularization, GapRegularizationError};
pub use query::PreparedRegion;
pub(crate) use simplification::{decimate_rings_inward, simplify_rings};
pub use simplification::{simplify_shapes, simplify_shapes_on_grid};

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

/// Parameter intervals of a segment inside a filled region. Boundary
/// crossings supply exact split points in the flattened representation;
/// midpoint containment then decides each interval, including cutouts.
pub(crate) fn segment_inside_intervals(
    region: &ContourSet,
    start: Point,
    end: Point,
) -> Vec<(f64, f64)> {
    let delta = end - start;
    let length_squared = delta.x * delta.x + delta.y * delta.y;
    if length_squared <= tol::EPSILON_MM * tol::EPSILON_MM {
        return Vec::new();
    }
    let cross = |a: Point, b: Point| a.x * b.y - a.y * b.x;
    let mut stations = vec![0.0, 1.0];
    for (a, b) in region.rings.iter().flat_map(ring_edges) {
        let edge = b - a;
        let denominator = cross(delta, edge);
        if denominator.abs() > tol::EPSILON_MM * delta.length().max(edge.length()) {
            let t = cross(a - start, edge) / denominator;
            let u = cross(a - start, delta) / denominator;
            if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
                stations.push(t);
            }
        } else if cross(a - start, delta).abs() <= tol::EPSILON_MM * delta.length() {
            for point in [a, b] {
                let relative = point - start;
                let t = (relative.x * delta.x + relative.y * delta.y) / length_squared;
                if (0.0..=1.0).contains(&t) {
                    stations.push(t);
                }
            }
        }
    }
    stations.sort_by(f64::total_cmp);
    stations.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
    let midpoints = stations
        .windows(2)
        .map(|pair| start + delta * ((pair[0] + pair[1]) / 2.0))
        .collect::<Vec<_>>();
    region
        .contains_points_batch(&midpoints)
        .into_iter()
        .zip(stations.windows(2))
        .filter_map(|(inside, pair)| inside.then_some((pair[0], pair[1])))
        .collect()
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
    let point = |[x, y]: [f64; 2]| Point::new(x, y);
    ring.iter()
        .copied()
        .zip(ring.iter().skip(1).chain(ring.first()).copied())
        .map(move |(start, end)| (point(start), point(end)))
}

/// Signed area of one ring (positive when counter-clockwise).
pub fn ring_signed_area(ring: &Ring) -> f64 {
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

/// Sutherland-Hodgman clip of a closed ring against a half-plane, kept where
/// `inside` is non-negative.
///
/// A concave ring can come back with edges doubled back along the cut. They
/// enclose nothing, so the signed area of the result is the true clipped area,
/// which is all this is used for.
fn clip_half_plane(ring: &Ring, inside: impl Fn([f64; 2]) -> f64) -> Ring {
    let mut clipped = Ring::new();
    for index in 0..ring.len() {
        let start = ring[index];
        let end = ring[(index + 1) % ring.len()];
        let (from, to) = (inside(start), inside(end));
        if (from < 0.0) != (to < 0.0) {
            let step = from / (from - to);
            clipped.push([
                start[0] + step * (end[0] - start[0]),
                start[1] + step * (end[1] - start[1]),
            ]);
        }
        if to >= 0.0 {
            clipped.push(end);
        }
    }
    clipped
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
        self.rings.is_empty()
    }

    pub fn bbox(&self) -> BBox {
        self.bbox
    }

    /// Net enclosed area.
    pub fn area(&self) -> f64 {
        rings_area(&self.rings)
    }

    /// Whether the region contains each point, as a one-or-zero indicator.
    ///
    /// Testing points one at a time walks every edge per point. This sweeps
    /// them instead: sort by height, and at each height solve the edge
    /// crossings once and share them across every point on that line. Sampling
    /// a region's coverage means asking about tens of thousands of points at
    /// once, where the difference is the difference between usable and not.
    fn contains_points(&self, points: &[Point]) -> Vec<f64> {
        let mut result = vec![0.0; points.len()];
        if self.is_empty() {
            return result;
        }
        // Only a ring whose bounds reach the query line crosses it, and a
        // ring wholly to one side of every point on the line contributes
        // crossings that balance to nothing, so those rings are skipped.
        let crossings_at = |y: f64, min_x: f64, max_x: f64| {
            self.rings
                .iter()
                .zip(&self.ring_bounds)
                .filter(move |(_, bounds)| {
                    bounds.min.y <= y
                        && y <= bounds.max.y
                        && bounds.min.x <= max_x + tol::EPSILON_MM
                        && min_x - tol::EPSILON_MM <= bounds.max.x
                })
                .flat_map(|(ring, _)| ring_edges(ring))
                .filter_map(move |(start, end)| horizontal_crossing(start, end, y))
        };
        let mut by_height = (0..points.len()).collect::<Vec<_>>();
        by_height.sort_by(|left, right| {
            points[*left]
                .y
                .total_cmp(&points[*right].y)
                .then_with(|| points[*left].x.total_cmp(&points[*right].x))
        });

        let mut first = 0;
        while first < by_height.len() {
            let y = points[by_height[first]].y;
            let mut last = first + 1;
            while last < by_height.len() && (points[by_height[last]].y - y).abs() <= tol::EPSILON_MM
            {
                last += 1;
            }
            let (min_x, max_x) = by_height[first..last]
                .iter()
                .map(|&point| points[point].x)
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), x| {
                    (low.min(x), high.max(x))
                });
            let mut crossings = crossings_at(y, min_x, max_x).collect::<Vec<_>>();
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            let mut crossing = 0;
            let mut winding = 0;
            for &point in &by_height[first..last] {
                while crossing < crossings.len() && crossings[crossing].0 <= points[point].x {
                    winding += crossings[crossing].1;
                    crossing += 1;
                }
                result[point] = f64::from(winding != 0);
            }
            first = last;
        }
        result
    }

    /// Test many points against the same region in one sweep.
    ///
    /// This is substantially cheaper than repeated [`Self::contains_point`]
    /// calls for geometry checks over thousands of drill locations. Unlike
    /// `contains_point` it tests the strict interior by winding number:
    /// points on or within tolerance of the boundary may land on either side.
    pub fn contains_points_batch(&self, points: &[Point]) -> Vec<bool> {
        self.contains_points(points)
            .into_iter()
            .map(|coverage| coverage > 0.0)
            .collect()
    }

    /// What fraction of each cell of a regular grid over `bounds` the region
    /// covers, row-major from the bottom-left cell.
    ///
    /// Measured as an area rather than sampled. A periodic fill — a hatch, a
    /// thieving lattice — beats against any sampling pitch and comes back as a
    /// moire pattern that is an artefact of the sampling and not of the copper.
    ///
    /// Area is additive over the rings of a regularized region, holes included
    /// with their sign, so each ring is clipped to the cells its bounds reach
    /// and its signed area accumulated there. Clipping runs a row at a time so a
    /// ring meets only the columns of the band it actually crosses.
    pub fn grid_coverage(&self, bounds: BBox, columns: usize, rows: usize) -> Vec<f64> {
        assert!(columns > 0 && rows > 0, "a grid needs at least one cell");
        let width = bounds.width() / columns as f64;
        let height = bounds.height() / rows as f64;
        let index = |value: f64, origin: f64, span: f64, count: usize| {
            ((value - origin) / span)
                .floor()
                .clamp(0.0, count as f64 - 1.0) as usize
        };
        let mut areas = vec![0.0; columns * rows];
        for (ring, &ring_bbox) in self.rings.iter().zip(&self.ring_bounds) {
            for row in index(ring_bbox.min.y, bounds.min.y, height, rows)
                ..=index(ring_bbox.max.y, bounds.min.y, height, rows)
            {
                let floor = bounds.min.y + row as f64 * height;
                let band =
                    clip_half_plane(&clip_half_plane(ring, |point| point[1] - floor), |point| {
                        floor + height - point[1]
                    });
                let band_bbox = rings_bbox(std::slice::from_ref(&band));
                for column in index(band_bbox.min.x, bounds.min.x, width, columns)
                    ..=index(band_bbox.max.x, bounds.min.x, width, columns)
                {
                    let left = bounds.min.x + column as f64 * width;
                    let cell = clip_half_plane(
                        &clip_half_plane(&band, |point| point[0] - left),
                        |point| left + width - point[0],
                    );
                    areas[row * columns + column] += ring_signed_area(&cell);
                }
            }
        }
        areas.iter().map(|area| area / (width * height)).collect()
    }

    /// What fraction of each cell the region covers.
    ///
    /// Cells are centred on `centers` and share one `(width, height)`. Coverage
    /// is estimated by stratified subsampling rather than by intersecting
    /// geometry, so a trace narrower than a cell contributes its true share
    /// instead of aliasing to nothing or to everything.
    pub fn cell_coverage(&self, centers: &[Point], cell: (f64, f64)) -> Vec<f64> {
        const STRATA: usize = 3;
        let offset = |index: usize, span: f64| ((index as f64 + 0.5) / STRATA as f64 - 0.5) * span;
        let subsamples = centers
            .iter()
            .flat_map(|center| {
                (0..STRATA).flat_map(move |row| {
                    (0..STRATA).map(move |column| {
                        Point::new(
                            center.x + offset(column, cell.0),
                            center.y + offset(row, cell.1),
                        )
                    })
                })
            })
            .collect::<Vec<_>>();
        let coverage = self.contains_points(&subsamples);
        let (tiles, _) = coverage.as_chunks::<{ STRATA * STRATA }>();
        tiles
            .iter()
            .map(|tile| tile.iter().sum::<f64>() / (STRATA * STRATA) as f64)
            .collect()
    }

    /// Portions of `start..end` covered by the region, in query direction.
    ///
    /// The region boundary is covered. Point-only contacts are omitted.
    pub fn segment_spans(&self, start: Point, end: Point) -> Vec<(Point, Point)> {
        let direction = end - start;
        let length = direction.length();
        if self.is_empty() || !start.is_finite() || !end.is_finite() || length == 0.0 {
            return Vec::new();
        }

        let epsilon = self.tolerance().max(tol::EPSILON_MM);
        let parameter_epsilon = (epsilon / length).min(1.0);
        let cross = |left: Point, right: Point| left.x * right.y - left.y * right.x;
        let mut breaks = vec![0.0, 1.0];
        for (edge_start, edge_end) in self.rings.iter().flat_map(ring_edges) {
            let edge = edge_end - edge_start;
            let offset = edge_start - start;
            let denominator = cross(direction, edge);
            let parallel_epsilon = f64::EPSILON * length * edge.length() * 8.0;
            if denominator.abs() <= parallel_epsilon {
                // A coincident edge contributes both ends of its overlap. The
                // midpoint classification below then includes that boundary.
                if cross(direction, offset).abs() <= epsilon * length {
                    let length_squared = length * length;
                    for point in [edge_start, edge_end] {
                        let t = ((point - start).x * direction.x + (point - start).y * direction.y)
                            / length_squared;
                        if t >= -parameter_epsilon && t <= 1.0 + parameter_epsilon {
                            breaks.push(t.clamp(0.0, 1.0));
                        }
                    }
                }
                continue;
            }

            let t = cross(offset, edge) / denominator;
            let u = cross(offset, direction) / denominator;
            if t >= -parameter_epsilon
                && t <= 1.0 + parameter_epsilon
                && u >= -parameter_epsilon
                && u <= 1.0 + parameter_epsilon
            {
                breaks.push(t.clamp(0.0, 1.0));
            }
        }

        breaks.sort_by(f64::total_cmp);
        breaks.dedup_by(|left, right| (*left - *right).abs() <= parameter_epsilon);
        let point_at = |t: f64| start + direction * t;
        let mut spans: Vec<(Point, Point)> = Vec::new();
        for interval in breaks.windows(2) {
            let (from, to) = (interval[0], interval[1]);
            if to - from <= parameter_epsilon || !self.contains_point(point_at((from + to) / 2.0)) {
                continue;
            }
            let next = (point_at(from), point_at(to));
            if let Some(previous) = spans.last_mut()
                && previous.1.distance_to(next.0) <= epsilon
            {
                previous.1 = next.1;
            } else {
                spans.push(next);
            }
        }
        spans
    }

    /// Whether the regularized region contains the point, including its boundary.
    pub fn contains_point(&self, point: Point) -> bool {
        if self.is_empty()
            || point.x < self.bbox.min.x
            || point.x > self.bbox.max.x
            || point.y < self.bbox.min.y
            || point.y > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance().max(tol::EPSILON_MM);
        let rings_near = |reach: f64| {
            self.rings
                .iter()
                .zip(&self.ring_bounds)
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

    /// Whether a closed disk is fully contained in the regularized region.
    ///
    /// The boundary check is exact for the flattened representation used by
    /// `ContourSet` boolean operations.
    pub fn contains_disk(&self, center: Point, radius: f64) -> bool {
        if !radius.is_finite() || radius < 0.0 || !center.is_finite() {
            return false;
        }
        if !self.contains_point(center) {
            return false;
        }
        if radius == 0.0 {
            return true;
        }
        if center.x - radius < self.bbox.min.x
            || center.x + radius > self.bbox.max.x
            || center.y - radius < self.bbox.min.y
            || center.y + radius > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance().max(tol::EPSILON_MM);
        self.rings
            .iter()
            .all(|ring| ring_boundary_distance(ring, center) + epsilon >= radius)
    }
}

pub(crate) fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

/// Where an edge crosses the horizontal line at `y`, with its direction.
fn horizontal_crossing(start: Point, end: Point, y: f64) -> Option<(f64, i32)> {
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
fn ring_winding(ring: &Ring, point: Point) -> i32 {
    ring_edges(ring)
        .filter_map(|(start, end)| horizontal_crossing(start, end, point.y))
        .filter_map(|(x, direction)| (x <= point.x).then_some(direction))
        .sum()
}

fn flatten_shapes(shapes: Vec<Shape>) -> Vec<Ring> {
    shapes.into_iter().flatten().collect()
}

fn ring_boundary_distance(ring: &Ring, point: Point) -> f64 {
    ring_edges(ring)
        .map(|(start, end)| dist::point_segment(point, start, end).0)
        .fold(f64::INFINITY, f64::min)
}
