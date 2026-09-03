//! Geometry measurements shared by manufacturability checks.
//!
//! Every query here answers with a [`Distance`]: a signed length between
//! two witness points, carrying the uncertainty of the flattened boundaries
//! it was measured against. Deciding whether a distance violates a limit is
//! the caller's policy, via [`Distance::certainly_below`].

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::geom::bbox::BBox;
use crate::geom::dist;
use crate::geom::point::Point;
use crate::geom::region::{ContourSet, Ring, TwoSidedResidualComponent, ring_edges};
use crate::geom::tol;

pub use crate::geom::dist::Distance;

/// Exact candidate index over ring or shape bounds. Large enclosing rings
/// remain queryable even when none of their boundary segments cross a small
/// region of interest. The returned IDs retain the source ordering.
#[derive(Debug)]
pub struct BBoxIndex {
    bounds: Vec<BBox>,
    cells: HashMap<(i64, i64), Vec<usize>>,
    enclosing: Vec<usize>,
    pitch: f64,
}

impl BBoxIndex {
    pub fn new(bounds: Vec<BBox>) -> Self {
        let total = bounds.iter().copied().fold(BBox::empty(), BBox::union);
        let pitch = if total.is_empty() {
            2.0
        } else {
            (total.width().max(total.height()) / 64.0).max(2.0)
        };
        let mut index = Self {
            bounds,
            cells: HashMap::new(),
            enclosing: Vec::new(),
            pitch,
        };
        for (id, bbox) in index.bounds.iter().enumerate() {
            if bbox.is_empty() {
                continue;
            }
            let (x0, y0, x1, y1) = index.cells_for(*bbox);
            if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > 256 {
                index.enclosing.push(id);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    index.cells.entry((x, y)).or_default().push(id);
                }
            }
        }
        index
    }

    fn cells_for(&self, bbox: BBox) -> (i64, i64, i64, i64) {
        let cell = |value: f64| (value / self.pitch).floor() as i64;
        (
            cell(bbox.min.x),
            cell(bbox.min.y),
            cell(bbox.max.x),
            cell(bbox.max.y),
        )
    }

    pub fn query(&self, bbox: BBox) -> Vec<usize> {
        if bbox.is_empty() {
            return Vec::new();
        }
        let (x0, y0, x1, y1) = self.cells_for(bbox);
        if (x1 - x0 + 1).saturating_mul(y1 - y0 + 1) > 4096 {
            return self
                .bounds
                .iter()
                .enumerate()
                .filter_map(|(id, bounds)| bounds.intersects(bbox).then_some(id))
                .collect();
        }
        let mut candidates = self.enclosing.clone();
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(ids) = self.cells.get(&(x, y)) {
                    candidates.extend(ids);
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates.retain(|&id| self.bounds[id].intersects(bbox));
        candidates
    }
}

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
    bounds: Vec<BBox>,
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
        let bounds = segments
            .iter()
            .map(|&(start, end)| segment_bounds(start, end))
            .collect();
        let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (index, &(start, end)) in segments.iter().enumerate() {
            for cell in corridor_cells(start, end, 0.0, cell_size_mm) {
                cells.entry(cell).or_default().push(index as u32);
            }
        }
        Self {
            cell_size_mm,
            segments,
            bounds,
            cells,
        }
    }

    /// Nearest boundary point no farther than `max_distance_mm` from `point`.
    pub fn nearest_within(&self, point: Point, max_distance_mm: f64) -> Option<Distance> {
        indexed_edge_ids_near_in_encounter_order(self, point, point, max_distance_mm)
            .into_iter()
            .map(|index| {
                let (start, end) = self.segments[index as usize];
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
        indexed_edge_ids_near_in_encounter_order(self, start, end, max_distance_mm)
            .into_iter()
            .map(|index| {
                let (edge_start, edge_end) = self.segments[index as usize];
                let (mm, on_query, on_boundary) = dist::segments(start, end, edge_start, edge_end);
                Distance::flattened(mm, on_query, on_boundary, 1)
            })
            .filter(|distance| distance.mm <= max_distance_mm + tol::EPSILON_MM)
            .min_by(|left, right| left.mm.total_cmp(&right.mm))
    }

    /// Parameter intervals along a segment within `distance_mm` of the
    /// indexed boundary. Unlike a nearest-point query, these certify the
    /// complete covered extent, including gaps between disjoint boundaries.
    pub fn segment_boundary_intervals(
        &self,
        start: Point,
        end: Point,
        distance_mm: f64,
    ) -> Vec<(f64, f64)> {
        merge_intervals(
            indexed_edges_near(self, start, end, distance_mm)
                .into_iter()
                .flat_map(|(a, b)| segment_capsule_intervals(start, end, a, b, distance_mm))
                .collect(),
        )
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
}

fn segment_bounds(start: Point, end: Point) -> BBox {
    BBox::new(
        Point::new(start.x.min(end.x), start.y.min(end.y)),
        Point::new(start.x.max(end.x), start.y.max(end.y)),
    )
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

/// Shortest clearance between two filled regions when it is no greater than
/// `maximum_mm`.
///
/// `second_boundary` must index `second`. Overlapping, contained, or touching
/// regions have zero clearance. `None` means at least one input is empty or
/// the clearance is greater than `maximum_mm`.
pub fn region_clearance_within(
    first: &ContourSet,
    second: &ContourSet,
    second_boundary: &RegionBoundaryIndex,
    maximum_mm: f64,
) -> Option<Distance> {
    if first.is_empty() || second.is_empty() {
        return None;
    }

    if first.bbox.intersects(second.bbox)
        && let Some(point) =
            contained_vertex(first, second).or_else(|| contained_vertex(second, first))
    {
        return Some(Distance::flattened(0.0, point, point, 2));
    }

    first
        .rings
        .iter()
        .flat_map(ring_edges)
        .filter_map(|(first_start, first_end)| {
            second_boundary
                .segment_nearest_within(first_start, first_end, maximum_mm)
                .map(|distance| distance.also_flattened(1))
        })
        .min_by(|left, right| left.mm.total_cmp(&right.mm))
}

/// A vertex of `subject` inside `container`, batched in one winding sweep.
fn contained_vertex(subject: &ContourSet, container: &ContourSet) -> Option<Point> {
    let vertices = subject
        .rings
        .iter()
        .flat_map(|ring| ring.iter())
        .map(|&[x, y]| Point::new(x, y))
        .filter(|point| {
            point.x >= container.bbox.min.x
                && point.x <= container.bbox.max.x
                && point.y >= container.bbox.min.y
                && point.y <= container.bbox.max.y
        })
        .collect::<Vec<_>>();
    if vertices.is_empty() {
        return None;
    }
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

/// A connected local clearance failure. The paths are the participating
/// boundary portions, not whole-subject bounds. `overlap` is actual shared
/// material, and is empty for a gap or point/line contact.
#[derive(Debug, Clone)]
pub struct ClearanceSite {
    pub distance: Distance,
    pub bbox: BBox,
    pub first_paths: Vec<Vec<Point>>,
    pub second_paths: Vec<Vec<Point>>,
    pub overlap: ContourSet,
}

/// All connected sub-minimum portions of linework against a filled image.
/// Segment/capsule intersection gives the actual start and stop of each
/// failing span in the flattened representation. The reach excludes the
/// measurement uncertainty, so the highlighted spans are certainly below
/// the limit, not the larger broad-phase search envelope.
pub fn linework_clearance_sites(
    lines: &[(Point, Point)],
    material: &ContourSet,
    index: &RegionBoundaryIndex,
    minimum_mm: f64,
    flattened_linework: u32,
) -> Vec<ClearanceSite> {
    let flattened = flattened_linework + 1;
    let reach = minimum_mm - f64::from(flattened) * tol::FLATTEN_MM - 1e-6;
    if reach <= 0.0 || material.is_empty() {
        return Vec::new();
    }
    let mut pending = Vec::with_capacity(lines.len());
    let mut midpoints = Vec::new();
    for &(start, end) in lines {
        let edges = indexed_edges_near(index, start, end, reach);
        let near = merge_intervals(
            edges
                .iter()
                .flat_map(|&(a, b)| segment_capsule_intervals(start, end, a, b, reach))
                .collect(),
        );
        // A segment can run deep inside material, beyond every boundary
        // capsule. Gaps between capsule intervals cannot cross a boundary,
        // so one batched containment test per gap settles that entire span.
        let gaps = interval_complement(&near);
        let delta = end - start;
        let midpoint_offset = midpoints.len();
        midpoints.extend(gaps.iter().map(|&(a, b)| start + delta * ((a + b) / 2.0)));
        pending.push((start, end, near, gaps, midpoint_offset));
    }
    // One sweep for the whole reference item, not one walk of a panel's
    // complete copper image for every little boundary segment.
    let inside = material.contains_points_batch(&midpoints);
    let mut spans = Vec::new();
    for (start, end, near, gaps, midpoint_offset) in pending {
        let delta = end - start;
        let intervals = merge_intervals(
            near.into_iter()
                .chain(
                    gaps.into_iter()
                        .zip(inside[midpoint_offset..].iter().copied())
                        .filter_map(|(interval, inside)| inside.then_some(interval)),
                )
                .collect(),
        );
        for (a, b) in intervals {
            let (a, b) = (start + delta * a, start + delta * b);
            spans.push((a, b));
        }
    }
    let endpoints = spans.iter().flat_map(|&(a, b)| [a, b]).collect::<Vec<_>>();
    let endpoint_inside = material.contains_points_batch(&endpoints);
    connected_line_groups(&spans)
        .into_iter()
        .filter_map(|group| {
            let mut nearest: Option<Distance> = None;
            let mut first_paths = Vec::new();
            let mut second_intervals: BTreeMap<u32, Vec<(f64, f64)>> = BTreeMap::new();
            let mut bbox = BBox::empty();
            for span_index in group {
                let (start, end) = spans[span_index];
                first_paths.push(vec![start, end]);
                bbox.include_point(start);
                bbox.include_point(end);
                let distance = if endpoint_inside[2 * span_index] {
                    Some(Distance::flattened(0.0, start, start, flattened))
                } else if endpoint_inside[2 * span_index + 1] {
                    Some(Distance::flattened(0.0, end, end, flattened))
                } else {
                    index
                        .segment_nearest_within(start, end, minimum_mm)
                        .map(|distance| distance.also_flattened(flattened_linework))
                };
                if let Some(distance) = distance
                    && nearest.is_none_or(|best| distance.mm < best.mm)
                {
                    nearest = Some(distance);
                }
                for edge_id in indexed_edge_ids_near(index, start, end, reach) {
                    let (a, b) = index.segments[edge_id as usize];
                    second_intervals
                        .entry(edge_id)
                        .or_default()
                        .extend(segment_capsule_intervals(a, b, start, end, reach));
                }
            }
            // A long source edge can be close to many consecutive reference
            // spans. Preserve its exact union once, keyed by authoritative
            // segment identity rather than rounded point coordinates.
            let second_paths = second_intervals
                .into_iter()
                .flat_map(|(edge_id, intervals)| {
                    let (start, end) = index.segments[edge_id as usize];
                    let delta = end - start;
                    merge_intervals(intervals)
                        .into_iter()
                        .map(move |(low, high)| vec![start + delta * low, start + delta * high])
                })
                .collect::<Vec<_>>();
            for point in second_paths.iter().flatten() {
                bbox.include_point(*point);
            }
            let distance = nearest?;
            bbox.include_point(distance.first);
            bbox.include_point(distance.second);
            Some(ClearanceSite {
                distance,
                bbox,
                first_paths,
                second_paths,
                overlap: ContourSet::empty(material.tolerance),
            })
        })
        .collect()
}

/// Local sites between two filled regions. Intersection components supply
/// explicit overlap geometry, including containment with no near boundary.
pub fn region_clearance_sites(
    first: &ContourSet,
    second: &ContourSet,
    minimum_mm: f64,
) -> Vec<ClearanceSite> {
    let index = RegionBoundaryIndex::new(second, minimum_mm);
    region_clearance_sites_with_index(first, second, &index, minimum_mm)
}

/// Local sites between two filled regions, reusing an index of `second`.
pub fn region_clearance_sites_with_index(
    first: &ContourSet,
    second: &ContourSet,
    second_boundary: &RegionBoundaryIndex,
    minimum_mm: f64,
) -> Vec<ClearanceSite> {
    let lines = first.rings.iter().flat_map(ring_edges).collect::<Vec<_>>();
    let mut sites = linework_clearance_sites(&lines, second, second_boundary, minimum_mm, 1);
    for overlap in first.intersection(second).connected_components() {
        let Some(point) = overlap
            .rings
            .first()
            .and_then(|ring| ring.first())
            .map(|&[x, y]| Point::new(x, y))
        else {
            continue;
        };
        let mut joined = ClearanceSite {
            distance: Distance::flattened(0.0, point, point, 2),
            bbox: overlap.bbox,
            first_paths: Vec::new(),
            second_paths: Vec::new(),
            overlap,
        };
        let overlap_boundary = RegionBoundaryIndex::new(&joined.overlap, tol::REGION_MM);
        let mut position = 0;
        while position < sites.len() {
            let touches = sites[position].first_paths.iter().any(|path| {
                path.windows(2).any(|pair| {
                    !crate::geom::region::segment_inside_intervals(
                        &joined.overlap,
                        pair[0],
                        pair[1],
                    )
                    .is_empty()
                        || joined.overlap.contains_point(pair[0])
                        || joined.overlap.contains_point(pair[1])
                        || overlap_boundary
                            .segment_nearest_within(pair[0], pair[1], tol::REGION_MM)
                            .is_some()
                })
            });
            if !touches {
                position += 1;
                continue;
            }
            let site = sites.remove(position);
            joined.bbox = joined.bbox.union(site.bbox);
            joined.first_paths.extend(site.first_paths);
            joined.second_paths.extend(site.second_paths);
            joined.overlap = joined.overlap.union(&site.overlap);
        }
        sites.push(joined);
    }
    sites
}

/// A local required-clearance band around the supplied reference paths.
pub fn linework_envelope(paths: &[Vec<Point>], radius_mm: f64) -> ContourSet {
    use crate::geom::path::{ContourBuf, PathCmd};
    use crate::geom::{Paint, PathArena, StrokeStyle};
    let contours = paths
        .iter()
        .filter(|path| path.len() >= 2)
        .map(|path| {
            ContourBuf::new(
                std::iter::once(PathCmd::move_to(path[0]))
                    .chain(path[1..].iter().copied().map(PathCmd::line_to))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut arena = PathArena::default();
    let path = arena.push_path(Paint::Stroke(StrokeStyle::round(2.0 * radius_mm)), contours);
    ContourSet::from_painted_paths(
        &arena,
        std::iter::once(&arena.paths[path as usize]),
        tol::REGION_MM,
    )
}

/// Circular material in the same flattened representation as check images.
/// Analytic diameter/radial measurements should continue to use the circle
/// parameters; this region is for boolean evidence such as missing copper.
pub fn circular_region(center: Point, radius_mm: f64) -> ContourSet {
    let Some(circle) = crate::geom::shapes::circle(2.0 * radius_mm) else {
        return ContourSet::empty(tol::REGION_MM);
    };
    let circle =
        crate::geom::path::transform_cmds(circle.cmds, crate::geom::Affine2::translation(center));
    ContourSet::from_filled_contours(&[circle], tol::REGION_MM)
}

fn indexed_edges_near(
    index: &RegionBoundaryIndex,
    start: Point,
    end: Point,
    reach: f64,
) -> Vec<(Point, Point)> {
    indexed_edge_ids_near(index, start, end, reach)
        .into_iter()
        .map(|id| index.segments[id as usize])
        .collect()
}

fn indexed_edge_ids_near(
    index: &RegionBoundaryIndex,
    start: Point,
    end: Point,
    reach: f64,
) -> Vec<u32> {
    let mut ids = indexed_edge_candidates_near(index, start, end, reach);
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn indexed_edge_ids_near_in_encounter_order(
    index: &RegionBoundaryIndex,
    start: Point,
    end: Point,
    reach: f64,
) -> Vec<u32> {
    let mut ids = indexed_edge_candidates_near(index, start, end, reach);
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    ids
}

fn indexed_edge_candidates_near(
    index: &RegionBoundaryIndex,
    start: Point,
    end: Point,
    reach: f64,
) -> Vec<u32> {
    let query = segment_bounds(start, end).expand(reach + tol::EPSILON_MM);
    corridor_cells(start, end, reach, index.cell_size_mm)
        .filter_map(|cell| index.cells.get(&cell))
        .flatten()
        .copied()
        .filter(|&id| index.bounds[id as usize].intersects(query))
        .collect()
}

fn linear_interval(value: f64, slope: f64, minimum: f64, maximum: f64) -> Option<(f64, f64)> {
    if slope.abs() <= f64::EPSILON {
        return (value >= minimum && value <= maximum).then_some((0.0, 1.0));
    }
    let (a, b) = ((minimum - value) / slope, (maximum - value) / slope);
    let interval = (a.min(b).max(0.0), a.max(b).min(1.0));
    (interval.0 < interval.1).then_some(interval)
}

fn intersect_interval(first: (f64, f64), second: (f64, f64)) -> Option<(f64, f64)> {
    let interval = (first.0.max(second.0), first.1.min(second.1));
    (interval.0 < interval.1).then_some(interval)
}

fn segment_disk_interval(
    start: Point,
    end: Point,
    center: Point,
    radius: f64,
) -> Option<(f64, f64)> {
    let delta = end - start;
    let relative = start - center;
    let a = delta.x * delta.x + delta.y * delta.y;
    let c = relative.x * relative.x + relative.y * relative.y - radius * radius;
    if a <= f64::EPSILON {
        return (c < 0.0).then_some((0.0, 1.0));
    }
    let b = 2.0 * (relative.x * delta.x + relative.y * delta.y);
    let discriminant = b * b - 4.0 * a * c;
    if discriminant <= 0.0 {
        return None;
    }
    let root = discriminant.sqrt();
    intersect_interval(
        ((-b - root) / (2.0 * a), (-b + root) / (2.0 * a)),
        (0.0, 1.0),
    )
}

/// Parameters where a moving point is within `radius` of a closed segment.
/// The capsule is an infinite strip clipped to the segment plus its end disks.
fn segment_capsule_intervals(
    start: Point,
    end: Point,
    a: Point,
    b: Point,
    radius: f64,
) -> Vec<(f64, f64)> {
    let mut intervals = [
        segment_disk_interval(start, end, a, radius),
        segment_disk_interval(start, end, b, radius),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let edge = b - a;
    let length = edge.length();
    if length > tol::EPSILON_MM {
        let direction = edge / length;
        let delta = end - start;
        let relative = start - a;
        let projection = relative.x * direction.x + relative.y * direction.y;
        let along = delta.x * direction.x + delta.y * direction.y;
        let perpendicular = relative.x * direction.y - relative.y * direction.x;
        let across = delta.x * direction.y - delta.y * direction.x;
        if let (Some(along), Some(across)) = (
            linear_interval(projection, along, 0.0, length),
            linear_interval(perpendicular, across, -radius, radius),
        ) && let Some(interval) = intersect_interval(along, across)
        {
            intervals.push(interval);
        }
    }
    merge_intervals(intervals)
}

fn merge_intervals(mut intervals: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for interval in intervals {
        if let Some(last) = merged.last_mut()
            && interval.0 <= last.1 + f64::EPSILON
        {
            last.1 = last.1.max(interval.1);
            continue;
        }
        merged.push(interval);
    }
    merged
}

fn interval_complement(intervals: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut result = Vec::new();
    let mut previous = 0.0;
    for &(start, end) in intervals {
        if start > previous {
            result.push((previous, start));
        }
        previous = previous.max(end);
    }
    if previous < 1.0 {
        result.push((previous, 1.0));
    }
    result
}

fn connected_line_groups(lines: &[(Point, Point)]) -> Vec<Vec<usize>> {
    let mut parents = (0..lines.len()).collect::<Vec<_>>();
    let mut endpoints: HashMap<(i64, i64), Vec<(Point, usize)>> = HashMap::new();
    fn root(parents: &mut [usize], mut id: usize) -> usize {
        while parents[id] != id {
            parents[id] = parents[parents[id]];
            id = parents[id];
        }
        id
    }
    for (id, &(first, second)) in lines.iter().enumerate() {
        for point in [first, second] {
            let key = (
                (point.x / tol::REGION_MM).round() as i64,
                (point.y / tol::REGION_MM).round() as i64,
            );
            for x in key.0 - 1..=key.0 + 1 {
                for y in key.1 - 1..=key.1 + 1 {
                    for &(other_point, other) in endpoints.get(&(x, y)).into_iter().flatten() {
                        if point.distance_to(other_point) <= tol::REGION_MM {
                            let (a, b) = (root(&mut parents, id), root(&mut parents, other));
                            parents[a] = b;
                        }
                    }
                }
            }
            endpoints.entry(key).or_default().push((point, id));
        }
    }
    let mut positions = HashMap::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for id in 0..lines.len() {
        let owner = root(&mut parents, id);
        let position = *positions.entry(owner).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[position].push(id);
    }
    groups
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
    /// Guarded morphology residue; context, not an exact failure footprint.
    pub candidate: ContourSet,
    /// The actual maximal disk that produced `width`, not a disk inferred
    /// from the midpoint of its generally non-opposite tangencies.
    pub disk: WidthDisk,
    /// Connected confirmed sub-minimum portions, each with its own minimum.
    pub sites: Vec<ThinSite>,
}

#[derive(Debug, Clone, Copy)]
pub struct WidthDisk {
    pub center: Point,
    pub radius_mm: f64,
    pub width: Distance,
}

#[derive(Debug, Clone)]
pub struct ThinSite {
    pub bbox: BBox,
    pub disk: WidthDisk,
    /// Cells of the verified medial axis. A zero-dimensional cell repeats
    /// its point so vertices and spans share one representation.
    pub axis: Vec<Vec<Point>>,
    /// Actual source-boundary contacts along the verified axis. A vertex
    /// contact is a zero-length segment; these also retain isolated disk
    /// tangencies so occurrence attribution can prove the entire construction.
    pub walls: Vec<(Point, Point)>,
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
    min_width_disk(region).map(|disk| disk.width)
}

pub fn min_width_disk(region: &ContourSet) -> Option<WidthDisk> {
    let erase_all = 2.0 * region.bbox.width().max(region.bbox.height());
    thin_features(region, erase_all)
        .into_iter()
        .map(|piece| piece.disk)
        .min_by(|left, right| left.width.mm.total_cmp(&right.width.mm))
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
        .filter_map(|component| {
            let narrow_axis = component
                .axis
                .iter()
                .flat_map(|axis| {
                    let reach =
                        (minimum_mm - 2.0 * tol::FLATTEN_MM - 2.0 * axis.uncertainty_mm - 1e-6)
                            / 2.0;
                    if reach <= 0.0 {
                        return Vec::new();
                    }
                    let first = segment_capsule_intervals(
                        axis.start,
                        axis.end,
                        axis.first_wall.0,
                        axis.first_wall.1,
                        reach,
                    );
                    let second = segment_capsule_intervals(
                        axis.start,
                        axis.end,
                        axis.second_wall.0,
                        axis.second_wall.1,
                        reach,
                    );
                    let delta = axis.end - axis.start;
                    first
                        .iter()
                        .flat_map(|&first| {
                            second
                                .iter()
                                .filter_map(move |&second| intersect_interval(first, second))
                        })
                        .map(|(start, end)| {
                            let (start, end) =
                                (axis.start + delta * start, axis.start + delta * end);
                            let at = |t| start + (end - start) * t;
                            let radius_at = |t| {
                                let center = at(t);
                                (dist::point_segment(center, axis.first_wall.0, axis.first_wall.1)
                                    .0
                                    + dist::point_segment(
                                        center,
                                        axis.second_wall.0,
                                        axis.second_wall.1,
                                    )
                                    .0)
                                    / 2.0
                            };
                            let (mut low, mut high) = (0.0, 1.0);
                            for _ in 0..40 {
                                let a = (2.0 * low + high) / 3.0;
                                let b = (low + 2.0 * high) / 3.0;
                                if radius_at(a) < radius_at(b) {
                                    high = b;
                                } else {
                                    low = a;
                                }
                            }
                            let t = [0.0, (low + high) / 2.0, 1.0]
                                .into_iter()
                                .min_by(|&a, &b| radius_at(a).total_cmp(&radius_at(b)))
                                .unwrap();
                            let center = at(t);
                            let radius_mm = radius_at(t);
                            let first =
                                dist::point_segment(center, axis.first_wall.0, axis.first_wall.1).1;
                            let second =
                                dist::point_segment(center, axis.second_wall.0, axis.second_wall.1)
                                    .1;
                            let walls = [axis.first_wall, axis.second_wall].map(|(a, b)| {
                                (
                                    dist::point_segment(start, a, b).1,
                                    dist::point_segment(end, a, b).1,
                                )
                            });
                            let mut width = Distance::flattened(2.0 * radius_mm, first, second, 2);
                            width.uncertainty_mm += 2.0 * axis.uncertainty_mm;
                            (
                                (start, end),
                                WidthDisk {
                                    center,
                                    radius_mm,
                                    width,
                                },
                                walls,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let lines = narrow_axis
                .iter()
                .map(|(line, _, _)| *line)
                .collect::<Vec<_>>();
            let sites = connected_line_groups(&lines)
                .into_iter()
                .map(|group| {
                    let disk = group
                        .iter()
                        .map(|&index| narrow_axis[index].1)
                        .min_by(|left, right| left.width.mm.total_cmp(&right.width.mm))
                        .unwrap();
                    let mut bbox = BBox::from_point(disk.center).expand(disk.radius_mm);
                    let mut walls = group
                        .iter()
                        .flat_map(|&index| narrow_axis[index].2)
                        .collect::<Vec<_>>();
                    walls.extend([
                        (disk.width.first, disk.width.first),
                        (disk.width.second, disk.width.second),
                    ]);
                    for &(start, end) in &walls {
                        bbox.include_point(start);
                        bbox.include_point(end);
                    }
                    let axis = group
                        .into_iter()
                        .map(|index| {
                            let (start, end) = lines[index];
                            bbox.include_point(start);
                            bbox.include_point(end);
                            vec![start, end]
                        })
                        .collect();
                    ThinSite {
                        bbox,
                        disk,
                        axis,
                        walls,
                    }
                })
                .collect::<Vec<_>>();
            let disk = WidthDisk {
                center: component.disk.center,
                radius_mm: component.disk.radius,
                width: component.width,
            };
            (!sites.is_empty()).then_some(ThinPiece {
                bbox: component.region.bbox,
                area_mm2: component.region.area(),
                width: component.width,
                length_mm: region_perimeter(&component.region) / 2.0,
                disk,
                sites,
                candidate: component.region,
            })
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
    fn clearance_sites_locate_every_disconnected_span_with_threshold_endpoints() {
        let material = rect_region(2.0, 0.1, 3.0, 1.0).union(&rect_region(7.0, 0.1, 8.0, 1.0));
        let sites = linework_clearance_sites(
            &[(Point::new(0.0, 0.0), Point::new(10.0, 0.0))],
            &material,
            &RegionBoundaryIndex::new(&material, 0.3),
            0.3,
            0,
        );
        assert_eq!(sites.len(), 2);
        let reach = 0.3 - tol::FLATTEN_MM - 1e-6;
        let extension = (reach * reach - 0.1_f64.powi(2)).sqrt();
        for (site, left) in sites.iter().zip([2.0, 7.0]) {
            assert!((site.distance.mm - 0.1).abs() < 1e-9);
            assert_eq!(site.first_paths.len(), 1);
            assert!((site.first_paths[0][0].x - (left - extension)).abs() < 1e-9);
            assert!((site.first_paths[0][1].x - (left + 1.0 + extension)).abs() < 1e-9);
            assert!(!site.second_paths.is_empty());
        }
    }

    #[test]
    fn clearance_sites_include_segments_deep_inside_material() {
        let material = rect_region(0.0, 0.0, 10.0, 10.0);
        let line = (Point::new(2.0, 5.0), Point::new(8.0, 5.0));
        let sites = linework_clearance_sites(
            &[line],
            &material,
            &RegionBoundaryIndex::new(&material, 0.2),
            0.2,
            0,
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].distance.mm, 0.0);
        assert_eq!(sites[0].first_paths, vec![vec![line.0, line.1]]);
    }

    #[test]
    fn clearance_sites_merge_repeated_contacts_on_the_same_source_edge() {
        let material =
            ContourSet::from_filled_contours(&[rect_at(0.0, 0.0, 1.0, 1.0)], tol::REGION_MM);
        let index = RegionBoundaryIndex::new(&material, 0.2);
        let sites = linework_clearance_sites(
            &[
                (Point::new(0.0, -0.1), Point::new(0.5, -0.1)),
                (Point::new(0.5, -0.1), Point::new(1.0, -0.1)),
            ],
            &material,
            &index,
            0.2,
            1,
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].first_paths.len(), 2);
        assert_eq!(
            sites[0].second_paths.len(),
            3,
            "bottom edge and two short side contacts, each once"
        );
        let bottom = sites[0]
            .second_paths
            .iter()
            .filter(|path| path.iter().all(|point| point.y.abs() < 1e-8))
            .collect::<Vec<_>>();
        assert_eq!(bottom.len(), 1);
        assert!((bottom[0][0].distance_to(bottom[0][1]) - 1.0).abs() < 1e-8);
    }

    #[test]
    fn a_degenerate_reference_point_still_has_a_spatial_site() {
        let material = rect_region(0.0, 0.0, 1.0, 1.0);
        let point = Point::new(0.5, 0.5);
        let sites = linework_clearance_sites(
            &[(point, point)],
            &material,
            &RegionBoundaryIndex::new(&material, 0.2),
            0.2,
            0,
        );
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].distance.mm, 0.0);
        assert_eq!(sites[0].bbox.min, point);
        assert_eq!(sites[0].bbox.max, point);
    }

    #[test]
    fn region_sites_do_not_hide_two_gaps_between_the_same_connected_pair() {
        let first = rect_region(0.0, 0.0, 10.0, 1.0);
        let second = rect_region(2.0, 1.1, 3.0, 4.0)
            .union(&rect_region(7.0, 1.1, 8.0, 4.0))
            .union(&rect_region(2.0, 3.0, 8.0, 4.0));
        assert_eq!(second.connected_components().len(), 1);
        let sites = region_clearance_sites(&first, &second, 0.2);
        assert_eq!(sites.len(), 2);
        let authoritative = region_clearance(&first, &second).unwrap().mm;
        assert!(
            (authoritative - 0.1).abs() < 1e-8,
            "boolean composition snaps coordinates"
        );
        assert!(
            sites
                .iter()
                .all(|site| (site.distance.mm - authoritative).abs() < 1e-12)
        );
        assert!(sites.iter().all(|site| site.bbox.width() < 2.0));
    }

    #[test]
    fn region_sites_keep_contained_overlap_without_a_near_outer_boundary() {
        let first = rect_region(-5.0, -5.0, 5.0, 5.0);
        let second = rect_region(-0.5, -0.5, 0.5, 0.5);
        let sites = region_clearance_sites(&first, &second, 0.2);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].distance.mm, 0.0);
        assert!((sites[0].overlap.area() - 1.0).abs() < 1e-9);
        assert!((sites[0].bbox.width() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn region_sites_merge_boundary_spans_with_their_shared_overlap() {
        let first = rect_region(0.0, 0.0, 2.0, 2.0);
        let second = rect_region(1.0, 0.5, 3.0, 1.5);
        let sites = region_clearance_sites(&first, &second, 0.2);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].distance.mm, 0.0);
        assert!((sites[0].overlap.area() - 1.0).abs() < 1e-9);
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
    fn thresholded_region_clearance_matches_authoritative_measurement() {
        let left = rect_region(0.0, 0.0, 2.0, 2.0);
        let right = rect_region(3.5, 0.5, 5.0, 1.5);
        let index = RegionBoundaryIndex::new(&right, 2.0);

        assert_eq!(
            region_clearance_within(&left, &right, &index, 2.0),
            region_clearance(&left, &right)
        );
        assert_eq!(region_clearance_within(&left, &right, &index, 1.0), None);

        let crossing = rect_region(1.0, -1.0, 1.5, 3.0);
        let crossing_index = RegionBoundaryIndex::new(&crossing, 0.1);
        assert_eq!(
            region_clearance_within(&left, &crossing, &crossing_index, 0.1),
            region_clearance(&left, &crossing)
        );

        let contained = rect_region(0.5, 0.5, 1.5, 1.5);
        let contained_index = RegionBoundaryIndex::new(&contained, 0.1);
        assert_eq!(
            region_clearance_within(&left, &contained, &contained_index, 0.1),
            region_clearance(&left, &contained)
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
    fn sub_resolution_backtracking_does_not_create_opposing_walls() {
        // A real zone boundary can contain a tiny reversal inside an otherwise
        // smooth clearance wall. The opening sheds the resulting nib, but its
        // two locally reversed edges are still one wall, not a copper width.
        let hole_points = [
            (139.5, -98.5),
            (140.5, -98.5),
            (140.5, -99.6),
            (140.384861, -99.716885),
            (140.382971, -99.718775),
            (140.364306, -99.742690),
            (140.363799, -99.743537),
            (140.363792, -99.743546),
            (140.363644, -99.743794),
            (140.346960, -99.796665),
            (140.346570, -99.796621),
            (140.346451, -99.797673),
            (140.346439, -99.797780),
            (140.346499, -99.798125),
            (140.346314, -99.798714),
            (140.346099, -99.801724),
            (140.347144, -99.801798),
            (140.357813, -99.862557),
            (140.356851, -99.862986),
            (140.357874, -99.865284),
            (140.5, -100.0),
            (140.5, -100.5),
            (139.5, -100.5),
        ];
        let hole = ContourBuf::new(
            std::iter::once(PathCmd::move_to(Point::new(
                hole_points[0].0,
                hole_points[0].1,
            )))
            .chain(
                hole_points[1..]
                    .iter()
                    .map(|&(x, y)| PathCmd::line_to(Point::new(x, y))),
            )
            .chain(std::iter::once(PathCmd::close()))
            .collect(),
        );
        let region = ContourSet::from_contours(
            &[rect_at(139.0, -101.0, 141.0, -98.0), hole],
            FillRule::NonZero,
            tol::REGION_MM,
        );
        let candidate_radius = (0.127 + MORPHOLOGY_CANDIDATE_GUARD_MM) / 2.0;

        assert!(
            !region
                .difference(&region.disk_open(candidate_radius))
                .is_empty(),
            "the opening must still localize the one-sided nib"
        );
        assert!(thin_features(&region, 0.127).is_empty());
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
        assert!((piece.disk.radius_mm * 2.0 - piece.width.mm).abs() < 1e-12);
        let points = piece
            .sites
            .iter()
            .flat_map(|site| &site.axis)
            .flatten()
            .collect::<Vec<_>>();
        assert!(
            !points.is_empty(),
            "the full narrow spur has a verified axis"
        );
        assert!(points.iter().any(|point| point.x < 10.2));
        assert!(points.iter().any(|point| point.x > 11.8));
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
        assert!(!findings[0].sites.is_empty());
        assert!(findings[0].sites.iter().all(|site| !site.axis.is_empty()));
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
        let piece = &findings[0];
        assert!(!piece.sites.is_empty());
        assert!(piece.sites.iter().all(|site| !site.axis.is_empty()));
        assert!((piece.disk.radius_mm * 2.0 - width.mm).abs() < 1e-12);
        assert!(
            piece
                .sites
                .iter()
                .flat_map(|site| &site.axis)
                .flatten()
                .all(|point| point.x > 11.65),
            "the confirmed axis must not inherit the guarded candidate's wider extent"
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
