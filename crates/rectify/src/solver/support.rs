use ndarray::Array2;

use crate::footprint::FootprintKind;
use crate::mesh::MeshData;
use crate::pose::{EulerPose, KICAD_IMPORT_BASIS, Mat3, apply_mat, rotation_matrix_kicad};
use crate::raster::{self, HoleLabelGrid, MaskGrid, PoseRaster};

use super::context::FootprintCtx;
use super::{CandidateResult, EPS};

/// Clamp small SMD / mixed z_offset values to zero. When the mesh bottom sits
/// close to the board plane, the small vertical offset is usually solder/lead
/// geometry rather than a deliberate placement offset. THT-only parts use the
/// drill-masked support solver instead; forcing them to the board datum hides
/// the pin-vs-body contact problem.
pub(crate) fn clamp_z_offset(ranked: &mut [CandidateResult], footprint_kind: FootprintKind) {
    const Z_CLAMP_SMD_POSITIVE_MM: f64 = 0.25;
    const Z_CLAMP_SMD_NEGATIVE_MM: f64 = 0.60;
    const Z_CLAMP_THT_MM: f64 = 0.15;

    let Some(best) = ranked.first_mut() else {
        return;
    };
    if footprint_kind == FootprintKind::ThtOnly {
        return;
    }

    if footprint_kind == FootprintKind::Mixed {
        if best.z_offset.abs() <= Z_CLAMP_THT_MM {
            best.z_offset = 0.0;
        }
        return;
    }

    // For SMD parts, small positive z_offset can be intentional package
    // seating height, so keep the historical conservative clamp. A modest
    // negative z_offset usually means the STEP datum already sits at the board
    // plane while the bottom-slab raster picked a recessed package feature;
    // clamping that case avoids patching footprints with artificial sinkage.
    if (-Z_CLAMP_SMD_NEGATIVE_MM..=Z_CLAMP_SMD_POSITIVE_MM).contains(&best.z_offset) {
        best.z_offset = 0.0;
    }
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct DrillMaskedSupport {
    pub(crate) support_z: f64,
    pub(crate) support_area_mm2: f64,
    pub(crate) support_bounds: [f64; 4],
    pub(crate) below_inside_area_mm2: f64,
    pub(crate) below_outside_area_mm2: f64,
}

pub(crate) struct DrillMaskedContactSurface {
    pub(crate) support: DrillMaskedSupport,
    pub(crate) surface_grid: Option<MaskGrid>,
    pub(crate) contact_points: Vec<ContactPoint>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ContactPoint {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Debug, Clone, Copy)]
struct SectionStats {
    pixel_count: usize,
    largest_component_px: usize,
    bounds: [f64; 4],
}

pub(crate) fn apply_drill_masked_support_z(
    candidate: &mut CandidateResult,
    mesh: &MeshData,
    raster: &PoseRaster,
    ctx: &FootprintCtx,
) {
    const MIN_SUPPORT_SHIFT_MM: f64 = 0.18;
    const MAX_ZERO_DATUM_SUPPORT_MM: f64 = 0.25;

    let drill_grid = if ctx.footprint_kind == FootprintKind::ThtOnly {
        ctx.physical_drill_contact_grid
            .as_ref()
            .or(ctx.physical_drill_grid.as_ref())
            .or(ctx.connected_hole_grid.as_ref())
    } else {
        ctx.physical_drill_grid.as_ref()
    };
    let Some(drill_grid) = drill_grid else {
        return;
    };
    let translation = (candidate.translation[0], candidate.translation[1]);
    if ctx.footprint_kind == FootprintKind::ThtOnly {
        let support = tht_drill_masked_first_contact_support_z(
            mesh,
            raster,
            drill_grid,
            candidate.pose,
            translation,
        );
        let Some(support) = support else {
            candidate.z_offset = 0.0;
            return;
        };
        candidate.z_offset = -support.support_z;
        candidate.score += tht_support_section_score(ctx, support.support_bounds, translation);
        apply_below_board_penalty(candidate, support, ctx.footprint_kind);
        return;
    }

    let Some(support) = drill_masked_support_z(raster, drill_grid, translation) else {
        return;
    };
    let current_contact_z = -candidate.z_offset;
    if support.support_z <= current_contact_z + MIN_SUPPORT_SHIFT_MM {
        return;
    }
    // Mixed/SMD connectors get this correction only when the masked support
    // plane is near the model's zero datum. Larger nonzero-Z mixed connectors
    // need a separate support model; forcing this heuristic there can move an
    // already-good explicit seating height away from the board datum.
    if support.support_z.abs() > MAX_ZERO_DATUM_SUPPORT_MM {
        return;
    }

    candidate.z_offset = -support.support_z;
    apply_below_board_penalty(candidate, support, ctx.footprint_kind);
}

fn apply_below_board_penalty(
    candidate: &mut CandidateResult,
    support: DrillMaskedSupport,
    footprint_kind: FootprintKind,
) {
    let below_total = support.below_inside_area_mm2 + support.below_outside_area_mm2;
    if below_total <= 0.02 {
        return;
    }

    let outside_weight = if footprint_kind == FootprintKind::ThtOnly {
        650.0
    } else {
        120.0
    };
    let outside_ratio_weight = if footprint_kind == FootprintKind::ThtOnly {
        140.0
    } else {
        45.0
    };
    let inside_reward = if footprint_kind == FootprintKind::ThtOnly {
        18.0
    } else {
        8.0
    };

    let outside_ratio = support.below_outside_area_mm2 / below_total.max(EPS);
    candidate.score -= outside_weight * support.below_outside_area_mm2;
    candidate.score -= outside_ratio_weight * outside_ratio;

    if support.below_inside_area_mm2 > 0.10 && outside_ratio < 0.25 {
        let penetration_ratio =
            (support.below_inside_area_mm2 / support.support_area_mm2.max(0.10)).min(1.0);
        candidate.score += inside_reward * penetration_ratio;
    }
}

fn tht_drill_masked_first_contact_support_z(
    mesh: &MeshData,
    raster: &PoseRaster,
    drill_grid: &MaskGrid,
    pose: EulerPose,
    translation: (f64, f64),
) -> Option<DrillMaskedSupport> {
    tht_drill_masked_first_contact_surface(mesh, raster, drill_grid, pose, translation)
        .map(|surface| surface.support)
}

pub(crate) fn tht_drill_masked_first_contact_surface(
    mesh: &MeshData,
    raster: &PoseRaster,
    drill_grid: &MaskGrid,
    pose: EulerPose,
    translation: (f64, f64),
) -> Option<DrillMaskedContactSurface> {
    const CONTACT_EPS_MM: f64 = 1e-6;
    const CONTACT_SLAB_MM: f64 = 0.03;
    const BELOW_EPS_MM: f64 = 0.03;

    let m = mesh_audit_transform_matrix(pose);
    let mut support_z = f64::INFINITY;
    for i in 0..mesh.num_vertices() {
        let p = apply_mat(&m, mesh.vertex(i));
        if !p[2].is_finite() {
            continue;
        }
        let x = p[0] + translation.0;
        let y = p[1] + translation.1;
        if mask_contains_world(drill_grid, x, y, 0) {
            continue;
        }
        if p[2] < support_z {
            support_z = p[2];
        }
    }
    if !support_z.is_finite() {
        return None;
    }

    let mut contact_points: Vec<ContactPoint> = Vec::new();
    let mut vertex_bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for i in 0..mesh.num_vertices() {
        let p = apply_mat(&m, mesh.vertex(i));
        if !p[2].is_finite() || p[2] > support_z + CONTACT_EPS_MM {
            continue;
        }
        let x = p[0] + translation.0;
        let y = p[1] + translation.1;
        if mask_contains_world(drill_grid, x, y, 0) {
            continue;
        }
        contact_points.push(ContactPoint { x, y });
        vertex_bounds[0] = vertex_bounds[0].min(p[0]);
        vertex_bounds[1] = vertex_bounds[1].min(p[1]);
        vertex_bounds[2] = vertex_bounds[2].max(p[0]);
        vertex_bounds[3] = vertex_bounds[3].max(p[1]);
    }
    if contact_points.is_empty() {
        return None;
    }

    let drill_mask = build_direct_drill_mask(raster, drill_grid, translation, 0);
    let surface_mask = support_slab_mask(raster, &drill_mask, support_z, CONTACT_SLAB_MM);
    let surface_grid = raster::trim_mask(&surface_mask, raster.bounds, raster.resolution_mm);
    let stats = section_stats_from_mask(&surface_mask, raster.bounds, raster.resolution_mm)
        .unwrap_or(SectionStats {
            pixel_count: contact_points.len(),
            largest_component_px: contact_points.len(),
            bounds: vertex_bounds,
        });
    let (below_inside_px, below_outside_px) =
        below_masked_section_counts(raster, &drill_mask, support_z - BELOW_EPS_MM);
    let px_area = raster.resolution_mm * raster.resolution_mm;
    Some(DrillMaskedContactSurface {
        support: DrillMaskedSupport {
            support_z,
            support_area_mm2: (stats.pixel_count as f64) * px_area,
            support_bounds: stats.bounds,
            below_inside_area_mm2: (below_inside_px as f64) * px_area,
            below_outside_area_mm2: (below_outside_px as f64) * px_area,
        },
        surface_grid,
        contact_points,
    })
}

fn mesh_audit_transform_matrix(pose: EulerPose) -> Mat3 {
    let rot = rotation_matrix_kicad(pose);
    let m = rot * KICAD_IMPORT_BASIS;
    let audit: Mat3 = Mat3::from_cols_array(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
    audit * m
}

fn tht_support_section_score(
    ctx: &FootprintCtx,
    support_bounds: [f64; 4],
    translation: (f64, f64),
) -> f64 {
    let shifted = [
        support_bounds[0] + translation.0,
        support_bounds[1] + translation.1,
        support_bounds[2] + translation.0,
        support_bounds[3] + translation.1,
    ];
    let target = ctx.alignment_bounds;
    let support_area =
        ((shifted[2] - shifted[0]).max(0.0) * (shifted[3] - shifted[1]).max(0.0)).max(EPS);
    let ix0 = shifted[0].max(target[0]);
    let iy0 = shifted[1].max(target[1]);
    let ix1 = shifted[2].min(target[2]);
    let iy1 = shifted[3].min(target[3]);
    let intersection = (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0);
    let containment = intersection / support_area;
    let outside_area = (support_area - intersection).max(0.0);
    let support_cx = 0.5 * (shifted[0] + shifted[2]);
    let support_cy = 0.5 * (shifted[1] + shifted[3]);
    let target_cx = 0.5 * (target[0] + target[2]);
    let target_cy = 0.5 * (target[1] + target[3]);
    let center_dist = ((support_cx - target_cx).powi(2) + (support_cy - target_cy).powi(2)).sqrt();

    80.0 * containment - 8.0 * outside_area - 3.0 * center_dist
}

pub(crate) fn drill_masked_support_z(
    raster: &PoseRaster,
    drill_grid: &MaskGrid,
    translation: (f64, f64),
) -> Option<DrillMaskedSupport> {
    const SUPPORT_SLAB_MM: f64 = 0.16;
    const BELOW_EPS_MM: f64 = 0.03;
    const DRILL_MARGIN_PX: i32 = 1;

    let allowed_penetration =
        build_allowed_penetration_mask(raster, drill_grid, translation, DRILL_MARGIN_PX);
    let (h, w) = raster.bottom_z.dim();
    let mut outside_values: Vec<f64> = Vec::new();
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if !z.is_finite() {
                continue;
            }
            if !allowed_penetration[(r, c)] {
                outside_values.push(z);
            }
        }
    }
    if outside_values.len() < 4 {
        return None;
    }

    outside_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let px_area = raster.resolution_mm * raster.resolution_mm;
    let min_abs_px = ((0.20 / px_area).ceil() as usize).max(4);
    let min_rel_px = ((outside_values.len() as f64) * 0.01).ceil() as usize;
    let min_support_px = min_abs_px.max(min_rel_px).min(outside_values.len());
    let min_component_px = ((0.08 / px_area).ceil() as usize)
        .max(4)
        .min(min_support_px);

    let mut support: Option<(f64, SectionStats)> = None;
    let mut j = 0usize;
    for i in 0..outside_values.len() {
        if i > 0 && (outside_values[i] - outside_values[i - 1]).abs() < 1e-6 {
            continue;
        }
        if j < i {
            j = i;
        }
        while j < outside_values.len() && outside_values[j] <= outside_values[i] + SUPPORT_SLAB_MM {
            j += 1;
        }
        if j.saturating_sub(i) < min_support_px {
            continue;
        }
        let Some(stats) = support_slab_stats(
            raster,
            &allowed_penetration,
            outside_values[i],
            SUPPORT_SLAB_MM,
        ) else {
            continue;
        };
        if stats.pixel_count >= min_support_px && stats.largest_component_px >= min_component_px {
            support = Some((outside_values[i], stats));
            break;
        }
    }
    let (support_z, stats) = support?;

    let mut below_inside_px = 0usize;
    let mut below_outside_px = 0usize;
    let below_cutoff = support_z - BELOW_EPS_MM;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if !z.is_finite() || z >= below_cutoff {
                continue;
            }
            if allowed_penetration[(r, c)] {
                below_inside_px += 1;
            } else {
                below_outside_px += 1;
            }
        }
    }

    let result = DrillMaskedSupport {
        support_z,
        support_area_mm2: (stats.pixel_count as f64) * px_area,
        support_bounds: stats.bounds,
        below_inside_area_mm2: (below_inside_px as f64) * px_area,
        below_outside_area_mm2: (below_outside_px as f64) * px_area,
    };
    Some(result)
}

fn build_direct_drill_mask(
    raster: &PoseRaster,
    drill_grid: &MaskGrid,
    translation: (f64, f64),
    drill_margin_px: i32,
) -> Array2<bool> {
    let (h, w) = raster.bottom_z.dim();
    let mut mask = Array2::<bool>::from_elem((h, w), false);
    for r in 0..h {
        let y = raster.bounds[1] + ((r as f64) + 0.5) * raster.resolution_mm + translation.1;
        for c in 0..w {
            let x = raster.bounds[0] + ((c as f64) + 0.5) * raster.resolution_mm + translation.0;
            if mask_contains_world(drill_grid, x, y, drill_margin_px) {
                mask[(r, c)] = true;
            }
        }
    }
    mask
}

fn below_masked_section_counts(
    raster: &PoseRaster,
    drill_mask: &Array2<bool>,
    z_cutoff: f64,
) -> (usize, usize) {
    let (h, w) = raster.bottom_z.dim();
    let mut below_inside_px = 0usize;
    let mut below_outside_px = 0usize;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if !z.is_finite() || z >= z_cutoff {
                continue;
            }
            if drill_mask[(r, c)] {
                below_inside_px += 1;
            } else {
                below_outside_px += 1;
            }
        }
    }
    (below_inside_px, below_outside_px)
}

fn build_allowed_penetration_mask(
    raster: &PoseRaster,
    drill_grid: &MaskGrid,
    translation: (f64, f64),
    drill_margin_px: i32,
) -> Array2<bool> {
    const LOW_ISLAND_HEIGHT_MM: f64 = 0.50;

    let (h, w) = raster.bottom_z.dim();
    let direct_drill = build_direct_drill_mask(raster, drill_grid, translation, drill_margin_px);
    let mut low_contact = Array2::<bool>::from_elem((h, w), false);
    let low_cutoff = raster.z_min + LOW_ISLAND_HEIGHT_MM;

    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if !z.is_finite() {
                continue;
            }
            if z <= low_cutoff {
                low_contact[(r, c)] = true;
            }
        }
    }

    let (labels, count) = raster::label_components(&low_contact);
    if count == 0 {
        return direct_drill;
    }

    let component_px = raster::component_pixel_counts(&labels, count);
    let mut drill_px = vec![0usize; count as usize];
    for r in 0..h {
        for c in 0..w {
            let label = labels[(r, c)];
            if label == 0 || !direct_drill[(r, c)] {
                continue;
            }
            drill_px[(label - 1) as usize] += 1;
        }
    }

    let mut allow_component = vec![false; count as usize];
    for idx in 0..count as usize {
        let hole_px = drill_px[idx];
        if hole_px == 0 {
            continue;
        }
        let comp_px = component_px[idx] as usize;
        let hole_ratio = (hole_px as f64) / (comp_px as f64).max(1.0);
        if hole_ratio >= 0.05 || comp_px <= hole_px.saturating_mul(8) {
            allow_component[idx] = true;
        }
    }
    let mut allowed = direct_drill;
    for r in 0..h {
        for c in 0..w {
            let label = labels[(r, c)];
            if label > 0 && allow_component[(label - 1) as usize] {
                allowed[(r, c)] = true;
            }
        }
    }
    allowed
}

fn support_slab_stats(
    raster: &PoseRaster,
    allowed_penetration: &Array2<bool>,
    support_z: f64,
    slab_mm: f64,
) -> Option<SectionStats> {
    let mask = support_slab_mask(raster, allowed_penetration, support_z, slab_mm);
    section_stats_from_mask(&mask, raster.bounds, raster.resolution_mm)
}

fn support_slab_mask(
    raster: &PoseRaster,
    allowed_penetration: &Array2<bool>,
    support_z: f64,
    slab_mm: f64,
) -> Array2<bool> {
    let (h, w) = raster.bottom_z.dim();
    let mut mask = Array2::<bool>::from_elem((h, w), false);
    let cutoff = support_z + slab_mm + 1e-6;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if !z.is_finite() || z < support_z - 1e-6 || z > cutoff {
                continue;
            }
            if allowed_penetration[(r, c)] {
                continue;
            }
            mask[(r, c)] = true;
        }
    }
    mask
}

fn section_stats_from_mask(
    mask: &Array2<bool>,
    bounds: [f64; 4],
    resolution_mm: f64,
) -> Option<SectionStats> {
    let (h, w) = mask.dim();
    let mut r_min = usize::MAX;
    let mut r_max = 0usize;
    let mut c_min = usize::MAX;
    let mut c_max = 0usize;
    let mut pixel_count = 0usize;
    for r in 0..h {
        for c in 0..w {
            if !mask[(r, c)] {
                continue;
            }
            pixel_count += 1;
            r_min = r_min.min(r);
            r_max = r_max.max(r);
            c_min = c_min.min(c);
            c_max = c_max.max(c);
        }
    }
    if pixel_count == 0 {
        return None;
    }
    let (labels, count) = raster::label_components(mask);
    let largest = raster::component_pixel_counts(&labels, count)
        .into_iter()
        .max()
        .unwrap_or(0) as usize;
    let min_x = bounds[0] + (c_min as f64) * resolution_mm;
    let min_y = bounds[1] + (r_min as f64) * resolution_mm;
    let max_x = bounds[0] + ((c_max as f64) + 1.0) * resolution_mm;
    let max_y = bounds[1] + ((r_max as f64) + 1.0) * resolution_mm;
    Some(SectionStats {
        pixel_count,
        largest_component_px: largest,
        bounds: [min_x, min_y, max_x, max_y],
    })
}

pub(crate) fn mask_contains_world(grid: &MaskGrid, x: f64, y: f64, margin_px: i32) -> bool {
    let col = ((x - grid.bounds[0]) / grid.resolution_mm).floor() as i32;
    let row = ((y - grid.bounds[1]) / grid.resolution_mm).floor() as i32;
    let (h, w) = grid.mask.dim();
    for dr in -margin_px..=margin_px {
        let rr = row + dr;
        if rr < 0 || rr >= h as i32 {
            continue;
        }
        for dc in -margin_px..=margin_px {
            let cc = col + dc;
            if cc >= 0 && cc < w as i32 && grid.mask[(rr as usize, cc as usize)] {
                return true;
            }
        }
    }
    false
}

pub(crate) fn hole_label_at_world(
    grid: &HoleLabelGrid,
    x: f64,
    y: f64,
    margin_px: i32,
) -> Option<usize> {
    let col = ((x - grid.bounds[0]) / grid.resolution_mm).floor() as i32;
    let row = ((y - grid.bounds[1]) / grid.resolution_mm).floor() as i32;
    let (h, w) = grid.labels.dim();
    for dr in -margin_px..=margin_px {
        let rr = row + dr;
        if rr < 0 || rr >= h as i32 {
            continue;
        }
        for dc in -margin_px..=margin_px {
            let cc = col + dc;
            if cc < 0 || cc >= w as i32 {
                continue;
            }
            let label = grid.labels[(rr as usize, cc as usize)];
            if label > 0 {
                return Some((label - 1) as usize);
            }
        }
    }
    None
}
