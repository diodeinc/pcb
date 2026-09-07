//! Prepare regularized regions from polygons, contours, and painted paths.

use super::flattening::flatten_contours;
use super::{ContourSet, PaintComposer, Ring, ring_signed_area, rings_bbox, simplify_rings};
use crate::geom::accuracy::numerical_error;
use crate::geom::path::{ContourBuf, Segment, stroke_to_fill};
use crate::geom::store::{Path, PathArena};
use crate::geom::{
    AccuracyError, Affine2, BBox, FillRule, GeometryAccuracy, Paint, Polarity, Resolution,
};

/// A closed polygon boundary, flattened to line segments.
/// Chords past which a single curve is being flattened to an absurd budget.
const MAX_CHORDS_PER_SEGMENT: f64 = 1.0e6;

impl ContourSet {
    /// Regularize source polygons. Their vertices are taken as exact; only
    /// coordinate rounding is charged, and checked like any other cost.
    pub fn from_rings(
        rings: Vec<Ring>,
        fill_rule: FillRule,
        resolution: Resolution,
    ) -> Result<Self, AccuracyError> {
        let uncertainty = numerical_error(rings_bbox(&rings));
        Self::from_regularized(simplify_rings(rings, fill_rule), resolution, uncertainty).checked()
    }

    /// Construct from known regularized polygons, preserving their history.
    /// `uncertainty_mm = 0` asserts these polygons are the actual source.
    /// Rings below the resolution's significance are dropped; significance
    /// is independent of the approximation budget and is not charged.
    pub fn from_regularized(
        mut rings: Vec<Ring>,
        resolution: Resolution,
        uncertainty_mm: f64,
    ) -> Self {
        let uncertainty_mm = if uncertainty_mm >= 0.0 {
            uncertainty_mm
        } else {
            f64::INFINITY
        };
        let min_area = resolution.tolerance_mm.powi(2);
        rings.retain(|ring| ring_signed_area(ring).abs() > min_area);
        let ring_bounds = rings
            .iter()
            .map(|ring| rings_bbox(std::slice::from_ref(ring)))
            .collect::<Vec<_>>();
        Self {
            bbox: ring_bounds.iter().copied().fold(BBox::empty(), BBox::union),
            rings,
            ring_bounds,
            resolution,
            uncertainty_mm,
        }
    }

    pub fn empty(resolution: Resolution) -> Self {
        Self {
            bbox: BBox::empty(),
            rings: Vec::new(),
            ring_bounds: Vec::new(),
            resolution,
            uncertainty_mm: 0.0,
        }
    }

    /// Prepare source contours under one fill rule. Curves are flattened
    /// within the resolution's budget; the result records what that cost on
    /// top of any approximation the contours already carried.
    pub fn from_contours(
        contours: &[ContourBuf],
        fill_rule: FillRule,
        resolution: Resolution,
    ) -> Result<Self, AccuracyError> {
        let accuracy = resolution.accuracy;
        if !resolution.is_valid()
            || contours.iter().any(|c| {
                !c.bbox.is_valid()
                    || !c.uncertainty_mm.is_finite()
                    || c.uncertainty_mm < 0.0
                    || !c.cmds.iter().all(|cmd| cmd.is_finite())
            })
        {
            return Err(AccuracyError::InvalidGeometry(
                "invalid coordinates or significance tolerance",
            ));
        }
        let prior = contours
            .iter()
            .map(|c| c.uncertainty_mm)
            .fold(0.0, f64::max);
        let bbox = contours.iter().fold(BBox::empty(), |b, c| b.union(c.bbox));
        let numeric = numerical_error(bbox);
        let remaining = accuracy.allowance(prior + numeric)?;
        // Guard against absurd budgets segment by segment, never against
        // input size: a flattened panel legitimately needs millions of
        // vertices, while one curve needing a million chords is a budget
        // no target can use.
        let absurd = |segment: Segment| {
            let bounds = segment.bbox();
            (bounds.width().max(bounds.height()) / remaining).sqrt() > MAX_CHORDS_PER_SEGMENT
        };
        if contours
            .iter()
            .flat_map(ContourBuf::segments)
            .filter(|segment| !matches!(segment, Segment::Line { .. }))
            .any(absurd)
        {
            return Err(AccuracyError::SubdivisionLimit);
        }
        let (rings, added) = flatten_contours(contours, remaining);
        Self::from_regularized(
            simplify_rings(rings, fill_rule),
            resolution,
            prior + added + numeric,
        )
        .checked()
    }

    /// Build the union of independently filled contours.
    ///
    /// Each contour is filled on its own (even-odd, so nesting makes holes and
    /// winding direction is irrelevant), then the contours are unioned. Use
    /// this when sibling contours are separate features; applying even-odd
    /// across the whole list would XOR duplicated geometry away.
    pub fn from_filled_contours(
        contours: &[ContourBuf],
        resolution: Resolution,
    ) -> Result<Self, AccuracyError> {
        let mut composer = PaintComposer::new(resolution);
        for contour in contours {
            composer.push(
                Polarity::Dark,
                Self::from_contours(
                    std::slice::from_ref(contour),
                    FillRule::EvenOdd,
                    resolution.strict(),
                )?,
            );
        }
        composer.finish()
    }

    /// Build the union of the geometric images painted by a set of paths.
    ///
    /// Filled paths are interpreted under their own fill rule and stroked
    /// paths are expanded with their native width, cap, and join. Unpainted
    /// paths are ignored. Object/feature polarity is deliberately outside
    /// this operation: this constructs geometric footprints, not a composed
    /// positive/negative layer image.
    pub fn from_painted_paths<'a>(
        arena: &PathArena,
        paths: impl IntoIterator<Item = &'a Path>,
        resolution: Resolution,
    ) -> Result<Self, AccuracyError> {
        Self::from_placed_painted_paths(
            arena,
            paths.into_iter().map(|path| (path, Affine2::IDENTITY)),
            resolution,
        )
    }

    /// Build the union of painted path occurrences after applying placement.
    ///
    /// Stroke outlines are constructed in the path's local frame before the
    /// affine transform is applied, so mirrored and scaled IPC placements
    /// retain the same geometric meaning as a materialized feature.
    pub fn from_placed_painted_paths<'a>(
        arena: &PathArena,
        paths: impl IntoIterator<Item = (&'a Path, Affine2)>,
        resolution: Resolution,
    ) -> Result<Self, AccuracyError> {
        if !resolution.is_valid() {
            return Err(AccuracyError::InvalidGeometry(
                "invalid significance tolerance",
            ));
        }
        let accuracy = resolution.accuracy;
        let mut composer = PaintComposer::new(resolution);
        for (path, placement) in paths {
            let contours = arena.path_contours(path);
            let contours = match path.paint {
                Paint::Fill { .. } => contours,
                Paint::Stroke(stroke) => {
                    let local =
                        GeometryAccuracy::new(accuracy.max_error_mm() / placement.max_scale())?;
                    stroke_to_fill(&contours, stroke.into(), local)?.unwrap_or_default()
                }
                Paint::None => continue,
            };
            let contours = contours
                .into_iter()
                .map(|contour| contour.transformed(placement))
                .collect::<Vec<_>>();
            let fill_rule = path.fill_rule().unwrap_or(FillRule::NonZero);
            composer.push(
                Polarity::Dark,
                Self::from_contours(&contours, fill_rule, resolution.strict())?,
            );
        }
        composer.finish()
    }

    pub fn rectangle(bbox: BBox, resolution: Resolution) -> Self {
        if bbox.is_empty() {
            return Self::empty(resolution);
        }
        let ring = vec![
            [bbox.min.x, bbox.min.y],
            [bbox.max.x, bbox.min.y],
            [bbox.max.x, bbox.max.y],
            [bbox.min.x, bbox.max.y],
        ];
        Self::from_regularized(vec![ring], resolution, 0.0)
    }
}
