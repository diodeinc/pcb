use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::raster::{self, CONTACT_THRESHOLDS_MM_DEFAULT, MaskGrid, PoseRaster};

use super::super::CandidateResult;
use super::super::context::FootprintCtx;
use super::super::scoring;
use super::super::support;

/// Port of `_evaluate_pose_holes` from `solver.py`. Try the full pin mask
/// (features below body level) first, then per-threshold bottom slabs, scoring
/// body contact against pads and rewarding pin features that land inside
/// holes.
pub(super) fn evaluate_pose(
    mesh: &MeshData,
    raster: &PoseRaster,
    pose: EulerPose,
    ctx: &FootprintCtx,
    _resolution_mm: f64,
) -> Option<CandidateResult> {
    let hole_grid = ctx.hole_grid.as_ref()?;
    let mut best: Option<CandidateResult> = None;

    let score_pin_grid = |pin_grid: &MaskGrid,
                          body_contact: Option<&MaskGrid>,
                          body_z: Option<f64>,
                          threshold_mm: f64|
     -> CandidateResult {
        let fft = raster::fft_translation_best(hole_grid, pin_grid);
        let translation = fft.translation;
        let mask_overlap = fft.mask_overlap;
        let scoring_grid = body_contact.unwrap_or(pin_grid);
        let scoring_z = body_z.unwrap_or(raster.z_min);
        let contact_labels = raster::label_components(&scoring_grid.mask);
        let contact_counts = raster::component_pixel_counts(&contact_labels.0, contact_labels.1);
        let mut candidate = scoring::score_candidate(
            ctx,
            scoring_grid,
            raster.bounds,
            &contact_labels.0,
            contact_labels.1,
            &contact_counts,
            scoring_z,
            pose,
            translation,
            mask_overlap,
            threshold_mm,
            "hole_align",
        );
        if let Some(hlg) = ctx.hole_label_grid.as_ref() {
            let detail = raster::raster_hole_reward_detail(pin_grid, translation, hlg);
            let hole_overlap = detail.overlap_area;
            let touched_holes = detail.touched_holes;
            let num_holes = hlg.num_holes;

            // Scale the per-hole reward with the total number of holes.
            let hole_ratio = if num_holes > 0 {
                (touched_holes as f64) / (num_holes as f64)
            } else {
                0.0
            };

            // Per-hole fill quality: reward holes whose fill ratio is
            // high (pin cross-section matches hole size) and penalize
            // holes that are barely touched (likely spurious overlap).
            let mut well_filled = 0usize;
            let mut total_fill = 0.0f64;
            for &fill in &detail.per_hole_fill {
                total_fill += fill;
                if fill > 0.3 {
                    well_filled += 1;
                }
            }
            let mean_fill = if num_holes > 0 {
                total_fill / (num_holes as f64)
            } else {
                0.0
            };

            // Unfilled-hole penalty.
            let unfilled_holes = num_holes.saturating_sub(touched_holes);
            candidate.score += 5.0 * hole_overlap
                + 12.0 * (touched_holes as f64)
                + 80.0 * hole_ratio
                + 20.0 * (well_filled as f64)
                + 60.0 * mean_fill
                - 250.0 * (unfilled_holes as f64);

            // Pin-outside-hole penalty. When we enter this branch via the
            // full-pin-mask path (`body_z.is_some()`), `pin_grid` is the
            // set of mesh pixels below the 25th-percentile body plane —
            // i.e. where real pins live. Any of those pixels that fail
            // to land inside a drilled hole represents pin metal driving
            // through solid PCB, which is physically infeasible. A large
            // per-mm² penalty breaks rotation ties between upside-down
            // poses (pin_grid catches spurious mesh artifacts off-hole)
            // and the unique pose that actually threads pins through the
            // drill pattern.
            //
            // We skip this on the threshold-slab branch (`body_z is None`)
            // because there `pin_grid` is a thin slab at `raster.z_min`,
            // which on an upside-down THT part is body, not pins, and
            // penalising it would incorrectly hit mixed SMD+THT parts.
            if body_z.is_some() {
                let px_area = pin_grid.resolution_mm * pin_grid.resolution_mm;
                let pin_total_mm2 = (pin_grid.pixel_count() as f64) * px_area;
                let pin_outside_mm2 = (pin_total_mm2 - hole_overlap).max(0.0);
                candidate.score -= 80.0 * pin_outside_mm2;
            }
        }
        support::apply_drill_masked_support_z(&mut candidate, mesh, raster, ctx);
        candidate
    };

    // Full pin mask (25th-percentile body split).
    if let Some((full_pin_grid, body_level_z)) = raster::build_pin_mask(raster) {
        let max_thr = CONTACT_THRESHOLDS_MM_DEFAULT
            .iter()
            .copied()
            .fold(0.0f64, f64::max);
        let body_contact = raster::build_body_contact_grid(raster, body_level_z, max_thr);
        let candidate = score_pin_grid(
            &full_pin_grid,
            body_contact.as_ref(),
            Some(body_level_z),
            0.0,
        );
        match best.as_ref() {
            None => best = Some(candidate),
            Some(cur) if candidate.score > cur.score => best = Some(candidate),
            _ => {}
        }
    }

    // Per-threshold bottom slabs.
    for (thr_idx, &threshold_mm) in CONTACT_THRESHOLDS_MM_DEFAULT.iter().enumerate() {
        if thr_idx >= 1
            && let Some(ref cur) = best
            && cur.score > 0.0
        {
            break;
        }
        let Some(pin_grid) = raster::build_contact_grid(raster, threshold_mm) else {
            continue;
        };
        let secondary = raster::build_secondary_contact_grid(raster, &pin_grid, threshold_mm);
        let (body_contact, body_z) = match &secondary {
            Some((g, z)) => (Some(g), Some(*z)),
            None => (None, None),
        };
        let candidate = score_pin_grid(&pin_grid, body_contact, body_z, threshold_mm);
        match best.as_ref() {
            None => best = Some(candidate),
            Some(cur) if candidate.score > cur.score => best = Some(candidate),
            _ => {}
        }
    }
    best
}
