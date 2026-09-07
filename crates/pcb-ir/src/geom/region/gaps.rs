//! Two-sided morphology residues, local widths, and void-gap regularization.

use super::{
    ContourSet, PreparedRegion, Ring, ring_edges, ring_signed_area, ring_winding,
    segment_inside_intervals, simplify_rings,
};
use crate::geom::accuracy::numerical_error;
use crate::geom::dist::{self, Distance};
use crate::geom::path::{ContourBuf, PathCmd};
use crate::geom::store::PathArena;
use crate::geom::{AccuracyError, BBox, FillRule, Paint, Point, tol};
use std::fmt;

use boostvoronoi::prelude::{
    Builder as VoronoiBuilder, CellIndex as VoronoiCellIndex, Diagram as VoronoiDiagram,
    EdgeIndex as VoronoiEdgeIndex, Line as VoronoiLine, Point as VoronoiPoint, SourceCategory,
    VoronoiVisualUtils,
};
use boostvoronoi::utils::visual_utils::SimpleAffine;

/// Sublevel set of a continuous convex function on [0, 1]. The geometric
/// uses here are sums of point-to-segment distances, so there is at most one
/// interval and bisection locates its ends independently of render sampling.
fn convex_sublevel_interval(value: impl Fn(f64) -> f64, limit: f64) -> Option<(f64, f64)> {
    let (first, last) = (value(0.0), value(1.0));
    if first <= limit && last <= limit {
        return Some((0.0, 1.0));
    }
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..48 {
        let a = (2.0 * low + high) / 3.0;
        let b = (low + 2.0 * high) / 3.0;
        if value(a) < value(b) {
            high = b;
        } else {
            low = a;
        }
    }
    let minimum = (low + high) / 2.0;
    if value(minimum) >= limit {
        return None;
    }
    let left = if first <= limit {
        0.0
    } else {
        let (mut outside, mut inside) = (0.0, minimum);
        for _ in 0..48 {
            let middle = (outside + inside) / 2.0;
            if value(middle) < limit {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        inside
    };
    let right = if last <= limit {
        1.0
    } else {
        let (mut inside, mut outside) = (minimum, 1.0);
        for _ in 0..48 {
            let middle = (inside + outside) / 2.0;
            if value(middle) < limit {
                inside = middle;
            } else {
                outside = middle;
            }
        }
        inside
    };
    Some((left, right))
}

/// Result of enforcing a minimum width for every two-sided void gap.
#[derive(Debug, Clone)]
pub struct DiskGapRegularization {
    /// Input material retained after local gap trimming and disk opening.
    pub kept: ContourSet,
    /// `source \ kept`.
    pub removed: ContourSet,
}

/// One connected opening/closing residue that two distinct source boundary
/// branches wall. `width` is the diameter of the narrowest maximal inscribed
/// disk inside it — the local width of the material or void `region`
/// represents, exact for the flattened polygon representation — counting
/// both branches as flattened inputs.
#[derive(Debug, Clone)]
pub(crate) struct TwoSidedResidualComponent {
    pub region: ContourSet,
    pub width: Distance,
    pub disk: InscribedDisk,
    pub axis: Vec<WidthAxisSegment>,
}

/// Failure to construct a narrow void's medial axis for gap regularization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRegularizationError(String);

impl fmt::Display for GapRegularizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GapRegularizationError {}
impl From<AccuracyError> for GapRegularizationError {
    fn from(error: AccuracyError) -> Self {
        Self(error.to_string())
    }
}

impl ContourSet {
    /// The component of each ring, by ring index, and the outer ring of
    /// each component. Regularized rings nest without crossing and holes
    /// are wound opposite their outer ring, so a hole belongs to the
    /// smallest outer ring around it.
    fn ring_components(&self) -> (Vec<usize>, Vec<usize>) {
        let areas = self.rings.iter().map(ring_signed_area).collect::<Vec<_>>();
        let mut outers = (0..self.rings.len())
            .filter(|&ring| areas[ring] > 0.0)
            .collect::<Vec<_>>();
        outers.sort_by(|&left, &right| areas[left].total_cmp(&areas[right]));
        let mut components = vec![usize::MAX; self.rings.len()];
        for (component, &outer) in outers.iter().enumerate() {
            components[outer] = component;
        }
        for (index, ring) in self.rings.iter().enumerate() {
            if areas[index] > 0.0 {
                continue;
            }
            let Some(&[x, y]) = ring.first() else {
                continue;
            };
            let point = Point::new(x, y);
            if let Some(&outer) = outers.iter().find(|&&outer| {
                self.ring_bounds[outer].contains_point(point)
                    && ring_winding(&self.rings[outer], point) != 0
            }) {
                components[index] = components[outer];
            }
        }
        (components, outers)
    }

    /// The material within `reach_mm` of walls that face each other across
    /// material within `material_mm` or across void within `void_mm`.
    ///
    /// Every ring within reach of such a wall comes along with the outer
    /// ring of its component, and the material is clipped to the reach
    /// around the facing walls. A hole farther away is filled and material
    /// farther away is cut, since neither reaches the facing walls; the
    /// cut's own edges disturb the morphology only within a disk diameter
    /// of themselves, so a reach of three radii keeps the residue at the
    /// facing walls exact.
    ///
    /// Walls face when they are not incident: on one ring, neither adjacent
    /// nor turned the same way, exactly as the width construction judges a
    /// wall pair. Material lies to the left of travel, so two walls of one
    /// component face across material when each has the other's nearest
    /// point on its left and across void when each has it on its right; a
    /// nearest point along a wall's own line, as at the corners of a notch
    /// or of aligned holes, leaves the side open and both reaches apply.
    /// Distinct components always face across void.
    fn facing_components(
        &self,
        material_mm: f64,
        void_mm: f64,
        reach_mm: f64,
    ) -> Result<Self, AccuracyError> {
        let (components, outers) = self.ring_components();
        let segments = source_boundary_segments(self);
        let boundary = PreparedRegion::from_segments(
            segments
                .iter()
                .map(|segment| (segment.start, segment.end))
                .collect(),
            self.uncertainty_mm,
        );
        let sees_left = |wall: &OrientedBoundarySegment, across: Point| {
            let along = wall.end - wall.start;
            let turn = along.x * across.y - along.y * across.x;
            let scale = 1e-6 * along.length() * across.length();
            (turn.abs() > scale).then_some(turn > 0.0)
        };
        let faces = |left: &OrientedBoundarySegment, right: &OrientedBoundarySegment| {
            if left.topology.ring == right.topology.ring
                && (boundary_segments_are_incident(left.topology, right.topology)
                    || left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y >= 0.0)
            {
                return false;
            }
            let same_component = components[left.topology.ring] == components[right.topology.ring];
            let farthest = if same_component {
                material_mm.max(void_mm)
            } else {
                void_mm
            };
            if !left.bbox.expand(farthest).intersects(right.bbox) {
                return false;
            }
            let (separation, nearest_left, nearest_right) =
                dist::segments(left.start, left.end, right.start, right.end);
            let across = nearest_right - nearest_left;
            let reach = match (
                same_component,
                sees_left(left, across),
                sees_left(right, -across),
            ) {
                (false, ..) | (true, Some(false), Some(false)) => void_mm,
                (true, Some(true), Some(true)) => material_mm,
                _ => material_mm.max(void_mm),
            };
            separation <= reach
        };
        let mut facing = vec![false; segments.len()];
        for (id, segment) in segments.iter().enumerate() {
            for other in boundary.segment_ids_meeting(segment.bbox.expand(material_mm.max(void_mm)))
            {
                let partner = &segments[other];
                if other <= id || (facing[id] && facing[other]) || !faces(segment, partner) {
                    continue;
                }
                facing[id] = true;
                facing[other] = true;
            }
        }
        let mut kept = vec![false; self.rings.len()];
        let mut windows = Vec::new();
        for segment in segments
            .iter()
            .zip(&facing)
            .filter_map(|(segment, &facing)| facing.then_some(segment))
        {
            let window = segment.bbox.expand(reach_mm);
            windows.push(vec![
                [window.min.x, window.min.y],
                [window.max.x, window.min.y],
                [window.max.x, window.max.y],
                [window.min.x, window.max.y],
            ]);
            for other in boundary.segment_ids_meeting(window) {
                kept[segments[other].topology.ring] = true;
            }
        }
        for (ring, &component) in components.iter().enumerate() {
            if kept[ring] {
                kept[outers[component]] = true;
            }
        }
        let material = Self::from_regularized(
            self.rings
                .iter()
                .zip(&kept)
                .filter(|&(_, &keep)| keep)
                .map(|(ring, _)| ring.clone())
                .collect(),
            self.resolution,
            self.uncertainty_mm,
        );
        material.intersection(&Self::from_regularized(
            simplify_rings(windows, FillRule::NonZero),
            self.resolution,
            0.0,
        ))
    }
    /// Diagnose distinct components whose Euclidean separation is less than
    /// the disk diameter `2 * radius`.
    ///
    /// For connected components `C_i`, returns
    /// `⋃_{i<j} (((C_i ⊕ D_r) ∩ (C_j ⊕ D_r)) \ self)` for pairs with
    /// `distance(C_i, C_j) < 2r`. Subtracting `self` localizes the diagnostic
    /// to the intervening void. Bounding-box pruning and segment distance avoid
    /// constructing dilations for non-conflicting pairs.
    ///
    /// Verification-only diagnostic: production gap analysis goes through
    /// [`ContourSet::disk_gap_violations`], which also covers gaps within one
    /// connected component.
    #[cfg(test)]
    pub(crate) fn disk_inter_component_gap_violations(
        &self,
        radius: f64,
    ) -> Result<Self, AccuracyError> {
        if self.is_empty() || radius <= 0.0 {
            return Ok(Self::empty(self.resolution));
        }

        let components = self.connected_components();
        let mut violations = Self::empty(self.resolution);
        for (index, left) in components.iter().enumerate() {
            for right in &components[index + 1..] {
                if !regions_within_distance(left, right, 2.0 * radius) {
                    continue;
                }
                let overlap = left
                    .disk_dilate(radius)?
                    .intersection(&right.disk_dilate(radius)?)?
                    .difference(self)?;
                violations = violations.union(&overlap)?;
            }
        }
        Ok(violations)
    }

    /// Enforce a diameter-`2 * gap_radius` minimum for every two-sided void gap.
    ///
    /// The result is the fixed point, reached from `self`, of one pure
    /// contraction `T` that removes a guard-widened tube around the boundary
    /// medial axis inside the narrow void phase and reopens with the
    /// filled-region disk:
    ///
    /// ```text
    /// T(S) = open(S \ (Γ(G_gap_radius(S)) ⊕ disk(gap_radius + guard)), disk(filled_radius)) ∩ S
    /// ```
    ///
    /// `G_r(X)` is the two-sided part of `close(X, disk(r)) \ X`, and `Γ(N)`
    /// is the boundary medial axis inside that phase — the least local cut,
    /// trimming both sides of every narrow gap without widening one-sided edge
    /// clearance. The guard keeps every checked quantity strictly separated
    /// from every constructed one: a cut leaves a `2 (gap_radius + guard)`
    /// void, so construction noise cannot push a trimmed gap back under the
    /// nominal test. `T` only removes material, so iterating from the source
    /// converges; a step that removes almost nothing is reported as an error
    /// instead of a silent stall.
    pub fn disk_regularize_gaps(
        &self,
        gap_radius: f64,
        filled_radius: f64,
        guard: f64,
    ) -> Result<DiskGapRegularization, GapRegularizationError> {
        if !gap_radius.is_finite()
            || gap_radius <= 0.0
            || !filled_radius.is_finite()
            || filled_radius <= 0.0
        {
            return Err(GapRegularizationError(
                "gap and filled-region radii must be finite and positive".to_string(),
            ));
        }
        if !guard.is_finite() || guard < 0.0 {
            return Err(GapRegularizationError(
                "gap-regularization guard must be finite and non-negative".to_string(),
            ));
        }

        let mut kept = self.clone();
        while let Some(next) = kept.narrow_gap_trim(gap_radius, filled_radius, guard)? {
            if kept.difference(&next)?.area() <= self.tolerance().powi(2) {
                return Err(GapRegularizationError(format!(
                    "gap regularization stalled with {:.9} mm² of void-gap violations",
                    kept.disk_gap_violations(gap_radius)?.area()
                )));
            }
            kept = next;
        }
        Ok(DiskGapRegularization {
            removed: self.difference(&kept)?,
            kept: kept.checked()?,
        })
    }

    /// One application of the gap-regularization contraction `T`, or `None`
    /// at its fixed point, where every two-sided void gap already admits the
    /// rolling disk.
    fn narrow_gap_trim(
        &self,
        gap_radius: f64,
        filled_radius: f64,
        guard: f64,
    ) -> Result<Option<Self>, GapRegularizationError> {
        let narrow_voids = self.disk_gap_violations(gap_radius)?;
        if narrow_voids.is_empty() {
            return Ok(None);
        }
        let keep_out = narrow_void_keep_out(self, &narrow_voids, gap_radius + guard)?;
        Ok(Some(
            self.difference(&keep_out)?
                .disk_open(filled_radius)?
                .intersection(self)?,
        ))
    }

    /// Unfilled material that violates the two-sided void-gap radius.
    ///
    /// The raw closing residual `close(self, disk(radius)) \ self` also contains
    /// the rounded bite at an isolated concave corner. A residual component is
    /// a gap when it contacts nonincident, separated source-boundary segments
    /// on distinct rings, or on one ring with opposing tangents — the latter
    /// distinguishes hairpins and notches from the bite of a single smooth
    /// concavity. An empty result proves no two facing boundary branches fail
    /// the rolling-disk test.
    pub fn disk_gap_violations(&self, radius: f64) -> Result<Self, AccuracyError> {
        if self.is_empty() || !(radius > 0.0 && radius.is_finite()) {
            return Ok(Self::empty(self.resolution));
        }
        two_sided_gap_residual(self, &closing_residual(self, radius)?).checked()
    }

    /// Connected material residues of the opening by `radius` whose local
    /// width can be under `width_mm`, with the local width of each.
    pub(crate) fn disk_feature_violation_components(
        &self,
        radius: f64,
        width_mm: f64,
    ) -> Result<Vec<TwoSidedResidualComponent>, AccuracyError> {
        // A width is a disk touching two facing walls, so it is at least
        // their separation: material whose walls never face each other that
        // closely has no width to find. Any width lies within a radius of
        // its walls, and the disks through a residue point reach a diameter
        // from it, so only the material within a few radii of facing walls
        // decides the residue there. Separate components share a width only
        // where they touch within tolerance. The snap-rounded width
        // construction moves walls by a tolerance, and `M \ (X ∩ M)` is
        // `M \ X`, so the opening's clip to the source is not needed to
        // find what the opening removed.
        self.two_sided_residual(radius, |region, radius| {
            let facing = width_mm + 4.0 * region.tolerance();
            let touching = 3.0 * region.tolerance() + region.uncertainty_mm;
            let reach = 3.0 * (radius + 2.0 * region.tolerance());
            let candidates = region.facing_components(facing, touching, reach)?;
            candidates.difference(&candidates.disk_erode(radius)?.disk_dilate(radius)?)
        })
    }

    /// Connected void residues of the closing by `radius` whose local width
    /// can be under `width_mm`, with the local width of each.
    pub(crate) fn disk_gap_violation_components(
        &self,
        radius: f64,
        width_mm: f64,
    ) -> Result<Vec<TwoSidedResidualComponent>, AccuracyError> {
        // A gap is a disk touching two walls facing across void, so it is
        // at least their separation. A residue point lies within a radius
        // of its walls and the disks that decide it reach a diameter
        // further, so the closing of the material within two diameters of
        // facing walls is the closing of the whole region there.
        self.two_sided_residual(radius, |region, radius| {
            let facing = width_mm + 4.0 * region.tolerance();
            let reach = 4.0 * (radius + 2.0 * region.tolerance());
            let candidates = region.facing_components(0.0, facing, reach)?;
            closing_residual(&candidates, radius)
        })
    }

    /// Morphological residue of this region, kept only where two distinct
    /// source-boundary branches wall it. A degenerate disk has no residue.
    fn two_sided_residual(
        &self,
        radius: f64,
        residual: impl FnOnce(&Self, f64) -> Result<Self, AccuracyError>,
    ) -> Result<Vec<TwoSidedResidualComponent>, AccuracyError> {
        if self.is_empty() || !(radius > 0.0 && radius.is_finite()) {
            return Ok(Vec::new());
        }
        Ok(two_sided_residual_components(
            self,
            &residual(self, radius)?,
            radius,
            self.budget().allowance(self.uncertainty_mm)?,
        ))
    }
}

const VORONOI_COORDINATES_PER_MM: f64 = 100_000.0;

#[derive(Debug, Clone, Copy)]
struct BoundarySegment {
    ring: usize,
    index: usize,
    ring_len: usize,
}

#[derive(Debug, Clone, Copy)]
struct OrientedBoundarySegment {
    topology: BoundarySegment,
    start: Point,
    end: Point,
    /// Source-ring direction averaged across one flattening-tolerance on
    /// either side. Sub-resolution backtracking must not turn one wall into
    /// two opposing walls.
    tangent: Point,
    bbox: BBox,
}

fn closing_residual(region: &ContourSet, radius: f64) -> Result<ContourSet, AccuracyError> {
    region.disk_close(radius)?.difference(region)
}

/// Every source boundary edge longer than the region tolerance, in ring
/// order. The kept edges are numbered consecutively, so two that meet
/// across a dropped sub-tolerance edge remain adjacent.
fn source_boundary_segments(source: &ContourSet) -> Vec<OrientedBoundarySegment> {
    source
        .rings
        .iter()
        .enumerate()
        .flat_map(|(ring_id, ring)| {
            let metric = RingArcLength::new(ring);
            // Keep the two-sided chord local even when the whole ring is
            // smaller than the ordinary geometry resolution.
            let tangent_radius = source
                .uncertainty_mm
                .max(source.tolerance())
                .max(numerical_error(source.bbox))
                .min(metric.perimeter() / 8.0);
            let kept = ring_edges(ring)
                .enumerate()
                .filter(|(_, (start, end))| start.distance_to(*end) > source.tolerance())
                .collect::<Vec<_>>();
            let ring_len = kept.len();
            kept.into_iter()
                .enumerate()
                .map(
                    move |(index, (edge, (start, end)))| OrientedBoundarySegment {
                        topology: BoundarySegment {
                            ring: ring_id,
                            index,
                            ring_len,
                        },
                        start,
                        end,
                        tangent: metric.edge_tangent(edge, tangent_radius),
                        bbox: BBox::spanning(start, end),
                    },
                )
        })
        .collect()
}

/// Canonical arc-length parameterization of a closed polygonal ring.
/// Consecutive entries are the stations at the ends of each source edge.
struct RingArcLength<'a> {
    ring: &'a Ring,
    stations: Vec<f64>,
}

impl<'a> RingArcLength<'a> {
    fn new(ring: &'a Ring) -> Self {
        let stations = std::iter::once(0.0)
            .chain(ring_edges(ring).scan(0.0, |station, (start, end)| {
                *station += start.distance_to(end);
                Some(*station)
            }))
            .collect();
        Self { ring, stations }
    }

    fn perimeter(&self) -> f64 {
        self.stations[self.ring.len()]
    }

    /// Point at a periodic arc-length station. Selecting by edge-end station
    /// skips zero-length edges as a consequence of the parameterization.
    fn point_at(&self, station: f64) -> Point {
        let station = station.rem_euclid(self.perimeter());
        let index = self.stations[1..].partition_point(|&end| end <= station);
        let start_station = self.stations[index];
        let end_station = self.stations[index + 1];
        let [start_x, start_y] = self.ring[index];
        let [end_x, end_y] = self.ring[(index + 1) % self.ring.len()];
        let start = Point::new(start_x, start_y);
        let end = Point::new(end_x, end_y);
        start + (end - start) * ((station - start_station) / (end_station - start_station))
    }

    /// Direction at one edge, measured as the chord between equal arc-length
    /// offsets around its midpoint. Tiny reversals therefore retain the
    /// direction of their resolution-scale wall instead of becoming an
    /// opposing wall.
    fn edge_tangent(&self, index: usize, radius: f64) -> Point {
        let midpoint = (self.stations[index] + self.stations[index + 1]) / 2.0;
        self.point_at(midpoint + radius) - self.point_at(midpoint - radius)
    }
}

/// Each connected component of `residual` that two distinct source-boundary
/// branches wall, with its local width.
///
/// A point of the residue is nearer than `reach` to the source boundary —
/// no disk of that radius covers it — so the boundary segments within
/// `reach` of the component are every segment its inscribed disks can
/// touch, and their Voronoi diagram restricted to the component is the
/// medial axis there. The narrowest maximal inscribed disk on that axis is
/// the component's width. Disks tangent only to incident segments are
/// corner spokes, not widths: discarding those leaves one-sided residue —
/// the bite an isolated corner sheds — with no width at all.
fn two_sided_residual_components(
    source: &ContourSet,
    residual: &ContourSet,
    reach: f64,
    approximation_mm: f64,
) -> Vec<TwoSidedResidualComponent> {
    if residual.is_empty() {
        return Vec::new();
    }
    let segments = source_boundary_segments(source);
    let boundary = PreparedRegion::from_segments(
        segments
            .iter()
            .map(|segment| (segment.start, segment.end))
            .collect(),
        source.uncertainty_mm,
    );
    let contact_tolerance = source
        .tolerance()
        .max(residual.tolerance())
        .max(numerical_error(source.bbox));
    let boundary_uncertainty = source.uncertainty_mm + std::f64::consts::SQRT_2 * contact_tolerance;
    residual
        .connected_components()
        .into_iter()
        .filter_map(|component| {
            let sites = boundary
                .segment_ids_meeting(component.bbox.expand(reach))
                .into_iter()
                .map(|id| segments[id])
                .collect::<Vec<_>>();
            component_width(
                &sites,
                &component,
                reach,
                contact_tolerance,
                approximation_mm,
                boundary_uncertainty,
            )
            .map(|geometry| TwoSidedResidualComponent {
                region: component,
                width: geometry
                    .disk
                    .width()
                    .also_uncertain(2.0 * boundary_uncertainty),
                disk: geometry.disk,
                axis: geometry.axis,
            })
        })
        .collect()
}

/// A boundary segment snapped to the tolerance grid, with its topology.
type GridSite = (VoronoiLine<i32>, OrientedBoundarySegment);

/// The sites snap-rounded to a planar set on the tolerance grid. The
/// Voronoi builder accepts segments that meet only at endpoints, while
/// regularized rings may touch along a seam (two rings sharing an edge),
/// at a vertex on another ring's edge, or fold into hairpins narrower than
/// the tolerance. On the grid, points within tolerance are one point:
/// zero-length and duplicate segments drop, a segment splits where another
/// endpoint lies on it, and of two segments that still cross — noise at
/// tolerance scale — the shorter drops. Pieces keep their parent's topology
/// and so remain incident to each other and to its neighbors.
fn planar_grid_sites(
    sites: &[OrientedBoundarySegment],
    quantize: impl Fn(Point) -> VoronoiPoint<i32>,
) -> Vec<GridSite> {
    let orient = |a: VoronoiPoint<i32>, b: VoronoiPoint<i32>, c: VoronoiPoint<i32>| -> i128 {
        (i128::from(b.x) - i128::from(a.x)) * (i128::from(c.y) - i128::from(a.y))
            - (i128::from(b.y) - i128::from(a.y)) * (i128::from(c.x) - i128::from(a.x))
    };
    let on_interior = |line: &VoronoiLine<i32>, point: VoronoiPoint<i32>| {
        point != line.start
            && point != line.end
            && orient(line.start, line.end, point) == 0
            && (line.start.x.min(line.end.x)..=line.start.x.max(line.end.x)).contains(&point.x)
            && (line.start.y.min(line.end.y)..=line.start.y.max(line.end.y)).contains(&point.y)
    };
    let length2 = |line: &VoronoiLine<i32>| {
        let dx = i128::from(line.end.x) - i128::from(line.start.x);
        let dy = i128::from(line.end.y) - i128::from(line.start.y);
        dx * dx + dy * dy
    };
    let crosses = |a: &VoronoiLine<i32>, b: &VoronoiLine<i32>| {
        orient(a.start, a.end, b.start).signum() * orient(a.start, a.end, b.end).signum() < 0
            && orient(b.start, b.end, a.start).signum() * orient(b.start, b.end, a.end).signum() < 0
    };
    // Sites keep their ring's traversal direction; a segment and its
    // reverse are one site.
    let key = |line: &VoronoiLine<i32>| {
        let (a, b) = ((line.start.x, line.start.y), (line.end.x, line.end.y));
        (a.min(b), a.max(b))
    };
    let snapped = sites
        .iter()
        .map(|site| {
            (
                VoronoiLine::new(quantize(site.start), quantize(site.end)),
                *site,
            )
        })
        .filter(|(line, _)| line.start != line.end)
        .collect::<Vec<_>>();
    let endpoints = snapped
        .iter()
        .flat_map(|(line, _)| [line.start, line.end])
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    let split = snapped
        .iter()
        .flat_map(|&(line, source)| {
            let mut stations = endpoints
                .iter()
                .copied()
                .filter(|&point| on_interior(&line, point))
                .collect::<Vec<_>>();
            // Collinear with the segment, so distance from its start orders
            // them along it whichever way it runs.
            let along = |point: &VoronoiPoint<i32>| {
                (i64::from(point.x) - i64::from(line.start.x)).abs()
                    + (i64::from(point.y) - i64::from(line.start.y)).abs()
            };
            stations.sort_by_key(along);
            stations.dedup();
            std::iter::once(line.start)
                .chain(stations)
                .chain(std::iter::once(line.end))
                .collect::<Vec<_>>()
                .windows(2)
                .map(|pair| (VoronoiLine::new(pair[0], pair[1]), source))
                .collect::<Vec<_>>()
        })
        .filter(|(line, _)| seen.insert(key(line)))
        .collect::<Vec<_>>();
    split
        .iter()
        .enumerate()
        .filter(|(index, (line, _))| {
            !split.iter().enumerate().any(|(other_index, (other, _))| {
                other_index != *index
                    && crosses(line, other)
                    && (length2(other), other_index) > (length2(line), *index)
            })
        })
        .map(|(_, site)| *site)
        .collect()
}

/// Whether every two sites are incident, and so one wall: segments of one
/// ring that are adjacent or turned no more than a quarter turn from each
/// other, the same judgement the width construction makes of a wall pair.
/// Nothing in such a set faces anything else, so a residue it walls alone
/// is the bite of one corner or gentle arc and has no width. A sharp tip
/// turns its walls to face each other and is measured.
fn one_wall(sites: &[OrientedBoundarySegment]) -> bool {
    sites.iter().enumerate().all(|(position, left)| {
        sites[position + 1..].iter().all(|right| {
            left.topology.ring == right.topology.ring
                && (boundary_segments_are_incident(left.topology, right.topology)
                    || left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y >= 0.0)
        })
    })
}

/// A maximal inscribed disk of the boundary's Voronoi diagram: its center,
/// radius, and the two tangency points on distinct walls.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InscribedDisk {
    pub center: Point,
    pub radius: f64,
    pub first: Point,
    pub second: Point,
}

impl InscribedDisk {
    fn width(self) -> Distance {
        Distance::exact(2.0 * self.radius, self.first, self.second)
    }
}

/// One cell of the boundary medial axis, with the two walls defining it.
/// A vertex has equal start/end points; curved edges are polylines within the
/// shared flattening tolerance. These are candidates until clipped to the
/// residue and the requested width.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WidthAxisSegment {
    pub start: Point,
    pub end: Point,
    pub first_wall: (Point, Point),
    pub second_wall: (Point, Point),
    pub uncertainty_mm: f64,
}

struct ComponentWidth {
    disk: InscribedDisk,
    axis: Vec<WidthAxisSegment>,
}

/// The narrowest maximal inscribed disk of `sites` inside `component`, as a
/// width between its two tangency points.
///
/// Candidates are the Voronoi vertices, and every non-incident edge sampled
/// along its length, at the apex of a parabolic or point–point edge, and
/// where it crosses the component boundary — the clearance along an edge is
/// linear or convex, so its minimum over the inside is at one of those.
/// Flattening a curve sprouts axis branches the source does not have, whose
/// disks sit inside a neighbor's disk up to the flattening tolerance; those
/// are pruned, so a flattened arc measures its diameter and a taper keeps
/// its tip.
fn component_width(
    sites: &[OrientedBoundarySegment],
    component: &ContourSet,
    reach: f64,
    contact_tolerance: f64,
    approximation_mm: f64,
    boundary_uncertainty: f64,
) -> Option<ComponentWidth> {
    if one_wall(sites) {
        return None;
    }
    let origin = component.bbox.min;
    let units_per_mm = 1.0 / contact_tolerance;
    let quantize = |point: Point| {
        VoronoiPoint::new(
            ((point.x - origin.x) * units_per_mm).round() as i32,
            ((point.y - origin.y) * units_per_mm).round() as i32,
        )
    };
    let unquantize =
        |[x, y]: [f64; 2]| Point::new(x / units_per_mm + origin.x, y / units_per_mm + origin.y);
    let grid = planar_grid_sites(sites, quantize);
    let lines = grid.iter().map(|(line, _)| *line).collect::<Vec<_>>();
    let sites = grid
        .iter()
        .map(|(line, source)| {
            let start = unquantize([f64::from(line.start.x), f64::from(line.start.y)]);
            let end = unquantize([f64::from(line.end.x), f64::from(line.end.y)]);
            OrientedBoundarySegment {
                topology: source.topology,
                start,
                end,
                tangent: source.tangent,
                bbox: BBox::spanning(start, end),
            }
        })
        .collect::<Vec<_>>();
    if lines.len() < 2 {
        return None;
    }
    // Which sites are one wall. A ring's two sides of a channel are
    // traversed in opposite directions, so two segments of one ring face
    // each other only when their directions oppose; ring-adjacent segments
    // and segments whose resolution-scale tangents are no more than a
    // quarter turn apart are the same wall, as a disk touching both edges
    // of a square corner is a corner disk, not a width. The averaged
    // tangent prevents a microscopic reversal from manufacturing an
    // opposing branch. Segments of different rings are one wall only where
    // they touch.
    let incident = |i: usize, j: usize| {
        let (a, b) = (&sites[i], &sites[j]);
        if a.topology.ring == b.topology.ring {
            boundary_segments_are_incident(a.topology, b.topology)
                || a.tangent.x * b.tangent.x + a.tangent.y * b.tangent.y >= 0.0
        } else {
            [lines[i].start, lines[i].end]
                .iter()
                .any(|point| [lines[j].start, lines[j].end].contains(point))
        }
    };
    let component_edges = component
        .rings
        .iter()
        .flat_map(ring_edges)
        .collect::<Vec<_>>();
    // A maximal disk centered in the component is no larger than the
    // component's clearance to its nearest wall, which the farthest vertex
    // from that wall bounds. Both walls of a width therefore lie within that
    // reach of the component; a corner's own bite is walled by its two edges
    // alone and never reaches the far side of the feature. Any disk within
    // the morphology reach that touches two eligible walls also proves those
    // walls are no farther apart than its diameter. Allow one contact
    // tolerance at each wall for the snap-rounded residual, and the chord
    // deviation of a sampled curved axis, then leave every surviving
    // measurement to the Voronoi construction below.
    let candidate_diameter = 2.0 * (reach + contact_tolerance);
    let farthest_vertex = |site: &OrientedBoundarySegment| {
        component
            .rings
            .iter()
            .flat_map(|ring| ring.iter())
            .map(|&[x, y]| dist::point_segment(Point::new(x, y), site.start, site.end).0)
            .fold(0.0, f64::max)
    };
    let clearance_bound = sites
        .iter()
        .map(farthest_vertex)
        .fold(f64::INFINITY, f64::min)
        + 2.0 * contact_tolerance
        + approximation_mm;
    let within_reach = sites
        .iter()
        .map(|site| {
            component_edges.iter().any(|&(start, end)| {
                dist::segments(start, end, site.start, site.end).0 <= clearance_bound
            })
        })
        .collect::<Vec<_>>();
    let has_reachable_wall_pair = (0..sites.len())
        .flat_map(|first| (first + 1..sites.len()).map(move |second| (first, second)))
        .any(|(first, second)| {
            within_reach[first]
                && within_reach[second]
                && !incident(first, second)
                && dist::segments(
                    sites[first].start,
                    sites[first].end,
                    sites[second].start,
                    sites[second].end,
                )
                .0 <= candidate_diameter
        });
    if !has_reachable_wall_pair {
        return None;
    }
    let diagram = VoronoiBuilder::<i32>::default()
        .with_segments(lines.iter())
        .and_then(VoronoiBuilder::build)
        .expect("snap-rounded boundary segments do not cross");
    // A cell's site index, and its point when the site is a segment end.
    let site_of = |cell: VoronoiCellIndex| {
        let cell = diagram.cell(cell).expect("diagram cell");
        let index = cell.source_index().usize();
        let point = match cell.source_category() {
            SourceCategory::SegmentStart => Some(sites[index].start),
            SourceCategory::SegmentEnd => Some(sites[index].end),
            SourceCategory::Segment | SourceCategory::SinglePoint => None,
        };
        (index, point)
    };
    let disk = |center: Point, first: usize, second: usize| {
        let (first_distance, first) =
            dist::point_segment(center, sites[first].start, sites[first].end);
        let (second_distance, second) =
            dist::point_segment(center, sites[second].start, sites[second].end);
        InscribedDisk {
            center,
            radius: (first_distance + second_distance) / 2.0,
            first,
            second,
        }
    };

    // Vertices: tangent to every site around them; a width needs two that
    // are not incident.
    let at_vertices = diagram
        .vertices()
        .iter()
        .filter_map(|vertex| {
            let around = diagram
                .edge_rot_next_iterator(vertex.get_incident_edge().ok()?)
                .filter_map(|edge| diagram.edge(edge).ok()?.cell().ok())
                .map(|cell| site_of(cell).0)
                .collect::<Vec<_>>();
            around
                .iter()
                .enumerate()
                .flat_map(|(position, &first)| {
                    around[position + 1..]
                        .iter()
                        .map(move |&second| (first, second))
                })
                .find(|&(first, second)| !incident(first, second))
                .map(|(first, second)| {
                    let center = unquantize([vertex.x(), vertex.y()]);
                    (
                        disk(center, first, second),
                        WidthAxisSegment {
                            start: center,
                            end: center,
                            first_wall: (sites[first].start, sites[first].end),
                            second_wall: (sites[second].start, sites[second].end),
                            uncertainty_mm: 0.0,
                        },
                    )
                })
        })
        .collect::<Vec<_>>();

    // Edges between non-incident sites, sampled along their length and at
    // the apex of a parabola (reflex vertex against a segment) or of a
    // point–point bisector; plus where they cross the component boundary.
    let (mut on_axis, mut on_boundary) = (Vec::new(), Vec::new());
    let mut axis = Vec::new();
    for edge in diagram.edges() {
        let twin = edge.twin().expect("diagram edge twin");
        let (first, first_point) = site_of(edge.cell().expect("diagram edge cell"));
        let (second, second_point) = site_of(
            diagram
                .edge(twin)
                .and_then(|twin| twin.cell())
                .expect("twin cell"),
        );
        if edge.id() > twin || !edge.is_primary() || incident(first, second) {
            continue;
        }
        let samples = voronoi_edge_samples(
            &diagram,
            edge.id(),
            &lines,
            component,
            reach,
            units_per_mm,
            approximation_mm,
        )
        .expect("voronoi edge samples")
        .into_iter()
        .map(unquantize)
        .collect::<Vec<_>>();
        axis.extend(samples.windows(2).map(|pair| WidthAxisSegment {
            start: pair[0],
            end: pair[1],
            first_wall: (sites[first].start, sites[first].end),
            second_wall: (sites[second].start, sites[second].end),
            uncertainty_mm: if edge.is_curved() {
                approximation_mm
            } else {
                0.0
            },
        }));
        let foot = |point: Point, site: usize| {
            dist::point_segment(point, sites[site].start, sites[site].end).1
        };
        let apex = match (first_point, second_point) {
            (Some(point), Some(other)) => Some(point.midpoint(other)),
            (Some(point), None) => Some(point.midpoint(foot(point, second))),
            (None, Some(point)) => Some(point.midpoint(foot(point, first))),
            (None, None) => None,
        };
        on_boundary.extend(samples.windows(2).flat_map(|pair| {
            component_edges.iter().filter_map(move |&(start, end)| {
                let (distance, on_edge, _) = dist::segments(pair[0], pair[1], start, end);
                (distance <= contact_tolerance).then(|| disk(on_edge, first, second))
            })
        }));
        on_axis.extend(
            samples
                .into_iter()
                .chain(apex)
                .map(|center| disk(center, first, second)),
        );
    }
    on_axis.extend(at_vertices.iter().map(|(disk, _)| *disk));

    // A disk is maximal only if its radius is its clearance to every site;
    // the builder's degenerate edges can put a center next to a wall it
    // does not touch.
    let clearance = |center: Point| {
        sites
            .iter()
            .map(|site| dist::point_segment(center, site.start, site.end).0)
            .fold(f64::INFINITY, f64::min)
    };
    let centers = on_axis.iter().map(|disk| disk.center).collect::<Vec<_>>();
    let present = on_axis
        .into_iter()
        .zip(component.contains_points_batch(&centers))
        .filter_map(|(disk, inside)| inside.then_some(disk))
        .chain(on_boundary)
        .filter(|disk| disk.radius <= clearance(disk.center) + contact_tolerance)
        .collect::<Vec<_>>();
    // A disk inside a larger present disk up to the flattening tolerance is
    // a branch the flattening sprouted, not a width of the source. Only
    // present disks prune: a void's exterior axis must not swallow a slit
    // thinner than the tolerance.
    let pruned = |disk: &InscribedDisk| {
        present.iter().any(|other| {
            other.radius > disk.radius
                && disk.center.distance_to(other.center) + disk.radius
                    <= other.radius + approximation_mm
        })
    };
    let minimum = present
        .iter()
        .filter(|disk| !pruned(disk))
        // The disk must survive the uncertainty of its boundary sites.
        .filter(|disk| disk.radius > boundary_uncertainty)
        .min_by(|left, right| left.width().mm.total_cmp(&right.width().mm))
        .copied()?;
    // Retain the actual interior axis, removing the portions rejected by the
    // same maximal-disk pruning. Both containment inequalities are convex
    // along a straight axis piece, so their endpoints can be located without
    // turning the candidate's bounding box into a claimed violation region.
    let span_axis = axis.into_iter().flat_map(|segment| {
        let delta = segment.end - segment.start;
        let at = |t| segment.start + delta * t;
        let radius = |t| {
            let point = at(t);
            (dist::point_segment(point, segment.first_wall.0, segment.first_wall.1).0
                + dist::point_segment(point, segment.second_wall.0, segment.second_wall.1).0)
                / 2.0
        };
        let mut retained = segment_inside_intervals(component, segment.start, segment.end);
        for other in &present {
            if retained.is_empty() {
                break;
            }
            if dist::point_segment(other.center, segment.start, segment.end).0
                > other.radius + approximation_mm
            {
                continue;
            }
            let Some(larger) = convex_sublevel_interval(radius, other.radius - tol::EPSILON_MM)
            else {
                continue;
            };
            let Some(contained) = convex_sublevel_interval(
                |t| at(t).distance_to(other.center) + radius(t),
                other.radius + approximation_mm,
            ) else {
                continue;
            };
            let removed = (larger.0.max(contained.0), larger.1.min(contained.1));
            if removed.0 >= removed.1 {
                continue;
            }
            retained = retained
                .into_iter()
                .flat_map(|(start, end)| {
                    let mut pieces = Vec::with_capacity(2);
                    if start < removed.0 {
                        pieces.push((start, end.min(removed.0)));
                    }
                    if end > removed.1 {
                        pieces.push((start.max(removed.1), end));
                    }
                    pieces
                })
                .collect();
        }
        retained
            .into_iter()
            .filter_map(move |(start, end)| {
                let (start, end) = (at(start), at(end));
                (start.distance_to(end) > contact_tolerance).then_some(WidthAxisSegment {
                    start,
                    end,
                    ..segment
                })
            })
            .collect::<Vec<_>>()
    });
    // Voronoi vertices are the zero-dimensional cells of the same medial-axis
    // complex. Preserve the maximal ones as zero-length axis segments so
    // islands and symmetric tips use the exact same construction as spans.
    let vertex_axis = at_vertices
        .into_iter()
        .filter(|(disk, _)| {
            component.contains_point(disk.center)
                && disk.radius <= clearance(disk.center) + contact_tolerance
                && disk.width().mm > 2.0 * contact_tolerance
                && !pruned(disk)
        })
        .map(|(_, segment)| segment);
    let axis = span_axis.chain(vertex_axis).collect();
    Some(ComponentWidth {
        disk: minimum,
        axis,
    })
}

/// The closing residue kept only where two distinct source-boundary
/// branches wall it — the balancing certificate's conservative notion of a
/// narrow void. Contacts on distinct rings always face each other across
/// void, whatever their relative angle; same-ring contacts must oppose, so
/// the rounded bite of one smooth concavity is not mistaken for a gap.
fn two_sided_gap_residual(source: &ContourSet, residual: &ContourSet) -> ContourSet {
    let source_segments = source_boundary_segments(source);
    let boundary = PreparedRegion::from_segments(
        source_segments
            .iter()
            .map(|segment| (segment.start, segment.end))
            .collect(),
        source.uncertainty_mm,
    );
    let contact_tolerance = source
        .tolerance()
        .max(residual.tolerance())
        .max(numerical_error(source.bbox));

    let rings = residual
        .connected_components()
        .into_iter()
        .filter(|component| {
            let contacts = boundary
                .segment_ids_meeting(component.bbox.expand(contact_tolerance))
                .into_iter()
                .map(|id| &source_segments[id])
                .filter(|segment| {
                    region_boundary_within_distance(
                        component,
                        segment.start,
                        segment.end,
                        contact_tolerance,
                    )
                })
                .collect::<Vec<_>>();
            contacts.iter().enumerate().any(|(index, left)| {
                contacts[index + 1..].iter().any(|right| {
                    let (separation, _, _) =
                        dist::segments(left.start, left.end, right.start, right.end);
                    // Contacts on distinct rings always face each other across
                    // void, whatever their relative angle. Same-ring pairs
                    // must additionally oppose so the rounded bite of one
                    // smooth concavity is not mistaken for a gap; walls at
                    // exactly 90° remain ambiguous there by construction.
                    !boundary_segments_are_incident(left.topology, right.topology)
                        && separation > contact_tolerance
                        && (left.topology.ring != right.topology.ring
                            || boundary_tangents_oppose(left, right))
                })
            })
        })
        .flat_map(|component| component.rings)
        .collect();
    ContourSet::from_regularized(
        simplify_rings(rings, FillRule::NonZero),
        residual.resolution,
        residual.uncertainty_mm,
    )
}

fn region_boundary_within_distance(
    region: &ContourSet,
    start: Point,
    end: Point,
    distance: f64,
) -> bool {
    let expanded = BBox::spanning(start, end).expand(distance);
    region
        .rings
        .iter()
        .flat_map(ring_edges)
        .any(|(other_start, other_end)| {
            expanded.intersects(BBox::spanning(other_start, other_end))
                && dist::segments(start, end, other_start, other_end).0 <= distance
        })
}

fn boundary_tangents_oppose(
    left: &OrientedBoundarySegment,
    right: &OrientedBoundarySegment,
) -> bool {
    left.tangent.x * right.tangent.x + left.tangent.y * right.tangent.y < 0.0
}

/// Keep-out whose removal widens every narrow void: a radius-`radius` tube
/// around the boundary medial axis inside the narrow phase. A void component
/// thinner than the axis stroke has no representable axis and is swept whole
/// instead; it sits far below the regularization scale, so even that blunt
/// cut stays local, and the keep-out always covers every component.
fn narrow_void_keep_out(
    source: &ContourSet,
    narrow_voids: &ContourSet,
    radius: f64,
) -> Result<ContourSet, GapRegularizationError> {
    let axis_keep_out = narrow_void_medial_axis_keep_out(source, narrow_voids, radius)?;
    let mut keep_out = axis_keep_out.clone();
    for component in narrow_voids.connected_components() {
        if component.intersection(&axis_keep_out)?.is_empty() {
            keep_out = keep_out.union(&component.disk_dilate(radius)?)?;
        }
    }
    Ok(keep_out)
}

fn narrow_void_medial_axis_keep_out(
    source: &ContourSet,
    narrow_voids: &ContourSet,
    radius: f64,
) -> Result<ContourSet, GapRegularizationError> {
    if narrow_voids.is_empty() {
        return Ok(ContourSet::empty(source.resolution));
    }
    let accuracy = source.budget();
    let boundary = PreparedRegion::from_segments(
        source.rings.iter().flat_map(ring_edges).collect(),
        source.uncertainty_mm,
    );
    // A closing residual is within the disk radius of its nearest source boundary.
    let void_boundary = narrow_voids.prepare_query();
    let relevant = narrow_voids
        .ring_bounds
        .iter()
        .flat_map(|bounds| boundary.segment_ids_meeting(bounds.expand(radius)))
        .filter(|&id| {
            let (start, end) = boundary.segments[id];
            void_boundary
                .segment_nearest_within(start, end, radius)
                .is_some()
        })
        .collect::<std::collections::HashSet<_>>();
    let mut source_index = 0;
    let origin = Point::new(source.bbox.min.x, source.bbox.min.y);
    let mut segments = Vec::<VoronoiLine<i32>>::new();
    let mut boundary_segments = Vec::new();
    for (ring_id, ring) in source.rings.iter().enumerate() {
        for index in 0..ring.len() {
            let needed = relevant.contains(&source_index);
            source_index += 1;
            if !needed {
                continue;
            }
            let [start_x, start_y] = ring[index];
            let [end_x, end_y] = ring[(index + 1) % ring.len()];
            if (end_x - start_x).hypot(end_y - start_y) <= source.tolerance() {
                continue;
            }
            let start = quantize_voronoi_point(ring[index], origin)?;
            let end = quantize_voronoi_point(ring[(index + 1) % ring.len()], origin)?;
            if start == end {
                continue;
            }
            segments.push(VoronoiLine::new(start, end));
            boundary_segments.push(BoundarySegment {
                ring: ring_id,
                index,
                ring_len: ring.len(),
            });
        }
    }

    let diagram = VoronoiBuilder::<i32>::default()
        .with_segments(segments.iter())
        .and_then(VoronoiBuilder::build)
        .map_err(|error| {
            GapRegularizationError(format!(
                "could not construct boundary Voronoi diagram: {error}"
            ))
        })?;
    let mut contours = Vec::new();
    for edge in diagram.edges() {
        let twin = edge.twin().map_err(gap_regularization_error)?;
        if edge.id() > twin || !edge.is_primary() {
            continue;
        }
        let left = diagram
            .cell(edge.cell().map_err(gap_regularization_error)?)
            .map_err(gap_regularization_error)?;
        let right = diagram
            .cell(
                diagram
                    .edge(twin)
                    .and_then(|edge| edge.cell())
                    .map_err(gap_regularization_error)?,
            )
            .map_err(gap_regularization_error)?;
        let left_boundary = boundary_segments
            .get(left.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        let right_boundary = boundary_segments
            .get(right.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        if boundary_segments_are_incident(*left_boundary, *right_boundary) {
            continue;
        }
        let samples = voronoi_edge_samples(
            &diagram,
            edge.id(),
            &segments,
            source,
            radius,
            VORONOI_COORDINATES_PER_MM,
            accuracy.allowance(source.uncertainty_mm)?,
        )?;
        let mut commands = Vec::with_capacity(samples.len());
        for sample in samples {
            let point = Point::new(
                sample[0] / VORONOI_COORDINATES_PER_MM + origin.x,
                sample[1] / VORONOI_COORDINATES_PER_MM + origin.y,
            );
            if commands
                .last()
                .and_then(|command: &PathCmd| command.end_point())
                .is_some_and(|previous| previous.distance_to(point) <= tol::EPSILON_MM)
            {
                continue;
            }
            commands.push(if commands.is_empty() {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        if commands.len() >= 2 {
            contours.push(ContourBuf::new(commands).with_uncertainty(
                source.uncertainty_mm
                    + accuracy.allowance(source.uncertainty_mm)?
                    + std::f64::consts::SQRT_2 / VORONOI_COORDINATES_PER_MM,
            ));
        }
    }

    if contours.is_empty() {
        return Ok(ContourSet::empty(source.resolution));
    }
    // A narrow filled stroke makes the one-dimensional axis available to the
    // existing set algebra. Intersecting with a slightly eroded narrow-void
    // phase removes the finite stroke's boundary fringe and the exterior axis.
    let axis_stroke_radius = tol::REGION_MM;
    let mut arena = PathArena::default();
    let path = arena.push_path(
        Paint::Stroke(crate::geom::StrokeStyle::round(2.0 * axis_stroke_radius)),
        contours,
    );
    let medial_axis = ContourSet::from_painted_paths(
        &arena,
        std::iter::once(&arena.paths[path as usize]),
        source.resolution,
    )?;
    let interior_axis =
        medial_axis.intersection(&narrow_voids.disk_erode(2.0 * axis_stroke_radius)?)?;
    let keep_out = interior_axis.disk_dilate(radius)?;
    Ok(keep_out.intersection(&source.disk_dilate(radius)?)?)
}

fn boundary_segments_are_incident(left: BoundarySegment, right: BoundarySegment) -> bool {
    if left.ring != right.ring {
        return false;
    }
    let distance = left.index.abs_diff(right.index);
    distance.min(left.ring_len - distance) <= 1
}

fn quantize_voronoi_point(
    [x, y]: [f64; 2],
    origin: Point,
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    fn coordinate(value: f64, origin: f64) -> Result<i32, GapRegularizationError> {
        let scaled = ((value - origin) * VORONOI_COORDINATES_PER_MM).round();
        if !scaled.is_finite() || scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
            return Err(GapRegularizationError(
                "component geometry exceeds the Voronoi coordinate range".to_string(),
            ));
        }
        Ok(scaled as i32)
    }

    Ok(VoronoiPoint::new(
        coordinate(x, origin.x)?,
        coordinate(y, origin.y)?,
    ))
}

fn gap_regularization_error(error: boostvoronoi::BvError) -> GapRegularizationError {
    GapRegularizationError(format!("invalid boundary Voronoi diagram: {error}"))
}

fn voronoi_edge_samples(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
    units_per_mm: f64,
    approximation_mm: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let affine = SimpleAffine::default();
    let mut samples = if let (Some(start), Some(end)) = (
        edge.vertex0(),
        diagram
            .edge_get_vertex1(edge_id)
            .map_err(gap_regularization_error)?,
    ) {
        let start = diagram.vertex(start).map_err(gap_regularization_error)?;
        let end = diagram.vertex(end).map_err(gap_regularization_error)?;
        vec![
            affine.transform(start.x(), start.y()),
            affine.transform(end.x(), end.y()),
        ]
    } else {
        clip_infinite_voronoi_edge(diagram, edge_id, segments, region, radius, units_per_mm)?
    };

    if edge.is_curved() {
        let cell = edge.cell().map_err(gap_regularization_error)?;
        let twin_cell = diagram
            .edge(edge.twin().map_err(gap_regularization_error)?)
            .and_then(|edge| edge.cell())
            .map_err(gap_regularization_error)?;
        let (point_cell, segment_cell) = if diagram
            .cell(cell)
            .map_err(gap_regularization_error)?
            .contains_point()
        {
            (cell, twin_cell)
        } else {
            (twin_cell, cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        VoronoiVisualUtils::discretize(
            &point,
            segment,
            approximation_mm * units_per_mm,
            &affine,
            &mut samples,
        );
    }
    Ok(samples)
}

fn clip_infinite_voronoi_edge(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
    units_per_mm: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let cell = edge.cell().map_err(gap_regularization_error)?;
    let twin_cell = diagram
        .edge(edge.twin().map_err(gap_regularization_error)?)
        .and_then(|edge| edge.cell())
        .map_err(gap_regularization_error)?;
    let left = diagram.cell(cell).map_err(gap_regularization_error)?;
    let right = diagram.cell(twin_cell).map_err(gap_regularization_error)?;
    let (origin, direction) = if left.contains_point() && right.contains_point() {
        let left = voronoi_cell_point(diagram, cell, segments)?;
        let right = voronoi_cell_point(diagram, twin_cell, segments)?;
        (
            [
                (left.x as f64 + right.x as f64) * 0.5,
                (left.y as f64 + right.y as f64) * 0.5,
            ],
            [
                left.y as f64 - right.y as f64,
                right.x as f64 - left.x as f64,
            ],
        )
    } else {
        let (point_cell, segment_cell) = if left.contains_segment() {
            (twin_cell, cell)
        } else {
            (cell, twin_cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        let origin = [point.x as f64, point.y as f64];
        let dx = segment.end.x - segment.start.x;
        let dy = segment.end.y - segment.start.y;
        let direction = if ([segment.start.x as f64, segment.start.y as f64] == origin)
            ^ left.contains_point()
        {
            [dy as f64, -dx as f64]
        } else {
            [-dy as f64, dx as f64]
        };
        (origin, direction)
    };
    let reach = (region.bbox.width().max(region.bbox.height()) + 4.0 * radius) * units_per_mm;
    let direction_scale = direction[0].abs().max(direction[1].abs());
    if direction_scale == 0.0 {
        return Err(GapRegularizationError(
            "infinite Voronoi edge has no direction".to_string(),
        ));
    }
    let coefficient = reach / direction_scale;
    let affine = SimpleAffine::default();
    let start = edge
        .vertex0()
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] - direction[0] * coefficient,
            origin[1] - direction[1] * coefficient,
        ]);
    let end = diagram
        .edge_get_vertex1(edge_id)
        .map_err(gap_regularization_error)?
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] + direction[0] * coefficient,
            origin[1] + direction[1] * coefficient,
        ]);
    Ok(vec![start, end])
}

fn voronoi_cell_point(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &[VoronoiLine<i32>],
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    let cell = diagram.cell(cell).map_err(gap_regularization_error)?;
    let segment = segments.get(cell.source_index().usize()).ok_or_else(|| {
        GapRegularizationError("Voronoi point references an unknown segment".to_string())
    })?;
    Ok(match cell.source_category() {
        SourceCategory::SegmentStart => segment.start,
        SourceCategory::Segment | SourceCategory::SegmentEnd => segment.end,
        SourceCategory::SinglePoint => {
            return Err(GapRegularizationError(
                "unexpected standalone point in component Voronoi diagram".to_string(),
            ));
        }
    })
}

fn voronoi_cell_segment<'a>(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &'a [VoronoiLine<i32>],
) -> Result<&'a VoronoiLine<i32>, GapRegularizationError> {
    let index = diagram
        .cell(cell)
        .map_err(gap_regularization_error)?
        .source_index()
        .usize();
    segments.get(index).ok_or_else(|| {
        GapRegularizationError("Voronoi cell references an unknown segment".to_string())
    })
}

#[cfg(test)]
fn regions_within_distance(left: &ContourSet, right: &ContourSet, distance: f64) -> bool {
    if !left.bbox.expand(distance).intersects(right.bbox) {
        return false;
    }
    let threshold = distance + left.tolerance().max(right.tolerance());
    for left_ring in &left.rings {
        for left_index in 0..left_ring.len() {
            let [left_start_x, left_start_y] = left_ring[left_index];
            let [left_end_x, left_end_y] = left_ring[(left_index + 1) % left_ring.len()];
            let left_start = Point::new(left_start_x, left_start_y);
            let left_end = Point::new(left_end_x, left_end_y);
            let left_bbox = BBox::spanning(left_start, left_end).expand(threshold);
            for right_ring in &right.rings {
                for right_index in 0..right_ring.len() {
                    let [right_start_x, right_start_y] = right_ring[right_index];
                    let [right_end_x, right_end_y] =
                        right_ring[(right_index + 1) % right_ring.len()];
                    let right_start = Point::new(right_start_x, right_start_y);
                    let right_end = Point::new(right_end_x, right_end_y);
                    if !left_bbox.intersects(BBox::spanning(right_start, right_end)) {
                        continue;
                    }
                    let (separation, _, _) =
                        dist::segments(left_start, left_end, right_start, right_end);
                    if separation <= threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::super::tests::{rect, res};
    use super::*;
    #[test]
    fn component_width_prunes_only_beyond_grid_uncertainty() {
        let measure = |height| {
            let component = ContourSet::rectangle(rect(0.0, 0.0, 1.0, height), res(tol::REGION_MM));
            component_width(
                &source_boundary_segments(&component),
                &component,
                0.05,
                tol::REGION_MM,
                0.0025,
                std::f64::consts::SQRT_2 * tol::REGION_MM,
            )
        };

        let width = measure(0.101).expect("grid uncertainty keeps the reachable opposing walls");
        assert!((width.disk.width().mm - 0.101).abs() < 1e-9);

        assert!(measure(0.103).is_none());
    }

    #[test]
    fn ring_components_group_holes_with_the_smallest_outer_ring_around_them() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), res(tol::REGION_MM))
            .difference(&ContourSet::rectangle(
                rect(2.0, 2.0, 8.0, 8.0),
                res(tol::REGION_MM),
            ))
            .unwrap()
            .union(&ContourSet::rectangle(
                rect(3.0, 3.0, 7.0, 7.0),
                res(tol::REGION_MM),
            ))
            .unwrap()
            .difference(&ContourSet::rectangle(
                rect(4.0, 4.0, 6.0, 6.0),
                res(tol::REGION_MM),
            ))
            .unwrap()
            .union(&ContourSet::rectangle(
                rect(20.0, 0.0, 30.0, 10.0),
                res(tol::REGION_MM),
            ))
            .unwrap();
        assert_eq!(region.rings.len(), 5);

        let (components, _) = region.ring_components();
        let component_of = |min_x: f64| {
            let ring = region
                .ring_bounds
                .iter()
                .position(|bounds| bounds.min.x == min_x)
                .unwrap();
            components[ring]
        };
        assert_eq!(component_of(0.0), component_of(2.0));
        assert_eq!(component_of(3.0), component_of(4.0));
        assert_ne!(component_of(0.0), component_of(3.0));
        assert_ne!(component_of(0.0), component_of(20.0));
        assert_ne!(component_of(3.0), component_of(20.0));
    }

    #[test]
    fn facing_components_keep_only_walls_within_reach_of_each_other() {
        let trace = ContourSet::rectangle(rect(0.0, 0.0, 5.0, 0.1), res(tol::REGION_MM));
        let pad = ContourSet::rectangle(rect(10.0, 0.0, 12.0, 2.0), res(tol::REGION_MM));
        let neighbour = ContourSet::rectangle(rect(12.15, 0.0, 14.0, 2.0), res(tol::REGION_MM));
        let region = trace.union(&pad).unwrap().union(&neighbour).unwrap();
        assert_eq!(region.rings.len(), 3);

        let same_bounds = |left: BBox, right: BBox| {
            left.min.distance_to(right.min) < 1e-6 && left.max.distance_to(right.max) < 1e-6
        };
        // The trace's long walls face across material; a reach past the
        // whole trace keeps all of it and none of the pads.
        let material = region.facing_components(0.2, 0.01, 1.0).unwrap();
        assert_eq!(material.rings.len(), 1);
        assert!(same_bounds(material.bbox, trace.bbox));

        // The pads face each other across their gap, along their whole
        // aligned edges, and the trace is out of reach.
        let void = region.facing_components(0.0, 0.2, 0.5).unwrap();
        assert_eq!(void.rings.len(), 2);
        assert!(same_bounds(void.bbox, pad.bbox.union(neighbour.bbox)));

        let with_context = region.facing_components(0.0, 0.2, 10.0).unwrap();
        assert_eq!(with_context.rings.len(), 3);

        // A notch faces across void; a plane web between holes across material.
        let notched = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), res(tol::REGION_MM))
            .difference(&ContourSet::rectangle(
                rect(1.9, 2.0, 2.1, 4.5),
                res(tol::REGION_MM),
            ))
            .unwrap();
        assert_eq!(
            notched
                .facing_components(0.0, 0.3, 1.0)
                .unwrap()
                .rings
                .len(),
            1
        );
        assert!(
            notched
                .facing_components(0.15, 0.15, 1.0)
                .unwrap()
                .is_empty()
        );
        let webbed = ContourSet::rectangle(rect(0.0, 0.0, 8.0, 4.0), res(tol::REGION_MM))
            .difference(&ContourSet::rectangle(
                rect(1.0, 1.0, 1.9, 3.0),
                res(tol::REGION_MM),
            ))
            .unwrap()
            .difference(&ContourSet::rectangle(
                rect(2.1, 1.0, 3.0, 3.0),
                res(tol::REGION_MM),
            ))
            .unwrap()
            .difference(&ContourSet::rectangle(
                rect(6.0, 1.0, 7.0, 3.0),
                res(tol::REGION_MM),
            ))
            .unwrap();
        let nearby_web = webbed.facing_components(0.3, 0.0, 1.0).unwrap();
        assert_eq!(nearby_web.rings.len(), 3);
        assert!(nearby_web.contains_point(Point::new(2.0, 2.0)));
        assert!(nearby_web.contains_point(Point::new(2.0, 3.5)));
        assert!(!nearby_web.contains_point(Point::new(6.5, 2.0)));
        assert!(!nearby_web.contains_point(Point::new(5.0, 3.5)));
        assert!(webbed.facing_components(0.0, 0.15, 1.0).unwrap().is_empty());
    }

    #[test]
    fn planar_sites_split_along_the_segment_whichever_way_it_runs() {
        let site = |start: (f64, f64), end: (f64, f64), index: usize| OrientedBoundarySegment {
            topology: BoundarySegment {
                ring: index / 10,
                index,
                ring_len: 10,
            },
            start: Point::new(start.0, start.1),
            end: Point::new(end.0, end.1),
            tangent: Point::new(end.0 - start.0, end.1 - start.1),
            bbox: BBox::spanning(Point::new(start.0, start.1), Point::new(end.0, end.1)),
        };
        // A right-to-left host touched at two interior points by other rings.
        let sites = [
            site((10.0, 0.0), (0.0, 0.0), 0),
            site((7.0, 5.0), (7.0, 0.0), 10),
            site((3.0, 5.0), (3.0, 0.0), 20),
        ];
        let grid = planar_grid_sites(&sites, |point| {
            VoronoiPoint::new(point.x as i32, point.y as i32)
        });

        let host = grid
            .iter()
            .filter(|(_, site)| site.topology.index == 0)
            .map(|(line, _)| line)
            .collect::<Vec<_>>();
        assert_eq!(host.len(), 3);
        assert_eq!(host[0].start, VoronoiPoint::new(10, 0));
        for pair in host.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "pieces chain along the host");
        }
        assert_eq!(host[2].end, VoronoiPoint::new(0, 0));
    }
}
