use crate::footprint::{FootprintData, FootprintKind};
use crate::mesh::MeshData;
use crate::raster::{self, HoleLabelGrid, MaskGrid};

use super::context::{self, FootprintCtx};
use super::support;
use super::{CandidateResult, EPS};

const SPARSE_ANCHOR_MAX_PADS: usize = 10;
const SPARSE_ANCHOR_MAX_CONTACTS: usize = 6;
const SPARSE_ANCHOR_MAX_TRANSLATIONS: usize = 2;
const SPARSE_ANCHOR_COUNT_DELTA: usize = 2;
const SPARSE_ANCHOR_COST_LIMIT: f64 = 1.25;
const SPARSE_ANCHOR_MARGIN_MIN: f64 = 0.08;

#[derive(Debug, Clone)]
pub(crate) struct SparseAnchor {
    #[allow(dead_code)]
    index: usize,
    cx: f64,
    cy: f64,
    #[allow(dead_code)]
    area: f64,
    area_ratio: f64,
    norm_x: f64,
    norm_y: f64,
    rank_x: f64,
    rank_y: f64,
}

fn normalized_ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    if n <= 1 {
        return vec![0.5; n];
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        values[a]
            .partial_cmp(&values[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let denom = (n - 1) as f64;
    let mut ranks = vec![0.5; n];
    for (rank, &idx) in order.iter().enumerate() {
        ranks[idx] = (rank as f64) / denom;
    }
    ranks
}

fn build_sparse_anchors(items: &[(usize, f64, f64, f64)], bounds: [f64; 4]) -> Vec<SparseAnchor> {
    if items.is_empty() {
        return Vec::new();
    }
    let xs: Vec<f64> = items.iter().map(|i| i.1).collect();
    let ys: Vec<f64> = items.iter().map(|i| i.2).collect();
    let areas: Vec<f64> = items.iter().map(|i| i.3.max(EPS)).collect();
    let total_area = areas.iter().sum::<f64>().max(EPS);
    let width = (bounds[2] - bounds[0]).max(EPS);
    let height = (bounds[3] - bounds[1]).max(EPS);
    let rank_x = normalized_ranks(&xs);
    let rank_y = normalized_ranks(&ys);
    items
        .iter()
        .enumerate()
        .map(|(i, &(index, cx, cy, area))| SparseAnchor {
            index,
            cx,
            cy,
            area,
            area_ratio: areas[i] / total_area,
            norm_x: (cx - bounds[0]) / width,
            norm_y: (cy - bounds[1]) / height,
            rank_x: rank_x[i],
            rank_y: rank_y[i],
        })
        .collect()
}

pub(crate) fn build_pad_sparse_anchors(fp: &FootprintData) -> Vec<SparseAnchor> {
    if fp.pads.len() < 2 || fp.pads.len() > SPARSE_ANCHOR_MAX_PADS {
        return Vec::new();
    }
    let polys: Vec<raster::Polygon> = fp.pads.iter().map(raster::pad_to_polygon).collect();
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in &polys {
        min_x = min_x.min(p.bounds[0]);
        min_y = min_y.min(p.bounds[1]);
        max_x = max_x.max(p.bounds[2]);
        max_y = max_y.max(p.bounds[3]);
    }
    let items: Vec<(usize, f64, f64, f64)> = polys
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let area = polygon_area(p).max(EPS);
            let (cx, cy) = polygon_centroid(p);
            (i, cx, cy, area)
        })
        .collect();
    build_sparse_anchors(&items, [min_x, min_y, max_x, max_y])
}

fn polygon_area(p: &raster::Polygon) -> f64 {
    // Signed area over the outer ring. Pads are mostly convex single-ring
    // polygons, so this is a good enough estimate for anchor weighting.
    let mut total = 0.0f64;
    for ring in &p.rings {
        if ring.len() < 3 {
            continue;
        }
        let mut a = 0.0;
        for i in 0..ring.len() {
            let j = (i + 1) % ring.len();
            a += ring[i][0] * ring[j][1] - ring[j][0] * ring[i][1];
        }
        total += a.abs() * 0.5;
    }
    total
}

pub(crate) fn polygon_centroid(p: &raster::Polygon) -> (f64, f64) {
    // Area-weighted centroid across outer rings.
    let mut total_a = 0.0f64;
    let mut cx = 0.0f64;
    let mut cy = 0.0f64;
    for ring in &p.rings {
        if ring.len() < 3 {
            continue;
        }
        let mut a = 0.0f64;
        let mut rx = 0.0f64;
        let mut ry = 0.0f64;
        for i in 0..ring.len() {
            let j = (i + 1) % ring.len();
            let cross = ring[i][0] * ring[j][1] - ring[j][0] * ring[i][1];
            a += cross;
            rx += (ring[i][0] + ring[j][0]) * cross;
            ry += (ring[i][1] + ring[j][1]) * cross;
        }
        let area = a * 0.5;
        if area.abs() > EPS {
            total_a += area;
            cx += rx / 6.0;
            cy += ry / 6.0;
        }
    }
    if total_a.abs() > EPS {
        (cx / total_a, cy / total_a)
    } else {
        let [b0, b1, b2, b3] = p.bounds;
        (0.5 * (b0 + b2), 0.5 * (b1 + b3))
    }
}

fn build_contact_sparse_anchors(contact: &MaskGrid) -> Vec<SparseAnchor> {
    let (labels, count) = raster::label_components(&contact.mask);
    if count < 2 {
        return Vec::new();
    }
    let resolution_mm = contact.resolution_mm;
    let mut acc: Vec<(u32, f64, f64)> = vec![(0, 0.0, 0.0); count as usize];
    for ((r, c), &lbl) in labels.indexed_iter() {
        if lbl == 0 {
            continue;
        }
        let entry = &mut acc[(lbl - 1) as usize];
        entry.0 += 1;
        entry.1 += c as f64 + 0.5;
        entry.2 += r as f64 + 0.5;
    }
    let mut items: Vec<(usize, f64, f64, f64)> = Vec::new();
    for (i, &(n, sx, sy)) in acc.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let cx = contact.bounds[0] + (sx / n as f64) * resolution_mm;
        let cy = contact.bounds[1] + (sy / n as f64) * resolution_mm;
        let area = (n as f64) * resolution_mm * resolution_mm;
        items.push((i + 1, cx, cy, area.max(EPS)));
    }
    if items.len() < 2 {
        return Vec::new();
    }
    items.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    if items.len() > SPARSE_ANCHOR_MAX_CONTACTS {
        items.truncate(SPARSE_ANCHOR_MAX_CONTACTS);
    }
    build_sparse_anchors(&items, contact.bounds)
}

fn sparse_anchor_match_cost(pad: &SparseAnchor, contact: &SparseAnchor) -> f64 {
    let rank_delta = (pad.rank_x - contact.rank_x).abs() + (pad.rank_y - contact.rank_y).abs();
    let coord_delta = (pad.norm_x - contact.norm_x).abs() + (pad.norm_y - contact.norm_y).abs();
    let area_delta = ((pad.area_ratio + EPS) / (contact.area_ratio + EPS))
        .ln()
        .abs();
    1.1 * rank_delta + 0.7 * coord_delta + 0.25 * area_delta.min(3.0)
}

fn second_best_gap(values: &[f64], best_idx: usize) -> f64 {
    if values.len() <= 1 {
        return 1.0;
    }
    let best = values[best_idx];
    let mut second = f64::INFINITY;
    for (i, &v) in values.iter().enumerate() {
        if i == best_idx {
            continue;
        }
        if v < second {
            second = v;
        }
    }
    second - best
}

fn argmin(values: &[f64]) -> usize {
    let mut best = 0usize;
    let mut best_v = f64::INFINITY;
    for (i, &v) in values.iter().enumerate() {
        if v < best_v {
            best_v = v;
            best = i;
        }
    }
    best
}

/// Port of `solve_translation_sparse_pad_contact_anchors`. Returns up to two
/// combinatorially matched translation candidates.
pub(crate) fn solve_translation_sparse_pad_contact_anchors(
    pad_anchors: &[SparseAnchor],
    contact: &MaskGrid,
) -> Vec<(f64, f64)> {
    if pad_anchors.len() < 2 || pad_anchors.len() > SPARSE_ANCHOR_MAX_PADS {
        return Vec::new();
    }
    let contact_anchors = build_contact_sparse_anchors(contact);
    if contact_anchors.len() < 2 {
        return Vec::new();
    }
    if contact_anchors.len() > pad_anchors.len() + SPARSE_ANCHOR_COUNT_DELTA {
        return Vec::new();
    }
    let nc = contact_anchors.len();
    let np = pad_anchors.len();
    let mut costs = vec![0.0f64; nc * np];
    for ci in 0..nc {
        for pi in 0..np {
            costs[ci * np + pi] = sparse_anchor_match_cost(&pad_anchors[pi], &contact_anchors[ci]);
        }
    }
    let best_pad_for_contact: Vec<usize> = (0..nc)
        .map(|ci| argmin(&costs[ci * np..(ci + 1) * np]))
        .collect();
    let mut col: Vec<f64> = vec![0.0; nc];
    let best_contact_for_pad: Vec<usize> = (0..np)
        .map(|pi| {
            for ci in 0..nc {
                col[ci] = costs[ci * np + pi];
            }
            argmin(&col)
        })
        .collect();
    let mut proposals: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
    let resolution_mm = contact.resolution_mm;
    for ci in 0..nc {
        let pi = best_pad_for_contact[ci];
        if best_contact_for_pad[pi] != ci {
            continue;
        }
        let match_cost = costs[ci * np + pi];
        let row: &[f64] = &costs[ci * np..(ci + 1) * np];
        let contact_gap = second_best_gap(row, pi);
        for ci2 in 0..nc {
            col[ci2] = costs[ci2 * np + pi];
        }
        let pad_gap = second_best_gap(&col, ci);
        if match_cost > SPARSE_ANCHOR_COST_LIMIT
            && contact_gap.min(pad_gap) < SPARSE_ANCHOR_MARGIN_MIN
        {
            continue;
        }
        let pad_anchor = &pad_anchors[pi];
        let contact_anchor = &contact_anchors[ci];
        let tx = pad_anchor.cx - contact_anchor.cx;
        let ty = pad_anchor.cy - contact_anchor.cy;
        let key = (
            (tx / resolution_mm).round() as i32,
            (ty / resolution_mm).round() as i32,
        );
        if !seen.insert(key) {
            continue;
        }
        let area_key = -pad_anchor.area_ratio.min(contact_anchor.area_ratio);
        proposals.push((match_cost, area_key, tx, ty));
    }
    proposals.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    });
    proposals
        .into_iter()
        .take(SPARSE_ANCHOR_MAX_TRANSLATIONS)
        .map(|(_, _, tx, ty)| (tx, ty))
        .collect()
}

/// Port of `refine_translation_bbox_contact_pads` from `solver.py`. Aligns the
/// combined bbox of pad-overlapping contact islands with the bbox of the pads
/// they touch; returns `None` when the shift is negligible or too large.
pub(crate) fn refine_translation_bbox_contact_pads(
    ctx: &FootprintCtx,
    contact: &MaskGrid,
    translation: (f64, f64),
) -> Option<(f64, f64)> {
    let (tx, ty) = translation;
    let resolution_mm = contact.resolution_mm;
    let num_pads = ctx.pad_shape_bounds.len();
    if num_pads == 0 {
        return None;
    }
    let (labels, count) = raster::label_components(&contact.mask);
    if count == 0 {
        return None;
    }
    let counts = raster::component_pixel_counts(&labels, count);
    let (label_h, label_w) = ctx.pad_label_grid.dim();
    let (src_h, src_w) = contact.mask.dim();
    let dx_px =
        ((contact.bounds[0] + tx - ctx.pad_label_grid.bounds[0]) / resolution_mm).round() as i32;
    let dy_px =
        ((contact.bounds[1] + ty - ctx.pad_label_grid.bounds[1]) / resolution_mm).round() as i32;
    let lbl_y0 = dy_px.max(0) as usize;
    let lbl_x0 = dx_px.max(0) as usize;
    let lbl_y1 = ((dy_px + src_h as i32).max(0) as usize).min(label_h);
    let lbl_x1 = ((dx_px + src_w as i32).max(0) as usize).min(label_w);

    let cc = count as usize;
    // overlap[island_idx * num_pads + pad_idx] (pad_idx 0..num_pads)
    let mut overlap = vec![0u32; cc * num_pads];
    let mut total_pad_per_island = vec![0u32; cc];
    if lbl_y1 > lbl_y0 && lbl_x1 > lbl_x0 {
        let src_y0 = (lbl_y0 as i32 - dy_px) as usize;
        let src_x0 = (lbl_x0 as i32 - dx_px) as usize;
        for dr in 0..(lbl_y1 - lbl_y0) {
            for dc in 0..(lbl_x1 - lbl_x0) {
                let island = labels[(src_y0 + dr, src_x0 + dc)];
                if island == 0 {
                    continue;
                }
                let pad = ctx.pad_label_grid.labels[(lbl_y0 + dr, lbl_x0 + dc)];
                if pad == 0 {
                    continue;
                }
                overlap[(island - 1) as usize * num_pads + (pad - 1) as usize] += 1;
                total_pad_per_island[(island - 1) as usize] += 1;
            }
        }
    }
    let mut overlapping_ids: Vec<u32> = Vec::new();
    let mut touched_pads: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for island_idx in 0..cc {
        let island_area_px = counts[island_idx];
        if island_area_px == 0 {
            continue;
        }
        let total_pad_overlap = total_pad_per_island[island_idx];
        let overlap_ratio = (total_pad_overlap as f64) / (island_area_px as f64).max(EPS);
        if overlap_ratio < 0.15 {
            continue;
        }
        overlapping_ids.push(island_idx as u32 + 1);
        for pi in 0..num_pads {
            if overlap[island_idx * num_pads + pi] > 0 {
                touched_pads.insert(pi);
            }
        }
    }
    if overlapping_ids.is_empty() || touched_pads.is_empty() {
        return None;
    }
    // Combined bbox of overlapping islands in contact-local coords.
    let mut cmin_r = usize::MAX;
    let mut cmax_r = 0usize;
    let mut cmin_c = usize::MAX;
    let mut cmax_c = 0usize;
    let mut any = false;
    for ((r, c), &lbl) in labels.indexed_iter() {
        if lbl == 0 {
            continue;
        }
        if !overlapping_ids.contains(&lbl) {
            continue;
        }
        any = true;
        if r < cmin_r {
            cmin_r = r;
        }
        if r > cmax_r {
            cmax_r = r;
        }
        if c < cmin_c {
            cmin_c = c;
        }
        if c > cmax_c {
            cmax_c = c;
        }
    }
    if !any {
        return None;
    }
    let contact_min_x = contact.bounds[0] + (cmin_c as f64) * resolution_mm;
    let contact_max_x = contact.bounds[0] + ((cmax_c + 1) as f64) * resolution_mm;
    let contact_min_y = contact.bounds[1] + (cmin_r as f64) * resolution_mm;
    let contact_max_y = contact.bounds[1] + ((cmax_r + 1) as f64) * resolution_mm;
    let contact_cx = 0.5 * (contact_min_x + contact_max_x) + tx;
    let contact_cy = 0.5 * (contact_min_y + contact_max_y) + ty;

    let mut pad_min_x = f64::INFINITY;
    let mut pad_min_y = f64::INFINITY;
    let mut pad_max_x = f64::NEG_INFINITY;
    let mut pad_max_y = f64::NEG_INFINITY;
    for &pi in &touched_pads {
        let b = ctx.pad_shape_bounds[pi];
        if b[0] < pad_min_x {
            pad_min_x = b[0];
        }
        if b[1] < pad_min_y {
            pad_min_y = b[1];
        }
        if b[2] > pad_max_x {
            pad_max_x = b[2];
        }
        if b[3] > pad_max_y {
            pad_max_y = b[3];
        }
    }
    let pad_cx = 0.5 * (pad_min_x + pad_max_x);
    let pad_cy = 0.5 * (pad_min_y + pad_max_y);
    let refined_tx = tx + (pad_cx - contact_cx);
    let refined_ty = ty + (pad_cy - contact_cy);
    let shift = (refined_tx - tx).abs().max((refined_ty - ty).abs());
    if !(0.001..=1.0).contains(&shift) {
        return None;
    }
    Some((refined_tx, refined_ty))
}

/// Sub-pixel translation refinement. For each pad, compute the centroid of
/// contact pixels that land on that pad (in world coordinates) given the
/// current translation, then shift translation so that these per-pad contact
/// centroids align with the geometric pad centroids. Area-weighted by
/// contact-pixel count so heavily covered pads dominate.
///
/// This corrects the quantization floor introduced by rasterizing translations
/// at `RESOLUTION_MM` (0.10 mm). The pre-existing `refine_translation_bbox_*`
/// pass compares a single combined contact bbox against touched pads, which
/// gives a coarse shift; this routine solves the much finer per-pad alignment.
fn refine_translation_per_pad_centroid(
    ctx: &FootprintCtx,
    contact: &MaskGrid,
    translation: (f64, f64),
) -> Option<(f64, f64)> {
    let (tx, ty) = translation;
    let resolution_mm = contact.resolution_mm;
    let num_pads = ctx.pad_centroids.len();
    if num_pads == 0 {
        return None;
    }
    let (label_h, label_w) = ctx.pad_label_grid.dim();
    let (src_h, src_w) = contact.mask.dim();
    let dx_px =
        ((contact.bounds[0] + tx - ctx.pad_label_grid.bounds[0]) / resolution_mm).round() as i32;
    let dy_px =
        ((contact.bounds[1] + ty - ctx.pad_label_grid.bounds[1]) / resolution_mm).round() as i32;
    let lbl_y0 = dy_px.max(0) as usize;
    let lbl_x0 = dx_px.max(0) as usize;
    let lbl_y1 = ((dy_px + src_h as i32).max(0) as usize).min(label_h);
    let lbl_x1 = ((dx_px + src_w as i32).max(0) as usize).min(label_w);
    if lbl_y1 <= lbl_y0 || lbl_x1 <= lbl_x0 {
        return None;
    }
    let src_y0 = (lbl_y0 as i32 - dy_px) as usize;
    let src_x0 = (lbl_x0 as i32 - dx_px) as usize;

    // Per-pad contact-pixel accumulator (sum of pixel-center world coords).
    let mut sum_x = vec![0.0f64; num_pads];
    let mut sum_y = vec![0.0f64; num_pads];
    let mut counts = vec![0u32; num_pads];
    for dr in 0..(lbl_y1 - lbl_y0) {
        for dc in 0..(lbl_x1 - lbl_x0) {
            let sr = src_y0 + dr;
            let sc = src_x0 + dc;
            if !contact.mask[(sr, sc)] {
                continue;
            }
            let pad = ctx.pad_label_grid.labels[(lbl_y0 + dr, lbl_x0 + dc)];
            if pad == 0 {
                continue;
            }
            // Contact pixel world coordinate (pixel center) in the *contact*
            // frame pre-translation; translation shifts the whole thing.
            let contact_x = contact.bounds[0] + (sc as f64 + 0.5) * resolution_mm;
            let contact_y = contact.bounds[1] + (sr as f64 + 0.5) * resolution_mm;
            let idx = (pad - 1) as usize;
            sum_x[idx] += contact_x;
            sum_y[idx] += contact_y;
            counts[idx] += 1;
        }
    }

    // Per-pad equal-weight residual: each covered pad contributes equally
    // to the translation correction, regardless of how many contact pixels
    // it has. This avoids bias from asymmetric coverage (e.g., connectors
    // where one side has much more contact surface than the other).
    let total: u32 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let mut pad_cx = 0.0f64;
    let mut pad_cy = 0.0f64;
    let mut ctc_cx = 0.0f64;
    let mut ctc_cy = 0.0f64;
    let mut covered = 0usize;
    for (i, &n) in counts.iter().enumerate() {
        if n == 0 {
            continue;
        }
        // Skip pads with low coverage: the contact centroid of a barely-
        // touched pad is noisy and biases the translation. Only include
        // pads where the contact covers at least 20% of the pad area.
        let pad_area = ctx.pad_label_grid.areas_px[i].max(1);
        let coverage = (n as f64) / (pad_area as f64);
        if coverage < 0.20 {
            continue;
        }
        let (pcx, pcy) = ctx.pad_centroids[i];
        // Per-pad centroid of contact pixels
        let contact_cx_i = sum_x[i] / (n as f64);
        let contact_cy_i = sum_y[i] / (n as f64);
        pad_cx += pcx;
        pad_cy += pcy;
        ctc_cx += contact_cx_i;
        ctc_cy += contact_cy_i;
        covered += 1;
    }
    if covered == 0 {
        return None;
    }
    let covered_f = covered as f64;
    let pad_mean_x = pad_cx / covered_f;
    let pad_mean_y = pad_cy / covered_f;
    let ctc_mean_x = ctc_cx / covered_f;
    let ctc_mean_y = ctc_cy / covered_f;
    // New translation = pad_mean - contact_mean (contact_mean already
    // includes the raw pre-translation pixel positions).
    let refined_tx = pad_mean_x - ctc_mean_x;
    let refined_ty = pad_mean_y - ctc_mean_y;
    let shift = (refined_tx - tx).abs().max((refined_ty - ty).abs());
    // Reject null steps. The closed-form LSQ step is stable, so we let the
    // caller iterate freely; the caller is responsible for validating the
    // final translation against the pixel-overlap objective.
    if shift < 1e-6 {
        return None;
    }
    Some((refined_tx, refined_ty))
}

/// THT analogue of `refine_translation_per_pad_centroid`: align per-hole
/// centroids instead of per-pad centroids.
fn refine_translation_per_hole_centroid(
    ctx: &FootprintCtx,
    pin_grid: &MaskGrid,
    translation: (f64, f64),
) -> Option<(f64, f64)> {
    let (_, hlg, hole_centroids) = active_hole_alignment_grid(ctx)?;
    if hole_centroids.is_empty() {
        return None;
    }
    let resolution_mm = hlg.resolution_mm;
    let (hh, hw) = hlg.dim();
    let (ph, pw) = pin_grid.mask.dim();
    let (tx, ty) = translation;
    let col_off = ((pin_grid.bounds[0] + tx - hlg.bounds[0]) / resolution_mm).round() as i32;
    let row_off = ((pin_grid.bounds[1] + ty - hlg.bounds[1]) / resolution_mm).round() as i32;
    let r0 = row_off.max(0) as usize;
    let c0 = col_off.max(0) as usize;
    let r1 = ((row_off + ph as i32).max(0) as usize).min(hh);
    let c1 = ((col_off + pw as i32).max(0) as usize).min(hw);
    if r1 <= r0 || c1 <= c0 {
        return None;
    }
    let sr0 = (r0 as i32 - row_off) as usize;
    let sc0 = (c0 as i32 - col_off) as usize;
    let num_holes = hole_centroids.len();
    let mut sum_x = vec![0.0f64; num_holes];
    let mut sum_y = vec![0.0f64; num_holes];
    let mut counts = vec![0u32; num_holes];
    for dr in 0..(r1 - r0) {
        for dc in 0..(c1 - c0) {
            if !pin_grid.mask[(sr0 + dr, sc0 + dc)] {
                continue;
            }
            let h = hlg.labels[(r0 + dr, c0 + dc)];
            if h == 0 {
                continue;
            }
            let pin_x = pin_grid.bounds[0] + (sc0 as f64 + dc as f64 + 0.5) * resolution_mm;
            let pin_y = pin_grid.bounds[1] + (sr0 as f64 + dr as f64 + 0.5) * resolution_mm;
            let idx = (h - 1) as usize;
            sum_x[idx] += pin_x;
            sum_y[idx] += pin_y;
            counts[idx] += 1;
        }
    }
    let total: u32 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let total_f = total as f64;
    let mut hole_cx = 0.0f64;
    let mut hole_cy = 0.0f64;
    let mut pin_cx = 0.0f64;
    let mut pin_cy = 0.0f64;
    for (i, &n) in counts.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let (hcx, hcy) = hole_centroids[i];
        hole_cx += (n as f64) * hcx;
        hole_cy += (n as f64) * hcy;
        pin_cx += sum_x[i];
        pin_cy += sum_y[i];
    }
    let refined_tx = (hole_cx - pin_cx) / total_f;
    let refined_ty = (hole_cy - pin_cy) / total_f;
    let shift = (refined_tx - tx).abs().max((refined_ty - ty).abs());
    if shift < 1e-6 {
        return None;
    }
    Some((refined_tx, refined_ty))
}

type HoleAlignmentGrid<'a> = (&'a MaskGrid, &'a HoleLabelGrid, &'a [(f64, f64)]);

pub(crate) fn active_hole_alignment_grid(ctx: &FootprintCtx) -> Option<HoleAlignmentGrid<'_>> {
    if ctx.footprint_kind == FootprintKind::ThtOnly
        && let (Some(grid), Some(labels)) = (
            ctx.connected_hole_grid.as_ref(),
            ctx.connected_hole_label_grid.as_ref(),
        )
    {
        return Some((grid, labels, &ctx.connected_hole_centroids));
    }
    Some((
        ctx.hole_grid.as_ref()?,
        ctx.hole_label_grid.as_ref()?,
        &ctx.hole_centroids,
    ))
}
/// Apply a sub-pixel centroid-alignment step to the top-ranked candidate.
/// Rebuilds the winning pose's contact grid and shifts `translation` onto the
/// continuous optimum so the reported offset is no longer quantized to the
/// rasterization step.
pub(crate) fn refine_best_translation(
    fp: &FootprintData,
    mesh: &MeshData,
    ranked: &mut [CandidateResult],
    resolution_mm: f64,
) {
    let Some(best) = ranked.first_mut() else {
        return;
    };
    let Some(ctx) = context::build_context(fp, resolution_mm) else {
        return;
    };
    let Some(raster) = raster::rasterize_mesh_bottom(mesh, best.pose, resolution_mm) else {
        return;
    };
    let translation = (best.translation[0], best.translation[1]);

    // Iterate the closed-form centroid refinement. Each pass rebuilds the
    // contact↔pad mapping from the current translation; a few passes converge
    // to the continuous optimum. We keep iterating freely (no per-step cap)
    // because the closed-form LSQ update is stable by construction.
    //
    // Guard: evaluate a pixel-quantised overlap score at the final converged
    // translation and accept only if it did not regress versus the coarse
    // starting candidate. Intermediate iterations are allowed to cross pixel
    // boundaries freely; we check the pixel-overlap objective only at the
    // endpoint so symmetric large pads (where shifting within the pad keeps
    // pixel overlap flat) can still refine to their true continuous optimum.
    const MAX_ITERATIONS: usize = 16;
    const CONVERGE_MM: f64 = 1e-5;
    // Penalty on contact pixels that leave the pad grid. Same weight as
    // `score_candidate` (2.8× outside - overlap) so the guard tracks the
    // optimisation target.
    const OUTSIDE_PENALTY: f64 = 2.8;
    if ctx.has_holes && ctx.hole_grid.is_some() {
        let pin_grid = if best.threshold_mm == 0.0 {
            raster::build_pin_mask(&raster).map(|(g, _)| g)
        } else {
            raster::build_contact_grid(&raster, best.threshold_mm)
        };
        let Some(pin_grid) = pin_grid else { return };
        let Some((hole_grid, _, _)) = active_hole_alignment_grid(&ctx) else {
            return;
        };
        let baseline_score = overlap_score(hole_grid, &pin_grid, translation, OUTSIDE_PENALTY);
        let mut cur = translation;
        for _ in 0..MAX_ITERATIONS {
            let Some(next) = refine_translation_per_hole_centroid(&ctx, &pin_grid, cur) else {
                break;
            };
            let shift = (next.0 - cur.0).abs().max((next.1 - cur.1).abs());
            cur = next;
            if shift < CONVERGE_MM {
                break;
            }
        }
        let cur_score = overlap_score(hole_grid, &pin_grid, cur, OUTSIDE_PENALTY);
        if cur_score + 1e-9 >= baseline_score {
            best.translation = [cur.0, cur.1];
        }
    } else {
        let Some(contact) = raster::build_contact_grid(&raster, best.threshold_mm) else {
            return;
        };
        let baseline_score = overlap_score(&ctx.pad_grid, &contact, translation, OUTSIDE_PENALTY);
        let mut cur = translation;
        for _ in 0..MAX_ITERATIONS {
            let Some(next) = refine_translation_per_pad_centroid(&ctx, &contact, cur) else {
                break;
            };
            let shift = (next.0 - cur.0).abs().max((next.1 - cur.1).abs());
            cur = next;
            if shift < CONVERGE_MM {
                break;
            }
        }
        let cur_score = overlap_score(&ctx.pad_grid, &contact, cur, OUTSIDE_PENALTY);
        if cur_score + 1e-9 >= baseline_score {
            best.translation = [cur.0, cur.1];
        }
    }
    support::apply_drill_masked_support_z(best, mesh, &raster, &ctx);
}

/// Compute `overlap - penalty * outside` in pixel-area units. The pad/contact
/// translation is quantised to the raster step via `translation_to_shift`, so
/// this function evaluates the *pixel-quantised* overlap for a continuous
/// translation — good enough for the guard since translations that differ by
/// less than half a pixel produce the same shift.
fn overlap_score(
    pad_grid: &MaskGrid,
    contact: &MaskGrid,
    translation: (f64, f64),
    outside_penalty: f64,
) -> f64 {
    let shift = raster::translation_to_shift(pad_grid, contact, translation);
    let (overlap_px, contact_px) = raster::overlay_counts(pad_grid, contact, shift);
    let outside = contact_px.saturating_sub(overlap_px);
    (overlap_px as f64) - outside_penalty * (outside as f64)
}
