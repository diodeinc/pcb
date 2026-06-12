use crate::footprint::{FootprintData, FootprintKind, PadShape, Segment};
use crate::raster::{self, HoleLabelGrid, MaskGrid, PadLabelGrid};

use super::EPS;
use super::translation::{SparseAnchor, build_pad_sparse_anchors, polygon_centroid};

const THT_DRILL_CONTACT_MARGIN_FRAC: f64 = 0.50;

pub(crate) struct FootprintCtx {
    pub(crate) pad_grid: MaskGrid,
    pub(crate) pad_label_grid: PadLabelGrid,
    pub(crate) pad_count: usize,
    pub(crate) pad_pixel_count: usize,
    pub(crate) hole_grid: Option<MaskGrid>,
    pub(crate) hole_label_grid: Option<HoleLabelGrid>,
    pub(crate) connected_hole_grid: Option<MaskGrid>,
    pub(crate) connected_hole_label_grid: Option<HoleLabelGrid>,
    pub(crate) connected_hole_contact_grid: Option<MaskGrid>,
    pub(crate) physical_drill_grid: Option<MaskGrid>,
    pub(crate) physical_drill_contact_grid: Option<MaskGrid>,
    pub(crate) mechanical_drill_label_grid: Option<HoleLabelGrid>,
    pub(crate) has_holes: bool,
    pub(crate) footprint_kind: FootprintKind,
    /// Axis-aligned bbox of the pad union: `[min_x, min_y, max_x, max_y]`.
    pub(crate) pad_bounds: [f64; 4],
    /// Footprint alignment bounds (courtyard or fab if plausible, else pad).
    /// Mirrors `footprint_alignment_bounds` in the Python solver.
    pub(crate) alignment_bounds: [f64; 4],
    /// Per-pad axis-aligned bbox. Indexed matching `fp.pads`.
    pub(crate) pad_shape_bounds: Vec<[f64; 4]>,
    /// Per-pad geometric centroid in world coords, from the polygon geometry
    /// (not the raster). Used for sub-pixel translation refinement.
    pub(crate) pad_centroids: Vec<(f64, f64)>,
    /// Per-hole geometric centroid in world coords, for THT sub-pixel
    /// refinement. Empty when the footprint has no holes.
    pub(crate) hole_centroids: Vec<(f64, f64)>,
    /// Per connected through-hole drill centroid in world coords. Used by the
    /// THT-only pin-island pipeline so mechanical holes do not steer scoring.
    pub(crate) connected_hole_centroids: Vec<(f64, f64)>,
    /// Sparse anchors built from pad centroids, used for combinatorial
    /// translation search (mirrors Python's `build_pad_sparse_anchors`).
    pub(crate) pad_anchors: Vec<SparseAnchor>,
}

pub(crate) fn build_context(fp: &FootprintData, resolution_mm: f64) -> Option<FootprintCtx> {
    let pad_grid = raster::rasterize_pad_union(&fp.pads, resolution_mm)?;
    let pad_label_grid = raster::rasterize_pad_labels(&fp.pads, resolution_mm)?;
    let pad_count = fp.pads.len();
    let pad_pixel_count = pad_grid.pixel_count();
    let hole_grid = if fp.has_holes() {
        raster::rasterize_pad_union(&fp.holes, resolution_mm)
    } else {
        None
    };
    let hole_label_grid = if fp.has_holes() {
        raster::rasterize_hole_labels(&fp.holes, resolution_mm)
    } else {
        None
    };
    let connected_hole_grid = if !fp.connected_holes.is_empty() {
        raster::rasterize_pad_union(&fp.connected_holes, resolution_mm)
    } else {
        None
    };
    let connected_hole_label_grid = if !fp.connected_holes.is_empty() {
        raster::rasterize_hole_labels(&fp.connected_holes, resolution_mm)
    } else {
        None
    };
    let connected_hole_contact_grid = if !fp.connected_holes.is_empty() {
        let expanded_holes =
            scale_pad_shapes(&fp.connected_holes, 1.0 + THT_DRILL_CONTACT_MARGIN_FRAC);
        raster::rasterize_pad_union(&expanded_holes, resolution_mm)
    } else {
        None
    };
    let physical_drill_grid = if !fp.physical_drills.is_empty() {
        raster::rasterize_pad_union(&fp.physical_drills, resolution_mm)
    } else {
        None
    };
    let physical_drill_contact_grid = if !fp.physical_drills.is_empty() {
        let expanded_drills =
            scale_pad_shapes(&fp.physical_drills, 1.0 + THT_DRILL_CONTACT_MARGIN_FRAC);
        raster::rasterize_pad_union(&expanded_drills, resolution_mm)
    } else {
        None
    };
    let mechanical_drill_label_grid = if !fp.mechanical_drills.is_empty() {
        raster::rasterize_hole_labels(&fp.mechanical_drills, resolution_mm)
    } else {
        None
    };
    let pad_bounds = pad_grid.bounds;
    let alignment_bounds = footprint_alignment_bounds(fp, pad_bounds);
    let pad_shape_bounds: Vec<[f64; 4]> = fp
        .pads
        .iter()
        .map(|p| raster::pad_to_polygon(p).bounds)
        .collect();
    let pad_centroids: Vec<(f64, f64)> = fp
        .pads
        .iter()
        .map(|p| polygon_centroid(&raster::pad_to_polygon(p)))
        .collect();
    let hole_centroids: Vec<(f64, f64)> = if fp.has_holes() {
        fp.holes
            .iter()
            .map(|p| polygon_centroid(&raster::pad_to_polygon(p)))
            .collect()
    } else {
        Vec::new()
    };
    let connected_hole_centroids: Vec<(f64, f64)> = fp
        .connected_holes
        .iter()
        .map(|p| polygon_centroid(&raster::pad_to_polygon(p)))
        .collect();
    let pad_anchors = build_pad_sparse_anchors(fp);
    Some(FootprintCtx {
        pad_grid,
        pad_label_grid,
        pad_count,
        pad_pixel_count,
        hole_grid,
        hole_label_grid,
        connected_hole_grid,
        connected_hole_label_grid,
        connected_hole_contact_grid,
        physical_drill_grid,
        physical_drill_contact_grid,
        mechanical_drill_label_grid,
        has_holes: fp.has_holes(),
        footprint_kind: fp.footprint_kind(),
        pad_bounds,
        alignment_bounds,
        pad_shape_bounds,
        pad_centroids,
        hole_centroids,
        connected_hole_centroids,
        pad_anchors,
    })
}

fn scale_pad_shapes(pads: &[PadShape], scale: f64) -> Vec<PadShape> {
    pads.iter()
        .cloned()
        .map(|mut pad| {
            pad.size = [pad.size[0] * scale, pad.size[1] * scale];
            pad
        })
        .collect()
}

/// Segment-endpoint bbox. Returns None if no segments are present.
fn segments_bbox(segs: &[Segment]) -> Option<[f64; 4]> {
    if segs.is_empty() {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in segs {
        for p in [s.a, s.b] {
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
    if !min_x.is_finite() || !min_y.is_finite() {
        return None;
    }
    Some([min_x, min_y, max_x, max_y])
}

/// Port of `footprint_alignment_bounds` from `solver.py`. Prefer the courtyard
/// bbox when plausible (width/height ≥ 35% of pad bbox), then fab, then silk,
/// else fall back to the pad bbox.
pub(crate) fn footprint_alignment_bounds(fp: &FootprintData, pad_bounds: [f64; 4]) -> [f64; 4] {
    let pad_w = (pad_bounds[2] - pad_bounds[0]).max(EPS);
    let pad_h = (pad_bounds[3] - pad_bounds[1]).max(EPS);
    let is_plausible = |b: [f64; 4]| -> bool {
        let w = (b[2] - b[0]).max(EPS);
        let h = (b[3] - b[1]).max(EPS);
        w >= 0.35 * pad_w && h >= 0.35 * pad_h
    };
    if let Some(segs) = fp.courtyard.as_ref()
        && let Some(b) = segments_bbox(segs)
        && is_plausible(b)
    {
        return b;
    }
    if let Some(segs) = fp.fab.as_ref()
        && let Some(b) = segments_bbox(segs)
        && is_plausible(b)
    {
        return b;
    }
    if let Some(segs) = fp.silk.as_ref()
        && let Some(b) = segments_bbox(segs)
        && is_plausible(b)
    {
        return b;
    }
    pad_bounds
}
