use std::collections::BTreeMap;

use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::raster::{self, CONTACT_THRESHOLDS_MM_DEFAULT};

use super::super::CandidateResult;
use super::super::context::FootprintCtx;
use super::super::scoring;
use super::super::translation;

pub(super) fn evaluate_pose(
    _mesh: &MeshData,
    raster: &raster::PoseRaster,
    pose: EulerPose,
    ctx: &FootprintCtx,
    resolution_mm: f64,
) -> Option<CandidateResult> {
    let mut best: Option<CandidateResult> = None;
    for &threshold_mm in CONTACT_THRESHOLDS_MM_DEFAULT {
        let Some(contact) = raster::build_contact_grid(raster, threshold_mm) else {
            continue;
        };
        let contact_labels = raster::label_components(&contact.mask);
        let contact_counts = raster::component_pixel_counts(&contact_labels.0, contact_labels.1);

        // Gather translation candidates. BTreeMap gives a deterministic
        // iteration order (by key) — HashMap's randomized order would make
        // tie-breaks during bbox refinement and final scoring non-reproducible.
        let mut proposals: BTreeMap<(i32, i32), (f64, f64, f64, &'static str)> = BTreeMap::new();
        let remember = |map: &mut BTreeMap<(i32, i32), (f64, f64, f64, &'static str)>,
                        t: (f64, f64),
                        mask_overlap: f64,
                        src: &'static str| {
            let key = (
                (t.0 / resolution_mm).round() as i32,
                (t.1 / resolution_mm).round() as i32,
            );
            let entry = map.entry(key).or_insert((t.0, t.1, mask_overlap, src));
            if mask_overlap > entry.2 {
                *entry = (t.0, t.1, mask_overlap, src);
            }
        };
        // Alignment-bounds centroid (courtyard/fab if plausible, else pad).
        let fab_centroid = raster::centroid_translation(ctx.alignment_bounds, contact.bounds);
        remember(
            &mut proposals,
            fab_centroid,
            0.0,
            "fab_obstruction_bbox_centroid",
        );
        // Direct pad-bbox vs contact-bbox centroid.
        let contact_centroid = raster::centroid_translation(ctx.pad_bounds, contact.bounds);
        remember(
            &mut proposals,
            contact_centroid,
            0.0,
            "contact_pad_bbox_centroid",
        );
        // FFT candidates.
        for (i, cand) in raster::fft_translation_candidates(&ctx.pad_grid, &contact)
            .into_iter()
            .enumerate()
        {
            let src = if i == 0 { "fft_pad" } else { "fft_pad_peak" };
            remember(&mut proposals, cand.translation, cand.mask_overlap, src);
        }
        // Sparse-anchor combinatorial matches.
        for t in
            translation::solve_translation_sparse_pad_contact_anchors(&ctx.pad_anchors, &contact)
        {
            remember(&mut proposals, t, 0.0, "sparse_pad_contact_anchor");
        }
        // Bbox-refinement of the single best-overlap candidate.
        {
            let mut entries: Vec<(f64, f64, f64, &'static str)> =
                proposals.values().cloned().collect();
            entries.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((tx, ty, mask_overlap, src)) = entries.first().copied()
                && !src.contains("bbox_refined")
                && let Some(refined) =
                    translation::refine_translation_bbox_contact_pads(ctx, &contact, (tx, ty))
            {
                let refined_src: &'static str = match src {
                    "fab_obstruction_bbox_centroid" => "fab_obstruction_bbox_centroid_bbox_refined",
                    "contact_pad_bbox_centroid" => "contact_pad_bbox_centroid_bbox_refined",
                    "fft_pad" => "fft_pad_bbox_refined",
                    "fft_pad_peak" => "fft_pad_peak_bbox_refined",
                    "sparse_pad_contact_anchor" => "sparse_pad_contact_anchor_bbox_refined",
                    _ => "bbox_refined",
                };
                remember(&mut proposals, refined, mask_overlap, refined_src);
            }
        }

        for (tx, ty, mask_overlap, src) in proposals.values().cloned() {
            let candidate = scoring::score_candidate(
                ctx,
                &contact,
                raster.bounds,
                &contact_labels.0,
                contact_labels.1,
                &contact_counts,
                raster.z_min,
                pose,
                (tx, ty),
                mask_overlap,
                threshold_mm,
                src,
            );
            match best.as_ref() {
                None => best = Some(candidate),
                Some(cur) if candidate.score > cur.score => best = Some(candidate),
                _ => {}
            }
        }
    }
    best
}
