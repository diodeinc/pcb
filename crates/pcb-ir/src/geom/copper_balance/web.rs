//! Sliver-free all-lines hexagonal honeycomb web construction.
//!
//! Lowers a solved perforated copper-balance field into dark-only round-capped
//! polylines: one perimeter trace along the safe-region boundary and shared
//! Voronoi cell walls chained by aperture width. Every stroke lies strictly
//! inside `usable`, every copper feature has width `>= min_copper_web_mm`,
//! grazing contacts and narrow rim void slivers are suppressed, and dangling
//! interior stubs are pruned.

use std::collections::BTreeMap;
use std::collections::hash_map::Entry;

use rustc_hash::FxHashMap;

use super::lattice::SiteTable;
use super::{DenseCopperBalanceProfile, DenseCopperLattice, DenseCopperLatticeSite, SQRT_3};
use crate::geom::path::{ContourBuf, PathCmd, stroke_to_fill};
use crate::geom::region::PreparedRegion;
use crate::geom::{
    AccuracyError, BBox, ContourSet, FillRule, GeometryAccuracy, Point, StrokeStyle,
};

/// Quantization step between consecutive honeycomb wall apertures (40 µm).
/// Keeps per-layer aperture counts small (~5–12 standard round `C` apertures)
/// while letting smooth density gradients step cleanly.
const WALL_WIDTH_STEP_MM: f64 = 0.04;

/// Extra inward erosion margin so stroked round caps stay strictly inside
/// `usable` after 1 µm Gerber coordinate rounding and curve tessellation.
const CONTAINMENT_GUARD_MM: f64 = 0.005;

/// Minimum kept clipped edge length when an edge touches the boundary.
const MIN_CLIPPED_SEGMENT_MM: f64 = 0.08;

/// Grid resolution for topological vertex keys during pruning and chaining.
const VERTEX_KEY_MM: f64 = 0.001;

/// Maximum rim void-sliver pruning iterations per calibration pass.
const MAX_SLIVER_PASSES: usize = 6;

type VertexKey = (i64, i64);

/// One continuous round-capped polyline of the copper-balancing honeycomb web.
#[derive(Debug, Clone, PartialEq)]
pub struct HoneycombPolyline {
    pub width_mm: f64,
    pub points: Vec<Point>,
}

/// Built honeycomb web together with its measured copper area.
#[derive(Debug, Clone)]
pub(super) struct BuiltHoneycombWeb {
    pub(super) polylines: Vec<HoneycombPolyline>,
    pub(super) area_mm2: f64,
}

#[derive(Debug, Clone, Copy)]
struct ClippedEdge {
    start: Point,
    end: Point,
    width_um: i64,
    start_on_boundary: bool,
    end_on_boundary: bool,
    interior_cell_edge: bool,
    sites: (DenseCopperLatticeSite, DenseCopperLatticeSite),
}

impl ClippedEdge {
    fn width_mm(self) -> f64 {
        self.width_um as f64 / 1000.0
    }

    fn midpoint(self) -> Point {
        Point::new(
            0.5 * (self.start.x + self.end.x),
            0.5 * (self.start.y + self.end.y),
        )
    }

    fn bbox(self, pad: f64) -> BBox {
        BBox::new(
            Point::new(self.start.x.min(self.end.x), self.start.y.min(self.end.y)),
            Point::new(self.start.x.max(self.end.x), self.start.y.max(self.end.y)),
        )
        .expand(pad)
    }
}

struct DraftPass {
    rim_w: f64,
    boundary_rings: Vec<Vec<Point>>,
    edges: Vec<ClippedEdge>,
    area_mm2: f64,
    area_slope_mm: f64,
}

/// Cached inward signed distance into `usable` (positive inside, negative outside).
struct DepthField<'a> {
    region: &'a PreparedRegion,
    cache: FxHashMap<VertexKey, f64>,
}

impl<'a> DepthField<'a> {
    fn new(region: &'a PreparedRegion) -> Self {
        Self {
            region,
            cache: FxHashMap::default(),
        }
    }

    fn depth(&mut self, p: Point) -> f64 {
        *self.cache.entry(vertex_key(p)).or_insert_with(|| {
            self.region
                .signed_distance(p)
                .map_or(f64::NEG_INFINITY, |d| -d.mm)
        })
    }
}

struct WebBuilder<'a> {
    usable: &'a ContourSet,
    depths: DepthField<'a>,
    rim_cache: FxHashMap<i64, Option<Vec<Vec<Point>>>>,
    lattice: DenseCopperLattice,
    site_table: SiteTable,
    site_radii: Vec<f64>,
    fallback_radius_mm: f64,
    w_min: f64,
    w_max: f64,
    profile: DenseCopperBalanceProfile,
}

/// Build the sliver-free all-lines honeycomb web for a perforated balance
/// layer from its smooth solved squared void radii, calibrating the discrete
/// aperture ladder to match `target_area_mm2`.
pub(super) fn build_honeycomb_web(
    usable: &ContourSet,
    lattice: DenseCopperLattice,
    sites: &[DenseCopperLatticeSite],
    squared_radii: &[f64],
    fallback_radius_mm: f64,
    target_area_mm2: f64,
    profile: DenseCopperBalanceProfile,
) -> Result<BuiltHoneycombWeb, AccuracyError> {
    if usable.is_empty() {
        return Ok(BuiltHoneycombWeb {
            polylines: Vec::new(),
            area_mm2: 0.0,
        });
    }

    let mut site_table = SiteTable::spanning(sites.iter());
    for site in sites {
        site_table.admit(*site);
    }
    let site_radii: Vec<f64> = squared_radii.iter().map(|r2| r2.max(0.0).sqrt()).collect();

    let w_min = profile.min_copper_web_mm;
    let w_max = (profile.pitch_mm - SQRT_3 * profile.min_void_radius_mm).max(w_min);
    let fallback_w = (profile.pitch_mm - SQRT_3 * fallback_radius_mm).clamp(w_min, w_max);
    let mean_w = if site_radii.is_empty() {
        fallback_w
    } else {
        let mean_r = site_radii.iter().sum::<f64>() / site_radii.len() as f64;
        (profile.pitch_mm - SQRT_3 * mean_r).clamp(w_min, w_max)
    };

    let prepared_usable = usable.prepare_query();
    let mut builder = WebBuilder {
        usable,
        depths: DepthField::new(&prepared_usable),
        rim_cache: FxHashMap::default(),
        lattice,
        site_table,
        site_radii,
        fallback_radius_mm,
        w_min,
        w_max,
        profile,
    };

    // Build first pass at zero width shift, then run up to 2 Newton/secant
    // calibration steps on a uniform additive width shift `delta_w` so the
    // discrete all-lines web matches `target_area_mm2`.
    let tolerance_mm2 = (usable.area() * 1.5e-3).max(1e-4);
    let mut best = builder.build_pass(mean_w, 0.0)?;
    let mut prev_delta = 0.0_f64;
    let mut prev_area = best.area_mm2;
    let mut prior_delta = 0.0_f64;
    let mut prior_area = best.area_mm2;
    let max_shift = (w_max - w_min).max(0.0);

    for iter in 0..2 {
        let area_error = target_area_mm2 - prev_area;
        if area_error.abs() <= tolerance_mm2 {
            break;
        }
        let next_delta = if iter == 0 {
            let slope = best
                .area_slope_mm
                .max(sites.len().max(1) as f64 * SQRT_3 * (profile.pitch_mm - mean_w).max(0.1));
            (prev_delta + area_error / slope).clamp(-max_shift, max_shift)
        } else {
            let d_area = prev_area - prior_area;
            let d_w = prev_delta - prior_delta;
            if d_area.abs() > 1e-6 && d_w.abs() > 1e-6 {
                (prev_delta + area_error / (d_area / d_w).max(1e-3)).clamp(-max_shift, max_shift)
            } else {
                break;
            }
        };
        if (next_delta - prev_delta).abs() < 0.001 {
            break;
        }
        let candidate = builder.build_pass(mean_w, next_delta)?;
        let cand_area = candidate.area_mm2;
        if (target_area_mm2 - cand_area).abs() < (target_area_mm2 - best.area_mm2).abs() {
            best = candidate;
        }
        prior_delta = prev_delta;
        prior_area = prev_area;
        prev_delta = next_delta;
        prev_area = cand_area;
    }

    Ok(BuiltHoneycombWeb {
        polylines: assemble_polylines(&best.edges, best.rim_w, &best.boundary_rings),
        area_mm2: best.area_mm2,
    })
}

impl<'a> WebBuilder<'a> {
    fn build_pass(&mut self, mean_w: f64, delta_w: f64) -> Result<DraftPass, AccuracyError> {
        let anchor_w = (mean_w + delta_w).clamp(self.w_min, self.w_max);
        let (rim_w, boundary_rings) = self.resolve_perimeter_rim(anchor_w)?;
        let ladder = build_width_ladder(anchor_w, self.w_min, self.w_max, rim_w);

        let pitch = self.lattice.pitch_mm;
        let cell_radius = pitch / SQRT_3;
        let vertices = hex_offsets(cell_radius);

        // Enumerate canonical Voronoi edges once (3 outgoing edges per lattice site)
        // and clip each against `{p : depth(p) >= w/2 + CONTAINMENT_GUARD_MM}`.
        let mut edges = Vec::new();
        let search_bbox = self.usable.bbox.expand(cell_radius);
        for site in self
            .lattice
            .sites_covering(self.usable.bbox.expand(cell_radius + pitch))
        {
            let center = self.lattice.center(site);
            let cell_box = BBox::new(
                Point::new(center.x - cell_radius, center.y - cell_radius),
                Point::new(center.x + cell_radius, center.y + cell_radius),
            );
            if !search_bbox.intersects(cell_box) {
                continue;
            }
            let r0 = self.site_radius(site);
            for (nbr, vi, vj) in outgoing_hex_neighbors(site) {
                let r_avg = match (r0, self.site_radius(nbr)) {
                    (Some(a), Some(b)) => 0.5 * (a + b),
                    (Some(a), None) | (None, Some(a)) => a,
                    (None, None) => self
                        .neighbor_radius(site)
                        .unwrap_or(self.fallback_radius_mm),
                };
                let w_um = quantize_width_um(
                    (pitch - SQRT_3 * r_avg + delta_w).clamp(self.w_min, self.w_max),
                    &ladder,
                );
                self.push_clipped_edge(
                    center + vertices[vi],
                    center + vertices[vj],
                    w_um,
                    (site, nbr),
                    cell_radius,
                    &mut edges,
                );
            }
        }

        prune_dangling_stubs(&mut edges);
        let area_mm2 = self.prune_and_measure_area(&boundary_rings, rim_w, &mut edges)?;
        let area_slope_mm = edges
            .iter()
            .map(|e| e.start.distance_to(e.end) * (1.0 - e.width_mm() / pitch).max(0.15))
            .sum();

        Ok(DraftPass {
            rim_w,
            boundary_rings,
            edges,
            area_mm2,
            area_slope_mm,
        })
    }

    fn resolve_perimeter_rim(
        &mut self,
        anchor_w: f64,
    ) -> Result<(f64, Vec<Vec<Point>>), AccuracyError> {
        let floor_w = self.profile.boundary_web_mm;
        let min_void = self.profile.minimum_partial_void_inradius_mm();
        // Safe rim width for even the narrowest certified strip (~1.0 mm from
        // the 0.5 mm opening radius), keeping a `> 2 * min_void` channel.
        let safe_rail_w = (0.5 - min_void - 0.03).max(floor_w).min(self.w_max);
        let desired_w = ((0.5 * anchor_w).clamp(floor_w, self.w_max) * 100.0).round() / 100.0;

        // Try `desired_w`, then `safe_rail_w` (if narrower), then `floor_w`,
        // skipping widths that collapse `usable` or pinch an interior channel
        // narrower than `2 * min_void`.
        let mut candidates = [desired_w, safe_rail_w.min(desired_w), floor_w];
        candidates.sort_by(|a, b| b.total_cmp(a));
        let mut last_um = None;
        for w in candidates {
            let w_um = (w * 1000.0).round() as i64;
            if last_um == Some(w_um) {
                continue;
            }
            last_um = Some(w_um);
            let Some(rings) = self.rim_rings(w_um)? else {
                continue;
            };
            if w > floor_w && self.rim_pinches_narrow_channel(w, min_void)? {
                continue;
            }
            return Ok((w, rings));
        }
        Ok((floor_w, Vec::new()))
    }

    fn rim_pinches_narrow_channel(
        &self,
        rim_w: f64,
        min_void_inradius: f64,
    ) -> Result<bool, AccuracyError> {
        let inner = self.usable.disk_erode(rim_w + CONTAINMENT_GUARD_MM)?;
        for comp in inner.connected_components() {
            if comp.disk_erode(min_void_inradius)?.is_empty() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn rim_rings(&mut self, width_um: i64) -> Result<Option<Vec<Vec<Point>>>, AccuracyError> {
        if let Entry::Vacant(slot) = self.rim_cache.entry(width_um) {
            let inset = self
                .usable
                .disk_erode(0.5 * (width_um as f64 / 1000.0) + CONTAINMENT_GUARD_MM)?;
            let rings = if inset.is_empty() {
                None
            } else {
                Some(extract_boundary_rings(&inset)?)
            };
            slot.insert(rings);
        }
        Ok(self.rim_cache[&width_um].clone())
    }

    fn site_radius(&self, site: DenseCopperLatticeSite) -> Option<f64> {
        self.site_table
            .sample(site)
            .and_then(|idx| self.site_radii.get(idx).copied())
    }

    fn neighbor_radius(&self, site: DenseCopperLatticeSite) -> Option<f64> {
        let mut sum = 0.0;
        let mut count = 0usize;
        for nbr in hex_neighbors(site) {
            if let Some(r) = self.site_radius(nbr) {
                sum += r;
                count += 1;
            }
        }
        (count > 0).then(|| sum / count as f64)
    }

    fn push_clipped_edge(
        &mut self,
        p0: Point,
        p1: Point,
        w_um: i64,
        sites: (DenseCopperLatticeSite, DenseCopperLatticeSite),
        edge_len: f64,
        out: &mut Vec<ClippedEdge>,
    ) {
        let min_depth = 0.5 * (w_um as f64 / 1000.0) + CONTAINMENT_GUARD_MM;
        let d0 = self.depths.depth(p0);
        let d1 = self.depths.depth(p1);
        // Signed distance is 1-Lipschitz: `(d0 + d1 + L)/2 < min_depth` proves the
        // segment lies wholly outside; `(d0 + d1 - L)/2 >= min_depth` proves it
        // lies wholly inside.
        if d0 < min_depth && d1 < min_depth && d0 + d1 + edge_len < 2.0 * min_depth {
            return;
        }
        let interior = |out: &mut Vec<ClippedEdge>| {
            out.push(ClippedEdge {
                start: p0,
                end: p1,
                width_um: w_um,
                start_on_boundary: false,
                end_on_boundary: false,
                interior_cell_edge: true,
                sites,
            });
        };
        if d0 >= min_depth && d1 >= min_depth && d0 + d1 - edge_len >= 2.0 * min_depth {
            interior(out);
            return;
        }

        let spans = clip_segment_by_depth(self.depths.region, p0, p1, d0, d1, min_depth);
        if spans.len() == 1
            && spans[0].0.distance_to(p0) < 1e-4
            && spans[0].1.distance_to(p1) < 1e-4
        {
            interior(out);
            return;
        }
        for (a, b) in spans {
            if a.distance_to(b) >= MIN_CLIPPED_SEGMENT_MM {
                out.push(ClippedEdge {
                    start: a,
                    end: b,
                    width_um: w_um,
                    start_on_boundary: a.distance_to(p0) >= 1e-4,
                    end_on_boundary: b.distance_to(p1) >= 1e-4,
                    interior_cell_edge: false,
                    sites,
                });
            }
        }
    }

    /// Iteratively prune grazing contacts and rim void slivers, then measure
    /// exact copper area using analytical area `(√3/12)*w*(2p-w)` for deep
    /// 6-wall interior cells and exact `stroke_to_fill` union for the rim.
    fn prune_and_measure_area(
        &mut self,
        boundary_rings: &[Vec<Point>],
        rim_w: f64,
        edges: &mut Vec<ClippedEdge>,
    ) -> Result<f64, AccuracyError> {
        let resolution = self.usable.resolution;
        let probe_accuracy = GeometryAccuracy::micrometres(15);

        let boundary_contours: Vec<ContourBuf> = boundary_rings
            .iter()
            .filter_map(|ring| polyline_contour(ring))
            .collect();
        let boundary_fill = if !boundary_contours.is_empty()
            && let Some(filled) = stroke_to_fill(
                &boundary_contours,
                StrokeStyle::round(rim_w),
                probe_accuracy,
            )? {
            Some(ContourSet::from_filled_contours(&filled, resolution)?)
        } else {
            None
        };

        if self.prune_rim_grazing_edges(edges, rim_w) {
            prune_dangling_stubs(edges);
        }

        let cell_radius = self.lattice.pitch_mm / SQRT_3;
        let deep_cell_margin = rim_w + cell_radius + 0.05;

        for pass in 0..MAX_SLIVER_PASSES {
            let (deep_cells, deep_area_mm2) = self.classify_deep_cells(edges, deep_cell_margin);

            let mut rim_fills = Vec::new();
            if let Some(ref bf) = boundary_fill {
                rim_fills.push(bf.clone());
            }
            let mut rim_edge_indices = Vec::new();
            let mut rim_segs_by_width: BTreeMap<i64, Vec<(Point, Point)>> = BTreeMap::new();
            for (idx, edge) in edges.iter().enumerate() {
                if deep_cells.contains_key(&edge.sites.0) && deep_cells.contains_key(&edge.sites.1)
                {
                    continue;
                }
                rim_edge_indices.push(idx);
                rim_segs_by_width
                    .entry(edge.width_um)
                    .or_default()
                    .push((edge.start, edge.end));
            }
            for (w_um, segs) in rim_segs_by_width {
                let style = StrokeStyle::round(w_um as f64 / 1000.0);
                let contours: Vec<ContourBuf> = chain_segments(&segs)
                    .iter()
                    .filter_map(|chain| polyline_contour(chain))
                    .collect();
                if !contours.is_empty()
                    && let Some(filled) = stroke_to_fill(&contours, style, probe_accuracy)?
                {
                    rim_fills.push(ContourSet::from_filled_contours(&filled, resolution)?);
                }
            }

            if rim_fills.is_empty() {
                return Ok(deep_area_mm2);
            }
            let rim_union = ContourSet::union_all(resolution, rim_fills)?;

            if pass + 1 < MAX_SLIVER_PASSES
                && !rim_edge_indices.is_empty()
                && let Some(drop_flags) =
                    self.find_sliver_edges_to_drop(&rim_union, &rim_edge_indices, edges)?
            {
                let mut idx = 0usize;
                edges.retain(|_| {
                    let keep = !drop_flags[idx];
                    idx += 1;
                    keep
                });
                prune_dangling_stubs(edges);
                if self.prune_rim_grazing_edges(edges, rim_w) {
                    prune_dangling_stubs(edges);
                }
                continue;
            }

            let rim_area_mm2 = self.measure_rim_area(&rim_union, &deep_cells)?;
            return Ok(deep_area_mm2 + rim_area_mm2);
        }

        unreachable!("loop returns on final pass")
    }

    fn classify_deep_cells(
        &mut self,
        edges: &[ClippedEdge],
        deep_cell_margin: f64,
    ) -> (FxHashMap<DenseCopperLatticeSite, f64>, f64) {
        let pitch = self.lattice.pitch_mm;
        let mut site_widths: FxHashMap<DenseCopperLatticeSite, Vec<i64>> = FxHashMap::default();
        for edge in edges {
            if edge.interior_cell_edge {
                site_widths
                    .entry(edge.sites.0)
                    .or_default()
                    .push(edge.width_um);
                site_widths
                    .entry(edge.sites.1)
                    .or_default()
                    .push(edge.width_um);
            }
        }
        let mut deep_cells = FxHashMap::default();
        let mut deep_area_mm2 = 0.0;
        for (site, widths) in &site_widths {
            // Only apply the analytical uniform-hex area shortcut when all 6
            // walls share the same aperture width; mixed-width transition cells
            // fall through to exact stroked-polygon measurement.
            if widths.len() == 6
                && widths.iter().all(|&w| w == widths[0])
                && self.depths.depth(self.lattice.center(*site)) >= deep_cell_margin
            {
                let w = widths[0] as f64 / 1000.0;
                let cell_cu = (SQRT_3 / 2.0) * w * (2.0 * pitch - w);
                deep_cells.insert(*site, cell_cu);
                deep_area_mm2 += cell_cu;
            }
        }
        (deep_cells, deep_area_mm2)
    }

    fn find_sliver_edges_to_drop(
        &mut self,
        rim_union: &ContourSet,
        rim_edge_indices: &[usize],
        edges: &[ClippedEdge],
    ) -> Result<Option<Vec<bool>>, AccuracyError> {
        let pitch = self.lattice.pitch_mm;
        let resolution = self.usable.resolution;
        let min_void_inradius = self.profile.minimum_partial_void_inradius_mm();
        let cell_sz = pitch.max(0.5);

        let mut rim_grid: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
        for &idx in rim_edge_indices {
            let eb = edges[idx].bbox(0.5 * edges[idx].width_mm() + 0.04);
            for gx in (eb.min.x / cell_sz).floor() as i32..=(eb.max.x / cell_sz).floor() as i32 {
                for gy in (eb.min.y / cell_sz).floor() as i32..=(eb.max.y / cell_sz).floor() as i32
                {
                    rim_grid.entry((gx, gy)).or_default().push(idx);
                }
            }
        }

        let mut drop_flags = vec![false; edges.len()];
        let mut dropped_any = false;
        for ring in rim_union.rings() {
            let sarea = ring_signed_area(ring);
            if sarea >= 0.0 {
                continue;
            }
            let hole_bbox = ring_bbox(ring);
            if -sarea > 2.5 * pitch * pitch
                && hole_bbox.width().min(hole_bbox.height()) > 1.5 * pitch
            {
                continue;
            }
            let mut ccw = ring.to_vec();
            ccw.reverse();
            let void_poly = ContourSet::from_rings(vec![ccw], FillRule::NonZero, resolution)?;
            if void_poly.is_empty() || !void_poly.disk_erode(min_void_inradius)?.is_empty() {
                continue;
            }
            // Skip clearance cutouts that lie outside `usable`.
            let outside = void_poly.difference(&self.usable.reaching(hole_bbox))?;
            if !outside.is_empty() && outside.area() > 0.25 * void_poly.area() {
                continue;
            }

            // Drop the closest boundary-side rim edge touching this void sliver,
            // breaking ties by shortest then narrowest wall.
            let mut best: Option<(usize, (u8, i64, i64, i64))> = None;
            let gx0 = (hole_bbox.min.x / cell_sz).floor() as i32;
            let gx1 = (hole_bbox.max.x / cell_sz).floor() as i32;
            let gy0 = (hole_bbox.min.y / cell_sz).floor() as i32;
            let gy1 = (hole_bbox.max.y / cell_sz).floor() as i32;
            for gx in gx0..=gx1 {
                for gy in gy0..=gy1 {
                    let Some(bucket) = rim_grid.get(&(gx, gy)) else {
                        continue;
                    };
                    for &idx in bucket {
                        let edge = edges[idx];
                        let pad = 0.5 * edge.width_mm() + 0.04;
                        if !edge.bbox(pad).intersects(hole_bbox) {
                            continue;
                        }
                        let touches = ring.iter().any(|&[x, y]| {
                            point_segment_distance(Point::new(x, y), edge.start, edge.end) <= pad
                        });
                        if !touches {
                            continue;
                        }
                        let tier = match (edge.start_on_boundary, edge.end_on_boundary) {
                            (true, true) => 0,
                            (true, false) | (false, true) => 1,
                            (false, false) => 2,
                        };
                        let score = (
                            tier,
                            (self.depths.depth(edge.midpoint()) * 1000.0).round() as i64,
                            (edge.start.distance_to(edge.end) * 1000.0).round() as i64,
                            edge.width_um,
                        );
                        if best.is_none_or(|(_, s)| score < s) {
                            best = Some((idx, score));
                        }
                    }
                }
            }
            if let Some((idx, _)) = best {
                drop_flags[idx] = true;
                dropped_any = true;
            }
        }

        Ok(dropped_any.then_some(drop_flags))
    }

    fn measure_rim_area(
        &self,
        rim_union: &ContourSet,
        deep_cells: &FxHashMap<DenseCopperLatticeSite, f64>,
    ) -> Result<f64, AccuracyError> {
        if deep_cells.is_empty() {
            return Ok(rim_union.area());
        }
        let hex = hex_offsets(self.lattice.pitch_mm / SQRT_3);
        let border_polys: Vec<Vec<[f64; 2]>> = deep_cells
            .keys()
            .filter(|&&site| {
                hex_neighbors(site)
                    .iter()
                    .any(|n| !deep_cells.contains_key(n))
            })
            .map(|&site| {
                let c = self.lattice.center(site);
                hex.iter().map(|&o| [c.x + o.x, c.y + o.y]).collect()
            })
            .collect();
        if border_polys.is_empty() {
            return Ok(rim_union.area());
        }
        let mask = ContourSet::from_rings(border_polys, FillRule::NonZero, self.usable.resolution)?;
        Ok(rim_union.difference(&mask)?.area())
    }

    /// Remove unanchored edges grazing the inner edge of the perimeter rim
    /// (`-0.06 mm <= gap <= +0.03 mm`), preventing tangential kissing contacts
    /// that create thin copper pinch necks below `min_copper_web_mm`.
    fn prune_rim_grazing_edges(&mut self, edges: &mut Vec<ClippedEdge>, rim_w: f64) -> bool {
        const GRAZE_MIN_MM: f64 = -0.06;
        const GRAZE_MAX_MM: f64 = 0.03;
        const ANCHORED_MAX_MM: f64 = -0.08;

        let rim_inner = rim_w + CONTAINMENT_GUARD_MM;
        let mut anchored: FxHashMap<VertexKey, ()> = FxHashMap::default();
        for edge in edges.iter() {
            let half_w = 0.5 * edge.width_mm();
            let g0 = self.depths.depth(edge.start) - half_w - rim_inner;
            let g1 = self.depths.depth(edge.end) - half_w - rim_inner;
            if g0 <= ANCHORED_MAX_MM || g1 <= ANCHORED_MAX_MM {
                anchored.insert(vertex_key(edge.start), ());
                anchored.insert(vertex_key(edge.end), ());
            }
        }

        let before = edges.len();
        edges.retain(|edge| {
            if edge.start_on_boundary || edge.end_on_boundary {
                return true;
            }
            let half_w = 0.5 * edge.width_mm();
            let g0 = self.depths.depth(edge.start) - half_w - rim_inner;
            let g1 = self.depths.depth(edge.end) - half_w - rim_inner;
            if g0 > GRAZE_MAX_MM + 0.5 && g1 > GRAZE_MAX_MM + 0.5 {
                return true;
            }
            let gm = self.depths.depth(edge.midpoint()) - half_w - rim_inner;
            let g_min = g0.min(g1).min(gm);
            if !(GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&g_min) {
                return true;
            }
            let graze = |g: f64| (GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&g);
            let unanchored = |p: Point| !anchored.contains_key(&vertex_key(p));
            !(graze(gm)
                || (graze(g0) && unanchored(edge.start))
                || (graze(g1) && unanchored(edge.end)))
        });
        edges.len() != before
    }
}

fn assemble_polylines(
    edges: &[ClippedEdge],
    rim_w: f64,
    boundary_rings: &[Vec<Point>],
) -> Vec<HoneycombPolyline> {
    let mut segs_by_width: BTreeMap<i64, Vec<(Point, Point)>> = BTreeMap::new();
    for edge in edges {
        segs_by_width
            .entry(edge.width_um)
            .or_default()
            .push((edge.start, edge.end));
    }
    let rim_w_um = (rim_w * 1000.0).round() as i64;
    if !boundary_rings.is_empty() {
        segs_by_width.entry(rim_w_um).or_default();
    }

    let mut polylines = Vec::new();
    for (w_um, segs) in segs_by_width {
        let width_mm = w_um as f64 / 1000.0;
        for points in chain_segments(&segs) {
            polylines.push(HoneycombPolyline { width_mm, points });
        }
        if w_um == rim_w_um {
            for ring in boundary_rings {
                if ring.len() >= 2 {
                    polylines.push(HoneycombPolyline {
                        width_mm,
                        points: ring.clone(),
                    });
                }
            }
        }
    }
    polylines
}

fn hex_offsets(cell_radius: f64) -> [Point; 6] {
    std::array::from_fn(|i| {
        let angle = i as f64 * std::f64::consts::FRAC_PI_3;
        Point::new(cell_radius * angle.cos(), cell_radius * angle.sin())
    })
}

fn hex_neighbors(site: DenseCopperLatticeSite) -> [DenseCopperLatticeSite; 6] {
    let p = site.column.rem_euclid(2);
    [
        DenseCopperLatticeSite {
            column: site.column,
            row: site.row + 1,
        },
        DenseCopperLatticeSite {
            column: site.column,
            row: site.row - 1,
        },
        DenseCopperLatticeSite {
            column: site.column + 1,
            row: site.row + p,
        },
        DenseCopperLatticeSite {
            column: site.column + 1,
            row: site.row + p - 1,
        },
        DenseCopperLatticeSite {
            column: site.column - 1,
            row: site.row + p,
        },
        DenseCopperLatticeSite {
            column: site.column - 1,
            row: site.row + p - 1,
        },
    ]
}

fn outgoing_hex_neighbors(
    site: DenseCopperLatticeSite,
) -> [(DenseCopperLatticeSite, usize, usize); 3] {
    let p = site.column.rem_euclid(2);
    [
        (
            DenseCopperLatticeSite {
                column: site.column,
                row: site.row + 1,
            },
            1,
            2,
        ),
        (
            DenseCopperLatticeSite {
                column: site.column + 1,
                row: site.row + p,
            },
            0,
            1,
        ),
        (
            DenseCopperLatticeSite {
                column: site.column + 1,
                row: site.row + p - 1,
            },
            5,
            0,
        ),
    ]
}

fn extract_boundary_rings(web_inset: &ContourSet) -> Result<Vec<Vec<Point>>, AccuracyError> {
    if web_inset.is_empty() {
        return Ok(Vec::new());
    }
    let decimated = web_inset.decimate_inward_within(0.010)?;
    let mut rings = Vec::with_capacity(decimated.ring_count());
    for ring in decimated.rings() {
        if ring.len() < 3 {
            continue;
        }
        let mut pts: Vec<Point> = ring.iter().map(|&[x, y]| Point::new(x, y)).collect();
        pts.push(pts[0]);
        rings.push(pts);
    }
    Ok(rings)
}

fn build_width_ladder(anchor_w: f64, w_min: f64, w_max: f64, boundary_web_mm: f64) -> Vec<i64> {
    let to_um = |w: f64| (w.clamp(w_min, w_max) * 1000.0).round() as i64;
    let min_um = to_um(w_min);
    let max_um = to_um(w_max);
    let anchor_um = to_um(anchor_w);
    let step_um = (WALL_WIDTH_STEP_MM * 1000.0).round() as i64;

    let mut widths = vec![min_um, max_um, anchor_um];
    if boundary_web_mm >= w_min && boundary_web_mm <= w_max {
        widths.push(to_um(boundary_web_mm));
    }
    for k in 1..=30 {
        if anchor_um + k * step_um <= max_um {
            widths.push(anchor_um + k * step_um);
        }
        if anchor_um - k * step_um >= min_um {
            widths.push(anchor_um - k * step_um);
        }
    }
    widths.sort_unstable();
    widths.dedup();
    widths
}

fn quantize_width_um(w_mm: f64, ladder: &[i64]) -> i64 {
    let target = (w_mm * 1000.0).round() as i64;
    *ladder
        .iter()
        .min_by_key(|&&cand| (cand - target).abs())
        .expect("width ladder is non-empty")
}

fn vertex_key(p: Point) -> VertexKey {
    (
        (p.x / VERTEX_KEY_MM).round() as i64,
        (p.y / VERTEX_KEY_MM).round() as i64,
    )
}

/// Clip `start..end` against `{p : depth_inside(p) >= min_depth}` using exact
/// 1-Lipschitz recursive subdivision on `PreparedRegion::signed_distance`.
fn clip_segment_by_depth(
    region: &PreparedRegion,
    start: Point,
    end: Point,
    d_start: f64,
    d_end: f64,
    min_depth: f64,
) -> Vec<(Point, Point)> {
    const LEAF_TOL_MM: f64 = 0.002;
    let delta = end - start;
    let total_len = delta.length();
    if total_len <= 1e-12 {
        return Vec::new();
    }

    struct Ctx<'a> {
        region: &'a PreparedRegion,
        start: Point,
        delta: Point,
        total_len: f64,
        min_depth: f64,
    }

    fn recurse(ctx: &Ctx<'_>, t0: f64, t1: f64, d0: f64, d1: f64, out: &mut Vec<(f64, f64)>) {
        let h = (t1 - t0) * ctx.total_len;
        if d0 < ctx.min_depth && d1 < ctx.min_depth && d0 + d1 + h < 2.0 * ctx.min_depth {
            return;
        }
        if (d0 >= ctx.min_depth && d1 >= ctx.min_depth)
            && (d0 + d1 - h >= 2.0 * ctx.min_depth || h <= LEAF_TOL_MM)
        {
            if let Some(last) = out.last_mut()
                && (last.1 - t0).abs() <= 1e-9
            {
                last.1 = t1;
            } else {
                out.push((t0, t1));
            }
            return;
        }
        if h <= LEAF_TOL_MM {
            return;
        }
        let tm = 0.5 * (t0 + t1);
        let dm = ctx
            .region
            .signed_distance(ctx.start + ctx.delta * tm)
            .map_or(f64::NEG_INFINITY, |d| -d.mm);
        recurse(ctx, t0, tm, d0, dm, out);
        recurse(ctx, tm, t1, dm, d1, out);
    }

    let ctx = Ctx {
        region,
        start,
        delta,
        total_len,
        min_depth,
    };
    let mut ts = Vec::new();
    recurse(&ctx, 0.0, 1.0, d_start, d_end, &mut ts);
    ts.into_iter()
        .map(|(t0, t1)| (start + delta * t0, start + delta * t1))
        .collect()
}

fn prune_dangling_stubs(edges: &mut Vec<ClippedEdge>) {
    loop {
        let mut degree: FxHashMap<VertexKey, usize> = FxHashMap::default();
        let mut on_boundary: FxHashMap<VertexKey, bool> = FxHashMap::default();
        for edge in edges.iter() {
            let (k0, k1) = (vertex_key(edge.start), vertex_key(edge.end));
            *degree.entry(k0).or_insert(0) += 1;
            *degree.entry(k1).or_insert(0) += 1;
            *on_boundary.entry(k0).or_insert(false) |= edge.start_on_boundary;
            *on_boundary.entry(k1).or_insert(false) |= edge.end_on_boundary;
        }
        let before = edges.len();
        edges.retain(|edge| {
            let (k0, k1) = (vertex_key(edge.start), vertex_key(edge.end));
            (on_boundary[&k0] || degree[&k0] >= 2) && (on_boundary[&k1] || degree[&k1] >= 2)
        });
        if edges.len() == before {
            break;
        }
    }
}

/// Greedily chain undirected segments of one aperture width into maximal
/// continuous polylines, starting from odd-degree vertices for fewer `D02` moves.
fn chain_segments(segments: &[(Point, Point)]) -> Vec<Vec<Point>> {
    if segments.is_empty() {
        return Vec::new();
    }
    type AdjEntry = (usize, Point, Point, VertexKey);
    let mut adj: FxHashMap<VertexKey, Vec<AdjEntry>> = FxHashMap::default();
    for (idx, &(p0, p1)) in segments.iter().enumerate() {
        let (k0, k1) = (vertex_key(p0), vertex_key(p1));
        if k0 == k1 {
            continue;
        }
        adj.entry(k0).or_default().push((idx, p0, p1, k1));
        adj.entry(k1).or_default().push((idx, p1, p0, k0));
    }
    let mut nodes: Vec<VertexKey> = adj.keys().copied().collect();
    nodes.sort_unstable_by_key(|k| (adj[k].len().is_multiple_of(2), *k));

    let mut used = vec![false; segments.len()];
    let mut polylines = Vec::new();
    let walk = |adj: &FxHashMap<VertexKey, Vec<AdjEntry>>,
                used: &mut [bool],
                mut cur: VertexKey,
                out: &mut Vec<Point>| {
        while let Some((nidx, _, np1, nk)) = adj
            .get(&cur)
            .and_then(|list| list.iter().find(|e| !used[e.0]).copied())
        {
            used[nidx] = true;
            out.push(np1);
            cur = nk;
        }
    };

    for start_node in nodes {
        let edges_at_start = adj[&start_node].clone();
        for (idx, p0, p1, next_k) in edges_at_start {
            if used[idx] {
                continue;
            }
            used[idx] = true;
            let mut chain = vec![p0, p1];
            walk(&adj, &mut used, next_k, &mut chain);
            let mut back = Vec::new();
            walk(&adj, &mut used, start_node, &mut back);
            if !back.is_empty() {
                back.reverse();
                back.extend(chain);
                chain = back;
            }
            polylines.push(chain);
        }
    }
    polylines
}

fn ring_signed_area(ring: &[[f64; 2]]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..ring.len() {
        let [x0, y0] = ring[i];
        let [x1, y1] = ring[(i + 1) % ring.len()];
        sum += x0 * y1 - x1 * y0;
    }
    0.5 * sum
}

fn ring_bbox(ring: &[[f64; 2]]) -> BBox {
    let mut bbox = BBox::empty();
    for &[x, y] in ring {
        bbox.include_point(Point::new(x, y));
    }
    bbox
}

fn point_segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let ab = b - a;
    let len2 = ab.x * ab.x + ab.y * ab.y;
    if len2 <= 1e-18 {
        return p.distance_to(a);
    }
    let ap = p - a;
    let t = ((ap.x * ab.x + ap.y * ab.y) / len2).clamp(0.0, 1.0);
    p.distance_to(Point::new(a.x + t * ab.x, a.y + t * ab.y))
}

fn polyline_contour(points: &[Point]) -> Option<ContourBuf> {
    let (&first, rest) = points.split_first()?;
    if rest.is_empty() {
        return None;
    }
    let mut cmds = Vec::with_capacity(points.len());
    cmds.push(PathCmd::move_to(first));
    for &point in rest {
        cmds.push(PathCmd::line_to(point));
    }
    Some(ContourBuf::new(cmds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::copper_balance::{
        DenseCopperBalanceResult, SpatialCopperBalanceLayerRequest, SpatialCopperBalanceRequest,
        generate_spatial_dense_copper_balance,
    };
    use crate::geom::{Resolution, shapes};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourSet {
        let res = Resolution::default().with_accuracy(GeometryAccuracy::micrometres(50));
        ContourSet::from_rings(
            vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]],
            FillRule::NonZero,
            res,
        )
        .unwrap()
    }

    fn solve_single_layer(safe: &ContourSet, target: f64) -> DenseCopperBalanceResult {
        let profile = DenseCopperBalanceProfile::V1;
        let empty = ContourSet::empty(safe.resolution);
        generate_spatial_dense_copper_balance(
            profile,
            SpatialCopperBalanceRequest {
                panel_region: safe,
                lattice_origin: Point::ZERO,
                layers: &[SpatialCopperBalanceLayerRequest {
                    safe_region: safe,
                    existing_copper: &empty,
                    density_domain: safe,
                    target_density: target,
                    stack_weight_mm2: 0.0,
                }],
            },
        )
        .unwrap()
        .layers
        .pop()
        .unwrap()
    }

    fn stroke_web(polylines: &[HoneycombPolyline], res: Resolution) -> ContourSet {
        let fills: Vec<_> = polylines
            .iter()
            .filter_map(|poly| {
                let c = polyline_contour(&poly.points)?;
                let filled = stroke_to_fill(&[c], StrokeStyle::round(poly.width_mm), res.accuracy)
                    .unwrap()?;
                Some(ContourSet::from_filled_contours(&filled, res).unwrap())
            })
            .collect();
        ContourSet::union_all(res, fills).unwrap()
    }

    fn assert_web_clean(safe: &ContourSet, res: &DenseCopperBalanceResult) -> ContourSet {
        let profile = DenseCopperBalanceProfile::V1;
        let copper = stroke_web(&res.web, safe.resolution);
        let leakage = copper.difference(safe).unwrap().area();
        assert!(leakage <= 1e-3, "leakage={leakage}");

        let exact_density = copper.area() / safe.area();
        assert!(
            (res.solution.achieved_density - exact_density).abs() <= 0.005,
            "measured={} exact={exact_density}",
            res.solution.achieved_density
        );

        let min_void_inradius = profile.minimum_partial_void_inradius_mm();
        for ring in copper.rings() {
            if ring_signed_area(ring) >= 0.0 {
                continue;
            }
            let mut ccw = ring.to_vec();
            ccw.reverse();
            let vp = ContourSet::from_rings(vec![ccw], FillRule::NonZero, safe.resolution).unwrap();
            let outside = vp.difference(safe).unwrap();
            if !outside.is_empty() && outside.area() > 0.25 * vp.area() {
                continue;
            }
            assert!(
                !vp.disk_erode(min_void_inradius).unwrap().is_empty(),
                "void sliver at {:?} area={}",
                vp.bbox,
                vp.area()
            );
        }
        let thin_cu = crate::geom::dfm::thin_features(&copper, 0.15).unwrap();
        assert!(thin_cu.is_empty(), "thin copper: {:?}", thin_cu[0].width);
        assert!(
            copper
                .disk_inter_component_gap_violations(0.08)
                .unwrap()
                .is_empty(),
            "inter-component copper gap violation"
        );
        copper
    }

    #[test]
    fn web_area_containment_and_slivers_across_densities() {
        for ((w, h), target) in [
            ((20.0, 10.0), 0.35),
            ((20.0, 10.0), 0.50),
            ((20.0, 10.0), 0.75),
            ((24.0, 16.0), 0.90),
            ((24.0, 16.0), 0.94),
            ((12.0, 1.10), 0.90),
            ((12.0, 0.90), 0.90),
        ] {
            let safe = rect(0.0, 0.0, w, h);
            let res = solve_single_layer(&safe, target);
            assert_web_clean(&safe, &res);
        }
    }

    #[test]
    fn web_has_zero_slivers_around_curved_holes_and_concave_corners() {
        let res = Resolution::default().with_accuracy(DenseCopperBalanceProfile::V1.accuracy);
        let mut safe = rect(0.0, 0.0, 24.0, 16.0)
            .difference(&rect(14.0, 8.0, 24.0, 16.0))
            .unwrap();
        for (center, radius) in [
            (Point::new(5.2, 4.7), 1.6),
            (Point::new(10.1, 11.3), 2.4),
            (Point::new(18.5, 4.1), 1.1),
            (Point::new(6.8, 12.2), 0.75),
        ] {
            let disk = ContourSet::from_contours(
                &[shapes::circle(radius).unwrap()],
                FillRule::NonZero,
                res,
            )
            .unwrap()
            .translated(center)
            .unwrap();
            safe = safe.difference(&disk).unwrap();
        }

        for target in [0.40, 0.65, 0.85] {
            let result = solve_single_layer(&safe, target);
            assert!(
                (result.solution.achieved_density - target).abs() <= 0.01,
                "target={target} achieved={}",
                result.solution.achieved_density
            );
            assert_web_clean(&safe, &result);
        }
    }

    /// Regression test for RS0001: a high-density horizontal honeycomb wall
    /// aligned nearly tangentially with a slightly sloped perimeter rim must
    /// not be left in grazing contact (which creates a sub-min-web copper pinch
    /// neck between the rim stroke and the parallel wall stroke).
    #[test]
    fn web_avoids_rim_grazing_copper_pinches() {
        let profile = DenseCopperBalanceProfile::V1;
        let res = Resolution::default().with_accuracy(profile.accuracy);
        let row_step = profile.pitch_mm * SQRT_3 / 2.0;

        for step in 0..16 {
            let top_y = 14.0 + (step as f64 / 16.0) * row_step;
            let safe = ContourSet::from_rings(
                vec![vec![
                    [0.0, 0.0],
                    [16.0, 0.0],
                    [16.0, top_y - 0.006],
                    [0.0, top_y],
                ]],
                FillRule::NonZero,
                res,
            )
            .unwrap();

            let result = solve_single_layer(&safe, 0.90);
            let copper = assert_web_clean(&safe, &result);

            let before = (copper.connected_components().len(), copper.rings().count());
            let eroded = copper.disk_erode(0.08).unwrap();
            let after = (eroded.connected_components().len(), eroded.rings().count());
            assert_eq!(
                before, after,
                "step={step} top_y={top_y:.4} thin copper pinch changed topology {before:?} -> {after:?}"
            );
        }
    }

    #[test]
    fn fill_selection_uses_thinner_stroked_web_at_low_density_crossover() {
        let safe = rect(0.0, 0.0, 40.0, 40.0);

        // At 14.6% on a broad gutter, the 0.20 mm honeycomb web (~27.4%) is
        // strictly closer to target than bare copper (0%), even though the
        // legacy rounded-hex model (~31.8%) would have chosen `None`.
        let crossover = solve_single_layer(&safe, 0.146);
        assert!(matches!(
            crossover.solution.mode,
            crate::geom::copper_balance::DenseCopperBalanceMode::Perforated { .. }
        ));
        assert!(crossover.solution.residual_error < 0.146);
        assert_web_clean(&safe, &crossover);

        // Between the stroked floor (~27.4%) and the legacy floor (~31.8%),
        // an unweighed layer calibrates down to the requested density.
        let low_band = solve_single_layer(&safe, 0.29);
        assert!(matches!(
            low_band.solution.mode,
            crate::geom::copper_balance::DenseCopperBalanceMode::Perforated { .. }
        ));
        assert!(
            low_band.solution.residual_error < 0.015,
            "expected ~0.29, got {:.4}",
            low_band.solution.achieved_density
        );
        assert_web_clean(&safe, &low_band);

        // Far below half the minimum web coverage still stays bare.
        let bare = solve_single_layer(&safe, 0.10);
        assert!(matches!(
            bare.solution.mode,
            crate::geom::copper_balance::DenseCopperBalanceMode::None
        ));
        assert!(bare.web.is_empty());

        // On a weighed stackup, crossover promotion happens before stack-moment
        // settlement so the mirror layer shifts toward the promoted web's
        // actual copper rather than away from it.
        let panel = rect(0.0, 0.0, 40.0, 40.0);
        let existing_l1 = rect(0.0, 0.0, 10.0, 40.0); // 25% fixed copper
        let safe_l1 = rect(10.0, 0.0, 40.0, 40.0); // 75% usable; desired +15% = 20% of usable
        let empty = ContourSet::empty(safe.resolution);
        let weighted = crate::geom::copper_balance::generate_spatial_dense_copper_balance(
            DenseCopperBalanceProfile::V1,
            crate::geom::copper_balance::SpatialCopperBalanceRequest {
                panel_region: &panel,
                lattice_origin: Point::ZERO,
                layers: &[
                    crate::geom::copper_balance::SpatialCopperBalanceLayerRequest {
                        density_domain: &panel,
                        existing_copper: &existing_l1,
                        safe_region: &safe_l1,
                        target_density: 0.36, // desired +11% of panel = 14.7% of usable
                        stack_weight_mm2: 1.0,
                    },
                    crate::geom::copper_balance::SpatialCopperBalanceLayerRequest {
                        density_domain: &panel,
                        existing_copper: &empty,
                        safe_region: &panel,
                        target_density: 0.36,
                        stack_weight_mm2: -1.0,
                    },
                ],
            },
        )
        .unwrap();
        assert!(matches!(
            weighted.layers[0].solution.mode,
            crate::geom::copper_balance::DenseCopperBalanceMode::Perforated { .. }
        ));
        let field = weighted.moment_field.expect("weighed stackup");
        assert!(
            field.achieved_rms <= field.initial_rms + 1e-6,
            "stack moment worsened: {field:?}"
        );
    }
}
