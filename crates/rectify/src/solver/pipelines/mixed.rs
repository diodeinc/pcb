use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::raster::{self, CONTACT_THRESHOLDS_MM_DEFAULT, MaskGrid, PoseRaster};

use super::super::CandidateResult;
use super::super::context::FootprintCtx;
use super::super::scoring;
use super::super::support;

const PIN_OUTSIDE_HOLE_PENALTY_PER_MM2: f64 = 80.0;

/// Port of `_evaluate_pose_holes` from `solver.py`.
///
/// Mixed mode has only one translation strategy: align a candidate pin mask to
/// the footprint's hole mask with FFT (`hole_align`). The search is therefore an
/// argmax over a small set of *pin/body extraction cases*:
///
/// 1. a full pin mask from the 25th-percentile body split, with body contact
///    scored against pads;
/// 2. one or more bottom-Z slabs swept over `CONTACT_THRESHOLDS_MM_DEFAULT`,
///    optionally with the next slab used as body contact.
///
/// Each case proposes a pin grid, a scoring/contact grid, a Z datum, and a slab
/// threshold. We score exactly one `hole_align` translation per case, then keep
/// the highest-scoring candidate for this pose.
pub(super) fn evaluate_pose(
    mesh: &MeshData,
    raster: &PoseRaster,
    pose: EulerPose,
    ctx: &FootprintCtx,
    _resolution_mm: f64,
) -> Option<CandidateResult> {
    let hole_grid = ctx.hole_grid.as_ref()?;
    let mut best: Option<CandidateResult> = None;

    if let Some(case) = full_pin_mask_case(raster) {
        maximize(
            &mut best,
            score_case(mesh, raster, pose, ctx, hole_grid, &case),
        );
    }

    // Threshold sweep over bottom slabs. Preserve the historical early-out:
    // always try the thinnest slab, then stop if either the full-pin case or
    // that first slab already produced a positive score.
    for (thr_idx, &threshold_mm) in CONTACT_THRESHOLDS_MM_DEFAULT.iter().enumerate() {
        if thr_idx >= 1
            && let Some(ref cur) = best
            && cur.score > 0.0
        {
            break;
        }
        let Some(case) = bottom_slab_case(raster, threshold_mm) else {
            continue;
        };
        maximize(
            &mut best,
            score_case(mesh, raster, pose, ctx, hole_grid, &case),
        );
    }

    best
}

struct MixedCase {
    /// The feature grid that is aligned to footprint holes.
    pin_grid: MaskGrid,
    /// Optional body/contact grid scored against footprint pads. When absent,
    /// `pin_grid` doubles as the scoring grid.
    body_contact: Option<MaskGrid>,
    /// Z datum associated with the scoring grid.
    scoring_z: f64,
    /// Contact-slab thickness represented by this case.
    threshold_mm: f64,
    /// Whether pin pixels outside holes should receive the large physical
    /// infeasibility penalty.
    penalize_pin_outside_holes: bool,
}

impl MixedCase {
    fn scoring_grid(&self) -> &MaskGrid {
        self.body_contact.as_ref().unwrap_or(&self.pin_grid)
    }
}

fn full_pin_mask_case(raster: &PoseRaster) -> Option<MixedCase> {
    let (pin_grid, body_level_z) = raster::build_pin_mask(raster)?;
    let max_thr = CONTACT_THRESHOLDS_MM_DEFAULT
        .iter()
        .copied()
        .fold(0.0f64, f64::max);
    let body_contact = raster::build_body_contact_grid(raster, body_level_z, max_thr);
    Some(MixedCase {
        pin_grid,
        body_contact,
        scoring_z: body_level_z,
        threshold_mm: 0.0,
        penalize_pin_outside_holes: true,
    })
}

fn bottom_slab_case(raster: &PoseRaster, threshold_mm: f64) -> Option<MixedCase> {
    let pin_grid = raster::build_contact_grid(raster, threshold_mm)?;
    let secondary = raster::build_secondary_contact_grid(raster, &pin_grid, threshold_mm);
    let (body_contact, scoring_z) = match secondary {
        Some((grid, z)) => (Some(grid), z),
        None => (None, raster.z_min),
    };
    let penalize_pin_outside_holes = body_contact.is_some();
    Some(MixedCase {
        pin_grid,
        body_contact,
        scoring_z,
        threshold_mm,
        // Keep previous scoring semantics: a separated secondary/body slab also
        // enabled the outside-hole penalty because `body_z` was `Some`.
        penalize_pin_outside_holes,
    })
}

fn score_case(
    mesh: &MeshData,
    raster: &PoseRaster,
    pose: EulerPose,
    ctx: &FootprintCtx,
    hole_grid: &MaskGrid,
    case: &MixedCase,
) -> CandidateResult {
    let fft = raster::fft_translation_best(hole_grid, &case.pin_grid);
    let translation = fft.translation;
    let scoring_grid = case.scoring_grid();
    let contact_labels = raster::label_components(&scoring_grid.mask);
    let contact_counts = raster::component_pixel_counts(&contact_labels.0, contact_labels.1);

    let mut candidate = scoring::score_candidate(
        ctx,
        scoring_grid,
        raster.bounds,
        &contact_labels.0,
        contact_labels.1,
        &contact_counts,
        case.scoring_z,
        pose,
        translation,
        fft.mask_overlap,
        case.threshold_mm,
        "hole_align",
    );

    apply_hole_fit_terms(ctx, &case.pin_grid, translation, case, &mut candidate);
    support::apply_drill_masked_support_z(&mut candidate, mesh, raster, ctx);
    candidate
}

fn apply_hole_fit_terms(
    ctx: &FootprintCtx,
    pin_grid: &MaskGrid,
    translation: (f64, f64),
    case: &MixedCase,
    candidate: &mut CandidateResult,
) {
    let Some(hlg) = ctx.hole_label_grid.as_ref() else {
        return;
    };

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

    // Per-hole fill quality: reward holes whose fill ratio is high (pin
    // cross-section matches hole size) and penalize holes that are barely
    // touched (likely spurious overlap).
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

    let unfilled_holes = num_holes.saturating_sub(touched_holes);
    candidate.score += 5.0 * hole_overlap
        + 12.0 * (touched_holes as f64)
        + 80.0 * hole_ratio
        + 20.0 * (well_filled as f64)
        + 60.0 * mean_fill
        - 250.0 * (unfilled_holes as f64);

    if case.penalize_pin_outside_holes {
        let px_area = pin_grid.resolution_mm * pin_grid.resolution_mm;
        let pin_total_mm2 = (pin_grid.pixel_count() as f64) * px_area;
        let pin_outside_mm2 = (pin_total_mm2 - hole_overlap).max(0.0);
        candidate.score -= PIN_OUTSIDE_HOLE_PENALTY_PER_MM2 * pin_outside_mm2;
    }
}

fn maximize(best: &mut Option<CandidateResult>, candidate: CandidateResult) {
    match best.as_ref() {
        None => *best = Some(candidate),
        Some(cur) if candidate.score > cur.score => *best = Some(candidate),
        _ => {}
    }
}
