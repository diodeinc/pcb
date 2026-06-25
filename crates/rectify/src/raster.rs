//! Rasterization primitives used by the pose solver.
//!
//! Direct ports of the Python solver's `build_mask_grid` / `build_pose_raster`
//! / `build_contact_grid` stack, reshaped for Rust's ndarray. Coordinate
//! conventions and grid resolution mirror `solver.py` exactly so per-pixel
//! counts and centroids match.

use std::sync::Arc;

use ndarray::Array2;
use num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::footprint::{PadKind, PadShape};
use crate::mesh::MeshData;
use crate::pose::EulerPose;
use crate::pose::{KICAD_IMPORT_BASIS, Mat3, apply_mat, rotation_matrix_kicad};

pub const RESOLUTION_MM: f64 = 0.10;

/// Contact-slab thresholds swept during SMD pose evaluation. Mirrors the
/// Python solver's `CONTACT_THRESHOLDS_MM`.
pub const CONTACT_THRESHOLDS_MM_DEFAULT: &[f64] =
    &[0.01, 0.02, 0.04, 0.06, 0.08, 0.12, 0.16, 0.30, 0.50];

/// Small buffer in mm used when deciding whether a pixel center is inside a
/// pad polygon. Matches `poly.buffer(resolution_mm * 0.02)` in the Python
/// rasterizer.
const POLY_EPS_FRAC: f64 = 0.02;

#[derive(Debug, Clone)]
pub struct MaskGrid {
    pub mask: Array2<bool>,
    /// `[min_x, min_y, max_x, max_y]` world-space bounds.
    pub bounds: [f64; 4],
    pub resolution_mm: f64,
}

impl MaskGrid {
    #[allow(dead_code)] // Useful public API.
    pub fn width(&self) -> usize {
        self.mask.ncols()
    }
    #[allow(dead_code)] // Useful public API.
    pub fn height(&self) -> usize {
        self.mask.nrows()
    }
    pub fn pixel_count(&self) -> usize {
        self.mask.iter().filter(|&&b| b).count()
    }
}

#[derive(Debug, Clone)]
pub struct PoseRaster {
    /// Per-pixel bottom-Z in mm (in the rotated audit frame). `f64::INFINITY`
    /// where no triangle was rasterized.
    pub bottom_z: Array2<f64>,
    /// Per-pixel top-Z in mm. Together with `bottom_z`, this gives the solver a
    /// cheap solid-interval approximation for z-slice cross sections.
    #[allow(dead_code)] // Retained for z-slice diagnostics and future support scoring.
    pub top_z: Array2<f64>,
    #[allow(dead_code)] // Retained: used by the rasterizer, useful for debug viz.
    pub body_mask: Array2<bool>,
    pub bounds: [f64; 4],
    pub resolution_mm: f64,
    pub z_min: f64,
}

/// Label grid flagging which pad (1-indexed) owns each pixel. `0` means no pad.
/// Pixels covered by overlapping pads receive the lowest pad index that
/// contains them (deterministic, matches the Python solver's `argmax` over a
/// first-touched rasterization).
#[derive(Debug, Clone)]
pub struct PadLabelGrid {
    pub labels: Array2<u16>,
    pub areas_px: Vec<u32>,
    pub bounds: [f64; 4],
    pub resolution_mm: f64,
}

impl PadLabelGrid {
    pub fn dim(&self) -> (usize, usize) {
        self.labels.dim()
    }
}

/// Label grid identifying hole ownership (1-indexed). Same layout as
/// `PadLabelGrid`. Mirrors Python's `_build_hole_label_grid`.
#[derive(Debug, Clone)]
pub struct HoleLabelGrid {
    pub labels: Array2<u16>,
    pub bounds: [f64; 4],
    pub resolution_mm: f64,
    pub num_holes: usize,
}

impl HoleLabelGrid {
    pub fn dim(&self) -> (usize, usize) {
        self.labels.dim()
    }
}

pub fn rasterize_hole_labels(holes: &[PadShape], resolution_mm: f64) -> Option<HoleLabelGrid> {
    if holes.is_empty() {
        return None;
    }
    let polys: Vec<Polygon> = holes.iter().map(pad_to_polygon).collect();
    let bounds = union_bounds(&polys)?;
    let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
    let mut labels = Array2::<u16>::zeros((height, width));
    let eps = resolution_mm * POLY_EPS_FRAC;
    for (idx, poly) in polys.iter().enumerate() {
        let hole_id = (idx + 1) as u16;
        let [pb_min_x, pb_min_y, pb_max_x, pb_max_y] = poly.bounds;
        let col0 =
            (((pb_min_x - eps - bounds[0]) / resolution_mm).floor() as isize).max(0) as usize;
        let col1 = (((pb_max_x + eps - bounds[0]) / resolution_mm).ceil() as isize)
            .min(width as isize)
            .max(0) as usize;
        let row0 =
            (((pb_min_y - eps - bounds[1]) / resolution_mm).floor() as isize).max(0) as usize;
        let row1 = (((pb_max_y + eps - bounds[1]) / resolution_mm).ceil() as isize)
            .min(height as isize)
            .max(0) as usize;
        for r in row0..row1 {
            let y = bounds[1] + ((r as f64) + 0.5) * resolution_mm;
            for c in col0..col1 {
                let x = bounds[0] + ((c as f64) + 0.5) * resolution_mm;
                if point_inside_polygon(poly, x, y, eps) && labels[(r, c)] == 0 {
                    labels[(r, c)] = hole_id;
                }
            }
        }
    }
    Some(HoleLabelGrid {
        labels,
        bounds,
        resolution_mm,
        num_holes: holes.len(),
    })
}

/// Extended hole-reward diagnostics.
pub struct HoleRewardDetail {
    /// Total overlap area in mm².
    pub overlap_area: f64,
    /// Number of distinct holes that have at least one pin pixel.
    pub touched_holes: usize,
    /// Per-hole fill ratio: pin pixels inside hole / hole area pixels.
    /// Indexed by hole id (0-based). 0.0 if the hole wasn't touched.
    pub per_hole_fill: Vec<f64>,
}

/// Detailed hole-reward computation with per-hole fill ratios.
pub fn raster_hole_reward_detail(
    pin_grid: &MaskGrid,
    translation: (f64, f64),
    hole_label_grid: &HoleLabelGrid,
) -> HoleRewardDetail {
    let resolution_mm = hole_label_grid.resolution_mm;
    let (hh, hw) = hole_label_grid.dim();
    let (ph, pw) = pin_grid.mask.dim();
    let (tx, ty) = translation;
    let pin_x0 = pin_grid.bounds[0] + tx;
    let pin_y0 = pin_grid.bounds[1] + ty;
    let col_off = ((pin_x0 - hole_label_grid.bounds[0]) / resolution_mm).round() as i32;
    let row_off = ((pin_y0 - hole_label_grid.bounds[1]) / resolution_mm).round() as i32;
    let r0 = row_off.max(0) as usize;
    let c0 = col_off.max(0) as usize;
    let r1 = ((row_off + ph as i32).max(0) as usize).min(hh);
    let c1 = ((col_off + pw as i32).max(0) as usize).min(hw);
    let num_holes = hole_label_grid.num_holes;
    if r1 <= r0 || c1 <= c0 {
        return HoleRewardDetail {
            overlap_area: 0.0,
            touched_holes: 0,
            per_hole_fill: vec![0.0; num_holes],
        };
    }
    let sr0 = (r0 as i32 - row_off) as usize;
    let sc0 = (c0 as i32 - col_off) as usize;
    let mut overlap_per_hole = vec![0u32; num_holes];
    let mut hole_area_px = vec![0u32; num_holes];
    // Count hole area pixels in the overlapping region.
    for dr in 0..(r1 - r0) {
        for dc in 0..(c1 - c0) {
            let h = hole_label_grid.labels[(r0 + dr, c0 + dc)];
            if h == 0 {
                continue;
            }
            hole_area_px[(h - 1) as usize] += 1;
            if pin_grid.mask[(sr0 + dr, sc0 + dc)] {
                overlap_per_hole[(h - 1) as usize] += 1;
            }
        }
    }
    // Also count hole pixels outside the overlap region for accurate area.
    // Walk the full hole label grid for holes not fully covered above.
    let mut full_hole_area = vec![0u32; num_holes];
    for r in 0..hh {
        for c in 0..hw {
            let h = hole_label_grid.labels[(r, c)];
            if h > 0 {
                full_hole_area[(h - 1) as usize] += 1;
            }
        }
    }
    let overlap_total: u32 = overlap_per_hole.iter().sum();
    let overlap_area = (overlap_total as f64) * resolution_mm * resolution_mm;
    let mut touched_holes = 0usize;
    let mut per_hole_fill = vec![0.0f64; num_holes];
    for i in 0..num_holes {
        let area = full_hole_area[i].max(1);
        let fill = (overlap_per_hole[i] as f64) / (area as f64);
        per_hole_fill[i] = fill;
        if overlap_per_hole[i] > 0 {
            touched_holes += 1;
        }
    }
    HoleRewardDetail {
        overlap_area,
        touched_holes,
        per_hole_fill,
    }
}

pub fn rasterize_pad_labels(pads: &[PadShape], resolution_mm: f64) -> Option<PadLabelGrid> {
    if pads.is_empty() {
        return None;
    }
    let polys: Vec<Polygon> = pads.iter().map(pad_to_polygon).collect();
    let bounds = union_bounds(&polys)?;
    let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
    let mut labels = Array2::<u16>::zeros((height, width));
    let mut areas = vec![0u32; pads.len()];
    let eps = resolution_mm * POLY_EPS_FRAC;
    for (idx, poly) in polys.iter().enumerate() {
        let pad_id = (idx + 1) as u16;
        rasterize_polygon_label_into(
            poly,
            bounds,
            resolution_mm,
            eps,
            &mut labels,
            pad_id,
            &mut areas[idx],
        );
    }
    Some(PadLabelGrid {
        labels,
        areas_px: areas,
        bounds,
        resolution_mm,
    })
}

fn rasterize_polygon_label_into(
    poly: &Polygon,
    grid_bounds: [f64; 4],
    resolution_mm: f64,
    eps: f64,
    labels: &mut Array2<u16>,
    label: u16,
    area_px: &mut u32,
) {
    let (grid_h, grid_w) = labels.dim();
    let [grid_min_x, grid_min_y, _, _] = grid_bounds;
    let [pb_min_x, pb_min_y, pb_max_x, pb_max_y] = poly.bounds;
    let row0 = (((pb_min_y - eps - grid_min_y) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_h as isize) as usize;
    let row1 = (((pb_max_y + eps - grid_min_y) / resolution_mm).ceil() as isize)
        .max(0)
        .min(grid_h as isize) as usize;
    let col0 = (((pb_min_x - eps - grid_min_x) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_w as isize) as usize;
    let col1 = (((pb_max_x + eps - grid_min_x) / resolution_mm).ceil() as isize)
        .max(0)
        .min(grid_w as isize) as usize;
    if row1 <= row0 || col1 <= col0 {
        return;
    }

    let mut xs = Vec::new();
    for r in row0..row1 {
        let y = grid_min_y + ((r as f64) + 0.5) * resolution_mm;
        for ring in &poly.rings {
            scanline_intersections(ring, y, &mut xs);
            for span in xs.chunks_exact(2) {
                let start = first_col_with_center_gt(span[0], grid_min_x, resolution_mm)
                    .max(col0 as isize)
                    .min(col1 as isize) as usize;
                let end = first_col_with_center_ge(span[1], grid_min_x, resolution_mm)
                    .max(col0 as isize)
                    .min(col1 as isize) as usize;
                if end <= start {
                    continue;
                }
                for c in start..end {
                    if labels[(r, c)] == 0 {
                        labels[(r, c)] = label;
                        *area_px += 1;
                    }
                }
            }
        }
    }
}

fn scanline_intersections(ring: &[[f64; 2]], y: f64, xs: &mut Vec<f64>) {
    xs.clear();
    let n = ring.len();
    if n < 3 {
        return;
    }
    let mut j = n - 1;
    for i in 0..n {
        let xi = ring[i][0];
        let yi = ring[i][1];
        let xj = ring[j][0];
        let yj = ring[j][1];
        if (yi > y) != (yj > y) {
            xs.push((xj - xi) * (y - yi) / (yj - yi + f64::EPSILON) + xi);
        }
        j = i;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
}

fn first_col_with_center_gt(x: f64, grid_min_x: f64, resolution_mm: f64) -> isize {
    (((x - grid_min_x) / resolution_mm - 0.5).floor() as isize) + 1
}

fn first_col_with_center_ge(x: f64, grid_min_x: f64, resolution_mm: f64) -> isize {
    ((x - grid_min_x) / resolution_mm - 0.5).ceil() as isize
}

/// Rasterize a union of pad shapes onto a fixed grid.
pub fn rasterize_pad_union(pads: &[PadShape], resolution_mm: f64) -> Option<MaskGrid> {
    let polys: Vec<Polygon> = pads.iter().map(pad_to_polygon).collect();
    if polys.is_empty() {
        return None;
    }
    let bounds = union_bounds(&polys)?;
    let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
    let mut mask = Array2::from_elem((height, width), false);
    let eps = resolution_mm * POLY_EPS_FRAC;
    for poly in &polys {
        rasterize_polygon_into(poly, bounds, resolution_mm, eps, &mut mask);
    }
    Some(MaskGrid {
        mask,
        bounds,
        resolution_mm,
    })
}

/// Rasterize a rotated mesh into a bottom-Z height field on a grid.
///
/// The mesh is first mapped into KiCad's import basis (`mesh_to_kicad_import_basis`
/// in `solver.py`), then rotated by `pose`, and finally rasterized on the
/// X/Y plane keeping the minimum triangle-interpolated Z per pixel.
pub fn rasterize_mesh_bottom(
    mesh: &MeshData,
    pose: EulerPose,
    resolution_mm: f64,
) -> Option<PoseRaster> {
    let rot = rotation_matrix_kicad(pose);
    let m = rot * KICAD_IMPORT_BASIS;
    // The Python `infer_footprint_pose` pipeline uses `audit_frame=True`, which
    // swaps Y/Z of the rotated vertices so the board plane sits on X/Y.
    let audit: Mat3 = Mat3::from_cols_array(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
    let m = audit * m;
    let triangles = rotate_triangles(mesh, &m);
    if triangles.is_empty() {
        return None;
    }
    build_pose_raster_from_triangles(&triangles, resolution_mm)
}

/// Build a `PoseRaster` from a triangle soup already expressed in the
/// rasterization frame. Exposed so the harness can feed synthetic inputs.
pub fn build_pose_raster_from_triangles(
    triangles: &[[[f64; 3]; 3]],
    resolution_mm: f64,
) -> Option<PoseRaster> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut z_floor = f64::INFINITY;
    for tri in triangles {
        for p in tri {
            if p[0] < min_x {
                min_x = p[0];
            }
            if p[1] < min_y {
                min_y = p[1];
            }
            if p[0] > max_x {
                max_x = p[0];
            }
            if p[1] > max_y {
                max_y = p[1];
            }
            if p[2] < z_floor {
                z_floor = p[2];
            }
        }
    }
    if !min_x.is_finite() || !min_y.is_finite() {
        return None;
    }
    let bounds = [min_x, min_y, max_x, max_y];
    let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
    let mut bottom_z = Array2::from_elem((height, width), f64::INFINITY);
    let mut top_z = Array2::from_elem((height, width), f64::NEG_INFINITY);
    let mut body_mask = Array2::from_elem((height, width), false);

    // Cull up-facing triangles that sit far above the contact zone. Mirrors
    // the `faces_up & far_from_bottom` cull in the Python rasterizer.
    const Z_CULL_MARGIN: f64 = 0.50;

    let mut target = RasterTarget {
        bottom_z: &mut bottom_z,
        top_z: &mut top_z,
        body_mask: &mut body_mask,
        bounds,
        resolution_mm,
    };
    for tri in triangles {
        let v0 = tri[0];
        let v1 = tri[1];
        let v2 = tri[2];
        let tri_min_z = v0[2].min(v1[2]).min(v2[2]);
        let normal_z = (v1[0] - v0[0]) * (v2[1] - v0[1]) - (v1[1] - v0[1]) * (v2[0] - v0[0]);
        let faces_up = normal_z > 1e-8;
        let far_from_bottom = tri_min_z > (z_floor + Z_CULL_MARGIN);
        let z_contributing = !(faces_up && far_from_bottom);
        rasterize_triangle(v0, v1, v2, &mut target, z_contributing);
    }

    let mut z_min = f64::INFINITY;
    let mut any_finite = false;
    for &z in bottom_z.iter() {
        if z.is_finite() {
            any_finite = true;
            if z < z_min {
                z_min = z;
            }
        }
    }
    if !any_finite {
        return None;
    }
    Some(PoseRaster {
        bottom_z,
        top_z,
        body_mask,
        bounds: [
            min_x,
            min_y,
            min_x + (width as f64) * resolution_mm,
            min_y + (height as f64) * resolution_mm,
        ],
        resolution_mm,
        z_min,
    })
}

/// Build the contact slab mask: all pixels whose bottom-Z lies within
/// `threshold_mm` of `raster.z_min`. Trimmed to its bounding box (matching
/// `trim_mask_grid` in the Python solver).
pub fn build_contact_grid(raster: &PoseRaster, threshold_mm: f64) -> Option<MaskGrid> {
    let (h, w) = raster.bottom_z.dim();
    let mut mask = Array2::from_elem((h, w), false);
    let cutoff = raster.z_min + threshold_mm + 1e-6;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() && z <= cutoff {
                mask[(r, c)] = true;
            }
        }
    }
    trim_mask(&mask, raster.bounds, raster.resolution_mm)
}

/// Build a pin mask: pixels with `bottom_z < body_z - 0.05` where `body_z` is
/// the 25th percentile of finite bottom-Z values. Returns `(pin_grid, body_z)`.
/// Mirrors Python's `build_pin_mask`.
pub fn build_pin_mask(raster: &PoseRaster) -> Option<(MaskGrid, f64)> {
    let (h, w) = raster.bottom_z.dim();
    let mut vals: Vec<f64> = Vec::new();
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() {
                vals.push(z);
            }
        }
    }
    if vals.is_empty() {
        return None;
    }
    let z_min = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let body_z = percentile_25(&mut vals);
    if body_z - z_min < 0.15 {
        return None;
    }
    let cutoff = body_z - 0.05;
    let mut mask = Array2::from_elem((h, w), false);
    let mut any = false;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() && z < cutoff {
                mask[(r, c)] = true;
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    let grid = trim_mask(&mask, raster.bounds, raster.resolution_mm)?;
    Some((grid, body_z))
}

fn percentile_25(vals: &mut [f64]) -> f64 {
    // Matches `np.percentile(z_vals, 25)` (linear interpolation).
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = vals.len();
    if n == 1 {
        return vals[0];
    }
    let pos = 0.25 * ((n - 1) as f64);
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = pos - (lo as f64);
    vals[lo] * (1.0 - frac) + vals[hi] * frac
}

/// Build a secondary contact slab that skips the pixels already covered by a
/// primary slab. Finds the next minimum Z among unmasked pixels and returns
/// pixels within `threshold_mm` of that minimum. Mirrors Python's
/// `build_secondary_contact_grid`.
pub fn build_secondary_contact_grid(
    raster: &PoseRaster,
    primary: &MaskGrid,
    threshold_mm: f64,
) -> Option<(MaskGrid, f64)> {
    let (h, w) = raster.bottom_z.dim();
    // Map `primary` pixels back into `raster` pixel space.
    let dx_px = ((primary.bounds[0] - raster.bounds[0]) / raster.resolution_mm).round() as i32;
    let dy_px = ((primary.bounds[1] - raster.bounds[1]) / raster.resolution_mm).round() as i32;
    let (sh, sw) = primary.mask.dim();
    let y0 = dy_px.max(0) as usize;
    let x0 = dx_px.max(0) as usize;
    let y1 = ((dy_px + sh as i32).max(0) as usize).min(h);
    let x1 = ((dx_px + sw as i32).max(0) as usize).min(w);
    let mut excluded = Array2::<bool>::from_elem((h, w), false);
    if y1 > y0 && x1 > x0 {
        let src_y0 = (y0 as i32 - dy_px) as usize;
        let src_x0 = (x0 as i32 - dx_px) as usize;
        for dr in 0..(y1 - y0) {
            for dc in 0..(x1 - x0) {
                if primary.mask[(src_y0 + dr, src_x0 + dc)] {
                    excluded[(y0 + dr, x0 + dc)] = true;
                }
            }
        }
    }
    let mut next_min = f64::INFINITY;
    for r in 0..h {
        for c in 0..w {
            if excluded[(r, c)] {
                continue;
            }
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() && z < next_min {
                next_min = z;
            }
        }
    }
    if !next_min.is_finite() {
        return None;
    }
    let cutoff = next_min + threshold_mm + 1e-6;
    let mut mask = Array2::<bool>::from_elem((h, w), false);
    let mut any = false;
    for r in 0..h {
        for c in 0..w {
            if excluded[(r, c)] {
                continue;
            }
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() && z <= cutoff {
                mask[(r, c)] = true;
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    let grid = trim_mask(&mask, raster.bounds, raster.resolution_mm)?;
    Some((grid, next_min))
}

/// Build a body-contact slab around `body_z`: pixels with `bottom_z >=
/// body_z - 0.05` within `threshold_mm` of `body_z`. Mirrors the Python call
/// `build_contact_grid(PoseRaster(..., z_min=body_z, ...), max(CONTACT_THRESHOLDS_MM))`
/// where pixels below `body_z - 0.05` have been masked out.
pub fn build_body_contact_grid(
    raster: &PoseRaster,
    body_z: f64,
    threshold_mm: f64,
) -> Option<MaskGrid> {
    let (h, w) = raster.bottom_z.dim();
    let mut mask = Array2::<bool>::from_elem((h, w), false);
    let cutoff = body_z + threshold_mm + 1e-6;
    let floor = body_z - 0.05;
    let mut any = false;
    for r in 0..h {
        for c in 0..w {
            let z = raster.bottom_z[(r, c)];
            if z.is_finite() && z >= floor && z <= cutoff {
                mask[(r, c)] = true;
                any = true;
            }
        }
    }
    if !any {
        return None;
    }
    trim_mask(&mask, raster.bounds, raster.resolution_mm)
}

/// Crop a `mask` to its bounding box, rebasing the `bounds` accordingly.
/// Returns `None` when the mask is empty.
pub fn trim_mask(mask: &Array2<bool>, bounds: [f64; 4], resolution_mm: f64) -> Option<MaskGrid> {
    let (h, w) = mask.dim();
    let mut r_min = usize::MAX;
    let mut r_max = 0usize;
    let mut c_min = usize::MAX;
    let mut c_max = 0usize;
    let mut any = false;
    for r in 0..h {
        for c in 0..w {
            if mask[(r, c)] {
                any = true;
                if r < r_min {
                    r_min = r;
                }
                if r > r_max {
                    r_max = r;
                }
                if c < c_min {
                    c_min = c;
                }
                if c > c_max {
                    c_max = c;
                }
            }
        }
    }
    if !any {
        return None;
    }
    let trimmed_h = r_max - r_min + 1;
    let trimmed_w = c_max - c_min + 1;
    let mut trimmed = Array2::from_elem((trimmed_h, trimmed_w), false);
    for r in 0..trimmed_h {
        for c in 0..trimmed_w {
            trimmed[(r, c)] = mask[(r + r_min, c + c_min)];
        }
    }
    let min_x = bounds[0] + (c_min as f64) * resolution_mm;
    let min_y = bounds[1] + (r_min as f64) * resolution_mm;
    let max_x = min_x + (trimmed_w as f64) * resolution_mm;
    let max_y = min_y + (trimmed_h as f64) * resolution_mm;
    Some(MaskGrid {
        mask: trimmed,
        bounds: [min_x, min_y, max_x, max_y],
        resolution_mm,
    })
}

// ---------------------------------------------------------------------------
// Polygon + rasterization helpers
// ---------------------------------------------------------------------------

/// 2D polygon, possibly as a union of convex rings (pads compose from one or
/// more rings after rotation/translation).
#[derive(Debug, Clone)]
pub struct Polygon {
    pub rings: Vec<Vec<[f64; 2]>>,
    pub bounds: [f64; 4],
}

impl Polygon {
    fn from_rings(rings: Vec<Vec<[f64; 2]>>) -> Self {
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for ring in &rings {
            for p in ring {
                if p[0] < min_x {
                    min_x = p[0];
                }
                if p[1] < min_y {
                    min_y = p[1];
                }
                if p[0] > max_x {
                    max_x = p[0];
                }
                if p[1] > max_y {
                    max_y = p[1];
                }
            }
        }
        Self {
            rings,
            bounds: [min_x, min_y, max_x, max_y],
        }
    }
}

fn union_bounds(polys: &[Polygon]) -> Option<[f64; 4]> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for p in polys {
        min_x = min_x.min(p.bounds[0]);
        min_y = min_y.min(p.bounds[1]);
        max_x = max_x.max(p.bounds[2]);
        max_y = max_y.max(p.bounds[3]);
    }
    if !min_x.is_finite() {
        None
    } else {
        Some([min_x, min_y, max_x, max_y])
    }
}

fn bounds_to_grid_size(bounds: [f64; 4], resolution_mm: f64) -> (usize, usize) {
    let w = ((bounds[2] - bounds[0]) / resolution_mm).ceil().max(1.0) as usize;
    let h = ((bounds[3] - bounds[1]) / resolution_mm).ceil().max(1.0) as usize;
    (w, h)
}

pub fn pad_to_polygon(pad: &PadShape) -> Polygon {
    let [cx, cy] = pad.at;
    let [w, h] = pad.size;
    let angle = pad.angle_deg.to_radians();
    let (sa, ca) = angle.sin_cos();
    let transform = |x: f64, y: f64| -> [f64; 2] { [cx + ca * x - sa * y, cy + sa * x + ca * y] };
    let rings = match pad.kind {
        PadKind::Rect | PadKind::RoundRect | PadKind::Trapezoid => {
            vec![rect_ring(w, h, &transform)]
        }
        PadKind::Circle => vec![circle_ring(w * 0.5, cx, cy, 48)],
        PadKind::Oval => vec![capsule_ring(w, h, &transform)],
    };
    Polygon::from_rings(rings)
}

fn rect_ring<F: Fn(f64, f64) -> [f64; 2]>(w: f64, h: f64, xf: &F) -> Vec<[f64; 2]> {
    let hw = w * 0.5;
    let hh = h * 0.5;
    vec![xf(-hw, -hh), xf(hw, -hh), xf(hw, hh), xf(-hw, hh)]
}

fn circle_ring(r: f64, cx: f64, cy: f64, n: usize) -> Vec<[f64; 2]> {
    let mut ring = Vec::with_capacity(n);
    for i in 0..n {
        let a = (i as f64) / (n as f64) * std::f64::consts::TAU;
        ring.push([cx + r * a.cos(), cy + r * a.sin()]);
    }
    ring
}

fn capsule_ring<F: Fn(f64, f64) -> [f64; 2]>(w: f64, h: f64, xf: &F) -> Vec<[f64; 2]> {
    // Approximate the oval as a high-segment polygon via explicit sampling.
    let r = w.min(h) * 0.5;
    let mut ring = Vec::with_capacity(48);
    if w >= h {
        let half = ((w * 0.5) - r).max(0.0);
        // right semicircle
        for i in 0..=24 {
            let a = -std::f64::consts::FRAC_PI_2 + (i as f64) / 24.0 * std::f64::consts::PI;
            ring.push(xf(half + r * a.cos(), r * a.sin()));
        }
        // left semicircle
        for i in 0..=24 {
            let a = std::f64::consts::FRAC_PI_2 + (i as f64) / 24.0 * std::f64::consts::PI;
            ring.push(xf(-half + r * a.cos(), r * a.sin()));
        }
    } else {
        let half = ((h * 0.5) - r).max(0.0);
        for i in 0..=24 {
            let a = (i as f64) / 24.0 * std::f64::consts::PI;
            ring.push(xf(r * a.cos(), half + r * a.sin()));
        }
        for i in 0..=24 {
            let a = std::f64::consts::PI + (i as f64) / 24.0 * std::f64::consts::PI;
            ring.push(xf(r * a.cos(), -half + r * a.sin()));
        }
    }
    ring
}

fn rasterize_polygon_into(
    poly: &Polygon,
    grid_bounds: [f64; 4],
    resolution_mm: f64,
    eps: f64,
    mask: &mut Array2<bool>,
) {
    let (grid_h, grid_w) = mask.dim();
    let [grid_min_x, grid_min_y, _, _] = grid_bounds;
    let [pb_min_x, pb_min_y, pb_max_x, pb_max_y] = poly.bounds;
    let col0 = (((pb_min_x - eps - grid_min_x) / resolution_mm).floor() as isize).max(0) as usize;
    let col1 = (((pb_max_x + eps - grid_min_x) / resolution_mm).ceil() as isize)
        .min(grid_w as isize)
        .max(0) as usize;
    let row0 = (((pb_min_y - eps - grid_min_y) / resolution_mm).floor() as isize).max(0) as usize;
    let row1 = (((pb_max_y + eps - grid_min_y) / resolution_mm).ceil() as isize)
        .min(grid_h as isize)
        .max(0) as usize;
    for r in row0..row1 {
        let y = grid_min_y + ((r as f64) + 0.5) * resolution_mm;
        for c in col0..col1 {
            if mask[(r, c)] {
                continue;
            }
            let x = grid_min_x + ((c as f64) + 0.5) * resolution_mm;
            if point_inside_polygon(poly, x, y, eps) {
                mask[(r, c)] = true;
            }
        }
    }
}

/// Inclusive point-in-polygon test with an `eps` tolerance. Even-odd rule
/// across all rings (disjoint-ring sums approximate `unary_union`'s behaviour
/// for the convex pad shapes we emit).
pub fn point_inside_polygon(poly: &Polygon, x: f64, y: f64, _eps: f64) -> bool {
    for ring in &poly.rings {
        if ring_contains(ring, x, y) {
            return true;
        }
    }
    false
}

fn ring_contains(ring: &[[f64; 2]], x: f64, y: f64) -> bool {
    let mut inside = false;
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let xi = ring[i][0];
        let yi = ring[i][1];
        let xj = ring[j][0];
        let yj = ring[j][1];
        let crosses =
            (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi + f64::EPSILON) + xi;
        if crosses {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ---------------------------------------------------------------------------
// Triangle rasterization
// ---------------------------------------------------------------------------

fn rotate_triangles(mesh: &MeshData, m: &Mat3) -> Vec<[[f64; 3]; 3]> {
    let num_v = mesh.num_vertices();
    let mut rotated = Vec::with_capacity(num_v);
    for i in 0..num_v {
        rotated.push(apply_mat(m, mesh.vertex(i)));
    }
    let num_f = mesh.num_faces();
    let mut tris = Vec::with_capacity(num_f);
    for f in 0..num_f {
        let a = mesh.faces[f * 3] as usize;
        let b = mesh.faces[f * 3 + 1] as usize;
        let c = mesh.faces[f * 3 + 2] as usize;
        if a >= num_v || b >= num_v || c >= num_v {
            continue;
        }
        tris.push([rotated[a], rotated[b], rotated[c]]);
    }
    tris
}

struct RasterTarget<'a> {
    bottom_z: &'a mut Array2<f64>,
    top_z: &'a mut Array2<f64>,
    body_mask: &'a mut Array2<bool>,
    bounds: [f64; 4],
    resolution_mm: f64,
}

/// Rasterize a single triangle, taking the minimum Z per covered pixel.
fn rasterize_triangle(
    v0: [f64; 3],
    v1: [f64; 3],
    v2: [f64; 3],
    target: &mut RasterTarget<'_>,
    z_contributing: bool,
) {
    let (grid_h, grid_w) = target.bottom_z.dim();
    let min_x = target.bounds[0];
    let min_y = target.bounds[1];
    let resolution_mm = target.resolution_mm;

    let tri_min_x = v0[0].min(v1[0]).min(v2[0]);
    let tri_max_x = v0[0].max(v1[0]).max(v2[0]);
    let tri_min_y = v0[1].min(v1[1]).min(v2[1]);
    let tri_max_y = v0[1].max(v1[1]).max(v2[1]);
    let ix0 = (((tri_min_x - min_x) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_w as isize - 1) as usize;
    let ix1 = (((tri_max_x - min_x) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_w as isize - 1) as usize;
    let iy0 = (((tri_min_y - min_y) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_h as isize - 1) as usize;
    let iy1 = (((tri_max_y - min_y) / resolution_mm).floor() as isize)
        .max(0)
        .min(grid_h as isize - 1) as usize;

    let denom = (v1[1] - v2[1]) * (v0[0] - v2[0]) + (v2[0] - v1[0]) * (v0[1] - v2[1]);
    if denom.abs() < 1e-12 {
        return;
    }
    let inv_denom = 1.0 / denom;

    for iy in iy0..=iy1 {
        let py = min_y + ((iy as f64) + 0.5) * resolution_mm;
        for ix in ix0..=ix1 {
            let px = min_x + ((ix as f64) + 0.5) * resolution_mm;
            let a = ((v1[1] - v2[1]) * (px - v2[0]) + (v2[0] - v1[0]) * (py - v2[1])) * inv_denom;
            let b = ((v2[1] - v0[1]) * (px - v2[0]) + (v0[0] - v2[0]) * (py - v2[1])) * inv_denom;
            let c = 1.0 - a - b;
            if a < -1e-6 || b < -1e-6 || c < -1e-6 {
                continue;
            }
            target.body_mask[(iy, ix)] = true;
            let z = a * v0[2] + b * v1[2] + c * v2[2];
            if z > target.top_z[(iy, ix)] {
                target.top_z[(iy, ix)] = z;
            }
            if z_contributing && z < target.bottom_z[(iy, ix)] {
                target.bottom_z[(iy, ix)] = z;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Simple centroid / overlap helpers used by the Phase A scorer
// ---------------------------------------------------------------------------

/// Pixel-count-weighted centroid of a boolean mask, returned in world
/// coordinates.
#[allow(dead_code)] // Useful public API.
pub fn mask_centroid(grid: &MaskGrid) -> Option<(f64, f64)> {
    let (h, w) = grid.mask.dim();
    let mut sx = 0.0;
    let mut sy = 0.0;
    let mut n = 0.0;
    for r in 0..h {
        for c in 0..w {
            if grid.mask[(r, c)] {
                sx += (c as f64) + 0.5;
                sy += (r as f64) + 0.5;
                n += 1.0;
            }
        }
    }
    if n <= 0.0 {
        return None;
    }
    Some((
        grid.bounds[0] + (sx / n) * grid.resolution_mm,
        grid.bounds[1] + (sy / n) * grid.resolution_mm,
    ))
}

/// Count overlap between a target grid and a source grid shifted by
/// `(dx_px, dy_px)` (target coord = source coord + shift). Returns the
/// (overlap_pixels, source_total_pixels) pair used by `score_candidate`.
pub fn overlay_counts(
    target: &MaskGrid,
    source: &MaskGrid,
    pixel_shift: (i32, i32),
) -> (usize, usize) {
    let (t_h, t_w) = target.mask.dim();
    let (s_h, s_w) = source.mask.dim();
    let (dx_px, dy_px) = pixel_shift;
    let target_y0 = dy_px.max(0) as usize;
    let target_x0 = dx_px.max(0) as usize;
    let source_y0 = (-dy_px).max(0) as usize;
    let source_x0 = (-dx_px).max(0) as usize;
    let height = (t_h as i32 - target_y0 as i32).min(s_h as i32 - source_y0 as i32);
    let width = (t_w as i32 - target_x0 as i32).min(s_w as i32 - source_x0 as i32);
    let source_total = source.pixel_count();
    if height <= 0 || width <= 0 {
        return (0, source_total);
    }
    let mut overlap = 0usize;
    for r in 0..height as usize {
        for c in 0..width as usize {
            if target.mask[(target_y0 + r, target_x0 + c)]
                && source.mask[(source_y0 + r, source_x0 + c)]
            {
                overlap += 1;
            }
        }
    }
    (overlap, source_total)
}

/// Convert a world-frame translation into the integer pixel shift used by
/// `overlay_counts`. `(dx_px, dy_px)` is rounded to the nearest grid step.
pub fn translation_to_shift(
    target: &MaskGrid,
    source: &MaskGrid,
    translation: (f64, f64),
) -> (i32, i32) {
    let (tx, ty) = translation;
    let dx = (source.bounds[0] + tx - target.bounds[0]) / target.resolution_mm;
    let dy = (source.bounds[1] + ty - target.bounds[1]) / target.resolution_mm;
    (dx.round() as i32, dy.round() as i32)
}

/// Bounds-centroid translation: align the centroids of two axis-aligned
/// bounding boxes. Used as the default coarse translation proposal.
pub fn centroid_translation(target_bounds: [f64; 4], source_bounds: [f64; 4]) -> (f64, f64) {
    let tcx = 0.5 * (target_bounds[0] + target_bounds[2]);
    let tcy = 0.5 * (target_bounds[1] + target_bounds[3]);
    let scx = 0.5 * (source_bounds[0] + source_bounds[2]);
    let scy = 0.5 * (source_bounds[1] + source_bounds[3]);
    (tcx - scx, tcy - scy)
}

// ---------------------------------------------------------------------------
// FFT mask correlation (ports `_mask_correlation` / `solve_translation_masks`)
// ---------------------------------------------------------------------------

/// Smallest fast FFT length >= `n`. Mirrors `scipy.fft.next_fast_len` (5-smooth).
fn next_fast_len(n: usize) -> usize {
    if n <= 1 {
        return n.max(1);
    }
    let mut best = usize::MAX;
    let mut p2 = 1usize;
    while p2 < best {
        let mut p3 = p2;
        while p3 < best {
            let mut p5 = p3;
            while p5 < best {
                if p5 >= n {
                    best = p5;
                    break;
                }
                p5 = p5.saturating_mul(5);
            }
            p3 = p3.saturating_mul(3);
            if p3 == 0 {
                break;
            }
        }
        p2 = p2.saturating_mul(2);
        if p2 == 0 {
            break;
        }
    }
    best
}

fn fft2d_inplace(buf: &mut [Complex<f64>], height: usize, width: usize, inverse: bool) {
    let mut planner = FftPlanner::<f64>::new();
    let fft_w: Arc<dyn Fft<f64>> = if inverse {
        planner.plan_fft_inverse(width)
    } else {
        planner.plan_fft_forward(width)
    };
    let fft_h: Arc<dyn Fft<f64>> = if inverse {
        planner.plan_fft_inverse(height)
    } else {
        planner.plan_fft_forward(height)
    };
    // Row-wise FFTs.
    for r in 0..height {
        let row = &mut buf[r * width..(r + 1) * width];
        fft_w.process(row);
    }
    // Transpose to run column FFTs as contiguous rows.
    let mut tmp = vec![Complex::<f64>::new(0.0, 0.0); width * height];
    for r in 0..height {
        for c in 0..width {
            tmp[c * height + r] = buf[r * width + c];
        }
    }
    for c in 0..width {
        let col = &mut tmp[c * height..(c + 1) * height];
        fft_h.process(col);
    }
    for r in 0..height {
        for c in 0..width {
            buf[r * width + c] = tmp[c * height + r];
        }
    }
}

/// 2D cross-correlation of `target` with `source` via zero-padded FFT, matching
/// the `_mask_correlation` output in `solver.py`. Returned grid has shape
/// `(target_h + source_h - 1, target_w + source_w - 1)`.
pub fn mask_correlation(target: &MaskGrid, source: &MaskGrid) -> Array2<f64> {
    let (t_h, t_w) = target.mask.dim();
    let (s_h, s_w) = source.mask.dim();
    let out_h = t_h + s_h - 1;
    let out_w = t_w + s_w - 1;
    let fft_h = next_fast_len(out_h);
    let fft_w = next_fast_len(out_w);

    let mut t_buf = vec![Complex::<f64>::new(0.0, 0.0); fft_h * fft_w];
    for r in 0..t_h {
        for c in 0..t_w {
            if target.mask[(r, c)] {
                t_buf[r * fft_w + c].re = 1.0;
            }
        }
    }
    // Source reversed to get correlation (i.e., conj in freq domain for real).
    let mut s_buf = vec![Complex::<f64>::new(0.0, 0.0); fft_h * fft_w];
    for r in 0..s_h {
        for c in 0..s_w {
            if source.mask[(r, c)] {
                let rr = s_h - 1 - r;
                let cc = s_w - 1 - c;
                s_buf[rr * fft_w + cc].re = 1.0;
            }
        }
    }

    fft2d_inplace(&mut t_buf, fft_h, fft_w, false);
    fft2d_inplace(&mut s_buf, fft_h, fft_w, false);
    let mut product = vec![Complex::<f64>::new(0.0, 0.0); fft_h * fft_w];
    for i in 0..product.len() {
        product[i] = t_buf[i] * s_buf[i];
    }
    fft2d_inplace(&mut product, fft_h, fft_w, true);
    // Normalize the inverse FFT.
    let norm = 1.0 / (fft_h as f64 * fft_w as f64);
    let mut out = Array2::<f64>::zeros((out_h, out_w));
    for r in 0..out_h {
        for c in 0..out_w {
            out[(r, c)] = product[r * fft_w + c].re * norm;
        }
    }
    out
}

/// Convert a pixel shift in the correlation grid to a world-space translation
/// matching `_translation_from_pixel_shift` in the Python solver.
pub fn translation_from_pixel_shift(
    target: &MaskGrid,
    source: &MaskGrid,
    dx_px: i32,
    dy_px: i32,
) -> (f64, f64) {
    let tx = target.bounds[0] - source.bounds[0] + (dx_px as f64) * target.resolution_mm;
    let ty = target.bounds[1] - source.bounds[1] + (dy_px as f64) * target.resolution_mm;
    (tx, ty)
}

#[derive(Debug, Clone)]
pub struct FftTranslation {
    pub translation: (f64, f64),
    #[allow(dead_code)] // Useful diagnostic field.
    pub shift: (i32, i32),
    pub mask_overlap: f64,
}

/// Single-argmax FFT translation. Mirrors Python's `solve_translation_masks`.
pub fn fft_translation_best(target: &MaskGrid, source: &MaskGrid) -> FftTranslation {
    let corr = mask_correlation(target, source);
    let (h, w) = corr.dim();
    let (s_h, s_w) = source.mask.dim();
    let px_area = target.resolution_mm * target.resolution_mm;
    let mut best_val = f64::NEG_INFINITY;
    let (mut best_r, mut best_c) = (0usize, 0usize);
    for r in 0..h {
        for c in 0..w {
            let v = corr[(r, c)];
            if v > best_val {
                best_val = v;
                best_r = r;
                best_c = c;
            }
        }
    }
    let dx_px = best_c as i32 - (s_w as i32 - 1);
    let dy_px = best_r as i32 - (s_h as i32 - 1);
    FftTranslation {
        translation: translation_from_pixel_shift(target, source, dx_px, dy_px),
        shift: (dx_px, dy_px),
        mask_overlap: best_val.max(0.0) * px_area,
    }
}

/// Port of `solve_translation_masks_candidates`: keep the argmax + a bounded
/// set of strong local maxima in a 3x3 neighborhood.
pub fn fft_translation_candidates(target: &MaskGrid, source: &MaskGrid) -> Vec<FftTranslation> {
    const MAX_PEAKS: usize = 2;
    const MIN_PEAK_RATIO: f64 = 0.60;
    const NEIGHBORHOOD: &[(i32, i32)] = &[(0, 0), (-1, 0), (1, 0)];

    let corr = mask_correlation(target, source);
    let (h, w) = corr.dim();
    let (s_h, s_w) = source.mask.dim();
    let px_area = target.resolution_mm * target.resolution_mm;

    let mut out: Vec<FftTranslation> = Vec::new();
    let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();

    let add = |ix: i32,
               iy: i32,
               out: &mut Vec<FftTranslation>,
               seen: &mut std::collections::HashSet<(i32, i32)>| {
        if ix < 0 || iy < 0 || ix >= w as i32 || iy >= h as i32 {
            return;
        }
        let dx_px = ix - (s_w as i32 - 1);
        let dy_px = iy - (s_h as i32 - 1);
        let key = (dx_px, dy_px);
        if !seen.insert(key) {
            return;
        }
        let t = translation_from_pixel_shift(target, source, dx_px, dy_px);
        let mask_overlap = corr[(iy as usize, ix as usize)] * px_area;
        out.push(FftTranslation {
            translation: t,
            shift: (dx_px, dy_px),
            mask_overlap,
        });
    };

    // Global argmax.
    let mut best_val = f64::NEG_INFINITY;
    let (mut best_r, mut best_c) = (0usize, 0usize);
    for r in 0..h {
        for c in 0..w {
            let v = corr[(r, c)];
            if v > best_val {
                best_val = v;
                best_r = r;
                best_c = c;
            }
        }
    }
    add(best_c as i32, best_r as i32, &mut out, &mut seen);
    if best_val <= 0.0 {
        return out;
    }

    // Local maxima in a 3x3 window at or above `best_val * MIN_PEAK_RATIO`.
    let threshold = best_val * MIN_PEAK_RATIO;
    let mut peaks: Vec<(f64, usize, usize)> = Vec::new();
    for r in 0..h {
        for c in 0..w {
            let v = corr[(r, c)];
            if v < threshold {
                continue;
            }
            let mut is_local_max = true;
            let r0 = r.saturating_sub(1);
            let r1 = (r + 1).min(h - 1);
            let c0 = c.saturating_sub(1);
            let c1 = (c + 1).min(w - 1);
            'outer: for rr in r0..=r1 {
                for cc in c0..=c1 {
                    if rr == r && cc == c {
                        continue;
                    }
                    if corr[(rr, cc)] > v {
                        is_local_max = false;
                        break 'outer;
                    }
                }
            }
            if is_local_max {
                peaks.push((v, r, c));
            }
        }
    }
    peaks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected = 0usize;
    for (_v, pr, pc) in peaks {
        let offsets: &[(i32, i32)] = if selected == 0 {
            &[(0, 0)]
        } else {
            NEIGHBORHOOD
        };
        for (dx, dy) in offsets {
            add(pc as i32 + dx, pr as i32 + dy, &mut out, &mut seen);
        }
        selected += 1;
        if selected >= MAX_PEAKS {
            break;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Connected components (4/8-connected; matches ndimage.label with a 3x3 ones
// structuring element used by the Python solver).
// ---------------------------------------------------------------------------

/// Label pixels into connected components using 8-connectivity. Returns
/// `(labels, count)` where background pixels are 0 and foreground labels run
/// from 1..=count. Mirrors `ndimage.label(mask, structure=ones((3,3)))`.
pub fn label_components(mask: &Array2<bool>) -> (Array2<u32>, u32) {
    let (h, w) = mask.dim();
    let mut labels = Array2::<u32>::zeros((h, w));
    let mut parent: Vec<u32> = vec![0];
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            let p = parent[x as usize];
            parent[x as usize] = parent[p as usize];
            x = parent[x as usize];
        }
        x
    }
    fn union(parent: &mut [u32], a: u32, b: u32) -> u32 {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra == rb {
            return ra;
        }
        let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
        parent[hi as usize] = lo;
        lo
    }
    let mut next_label: u32 = 1;
    for r in 0..h {
        for c in 0..w {
            if !mask[(r, c)] {
                continue;
            }
            let mut nbs: [u32; 4] = [0; 4];
            let mut n = 0;
            // 8-connectivity: up-left, up, up-right, left.
            if r > 0 {
                if c > 0 && labels[(r - 1, c - 1)] != 0 {
                    nbs[n] = labels[(r - 1, c - 1)];
                    n += 1;
                }
                if labels[(r - 1, c)] != 0 {
                    nbs[n] = labels[(r - 1, c)];
                    n += 1;
                }
                if c + 1 < w && labels[(r - 1, c + 1)] != 0 {
                    nbs[n] = labels[(r - 1, c + 1)];
                    n += 1;
                }
            }
            if c > 0 && labels[(r, c - 1)] != 0 {
                nbs[n] = labels[(r, c - 1)];
                n += 1;
            }
            if n == 0 {
                labels[(r, c)] = next_label;
                parent.push(next_label);
                next_label += 1;
            } else {
                let mut m = nbs[0];
                for &nb in &nbs[1..n] {
                    m = union(&mut parent, m, nb);
                }
                labels[(r, c)] = m;
            }
        }
    }
    // Second pass: compact labels.
    let mut remap: Vec<u32> = vec![0; parent.len()];
    let mut count: u32 = 0;
    for r in 0..h {
        for c in 0..w {
            let l = labels[(r, c)];
            if l == 0 {
                continue;
            }
            let root = find(&mut parent, l);
            let mut rm = remap[root as usize];
            if rm == 0 {
                count += 1;
                rm = count;
                remap[root as usize] = rm;
            }
            labels[(r, c)] = rm;
        }
    }
    (labels, count)
}

/// Per-component pixel counts for a labelled grid (labels 1..=count).
pub fn component_pixel_counts(labels: &Array2<u32>, count: u32) -> Vec<u32> {
    let mut counts = vec![0u32; count as usize];
    for &l in labels.iter() {
        if l == 0 {
            continue;
        }
        counts[(l - 1) as usize] += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_label(poly: &Polygon, bounds: [f64; 4], resolution_mm: f64) -> Array2<u16> {
        let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
        let mut labels = Array2::<u16>::zeros((height, width));
        let eps = resolution_mm * POLY_EPS_FRAC;
        let [pb_min_x, pb_min_y, pb_max_x, pb_max_y] = poly.bounds;
        let col0 =
            (((pb_min_x - eps - bounds[0]) / resolution_mm).floor() as isize).max(0) as usize;
        let col1 = (((pb_max_x + eps - bounds[0]) / resolution_mm).ceil() as isize)
            .min(width as isize)
            .max(0) as usize;
        let row0 =
            (((pb_min_y - eps - bounds[1]) / resolution_mm).floor() as isize).max(0) as usize;
        let row1 = (((pb_max_y + eps - bounds[1]) / resolution_mm).ceil() as isize)
            .min(height as isize)
            .max(0) as usize;
        for r in row0..row1 {
            let y = bounds[1] + ((r as f64) + 0.5) * resolution_mm;
            for c in col0..col1 {
                let x = bounds[0] + ((c as f64) + 0.5) * resolution_mm;
                if point_inside_polygon(poly, x, y, eps) {
                    labels[(r, c)] = 1;
                }
            }
        }
        labels
    }

    #[test]
    fn scanline_pad_label_matches_point_in_polygon_for_pad_shapes() {
        let pads = [
            PadShape {
                kind: PadKind::Rect,
                at: [0.13, -0.07],
                size: [1.7, 0.9],
                angle_deg: 23.0,
            },
            PadShape {
                kind: PadKind::Circle,
                at: [0.11, 0.19],
                size: [1.3, 1.3],
                angle_deg: 0.0,
            },
            PadShape {
                kind: PadKind::Oval,
                at: [-0.21, 0.17],
                size: [2.1, 0.8],
                angle_deg: -31.0,
            },
        ];
        let resolution_mm = 0.17;
        for pad in pads {
            let poly = pad_to_polygon(&pad);
            let bounds = poly.bounds;
            let (width, height) = bounds_to_grid_size(bounds, resolution_mm);
            let mut labels = Array2::<u16>::zeros((height, width));
            let mut area = 0;
            rasterize_polygon_label_into(
                &poly,
                bounds,
                resolution_mm,
                resolution_mm * POLY_EPS_FRAC,
                &mut labels,
                1,
                &mut area,
            );
            let brute = brute_label(&poly, bounds, resolution_mm);
            assert_eq!(labels, brute);
            assert_eq!(
                area,
                brute.iter().filter(|&&label| label == 1).count() as u32
            );
        }
    }

    #[test]
    fn pad_labels_keep_lowest_pad_id_for_overlaps() {
        let pads = vec![
            PadShape {
                kind: PadKind::Rect,
                at: [0.0, 0.0],
                size: [1.0, 1.0],
                angle_deg: 0.0,
            },
            PadShape {
                kind: PadKind::Rect,
                at: [0.0, 0.0],
                size: [1.0, 1.0],
                angle_deg: 0.0,
            },
        ];
        let labels = rasterize_pad_labels(&pads, 0.25).expect("labels");
        assert!(labels.labels.iter().any(|&label| label == 1));
        assert!(!labels.labels.iter().any(|&label| label == 2));
        assert_eq!(labels.areas_px[1], 0);
    }
}
