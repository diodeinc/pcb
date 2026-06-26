use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::raster::{self, HoleLabelGrid, PoseRaster};

use super::super::context::FootprintCtx;
use super::super::scoring;
use super::super::support::{self, ContactPoint};
use super::super::{CandidateResult, EPS};

/// THT-only pipeline: first identify the likely pin-facing side by taking a
/// cross-section just above the first mesh contact and checking that it has a
/// similar island count to the connected drill-hole pattern. Then align those
/// pin islands to the holes with FFT and score direct pin-through-hole fit.
pub(super) fn evaluate_pose(
    mesh: &MeshData,
    raster: &PoseRaster,
    pose: EulerPose,
    ctx: &FootprintCtx,
) -> Option<CandidateResult> {
    const PIN_CROSS_SECTIONS_MM: &[f64] = &[0.40, 0.70, 1.00, 1.50, 2.00];

    let hole_grid = ctx.connected_hole_grid.as_ref()?;
    let hole_label_grid = ctx.connected_hole_label_grid.as_ref()?;
    let expected_holes = hole_label_grid.num_holes;
    if expected_holes == 0 {
        return None;
    }
    let mut best: Option<CandidateResult> = None;

    for &cross_section_mm in PIN_CROSS_SECTIONS_MM {
        let Some(pin_grid) = raster::build_contact_grid(raster, cross_section_mm) else {
            continue;
        };
        let (_, pin_islands) = raster::label_components(&pin_grid.mask);

        let mut proposals = raster::fft_translation_candidates(hole_grid, &pin_grid);
        if proposals.is_empty() {
            proposals.push(raster::fft_translation_best(hole_grid, &pin_grid));
        }

        for fft in proposals {
            let detail =
                raster::raster_hole_reward_detail(&pin_grid, fft.translation, hole_label_grid);
            let pin_area = (pin_grid.pixel_count() as f64) * pin_grid.resolution_mm.powi(2);
            let outside_area = (pin_area - detail.overlap_area).max(0.0);
            let connected_outside_ratio = outside_area / pin_area.max(EPS);
            let mechanical_face_score =
                tht_mechanical_alignment_face_score(mesh, raster, pose, fft.translation, ctx)
                    .unwrap_or(0.0);
            let touched = detail.touched_holes;
            let unfilled = expected_holes.saturating_sub(touched);
            let hole_ratio = (touched as f64) / (expected_holes as f64);
            let mean_fill =
                detail.per_hole_fill.iter().sum::<f64>() / (expected_holes as f64).max(1.0);
            let count_delta = (pin_islands as i32 - expected_holes as i32).abs() as f64;
            let island_ratio = 1.0 / (1.0 + count_delta);

            let projection_score =
                scoring::tht_body_projection_score(ctx, raster.bounds, fft.translation);
            let base_score = 500.0 * hole_ratio
                + 180.0 * mean_fill
                + 160.0 * island_ratio
                + 4.0 * fft.mask_overlap
                + projection_score
                + mechanical_face_score
                - 260.0 * (unfilled as f64)
                - 320.0 * connected_outside_ratio
                - 900.0 * (connected_outside_ratio - 0.35).max(0.0)
                - 110.0 * count_delta;

            let candidate = CandidateResult {
                pose,
                translation: [fft.translation.0, fft.translation.1],
                z_offset: -raster.z_min,
                score: base_score,
                threshold_mm: cross_section_mm,
                translation_source: "tht_pin_island_fft".into(),
            };
            if !candidate.score.is_finite() {
                continue;
            }
            match best.as_ref() {
                None => best = Some(candidate),
                Some(cur) if candidate.score > cur.score => best = Some(candidate),
                _ => {}
            }
        }
    }
    best
}

fn tht_mechanical_alignment_face_score(
    mesh: &MeshData,
    raster: &PoseRaster,
    pose: EulerPose,
    translation: (f64, f64),
    ctx: &FootprintCtx,
) -> Option<f64> {
    let mechanical_labels = ctx.mechanical_drill_label_grid.as_ref()?;
    if mechanical_labels.num_holes == 0 {
        return None;
    }
    let conductive_mask = ctx
        .connected_hole_contact_grid
        .as_ref()
        .or(ctx.connected_hole_grid.as_ref())?;
    let surface = support::tht_drill_masked_first_contact_surface(
        mesh,
        raster,
        conductive_mask,
        pose,
        translation,
    )?;
    if let Some(score) =
        tht_mechanical_contact_point_score(&surface.contact_points, mechanical_labels)
    {
        return Some(score);
    }

    let surface_grid = surface.surface_grid.as_ref()?;
    let detail = raster::raster_hole_reward_detail(surface_grid, translation, mechanical_labels);
    let mechanical_holes = mechanical_labels.num_holes;
    let surface_area = (surface_grid.pixel_count() as f64) * surface_grid.resolution_mm.powi(2);
    let contained_ratio = detail.overlap_area / surface_area.max(EPS);
    let outside_ratio = (surface_area - detail.overlap_area).max(0.0) / surface_area.max(EPS);
    let touched_ratio = (detail.touched_holes as f64) / (mechanical_holes as f64);
    let mean_fill = detail.per_hole_fill.iter().sum::<f64>() / (mechanical_holes as f64);
    let unfilled_ratio =
        (mechanical_holes.saturating_sub(detail.touched_holes) as f64) / (mechanical_holes as f64);

    Some(
        110.0 * touched_ratio + 170.0 * contained_ratio + 35.0 * mean_fill
            - 230.0 * outside_ratio
            - 55.0 * unfilled_ratio,
    )
}

fn tht_mechanical_contact_point_score(
    contact_points: &[ContactPoint],
    mechanical_labels: &HoleLabelGrid,
) -> Option<f64> {
    let mechanical_holes = mechanical_labels.num_holes;
    if contact_points.is_empty() || mechanical_holes == 0 {
        return None;
    }

    let mut touched = vec![false; mechanical_holes];
    let mut inside_points = 0usize;
    for point in contact_points {
        let Some(hole_idx) = support::hole_label_at_world(mechanical_labels, point.x, point.y, 0)
        else {
            continue;
        };
        inside_points += 1;
        touched[hole_idx] = true;
    }

    let touched_holes = touched.iter().filter(|&&hit| hit).count();
    let inside_ratio = (inside_points as f64) / (contact_points.len() as f64);
    let outside_ratio = 1.0 - inside_ratio;
    let touched_ratio = (touched_holes as f64) / (mechanical_holes as f64);
    let unfilled_ratio =
        (mechanical_holes.saturating_sub(touched_holes) as f64) / (mechanical_holes as f64);

    Some(
        170.0 * touched_ratio + 210.0 * inside_ratio
            - 260.0 * outside_ratio
            - 65.0 * unfilled_ratio,
    )
}
