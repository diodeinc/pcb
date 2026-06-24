use crate::footprint::FootprintKind;
use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::raster;

use super::CandidateResult;
use super::context::FootprintCtx;

mod mixed;
mod smd;
mod tht;

pub(crate) fn evaluate_pose(
    mesh: &MeshData,
    pose: EulerPose,
    ctx: &FootprintCtx,
    resolution_mm: f64,
) -> Option<CandidateResult> {
    let raster = raster::rasterize_mesh_bottom(mesh, pose, resolution_mm)?;
    if ctx.has_holes && ctx.hole_grid.is_some() {
        if ctx.footprint_kind == FootprintKind::ThtOnly {
            return tht::evaluate_pose(mesh, &raster, pose, ctx)
                .or_else(|| mixed::evaluate_pose(mesh, &raster, pose, ctx, resolution_mm));
        }
        return mixed::evaluate_pose(mesh, &raster, pose, ctx, resolution_mm);
    }
    smd::evaluate_pose(mesh, &raster, pose, ctx, resolution_mm)
}
