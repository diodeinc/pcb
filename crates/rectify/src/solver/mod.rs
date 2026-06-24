//! The pose search driver. Ports `infer_best_pose` + `evaluate_pose_*` from
//! `research/pose3d/solver.py`.
//!
//! The solver keeps one shared orchestration path: load the STEP mesh, build a
//! footprint context, evaluate every axis-aligned pose through the appropriate
//! footprint-family pipeline, then apply the final translation/Z post-process.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::footprint::{self, FootprintData};
use crate::mesh;
use crate::pose::{EulerPose, candidate_poses};
use crate::raster::RESOLUTION_MM;

mod context;
mod pipelines;
mod scoring;
mod support;
mod translation;

pub(crate) const EPS: f64 = 1e-9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateResult {
    pub pose: EulerPose,
    pub translation: [f64; 2],
    pub z_offset: f64,
    pub score: f64,
    pub threshold_mm: f64,
    pub translation_source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SolveReport {
    pub path: PathBuf,
    pub footprint: String,
    pub previous_rotate: [i32; 3],
    pub previous_offset: [f64; 3],
    pub best: Option<CandidateResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ranked: Vec<CandidateResult>,
}

pub fn solve_best(fp: &FootprintData) -> Result<CandidateResult> {
    let step_bytes = load_step_bytes(fp)?;
    let mesh = mesh::tessellate(&step_bytes)?;
    let mut ranked = evaluate_all_poses(fp, &mesh, RESOLUTION_MM);
    translation::refine_best_translation(fp, &mesh, &mut ranked, RESOLUTION_MM);
    support::clamp_z_offset(&mut ranked, fp.footprint_kind());
    ranked
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("solver produced no candidates"))
}

pub fn solve_json(path: &Path, include_ranked: bool) -> Result<SolveReport> {
    let fp = footprint::parse(path)?;
    let step_bytes = load_step_bytes(&fp)?;
    let mesh = mesh::tessellate(&step_bytes)?;
    let mut ranked = evaluate_all_poses(&fp, &mesh, RESOLUTION_MM);
    translation::refine_best_translation(&fp, &mesh, &mut ranked, RESOLUTION_MM);
    support::clamp_z_offset(&mut ranked, fp.footprint_kind());

    let best = ranked.first().cloned();
    if !include_ranked {
        ranked.clear();
    }
    Ok(SolveReport {
        path: path.to_path_buf(),
        footprint: fp.name.clone(),
        previous_rotate: fp
            .require_model()
            .map(|m| [m.rotate.x, m.rotate.y, m.rotate.z])
            .unwrap_or([0, 0, 0]),
        previous_offset: fp.require_model().map(|m| m.offset).unwrap_or([0.0; 3]),
        best,
        ranked,
    })
}

fn load_step_bytes(fp: &FootprintData) -> Result<Cow<'_, [u8]>> {
    let model = fp.require_model()?;
    if let Some(bytes) = &model.embedded_step {
        return Ok(Cow::Borrowed(bytes));
    }
    if model.path.starts_with("kicad-embed://") {
        anyhow::bail!(
            "footprint references embedded model {} but no embedded bytes were found",
            model.filename
        );
    }
    let p = Path::new(&model.path);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        fp.path.parent().unwrap_or(Path::new(".")).join(p)
    };
    let bytes = std::fs::read(&resolved)
        .with_context(|| format!("failed to read STEP at {}", resolved.display()))?;
    Ok(Cow::Owned(bytes))
}

fn evaluate_all_poses(
    fp: &FootprintData,
    mesh: &crate::mesh::MeshData,
    resolution_mm: f64,
) -> Vec<CandidateResult> {
    let ctx = match context::build_context(fp, resolution_mm) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let poses = candidate_poses();

    // All 24 axis-aligned poses are evaluated without pruning. The Python
    // solver's projection-plausibility filter is removed: in Rust the full
    // evaluation is fast enough, and the filter can incorrectly discard
    // correct poses for components with overhangs or asymmetric leads.
    let mut results: Vec<CandidateResult> = poses
        .par_iter()
        .filter_map(|&pose| pipelines::evaluate_pose(mesh, pose, &ctx, resolution_mm))
        .collect();
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::footprint::{self, FootprintData, FootprintKind, PadShape, Segment};
    use crate::pose::EulerPose;
    use crate::raster::{MaskGrid, PoseRaster};
    use ndarray::Array2;

    fn candidate_with_z(z_offset: f64) -> CandidateResult {
        CandidateResult {
            pose: EulerPose::new(0, 0, 0),
            translation: [0.0, 0.0],
            z_offset,
            score: 0.0,
            threshold_mm: 0.0,
            translation_source: "test".into(),
        }
    }

    #[test]
    fn smd_z_clamp_is_asymmetric() {
        let mut ranked = vec![candidate_with_z(-0.60)];
        support::clamp_z_offset(&mut ranked, FootprintKind::SmdOnly);
        assert_eq!(ranked[0].z_offset, 0.0);

        let mut ranked = vec![candidate_with_z(0.30)];
        support::clamp_z_offset(&mut ranked, FootprintKind::SmdOnly);
        assert_eq!(ranked[0].z_offset, 0.30);
    }

    #[test]
    fn mixed_z_clamp_stays_conservative() {
        let mut ranked = vec![candidate_with_z(-0.60)];
        support::clamp_z_offset(&mut ranked, FootprintKind::Mixed);
        assert_eq!(ranked[0].z_offset, -0.60);

        let mut ranked = vec![candidate_with_z(0.10)];
        support::clamp_z_offset(&mut ranked, FootprintKind::Mixed);
        assert_eq!(ranked[0].z_offset, 0.0);
    }

    #[test]
    fn tht_only_z_clamp_preserves_support_solve() {
        let mut ranked = vec![candidate_with_z(-0.60)];
        support::clamp_z_offset(&mut ranked, FootprintKind::ThtOnly);
        assert_eq!(ranked[0].z_offset, -0.60);

        let mut ranked = vec![candidate_with_z(4.0)];
        support::clamp_z_offset(&mut ranked, FootprintKind::ThtOnly);
        assert_eq!(ranked[0].z_offset, 4.0);
    }

    #[test]
    fn alignment_bounds_fall_back_to_silk_when_fab_and_courtyard_are_missing() {
        let pad_bounds = [-1.0, -1.0, 1.0, 1.0];
        let fp = FootprintData {
            path: PathBuf::from("test.kicad_mod"),
            name: "test".into(),
            pads: vec![PadShape {
                kind: footprint::PadKind::Circle,
                at: [0.0, 0.0],
                size: [2.0, 2.0],
                angle_deg: 0.0,
            }],
            holes: Vec::new(),
            physical_drills: Vec::new(),
            connected_holes: Vec::new(),
            mechanical_drills: Vec::new(),
            silk: Some(vec![
                Segment {
                    a: [-5.0, -8.0],
                    b: [5.0, -8.0],
                },
                Segment {
                    a: [5.0, -8.0],
                    b: [5.0, 1.0],
                },
            ]),
            fab: None,
            courtyard: None,
            model: None,
            smd_pad_count: 0,
            thru_hole_pad_count: 1,
        };

        assert_eq!(
            context::footprint_alignment_bounds(&fp, pad_bounds),
            [-5.0, -8.0, 5.0, 1.0]
        );
    }

    #[test]
    fn drill_masked_support_ignores_hole_penetration_for_z() {
        let resolution_mm = 0.10;
        let mut bottom_z = Array2::<f64>::from_elem((20, 20), 0.0);
        let drill_grid = MaskGrid {
            mask: Array2::<bool>::from_elem((4, 4), true),
            bounds: [-0.2, -0.2, 0.2, 0.2],
            resolution_mm,
        };
        for r in 0..20 {
            let y = -1.0 + ((r as f64) + 0.5) * resolution_mm;
            for c in 0..20 {
                let x = -1.0 + ((c as f64) + 0.5) * resolution_mm;
                if support::mask_contains_world(&drill_grid, x, y, 0) {
                    bottom_z[(r, c)] = -1.0;
                }
            }
        }
        let raster = PoseRaster {
            top_z: bottom_z.mapv(|z| {
                if z.is_finite() {
                    0.0
                } else {
                    f64::NEG_INFINITY
                }
            }),
            bottom_z,
            body_mask: Array2::<bool>::from_elem((20, 20), false),
            bounds: [-1.0, -1.0, 1.0, 1.0],
            resolution_mm,
            z_min: -1.0,
        };

        let support = support::drill_masked_support_z(&raster, &drill_grid, (0.0, 0.0)).unwrap();
        assert_eq!(support.support_z, 0.0);
        assert!(support.below_inside_area_mm2 > 0.0);
        assert_eq!(support.below_outside_area_mm2, 0.0);
    }
}
