use ndarray::Array2;

use crate::pose::EulerPose;
use crate::raster::{self, MaskGrid, PadLabelGrid};

use super::context::FootprintCtx;
use super::{CandidateResult, EPS};

fn projection_alignment_score(
    ctx: &FootprintCtx,
    projection_bounds: [f64; 4],
    translation: (f64, f64),
) -> f64 {
    let align_w = (ctx.alignment_bounds[2] - ctx.alignment_bounds[0]).max(EPS);
    let align_h = (ctx.alignment_bounds[3] - ctx.alignment_bounds[1]).max(EPS);
    let pad_w = (ctx.pad_bounds[2] - ctx.pad_bounds[0]).max(EPS);
    let pad_h = (ctx.pad_bounds[3] - ctx.pad_bounds[1]).max(EPS);
    let align_diff = ((align_w / pad_w).ln().abs() + (align_h / pad_h).ln().abs()).min(4.0);
    if align_diff < 0.25 {
        return 0.0;
    }

    let shifted = [
        projection_bounds[0] + translation.0,
        projection_bounds[1] + translation.1,
        projection_bounds[2] + translation.0,
        projection_bounds[3] + translation.1,
    ];
    let proj_w = (shifted[2] - shifted[0]).max(EPS);
    let proj_h = (shifted[3] - shifted[1]).max(EPS);
    let align_cx = 0.5 * (ctx.alignment_bounds[0] + ctx.alignment_bounds[2]);
    let align_cy = 0.5 * (ctx.alignment_bounds[1] + ctx.alignment_bounds[3]);
    let proj_cx = 0.5 * (shifted[0] + shifted[2]);
    let proj_cy = 0.5 * (shifted[1] + shifted[3]);
    let center_dist = ((align_cx - proj_cx).powi(2) + (align_cy - proj_cy).powi(2)).sqrt();
    let size_err = ((proj_w / align_w).ln().abs() + (proj_h / align_h).ln().abs()).min(4.0);
    let outside = (ctx.alignment_bounds[0] - shifted[0]).max(0.0)
        + (ctx.alignment_bounds[1] - shifted[1]).max(0.0)
        + (shifted[2] - ctx.alignment_bounds[2]).max(0.0)
        + (shifted[3] - ctx.alignment_bounds[3]).max(0.0);

    if ctx.has_holes {
        6.0 * (-center_dist / 2.0).exp() - 2.0 * size_err - 0.5 * outside
    } else {
        18.0 * (-center_dist / 2.0).exp() - 4.0 * size_err - 1.0 * outside
    }
}

pub(crate) fn tht_body_projection_score(
    ctx: &FootprintCtx,
    projection_bounds: [f64; 4],
    translation: (f64, f64),
) -> f64 {
    let shifted = [
        projection_bounds[0] + translation.0,
        projection_bounds[1] + translation.1,
        projection_bounds[2] + translation.0,
        projection_bounds[3] + translation.1,
    ];
    let target = ctx.alignment_bounds;
    let body_w = (shifted[2] - shifted[0]).max(EPS);
    let body_h = (shifted[3] - shifted[1]).max(EPS);
    let target_w = (target[2] - target[0]).max(EPS);
    let target_h = (target[3] - target[1]).max(EPS);
    let body_area = body_w * body_h;
    let target_area = target_w * target_h;
    let ix0 = shifted[0].max(target[0]);
    let iy0 = shifted[1].max(target[1]);
    let ix1 = shifted[2].min(target[2]);
    let iy1 = shifted[3].min(target[3]);
    let intersection = (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0);
    let containment = intersection / body_area.max(EPS);
    let target_coverage = intersection / target_area.max(EPS);
    let outside_area = (body_area - intersection).max(0.0);
    let center_dist = {
        let body_cx = 0.5 * (shifted[0] + shifted[2]);
        let body_cy = 0.5 * (shifted[1] + shifted[3]);
        let target_cx = 0.5 * (target[0] + target[2]);
        let target_cy = 0.5 * (target[1] + target[3]);
        ((body_cx - target_cx).powi(2) + (body_cy - target_cy).powi(2)).sqrt()
    };
    let size_err = ((body_w - target_w).abs() / target_w) + ((body_h - target_h).abs() / target_h);

    300.0 * containment + 120.0 * target_coverage
        - 18.0 * outside_area
        - 18.0 * center_dist
        - 90.0 * size_err
}

/// Full port of `score_candidate` (SMD path) from the Python solver. Keeps the
/// same load-bearing terms so rankings agree pixel-for-pixel at
/// `RESOLUTION_MM = 0.10`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn score_candidate(
    ctx: &FootprintCtx,
    contact: &MaskGrid,
    projection_bounds: [f64; 4],
    contact_labels: &Array2<u32>,
    contact_label_count: u32,
    contact_counts: &[u32],
    z_min: f64,
    pose: EulerPose,
    translation: (f64, f64),
    mask_overlap: f64,
    threshold_mm: f64,
    translation_source: &str,
) -> CandidateResult {
    let px_area = ctx.pad_grid.resolution_mm * ctx.pad_grid.resolution_mm;
    let shift = raster::translation_to_shift(&ctx.pad_grid, contact, translation);
    let (overlap_px, contact_px) = raster::overlay_counts(&ctx.pad_grid, contact, shift);
    let overlap_area = (overlap_px as f64) * px_area;
    let contact_area = (contact_px as f64) * px_area;
    let outside_area = (contact_area - overlap_area).max(0.0);
    let pad_px = ctx.pad_pixel_count;
    let union_px = pad_px + contact_px - overlap_px;
    let iou = if union_px > 0 {
        (overlap_px as f64) / (union_px as f64)
    } else {
        0.0
    };
    let z_penalty_weight = if ctx.has_holes { 0.3 } else { 0.8 };
    let outside_weight = if ctx.has_holes { 0.8 } else { 2.8 };
    // The linear z penalty is asymmetric for SMD: penalize z_min > 0
    // (model bottom above board = floating) more than z_min < 0 (model
    // extends below board = typical for correct placement where z_offset
    // compensates). This breaks near-ties between upright and upside-down
    // for flat components.
    let z_linear = if ctx.has_holes {
        0.0
    } else if z_min > 0.0 {
        1.0 * z_min // floating above board
    } else {
        1.5 * z_min.abs() // extending below board
    };
    let mut score = overlap_area - outside_weight * outside_area + 0.8 * iou + 0.03 * mask_overlap
        - z_penalty_weight * z_min * z_min
        - z_linear;

    // Per-pad coverage, island reward, bridge penalty.
    let (
        covered_pad_count,
        covered_pad_area,
        missing_pad_area,
        min_cov_ratio,
        low_coverage_pad_count,
        coverage_ratios,
    ) = per_pad_coverage(&ctx.pad_label_grid, contact, translation);
    let missing_pad_count = ctx.pad_count.saturating_sub(covered_pad_count);
    // When scoring THT poses, the body-contact grid may cover only a
    // fraction of the SMD pads (the body stands upright). Halve the
    // per-pad penalty so hole-alignment signals can compete.
    let missing_pad_weight = if ctx.has_holes { 10.0 } else { 150.0 };
    score += 0.20 * covered_pad_area;
    score -= 4.5 * missing_pad_area;
    score -= missing_pad_weight * (missing_pad_count as f64);
    score -= 2.0 * (low_coverage_pad_count as f64);
    score -= 4.0 * (0.85 - min_cov_ratio).max(0.0);
    // Uniformity reward.
    if coverage_ratios.len() >= 2 && covered_pad_count >= 2 {
        let nonzero: Vec<f64> = coverage_ratios
            .iter()
            .copied()
            .filter(|&r| r > 0.01)
            .collect();
        if nonzero.len() >= 2 {
            let mean = nonzero.iter().sum::<f64>() / (nonzero.len() as f64);
            if mean > 0.1 {
                let var = nonzero.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>()
                    / (nonzero.len() as f64);
                let std = var.sqrt();
                let uniformity = 1.0 - (std / mean).min(1.0);
                score += 2.5 * uniformity;
            }
        }
    }

    // Island containment reward + bridging penalty.
    let (island_reward, bridge_penalty) = island_scoring(
        &ctx.pad_label_grid,
        contact,
        contact_labels,
        contact_label_count,
        contact_counts,
        translation,
    );
    score += island_reward;
    score -= bridge_penalty;

    // Bonus when contact components line up 1:1 with pads.
    if contact_label_count as usize == ctx.pad_count {
        score += 15.0 + 1.0 * (ctx.pad_count as f64);
    }

    // Centroid-alignment bonus: reward translations that place the contact
    // center close to the pad center. This breaks ties between similar-scoring
    // positions in favor of more geometrically centered placements. The bonus
    // is small (max ~3 points) to avoid overriding overlap-based signals.
    if !ctx.has_holes {
        let pad_cx = 0.5 * (ctx.pad_bounds[0] + ctx.pad_bounds[2]);
        let pad_cy = 0.5 * (ctx.pad_bounds[1] + ctx.pad_bounds[3]);
        let contact_cx = 0.5 * (contact.bounds[0] + contact.bounds[2]) + translation.0;
        let contact_cy = 0.5 * (contact.bounds[1] + contact.bounds[3]) + translation.1;
        let pad_dist = ((pad_cx - contact_cx).powi(2) + (pad_cy - contact_cy).powi(2)).sqrt();
        score += 5.0 * (-pad_dist / 1.5).exp();
        // Also reward alignment with courtyard/fab bounds when they differ.
        let align_cx = 0.5 * (ctx.alignment_bounds[0] + ctx.alignment_bounds[2]);
        let align_cy = 0.5 * (ctx.alignment_bounds[1] + ctx.alignment_bounds[3]);
        let align_dist = ((align_cx - contact_cx).powi(2) + (align_cy - contact_cy).powi(2)).sqrt();
        score += 2.0 * (-align_dist / 2.0).exp();
    }

    score += projection_alignment_score(ctx, projection_bounds, translation);

    CandidateResult {
        pose,
        translation: [translation.0, translation.1],
        z_offset: -z_min,
        score,
        threshold_mm,
        translation_source: translation_source.into(),
    }
}

/// Per-pad coverage via the rasterized pad-label grid. Returns:
/// `(covered_pad_count, covered_area_mm, missing_area_mm, min_coverage_ratio,
///   low_coverage_count, per_pad_coverage_ratios)`.
fn per_pad_coverage(
    pad_label_grid: &PadLabelGrid,
    contact: &MaskGrid,
    translation: (f64, f64),
) -> (usize, f64, f64, f64, usize, Vec<f64>) {
    let px_area = pad_label_grid.resolution_mm * pad_label_grid.resolution_mm;
    let (label_h, label_w) = pad_label_grid.dim();
    let (src_h, src_w) = contact.mask.dim();
    // Contact pixel `(r, c)` maps to pad-label pixel:
    //   lx = c + dx_px, ly = r + dy_px,
    // with (dx_px, dy_px) = round((contact.bounds[0..1] + tx - pad_label_bounds[0..1]) / res).
    let dx_px = ((contact.bounds[0] + translation.0 - pad_label_grid.bounds[0])
        / pad_label_grid.resolution_mm)
        .round() as i32;
    let dy_px = ((contact.bounds[1] + translation.1 - pad_label_grid.bounds[1])
        / pad_label_grid.resolution_mm)
        .round() as i32;
    let lbl_y0 = dy_px.max(0) as usize;
    let lbl_x0 = dx_px.max(0) as usize;
    let lbl_y1 = ((dy_px + src_h as i32).max(0) as usize).min(label_h);
    let lbl_x1 = ((dx_px + src_w as i32).max(0) as usize).min(label_w);

    let num_pads = pad_label_grid.areas_px.len();
    let mut covered_px: Vec<u32> = vec![0; num_pads];
    if lbl_y1 > lbl_y0 && lbl_x1 > lbl_x0 {
        let src_y0 = (lbl_y0 as i32 - dy_px) as usize;
        let src_x0 = (lbl_x0 as i32 - dx_px) as usize;
        for dr in 0..(lbl_y1 - lbl_y0) {
            for dc in 0..(lbl_x1 - lbl_x0) {
                if !contact.mask[(src_y0 + dr, src_x0 + dc)] {
                    continue;
                }
                let pad = pad_label_grid.labels[(lbl_y0 + dr, lbl_x0 + dc)];
                if pad == 0 {
                    continue;
                }
                covered_px[(pad - 1) as usize] += 1;
            }
        }
    }

    let mut covered_area = 0.0;
    let mut missing_area = 0.0;
    let mut min_cov_ratio = 1.0f64;
    let mut low_cov = 0usize;
    let mut covered_pad_count = 0usize;
    let mut coverage_ratios: Vec<f64> = Vec::with_capacity(num_pads);
    for (i, &cov) in covered_px.iter().enumerate() {
        let pad_area_px = pad_label_grid.areas_px[i].max(1);
        let ratio = (cov as f64) / (pad_area_px as f64);
        coverage_ratios.push(ratio);
        let cov_mm = (cov as f64) * px_area;
        let pad_area_mm = (pad_label_grid.areas_px[i] as f64) * px_area;
        covered_area += cov_mm;
        missing_area += (pad_area_mm - cov_mm).max(0.0);
        if cov > 0 {
            covered_pad_count += 1;
        }
        if ratio < min_cov_ratio {
            min_cov_ratio = ratio;
        }
        if ratio < 0.60 {
            low_cov += 1;
        }
    }
    (
        covered_pad_count,
        covered_area,
        missing_area,
        min_cov_ratio,
        low_cov,
        coverage_ratios,
    )
}

/// Port of `score_contact_islands_inside_pads`: classify each contact island
/// by its best-overlapping pad and reward contained islands, penalize islands
/// that bridge multiple pads.
fn island_scoring(
    pad_label_grid: &PadLabelGrid,
    contact: &MaskGrid,
    contact_labels: &Array2<u32>,
    contact_label_count: u32,
    contact_counts: &[u32],
    translation: (f64, f64),
) -> (f64, f64) {
    if contact_label_count == 0 {
        return (0.0, 0.0);
    }
    let resolution_mm = pad_label_grid.resolution_mm;
    let px_area = resolution_mm * resolution_mm;
    let num_pads = pad_label_grid.areas_px.len();
    let (label_h, label_w) = pad_label_grid.dim();
    let (src_h, src_w) = contact.mask.dim();
    let dx_px = ((contact.bounds[0] + translation.0 - pad_label_grid.bounds[0]) / resolution_mm)
        .round() as i32;
    let dy_px = ((contact.bounds[1] + translation.1 - pad_label_grid.bounds[1]) / resolution_mm)
        .round() as i32;
    let lbl_y0 = dy_px.max(0) as usize;
    let lbl_x0 = dx_px.max(0) as usize;
    let lbl_y1 = ((dy_px + src_h as i32).max(0) as usize).min(label_h);
    let lbl_x1 = ((dx_px + src_w as i32).max(0) as usize).min(label_w);

    let count = contact_label_count as usize;
    // overlap_matrix[island_idx * num_pads + pad_idx]
    let mut overlap = vec![0u32; count * num_pads];
    if lbl_y1 > lbl_y0 && lbl_x1 > lbl_x0 {
        let src_y0 = (lbl_y0 as i32 - dy_px) as usize;
        let src_x0 = (lbl_x0 as i32 - dx_px) as usize;
        for dr in 0..(lbl_y1 - lbl_y0) {
            for dc in 0..(lbl_x1 - lbl_x0) {
                let island = contact_labels[(src_y0 + dr, src_x0 + dc)];
                if island == 0 {
                    continue;
                }
                let pad = pad_label_grid.labels[(lbl_y0 + dr, lbl_x0 + dc)];
                if pad == 0 {
                    continue;
                }
                overlap[(island - 1) as usize * num_pads + (pad - 1) as usize] += 1;
            }
        }
    }
    let mut reward = 0.0;
    let mut bridge_penalty = 0.0;
    for island_idx in 0..count {
        let island_px = contact_counts[island_idx];
        if island_px == 0 {
            continue;
        }
        let island_area = (island_px as f64) * px_area;
        let row = &overlap[island_idx * num_pads..(island_idx + 1) * num_pads];
        let mut best_overlap_px = 0u32;
        let mut touched = 0usize;
        for &v in row {
            if v > best_overlap_px {
                best_overlap_px = v;
            }
            if v > 0 {
                touched += 1;
            }
        }
        let best_overlap = (best_overlap_px as f64) * px_area;
        let containment_ratio = best_overlap / island_area;
        if containment_ratio >= 0.98 {
            reward += 2.5 + 0.15 * island_area;
        } else if containment_ratio >= 0.85 {
            reward += 0.6 * containment_ratio;
        }
        if touched > 1 {
            bridge_penalty += 4.0 * ((touched - 1) as f64) + 0.3 * island_area;
        }
    }
    (reward, bridge_penalty)
}
