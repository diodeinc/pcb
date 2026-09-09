//! Two-sided morphology residues, local widths, and void-gap regularization.

use super::widths::WidthAxis;
use super::{
    ContourSet, PreparedRegion, Ring, ring_edges, ring_signed_area, ring_winding, simplify_rings,
};
use crate::geom::accuracy::numerical_error;
use crate::geom::dist;
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

/// Result of enforcing a minimum width for every two-sided void gap.
#[derive(Debug, Clone)]
pub struct DiskGapRegularization {
    /// Input material retained after local gap trimming and disk opening.
    pub kept: ContourSet,
    /// `source \ kept`.
    pub removed: ContourSet,
}

/// One connected opening/closing residue with resolved opposing contacts
/// on its medial axis. The caller measures only these axis cells, not the
/// conservative residue or a separate set of sampled disks.
#[derive(Debug, Clone)]
pub(crate) struct TwoSidedResidualComponent {
    pub region: ContourSet,
    pub boundary_uncertainty_mm: f64,
    pub axis: Vec<WidthAxis>,
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
    pub(crate) fn ring_components(&self) -> (Vec<usize>, Vec<usize>) {
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
        // where they touch within tolerance. The analytic width construction
        // uses the source walls directly, and `M \ (X ∩ M)` is `M \ X`, so
        // the opening's clip to the source is not needed to find what the
        // opening removed.
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
/// touch. Their pairwise analytic bisectors restricted to the component
/// form the medial axis there. The narrowest maximal inscribed disk on that
/// axis is the component's width. Disks tangent only to incident segments are
/// corner spokes, not widths: discarding those leaves one-sided residue —
/// the bite an isolated corner sheds — with no width at all.
fn two_sided_residual_components(
    source: &ContourSet,
    residual: &ContourSet,
    reach: f64,
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
    let complete_boundary = source.prepare_query();
    let boundary_uncertainty = source.uncertainty_mm + numerical_error(source.bbox);
    residual
        .connected_components()
        .into_iter()
        .filter_map(|component| {
            // Preserve the source-boundary index: candidate axes use the
            // nearby subset, while validation sees every (including short)
            // source edge through `complete_boundary`.
            let sites = boundary
                .segment_ids_meeting(component.bbox.expand(reach))
                .into_iter()
                .map(|id| segments[id])
                .collect::<Vec<_>>();
            let axis = component_axis(&sites, &component, &complete_boundary, reach);
            (!axis.is_empty()).then_some(TwoSidedResidualComponent {
                region: component,
                boundary_uncertainty_mm: boundary_uncertainty,
                axis,
            })
        })
        .collect()
}

/// Enumerate exact bisectors of every reachable pair of nonincident walls,
/// then let the analytic axis clip and validate itself against the residue
/// and the complete source boundary.
fn component_axis(
    sites: &[OrientedBoundarySegment],
    component: &ContourSet,
    complete_boundary: &PreparedRegion,
    reach: f64,
) -> Vec<WidthAxis> {
    if sites.len() < 2 {
        return Vec::new();
    }
    let error = numerical_error(component.bbox);
    let incident = |i: usize, j: usize| {
        let a = &sites[i];
        let b = &sites[j];
        if a.topology.ring == b.topology.ring {
            boundary_segments_are_incident(a.topology, b.topology)
        } else {
            [a.start, a.end]
                .iter()
                .any(|point| [b.start, b.end].contains(point))
        }
    };
    let component_edges = component
        .rings
        .iter()
        .flat_map(ring_edges)
        .collect::<Vec<_>>();
    // A component disk is no larger than its clearance to its nearest
    // source site. The farthest component vertex from that site is an exact
    // upper bound, so walls farther from the component cannot participate.
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
        + error;
    let within_reach = sites
        .iter()
        .map(|site| {
            component_edges.iter().any(|&(start, end)| {
                dist::segments(start, end, site.start, site.end).0 <= clearance_bound
            })
        })
        .collect::<Vec<_>>();
    let candidate_diameter = 2.0 * reach + error;

    let mut axes = Vec::new();
    for first in 0..sites.len() {
        for second in first + 1..sites.len() {
            if !within_reach[first] || !within_reach[second] || incident(first, second) {
                continue;
            }
            let first_wall = (sites[first].start, sites[first].end);
            let second_wall = (sites[second].start, sites[second].end);
            if dist::segments(first_wall.0, first_wall.1, second_wall.0, second_wall.1).0
                > candidate_diameter
            {
                continue;
            }
            axes.extend(
                WidthAxis::between(first_wall, second_wall, component.bbox)
                    .into_iter()
                    .flat_map(|axis| axis.in_region(component, complete_boundary)),
            );
        }
    }
    axes
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
}
