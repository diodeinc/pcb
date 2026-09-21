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
pub(crate) use gaps::TwoSidedResidualComponent;
pub use gaps::{DiskGapRegularization, GapRegularizationError};
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
    for (a, b) in region.rings.iter().flat_map(ring_edges) {
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
                .rings
                .iter()
                .zip(&self.ring_bounds)
                .filter(|(_, bounds)| {
                    bounds.min.y <= y
                        && y <= bounds.max.y
                        && bounds.min.x <= last.x
                        && first.x <= bounds.max.x
                })
                .flat_map(|(ring, _)| ring_edges(ring))
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
