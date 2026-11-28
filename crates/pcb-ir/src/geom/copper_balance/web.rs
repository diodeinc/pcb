//! Sliver-free all-lines hexagonal honeycomb web construction.
//!
//! Lowers a solved perforated copper-balance field into dark-only round-capped
//! polylines: one perimeter trace along the safe-region boundary and shared
//! Voronoi cell walls chained by aperture width. Every stroke lies strictly
//! inside `usable`, every copper feature has width `>= min_copper_web_mm`,
//! parallel narrow void slots against the boundary trace are suppressed, and
//! dangling interior stubs are pruned.

use std::collections::{BTreeMap, HashMap};

use rustc_hash::FxHashMap;

use super::lattice::SiteTable;
use super::{DenseCopperBalanceProfile, DenseCopperLattice, DenseCopperLatticeSite, SQRT_3};
use crate::geom::path::{ContourBuf, PathCmd, stroke_to_fill};
use crate::geom::region::PreparedRegion;
use crate::geom::{AccuracyError, BBox, ContourSet, FillRule, Point, StrokeStyle};

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
    area_slope_mm: f64,
    prev_delta_w: f64,
    prev_area_mm2: f64,
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
            area_slope_mm: 0.0,
            prev_delta_w: 0.0,
            prev_area_mm2: 0.0,
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

    // Mean solved wall width across admitted sites anchors the aperture ladder
    // so uniform rails land on one exact aperture without cell-to-cell dithering.
    let mean_w = if site_radii.is_empty() {
        fallback_w
    } else {
        let sum_r: f64 = site_radii.iter().sum();
        let mean_r = sum_r / site_radii.len() as f64;
        (profile.pitch_mm - SQRT_3 * mean_r).clamp(w_min, w_max)
    };

    let prepared_usable = usable.prepare_query();
    let mut depth_cache: FxHashMap<(i64, i64), f64> = FxHashMap::default();
    let mut rim_cache: FxHashMap<i64, Option<Vec<Vec<Point>>>> = FxHashMap::default();

    // Build first pass at zero width shift, anchored at the solver's mean wall
    // width. Then run up to 2 Newton/secant calibration steps on a uniform
    // additive width shift `delta_w` so the discrete all-lines web matches
    // `target_area_mm2` accurately across both small rims and full panels.
    let tolerance_mm2 = (usable.area() * 1.5e-3).max(1e-4);
    let mut best = build_pass(
        usable,
        &prepared_usable,
        &mut depth_cache,
        &mut rim_cache,
        lattice,
        &site_table,
        &site_radii,
        fallback_radius_mm,
        mean_w,
        0.0,
        w_min,
        w_max,
        profile,
    )?;

    let mut prev_delta = 0.0_f64;
    let mut prev_area = best.area_mm2;
    let max_shift_up = (w_max - w_min).max(0.0);
    let max_shift_down = -(w_max - w_min).max(0.0);

    for iter in 0..2 {
        let area_error = target_area_mm2 - prev_area;
        if area_error.abs() <= tolerance_mm2 {
            break;
        }
        let next_delta = if iter == 0 {
            let slope = best
                .area_slope_mm
                .max(sites.len().max(1) as f64 * SQRT_3 * (profile.pitch_mm - mean_w).max(0.1));
            (prev_delta + area_error / slope).clamp(max_shift_down, max_shift_up)
        } else {
            let d_area = prev_area - best.prev_area_mm2;
            let d_w = prev_delta - best.prev_delta_w;
            if d_area.abs() > 1e-6 && d_w.abs() > 1e-6 {
                let slope = (d_area / d_w).max(1e-3);
                (prev_delta + area_error / slope).clamp(max_shift_down, max_shift_up)
            } else {
                break;
            }
        };
        if (next_delta - prev_delta).abs() < 0.001 {
            break;
        }
        let mut candidate = build_pass(
            usable,
            &prepared_usable,
            &mut depth_cache,
            &mut rim_cache,
            lattice,
            &site_table,
            &site_radii,
            fallback_radius_mm,
            mean_w,
            next_delta,
            w_min,
            w_max,
            profile,
        )?;
        candidate.prev_delta_w = prev_delta;
        candidate.prev_area_mm2 = prev_area;
        let cand_area = candidate.area_mm2;
        if (target_area_mm2 - cand_area).abs() < (target_area_mm2 - best.area_mm2).abs() {
            best = candidate;
        } else {
            best.prev_delta_w = prev_delta;
            best.prev_area_mm2 = prev_area;
        }
        prev_delta = next_delta;
        prev_area = cand_area;
    }

    Ok(best)
}

#[allow(clippy::too_many_arguments)]
fn build_pass(
    usable: &ContourSet,
    prepared_usable: &PreparedRegion,
    depth_cache: &mut FxHashMap<(i64, i64), f64>,
    rim_cache: &mut FxHashMap<i64, Option<Vec<Vec<Point>>>>,
    lattice: DenseCopperLattice,
    site_table: &SiteTable,
    site_radii: &[f64],
    fallback_radius_mm: f64,
    mean_w: f64,
    delta_w: f64,
    w_min: f64,
    w_max: f64,
    profile: DenseCopperBalanceProfile,
) -> Result<BuiltHoneycombWeb, AccuracyError> {
    let anchor_w = (mean_w + delta_w).clamp(w_min, w_max);
    let desired_rim_w = ((0.5 * anchor_w).max(profile.boundary_web_mm) * 100.0).round() / 100.0;
    let desired_rim_w = desired_rim_w.clamp(profile.boundary_web_mm, w_max);
    let desired_rim_um = (desired_rim_w * 1000.0).round() as i64;
    let fallback_rim_um = (profile.boundary_web_mm * 1000.0).round() as i64;
    let (rim_w, boundary_rings) = {
        if let Some(rings) = get_or_extract_rim(usable, desired_rim_um, rim_cache)? {
            (desired_rim_w, rings.clone())
        } else if desired_rim_um > fallback_rim_um {
            let rings = get_or_extract_rim(usable, fallback_rim_um, rim_cache)?
                .clone()
                .unwrap_or_default();
            (profile.boundary_web_mm, rings)
        } else {
            (desired_rim_w, Vec::new())
        }
    };
    let ladder = build_width_ladder(anchor_w, w_min, w_max, rim_w);
    let pitch = lattice.pitch_mm;
    let cell_radius = pitch / SQRT_3;
    let edge_length = cell_radius;
    let vertices = std::array::from_fn::<Point, 6, _>(|i| {
        let angle = i as f64 * std::f64::consts::FRAC_PI_3;
        Point::new(cell_radius * angle.cos(), cell_radius * angle.sin())
    });

    // Enumerate canonical Voronoi edges once (3 outgoing edges per lattice site).
    let covering_sites = lattice.sites_covering(usable.bbox.expand(cell_radius + pitch));
    let mut raw_by_width: BTreeMap<
        i64,
        Vec<(Point, Point, DenseCopperLatticeSite, DenseCopperLatticeSite)>,
    > = BTreeMap::new();

    for site in covering_sites {
        let center = lattice.center(site);
        let cell_box = BBox::new(
            Point::new(center.x - cell_radius, center.y - cell_radius),
            Point::new(center.x + cell_radius, center.y + cell_radius),
        );
        if !usable.bbox.expand(cell_radius).intersects(cell_box) {
            continue;
        }
        let parity = site.column.rem_euclid(2);
        let neighbors = [
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
                    row: site.row + parity,
                },
                0,
                1,
            ),
            (
                DenseCopperLatticeSite {
                    column: site.column + 1,
                    row: site.row + parity - 1,
                },
                5,
                0,
            ),
        ];
        let r0 = lookup_site_radius(site_table, site_radii, site);
        for (nbr, vi, vj) in neighbors {
            let r1 = lookup_site_radius(site_table, site_radii, nbr);
            let r_avg = match (r0, r1) {
                (Some(a), Some(b)) => 0.5 * (a + b),
                (Some(a), None) | (None, Some(a)) => a,
                (None, None) => nearest_neighbor_radius(site_table, site_radii, site)
                    .unwrap_or(fallback_radius_mm),
            };
            let w_raw = (pitch - SQRT_3 * r_avg + delta_w).clamp(w_min, w_max);
            let w_um = quantize_width_um(w_raw, &ladder);
            raw_by_width.entry(w_um).or_default().push((
                center + vertices[vi],
                center + vertices[vj],
                site,
                nbr,
            ));
        }
    }

    // Cache signed depths of Voronoi vertices inside `usable` across passes
    // (each vertex is shared by 3 walls, cutting BVH queries by 3x).
    let mut depth_inside = |pt: Point| -> f64 {
        let key = vertex_key(pt);
        *depth_cache.entry(key).or_insert_with(|| {
            prepared_usable
                .signed_distance(pt)
                .map_or(f64::NEG_INFINITY, |d| -d.mm)
        })
    };

    let mut kept_edges: Vec<ClippedEdge> = Vec::new();
    for (w_um, candidates) in raw_by_width {
        let w = w_um as f64 / 1000.0;
        let min_depth = 0.5 * w + CONTAINMENT_GUARD_MM;

        for (p0, p1, s0, s1) in candidates {
            let d0 = depth_inside(p0);
            let d1 = depth_inside(p1);
            // Since inward signed distance is 1-Lipschitz, `(d0 + d1 + L)/2 < min_depth`
            // proves the entire segment stays outside `depth >= min_depth`, while
            // `(d0 + d1 - L)/2 >= min_depth` proves the entire segment stays inside.
            if d0 < min_depth && d1 < min_depth && d0 + d1 + edge_length < 2.0 * min_depth {
                continue;
            }
            if d0 >= min_depth && d1 >= min_depth && d0 + d1 - edge_length >= 2.0 * min_depth {
                kept_edges.push(ClippedEdge {
                    start: p0,
                    end: p1,
                    width_um: w_um,
                    start_on_boundary: false,
                    end_on_boundary: false,
                    interior_cell_edge: true,
                    sites: (s0, s1),
                });
                continue;
            }

            // Exact 1-Lipschitz distance clip against `{p : depth(p) >= min_depth}`
            // (zero per-width `disk_erode` calls and no artificial ray extensions).
            let spans = clip_segment_by_depth(prepared_usable, p0, p1, d0, d1, min_depth);
            if spans.len() == 1
                && spans[0].0.distance_to(p0) < 1e-4
                && spans[0].1.distance_to(p1) < 1e-4
            {
                kept_edges.push(ClippedEdge {
                    start: p0,
                    end: p1,
                    width_um: w_um,
                    start_on_boundary: false,
                    end_on_boundary: false,
                    interior_cell_edge: true,
                    sites: (s0, s1),
                });
            } else {
                for (a, b) in spans {
                    if a.distance_to(b) >= MIN_CLIPPED_SEGMENT_MM {
                        kept_edges.push(ClippedEdge {
                            start: a,
                            end: b,
                            width_um: w_um,
                            start_on_boundary: a.distance_to(p0) >= 1e-4,
                            end_on_boundary: b.distance_to(p1) >= 1e-4,
                            interior_cell_edge: false,
                            sites: (s0, s1),
                        });
                    }
                }
            }
        }
    }

    // Iteratively prune degree-1 interior vertices so no dangling dead-end
    // stubs survive anywhere along the perimeter.
    prune_dangling_stubs(&mut kept_edges);

    // Inspect enclosed voids in the rim against `minimum_partial_void_inradius_mm`,
    // remove any rim wall creating an unetchable void sliver, re-prune stubs,
    // and return the exact total copper area.
    let area_mm2 = prune_rim_void_slivers_and_measure_area(
        usable,
        prepared_usable,
        depth_cache,
        &boundary_rings,
        rim_w,
        &mut kept_edges,
        lattice,
        profile,
        6,
    )?;

    let area_slope_mm: f64 = kept_edges
        .iter()
        .map(|edge| {
            let w = edge.width_um as f64 / 1000.0;
            edge.start.distance_to(edge.end) * (1.0 - w / pitch).max(0.15)
        })
        .sum();

    // Chain edges of each width into continuous polylines and append the
    // perimeter web at `rim_w`.
    let mut segs_by_width: BTreeMap<i64, Vec<(Point, Point)>> = BTreeMap::new();
    for edge in &kept_edges {
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
        for chain in chain_segments(&segs) {
            polylines.push(HoneycombPolyline {
                width_mm,
                points: chain,
            });
        }
        if w_um == rim_w_um {
            for ring in &boundary_rings {
                if ring.len() >= 2 {
                    polylines.push(HoneycombPolyline {
                        width_mm,
                        points: ring.clone(),
                    });
                }
            }
        }
    }

    Ok(BuiltHoneycombWeb {
        polylines,
        area_mm2,
        area_slope_mm,
        prev_delta_w: delta_w,
        prev_area_mm2: area_mm2,
    })
}

fn get_or_extract_rim<'a>(
    usable: &ContourSet,
    width_um: i64,
    cache: &'a mut FxHashMap<i64, Option<Vec<Vec<Point>>>>,
) -> Result<&'a Option<Vec<Vec<Point>>>, AccuracyError> {
    if let std::collections::hash_map::Entry::Vacant(slot) = cache.entry(width_um) {
        let inset = erode_for_stroke(usable, width_um as f64 / 1000.0)?;
        let entry = if inset.is_empty() {
            None
        } else {
            Some(extract_boundary_rings(&inset)?)
        };
        slot.insert(entry);
    }
    Ok(&cache[&width_um])
}

fn erode_for_stroke(usable: &ContourSet, width_mm: f64) -> Result<ContourSet, AccuracyError> {
    usable.disk_erode(0.5 * width_mm + CONTAINMENT_GUARD_MM)
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
        if let Some(&first) = pts.first() {
            pts.push(first);
        }
        rings.push(pts);
    }
    Ok(rings)
}

fn build_width_ladder(anchor_w: f64, w_min: f64, w_max: f64, boundary_web_mm: f64) -> Vec<i64> {
    let mut widths = Vec::new();
    let push_mm = |out: &mut Vec<i64>, w: f64| {
        let um = (w.clamp(w_min, w_max) * 1000.0).round() as i64;
        out.push(um);
    };
    push_mm(&mut widths, w_min);
    push_mm(&mut widths, w_max);
    if boundary_web_mm >= w_min && boundary_web_mm <= w_max {
        push_mm(&mut widths, boundary_web_mm);
    }
    let anchor_um = (anchor_w.clamp(w_min, w_max) * 1000.0).round() as i64;
    let step_um = (WALL_WIDTH_STEP_MM * 1000.0).round() as i64;
    let min_um = (w_min * 1000.0).round() as i64;
    let max_um = (w_max * 1000.0).round() as i64;
    widths.push(anchor_um);
    for k in 1..=30 {
        let up = anchor_um + k * step_um;
        if up <= max_um {
            widths.push(up);
        }
        let down = anchor_um - k * step_um;
        if down >= min_um {
            widths.push(down);
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

fn lookup_site_radius(
    table: &SiteTable,
    radii: &[f64],
    site: DenseCopperLatticeSite,
) -> Option<f64> {
    table.sample(site).and_then(|idx| radii.get(idx).copied())
}

fn nearest_neighbor_radius(
    table: &SiteTable,
    radii: &[f64],
    site: DenseCopperLatticeSite,
) -> Option<f64> {
    let parity = site.column.rem_euclid(2);
    let neighbors = [
        (site.column, site.row + 1),
        (site.column, site.row - 1),
        (site.column + 1, site.row + parity),
        (site.column + 1, site.row + parity - 1),
        (site.column - 1, site.row + parity),
        (site.column - 1, site.row + parity - 1),
    ];
    let mut sum = 0.0;
    let mut count = 0usize;
    for (column, row) in neighbors {
        if let Some(r) = lookup_site_radius(table, radii, DenseCopperLatticeSite { column, row }) {
            sum += r;
            count += 1;
        }
    }
    (count > 0).then(|| sum / count as f64)
}

fn vertex_key(p: Point) -> (i64, i64) {
    (
        (p.x / VERTEX_KEY_MM).round() as i64,
        (p.y / VERTEX_KEY_MM).round() as i64,
    )
}

/// Clip `start..end` against `{p : depth_inside(p) >= min_depth}` using exact
/// 1-Lipschitz recursive subdivision on `PreparedRegion::signed_distance`.
///
/// Guarantees every point on every returned span satisfies `depth(p) >= min_depth - tol/2`
/// (with `tol = 0.002 mm`, strictly inside `CONTAINMENT_GUARD_MM = 0.005 mm`)
/// without running `usable.disk_erode` for every quantized stroke width.
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

    struct DepthClipCtx<'a> {
        region: &'a PreparedRegion,
        start: Point,
        delta: Point,
        total_len: f64,
        min_depth: f64,
    }

    fn recurse(
        ctx: &DepthClipCtx<'_>,
        t0: f64,
        t1: f64,
        d0: f64,
        d1: f64,
        out: &mut Vec<(f64, f64)>,
    ) {
        let h = (t1 - t0) * ctx.total_len;
        if d0 < ctx.min_depth && d1 < ctx.min_depth && d0 + d1 + h < 2.0 * ctx.min_depth {
            return;
        }
        if d0 >= ctx.min_depth && d1 >= ctx.min_depth && d0 + d1 - h >= 2.0 * ctx.min_depth {
            push_merged_span(out, t0, t1);
            return;
        }
        if h <= LEAF_TOL_MM {
            if d0 >= ctx.min_depth && d1 >= ctx.min_depth {
                push_merged_span(out, t0, t1);
            }
            return;
        }
        let tm = 0.5 * (t0 + t1);
        let pm = ctx.start + ctx.delta * tm;
        let dm = ctx
            .region
            .signed_distance(pm)
            .map_or(f64::NEG_INFINITY, |d| -d.mm);
        recurse(ctx, t0, tm, d0, dm, out);
        recurse(ctx, tm, t1, dm, d1, out);
    }

    fn push_merged_span(out: &mut Vec<(f64, f64)>, t0: f64, t1: f64) {
        if let Some(last) = out.last_mut()
            && (last.1 - t0).abs() <= 1e-9
        {
            last.1 = t1;
        } else {
            out.push((t0, t1));
        }
    }

    let ctx = DepthClipCtx {
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

/// Remove any unanchored edge that grazes the inner edge of the perimeter rim
/// (`-0.06 mm <= gap <= +0.03 mm`), preventing tangential kissing contacts that
/// create thin copper pinch necks below `min_copper_web_mm`.
fn prune_rim_grazing_edges(
    edges: &mut Vec<ClippedEdge>,
    prepared_usable: &PreparedRegion,
    depth_cache: &mut FxHashMap<(i64, i64), f64>,
    rim_w: f64,
) -> bool {
    const GRAZE_MIN_MM: f64 = -0.06;
    const GRAZE_MAX_MM: f64 = 0.03;
    const ANCHORED_MAX_MM: f64 = -0.08;

    let mut depth_of = |p: Point| -> f64 {
        let key = vertex_key(p);
        *depth_cache.entry(key).or_insert_with(|| {
            prepared_usable
                .signed_distance(p)
                .map_or(f64::NEG_INFINITY, |d| -d.mm)
        })
    };
    let rim_inner = rim_w + CONTAINMENT_GUARD_MM;

    // A vertex is firmly anchored into the rim if at least one incident edge
    // penetrates past `ANCHORED_MAX_MM` (e.g. a spoke clipped at `∂inset_w`).
    let mut anchored_verts: FxHashMap<(i64, i64), bool> = FxHashMap::default();
    for edge in edges.iter() {
        let half_w = 0.5 * (edge.width_um as f64 / 1000.0);
        let g0 = depth_of(edge.start) - half_w - rim_inner;
        let g1 = depth_of(edge.end) - half_w - rim_inner;
        if g0 <= ANCHORED_MAX_MM || g1 <= ANCHORED_MAX_MM {
            anchored_verts.insert(vertex_key(edge.start), true);
            anchored_verts.insert(vertex_key(edge.end), true);
        }
    }

    let before = edges.len();
    edges.retain(|edge| {
        if edge.start_on_boundary || edge.end_on_boundary {
            return true;
        }
        let half_w = 0.5 * (edge.width_um as f64 / 1000.0);
        let g0 = depth_of(edge.start) - half_w - rim_inner;
        let g1 = depth_of(edge.end) - half_w - rim_inner;
        // Fast skip for edges safely deep inside the panel.
        if g0 > GRAZE_MAX_MM + 0.5 && g1 > GRAZE_MAX_MM + 0.5 {
            return true;
        }
        let mid = Point::new(
            0.5 * (edge.start.x + edge.end.x),
            0.5 * (edge.start.y + edge.end.y),
        );
        let gm = depth_of(mid) - half_w - rim_inner;
        let g_min = g0.min(g1).min(gm);
        if (GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&g_min) {
            let k0 = vertex_key(edge.start);
            let k1 = vertex_key(edge.end);
            // Grazing the rim without an anchored spoke into the rim creates a
            // thin tangential pinch bridge (or sub-30 µm void cusp); drop it.
            let mid_grazes = (GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&gm);
            let v0_grazes_unanchored =
                (GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&g0) && !anchored_verts.contains_key(&k0);
            let v1_grazes_unanchored =
                (GRAZE_MIN_MM..=GRAZE_MAX_MM).contains(&g1) && !anchored_verts.contains_key(&k1);
            if mid_grazes || v0_grazes_unanchored || v1_grazes_unanchored {
                return false;
            }
        }
        true
    });
    edges.len() != before
}

fn prune_dangling_stubs(edges: &mut Vec<ClippedEdge>) {
    loop {
        let mut degree: FxHashMap<(i64, i64), usize> = FxHashMap::default();
        let mut on_boundary: FxHashMap<(i64, i64), bool> = FxHashMap::default();
        for edge in edges.iter() {
            let k0 = vertex_key(edge.start);
            let k1 = vertex_key(edge.end);
            *degree.entry(k0).or_insert(0) += 1;
            *degree.entry(k1).or_insert(0) += 1;
            *on_boundary.entry(k0).or_insert(false) |= edge.start_on_boundary;
            *on_boundary.entry(k1).or_insert(false) |= edge.end_on_boundary;
        }
        let before = edges.len();
        edges.retain(|edge| {
            let k0 = vertex_key(edge.start);
            let k1 = vertex_key(edge.end);
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
    type VertexKey = (i64, i64);
    type AdjEntry = (usize, Point, Point, VertexKey);
    let mut adj: HashMap<VertexKey, Vec<AdjEntry>> = HashMap::new();
    for (idx, &(p0, p1)) in segments.iter().enumerate() {
        let k0 = vertex_key(p0);
        let k1 = vertex_key(p1);
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

    for start_node in nodes {
        let edges_at_start = adj[&start_node].clone();
        for (idx, p0, p1, next_k) in edges_at_start {
            if used[idx] {
                continue;
            }
            used[idx] = true;
            let mut chain = vec![p0, p1];
            let mut cur_k = next_k;
            while let Some((nidx, _, np1, nk)) = adj
                .get(&cur_k)
                .and_then(|list| list.iter().find(|e| !used[e.0]).copied())
            {
                used[nidx] = true;
                chain.push(np1);
                cur_k = nk;
            }
            let mut back = Vec::new();
            let mut cur_k = start_node;
            while let Some((nidx, _, np1, nk)) = adj
                .get(&cur_k)
                .and_then(|list| list.iter().find(|e| !used[e.0]).copied())
            {
                used[nidx] = true;
                back.push(np1);
                cur_k = nk;
            }
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

/// Iteratively prune any rim wall whose stroke encloses a void narrower than
/// `profile.minimum_partial_void_inradius_mm()`, then measure exact copper area:
/// - Interior cells whose 6 walls are all unclipped interior walls (typically
///   ~90% of a panel) contribute exact analytical area `(√3/12) * w * (2p - w)`
///   per half-wall in O(1).
/// - The remaining rim strokes + perimeter boundary web are composed exactly
///   via `stroke_to_fill` and clipped outside the deep cells.
#[allow(clippy::too_many_arguments)]
fn prune_rim_void_slivers_and_measure_area(
    usable: &ContourSet,
    prepared_usable: &PreparedRegion,
    depth_cache: &mut FxHashMap<(i64, i64), f64>,
    boundary_rings: &[Vec<Point>],
    rim_w: f64,
    edges: &mut Vec<ClippedEdge>,
    lattice: DenseCopperLattice,
    profile: DenseCopperBalanceProfile,
    max_sliver_passes: usize,
) -> Result<f64, AccuracyError> {
    let pitch = lattice.pitch_mm;
    let cell_radius = pitch / SQRT_3;
    let resolution = usable.resolution;
    let probe_accuracy = crate::geom::GeometryAccuracy::micrometres(15);
    let min_void_inradius = profile.minimum_partial_void_inradius_mm();

    // Pre-stroke boundary rings once (reused across sliver-pruning passes).
    let web_style = StrokeStyle::round(rim_w);
    let boundary_contours: Vec<ContourBuf> = boundary_rings
        .iter()
        .filter_map(|ring| polyline_contour(ring))
        .collect();
    let boundary_fill = if !boundary_contours.is_empty()
        && let Some(filled) = stroke_to_fill(&boundary_contours, web_style, probe_accuracy)?
    {
        Some(ContourSet::from_filled_contours(&filled, resolution)?)
    } else {
        None
    };

    if prune_rim_grazing_edges(edges, prepared_usable, depth_cache, rim_w) {
        prune_dangling_stubs(edges);
    }

    let deep_cell_margin = rim_w + cell_radius + 0.05;

    for pass in 0..max_sliver_passes {
        // Count how many unclipped interior edges each lattice site has.
        let mut site_edge_widths: FxHashMap<DenseCopperLatticeSite, Vec<f64>> =
            FxHashMap::default();
        for edge in edges.iter() {
            if edge.interior_cell_edge {
                let w = edge.width_um as f64 / 1000.0;
                site_edge_widths.entry(edge.sites.0).or_default().push(w);
                site_edge_widths.entry(edge.sites.1).or_default().push(w);
            }
        }

        let mut deep_cells: FxHashMap<DenseCopperLatticeSite, f64> = FxHashMap::default();
        let mut deep_area_mm2 = 0.0;
        for (site, widths) in &site_edge_widths {
            if widths.len() != 6 {
                continue;
            }
            let center = lattice.center(*site);
            let depth = *depth_cache.entry(vertex_key(center)).or_insert_with(|| {
                prepared_usable
                    .signed_distance(center)
                    .map_or(f64::NEG_INFINITY, |d| -d.mm)
            });
            if depth >= deep_cell_margin {
                let cell_cu: f64 = widths
                    .iter()
                    .map(|&w| (SQRT_3 / 12.0) * w * (2.0 * pitch - w))
                    .sum();
                deep_cells.insert(*site, cell_cu);
                deep_area_mm2 += cell_cu;
            }
        }

        let mut rim_fills = Vec::new();
        if let Some(ref bf) = boundary_fill {
            rim_fills.push(bf.clone());
        }
        let mut rim_edge_indices = Vec::new();
        let mut rim_segs_by_width: BTreeMap<i64, Vec<(Point, Point)>> = BTreeMap::new();
        for (idx, edge) in edges.iter().enumerate() {
            if deep_cells.contains_key(&edge.sites.0) && deep_cells.contains_key(&edge.sites.1) {
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

        // Check every enclosed hole ring of `rim_union` (negative signed area)
        // for void slivers narrower than `2 * min_void_inradius`.
        let mut drop_flags = vec![false; edges.len()];
        let mut dropped_any = false;
        if pass + 1 < max_sliver_passes && !rim_edge_indices.is_empty() {
            let mut rim_grid: FxHashMap<(i32, i32), Vec<usize>> = FxHashMap::default();
            let cell_sz = pitch.max(0.5);
            for &idx in &rim_edge_indices {
                let edge = edges[idx];
                let pad = 0.5 * (edge.width_um as f64 / 1000.0) + 0.04;
                let gx0 = ((edge.start.x.min(edge.end.x) - pad) / cell_sz).floor() as i32;
                let gx1 = ((edge.start.x.max(edge.end.x) + pad) / cell_sz).floor() as i32;
                let gy0 = ((edge.start.y.min(edge.end.y) - pad) / cell_sz).floor() as i32;
                let gy1 = ((edge.start.y.max(edge.end.y) + pad) / cell_sz).floor() as i32;
                for gx in gx0..=gx1 {
                    for gy in gy0..=gy1 {
                        rim_grid.entry((gx, gy)).or_default().push(idx);
                    }
                }
            }
            for ring in rim_union.rings() {
                let sarea = ring_signed_area(ring);
                if sarea >= 0.0 {
                    continue;
                }
                let hole_area = -sarea;
                let hole_bbox = ring_bbox(ring);
                // The giant interior ocean of deep cells (or large board
                // cutouts) is trivially far wider than any void sliver.
                if hole_area > 2.5 * pitch * pitch
                    && hole_bbox.width().min(hole_bbox.height()) > 1.5 * pitch
                {
                    continue;
                }
                let mut ccw: Vec<[f64; 2]> = ring.to_vec();
                ccw.reverse();
                let void_poly = ContourSet::from_rings(vec![ccw], FillRule::NonZero, resolution)?;
                if void_poly.is_empty() {
                    continue;
                }
                if !void_poly.disk_erode(min_void_inradius)?.is_empty() {
                    continue;
                }
                // Only sliver candidates reach here: skip clearance cutouts
                // that lie outside `usable` (scoped to `hole_bbox` for O(1) cost).
                let local_usable = usable.reaching(hole_bbox);
                let outside = void_poly.difference(&local_usable)?;
                if !outside.is_empty() && outside.area() > 0.25 * void_poly.area() {
                    continue;
                }

                // Enclosed void sliver found: drop the single rim edge bounding
                // this sliver that sits closest to the boundary so the sliver
                // merges cleanly into the adjacent void.
                let mut best_edge: Option<(usize, (u8, i64, i64))> = None;
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
                            let w = edge.width_um as f64 / 1000.0;
                            let pad = 0.5 * w + 0.04;
                            let ebbox = BBox::new(
                                Point::new(
                                    edge.start.x.min(edge.end.x),
                                    edge.start.y.min(edge.end.y),
                                ),
                                Point::new(
                                    edge.start.x.max(edge.end.x),
                                    edge.start.y.max(edge.end.y),
                                ),
                            )
                            .expand(pad);
                            if !ebbox.intersects(hole_bbox) {
                                continue;
                            }
                            let touches = ring.iter().any(|&[x, y]| {
                                point_segment_distance(Point::new(x, y), edge.start, edge.end)
                                    <= pad
                            });
                            if !touches {
                                continue;
                            }
                            let tier: u8 = if edge.start_on_boundary && edge.end_on_boundary {
                                0
                            } else if edge.start_on_boundary || edge.end_on_boundary {
                                1
                            } else {
                                2
                            };
                            let mid = Point::new(
                                0.5 * (edge.start.x + edge.end.x),
                                0.5 * (edge.start.y + edge.end.y),
                            );
                            let mid_depth =
                                *depth_cache.entry(vertex_key(mid)).or_insert_with(|| {
                                    prepared_usable
                                        .signed_distance(mid)
                                        .map_or(f64::NEG_INFINITY, |d| -d.mm)
                                });
                            let score = (
                                tier,
                                (mid_depth * 1000.0).round() as i64,
                                (edge.start.distance_to(edge.end) * 1000.0).round() as i64,
                            );
                            if best_edge.is_none_or(|(_, s)| score < s) {
                                best_edge = Some((idx, score));
                            }
                        }
                    }
                }
                if let Some((idx, _)) = best_edge {
                    drop_flags[idx] = true;
                    dropped_any = true;
                }
            }
        }

        if dropped_any {
            let mut idx = 0usize;
            edges.retain(|_| {
                let keep = !drop_flags[idx];
                idx += 1;
                keep
            });
            prune_dangling_stubs(edges);
            if prune_rim_grazing_edges(edges, prepared_usable, depth_cache, rim_w) {
                prune_dangling_stubs(edges);
            }
            continue;
        }

        // Sliver-free: compute exact rim area excluding bordering deep cells.
        let rim_area_mm2 = if deep_cells.is_empty() {
            rim_union.area()
        } else {
            let mut border_deep_polys = Vec::new();
            let hex_offsets = std::array::from_fn::<Point, 6, _>(|i| {
                let angle = i as f64 * std::f64::consts::FRAC_PI_3;
                Point::new(cell_radius * angle.cos(), cell_radius * angle.sin())
            });
            for &site in deep_cells.keys() {
                let parity = site.column.rem_euclid(2);
                let neighbors = [
                    (site.column, site.row + 1),
                    (site.column, site.row - 1),
                    (site.column + 1, site.row + parity),
                    (site.column + 1, site.row + parity - 1),
                    (site.column - 1, site.row + parity),
                    (site.column - 1, site.row + parity - 1),
                ];
                let borders_rim = neighbors.iter().any(|&(column, row)| {
                    !deep_cells.contains_key(&DenseCopperLatticeSite { column, row })
                });
                if borders_rim {
                    let center = lattice.center(site);
                    let ring: Vec<[f64; 2]> = hex_offsets
                        .iter()
                        .map(|&off| [center.x + off.x, center.y + off.y])
                        .collect();
                    border_deep_polys.push(ring);
                }
            }
            if border_deep_polys.is_empty() {
                rim_union.area()
            } else {
                let border_mask =
                    ContourSet::from_rings(border_deep_polys, FillRule::NonZero, resolution)?;
                rim_union.difference(&border_mask)?.area()
            }
        };

        return Ok(deep_area_mm2 + rim_area_mm2);
    }

    unreachable!("loop returns on final pass when dropped_any is false")
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
    let proj = Point::new(a.x + t * ab.x, a.y + t * ab.y);
    p.distance_to(proj)
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
        SpatialCopperBalanceLayerRequest, SpatialCopperBalanceRequest,
        generate_spatial_dense_copper_balance,
    };
    use crate::geom::{GeometryAccuracy, Resolution};

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourSet {
        let res = Resolution::default().with_accuracy(GeometryAccuracy::micrometres(50));
        ContourSet::from_rings(
            vec![vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]],
            FillRule::NonZero,
            res,
        )
        .unwrap()
    }

    fn stroke_web(polylines: &[HoneycombPolyline], res: Resolution) -> ContourSet {
        let mut fills = Vec::new();
        for poly in polylines {
            if let Some(c) = polyline_contour(&poly.points)
                && let Some(filled) =
                    stroke_to_fill(&[c], StrokeStyle::round(poly.width_mm), res.accuracy).unwrap()
            {
                fills.push(ContourSet::from_filled_contours(&filled, res).unwrap());
            }
        }
        ContourSet::union_all(res, fills).unwrap()
    }

    #[test]
    fn web_area_containment_and_slivers_across_densities() {
        let profile = DenseCopperBalanceProfile::V1;
        let empty = ContourSet::empty(Resolution::default().with_accuracy(profile.accuracy));
        for ((w, h), target) in [
            ((20.0, 10.0), 0.35),
            ((20.0, 10.0), 0.50),
            ((20.0, 10.0), 0.75),
            ((24.0, 16.0), 0.90),
            ((24.0, 16.0), 0.94),
        ] {
            let safe = rect(0.0, 0.0, w, h);
            let res = generate_spatial_dense_copper_balance(
                profile,
                SpatialCopperBalanceRequest {
                    panel_region: &safe,
                    lattice_origin: Point::ZERO,
                    layers: &[SpatialCopperBalanceLayerRequest {
                        safe_region: &safe,
                        existing_copper: &empty,
                        density_domain: &safe,
                        target_density: target,
                        stack_weight_mm2: 0.0,
                    }],
                },
            )
            .unwrap()
            .layers
            .pop()
            .unwrap();

            let copper = stroke_web(&res.web, safe.resolution);
            let leakage = copper.difference(&safe).unwrap().area();
            let exact_density = copper.area() / safe.area();
            let measured_density = res.solution.achieved_density;
            let min_void_inradius = profile.minimum_partial_void_inradius_mm();
            let mut enclosed_voids = 0usize;
            let mut void_slivers = 0usize;
            for ring in copper.rings() {
                if ring_signed_area(ring) >= 0.0 {
                    continue;
                }
                let mut ccw = ring.to_vec();
                ccw.reverse();
                let vp =
                    ContourSet::from_rings(vec![ccw], FillRule::NonZero, safe.resolution).unwrap();
                enclosed_voids += 1;
                if vp.disk_erode(min_void_inradius).unwrap().is_empty() {
                    void_slivers += 1;
                    eprintln!("  void_sliver bbox={:?} area={:.6}", vp.bbox, vp.area());
                }
            }
            let copper_slivers = copper
                .connected_components()
                .into_iter()
                .filter(|c| c.disk_erode(0.09).unwrap().is_empty())
                .count();
            eprintln!(
                "{w}x{h} target={target:.3} mode={:?} measured={measured_density:.4} exact={exact_density:.4} diff={:.4} leakage={leakage:.6} enclosed_voids={enclosed_voids} void_slivers={void_slivers} copper_slivers={copper_slivers} polylines={}",
                res.solution.mode,
                (measured_density - exact_density).abs(),
                res.web.len(),
            );
            assert!(leakage <= 1e-3, "leakage={leakage}");
            assert!(
                (measured_density - exact_density).abs() <= 0.005,
                "area mismatch"
            );
            assert_eq!(copper_slivers, 0, "copper slivers found");
            assert_eq!(void_slivers, 0, "void slivers found");
        }
    }

    #[test]
    fn web_has_zero_slivers_around_curved_holes_and_concave_corners() {
        use crate::geom::shapes;

        let profile = DenseCopperBalanceProfile::V1;
        let res = Resolution::default().with_accuracy(profile.accuracy);
        let empty = ContourSet::empty(res);
        // L-shaped safe region with circular pad/via clearance cutouts of varying sizes.
        let base = rect(0.0, 0.0, 24.0, 16.0)
            .difference(&rect(14.0, 8.0, 24.0, 16.0))
            .unwrap();
        let holes = [
            (Point::new(5.2, 4.7), 1.6),
            (Point::new(10.1, 11.3), 2.4),
            (Point::new(18.5, 4.1), 1.1),
            (Point::new(6.8, 12.2), 0.75),
        ];
        let mut safe = base;
        for (center, radius) in holes {
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
            let result = generate_spatial_dense_copper_balance(
                profile,
                SpatialCopperBalanceRequest {
                    panel_region: &safe,
                    lattice_origin: Point::ZERO,
                    layers: &[SpatialCopperBalanceLayerRequest {
                        safe_region: &safe,
                        existing_copper: &empty,
                        density_domain: &safe,
                        target_density: target,
                        stack_weight_mm2: 0.0,
                    }],
                },
            )
            .unwrap()
            .layers
            .pop()
            .unwrap();

            let copper = stroke_web(&result.web, res);
            let leakage = copper.difference(&safe).unwrap().area();
            assert!(leakage <= 1e-3, "target={target} leakage={leakage}");

            let exact_density = copper.area() / safe.area();
            assert!(
                (result.solution.achieved_density - exact_density).abs() <= 0.005,
                "target={target} measured={} exact={exact_density}",
                result.solution.achieved_density
            );
            assert!(
                (result.solution.achieved_density - target).abs() <= 0.01,
                "target={target} achieved={}",
                result.solution.achieved_density
            );

            let min_void_inradius = profile.minimum_partial_void_inradius_mm();
            for ring in copper.rings() {
                if ring_signed_area(ring) >= 0.0 {
                    continue;
                }
                let mut ccw = ring.to_vec();
                ccw.reverse();
                let vp = ContourSet::from_rings(vec![ccw], FillRule::NonZero, res).unwrap();
                let outside = vp.difference(&safe).unwrap();
                if !outside.is_empty() && outside.area() > 0.25 * vp.area() {
                    continue; // clearance cutout
                }
                assert!(
                    !vp.disk_erode(min_void_inradius).unwrap().is_empty(),
                    "target={target} void sliver at {:?} area={}",
                    vp.bbox,
                    vp.area()
                );
            }
            for comp in copper.connected_components() {
                assert!(
                    !comp.disk_erode(0.09).unwrap().is_empty(),
                    "target={target} copper sliver at {:?}",
                    comp.bbox
                );
            }
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
        let empty = ContourSet::empty(res);

        // Sweep top-edge y across one full hex row pitch with a slight slope
        // matching RS0001's 0.0004 rad top boundary so a horizontal hex wall
        // repeatedly lands right at rim_w + wall_w/2 from the top boundary.
        let pitch = profile.pitch_mm;
        let row_step = pitch * SQRT_3 / 2.0;
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

            let result = generate_spatial_dense_copper_balance(
                profile,
                SpatialCopperBalanceRequest {
                    panel_region: &safe,
                    lattice_origin: Point::ZERO,
                    layers: &[SpatialCopperBalanceLayerRequest {
                        safe_region: &safe,
                        existing_copper: &empty,
                        density_domain: &safe,
                        target_density: 0.90,
                        stack_weight_mm2: 0.0,
                    }],
                },
            )
            .unwrap()
            .layers
            .pop()
            .unwrap();

            let copper = stroke_web(&result.web, res);
            assert!(
                copper.difference(&safe).unwrap().area() <= 1e-3,
                "step={step} leaked outside safe region"
            );

            // Eroding by 0.08 mm (160 µm minimum neck) must preserve topology
            // (connected components and hole count): a tangential grazing
            // bridge between a horizontal wall and the perimeter rim would
            // sever under erosion and drop a hole ring.
            let before_comps = copper.connected_components().len();
            let before_rings = copper.rings().count();
            let eroded = copper.disk_erode(0.08).unwrap();
            let after_comps = eroded.connected_components().len();
            let after_rings = eroded.rings().count();
            assert_eq!(
                (before_comps, before_rings),
                (after_comps, after_rings),
                "step={step} top_y={top_y:.4} thin copper pinch changed topology ({before_comps}, {before_rings}) -> ({after_comps}, {after_rings})"
            );
        }
    }
}
