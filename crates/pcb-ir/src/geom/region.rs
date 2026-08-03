//! Regularized planar regions and boolean composition.
//!
//! The flattened polygon form used for boolean set operations is a list of
//! [`Ring`]s (closed polygon boundaries). [`ContourSet`] is the regularized
//! region type built on top: union, difference, intersection, and disk
//! dilation over filled point sets, shared by every dialect so IPC, Gerber,
//! SVG, and comparison all use the same geometry semantics.

use std::fmt;

use boostvoronoi::prelude::{
    Builder as VoronoiBuilder, CellIndex as VoronoiCellIndex, Diagram as VoronoiDiagram,
    EdgeIndex as VoronoiEdgeIndex, Line as VoronoiLine, Point as VoronoiPoint, SourceCategory,
    VoronoiVisualUtils,
};
use boostvoronoi::utils::visual_utils::SimpleAffine;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use i_overlay::mesh::outline::offset::OutlineOffset;
use i_overlay::mesh::style::{LineJoin as OutlineLineJoin, OutlineStyle};

use crate::geom::bbox::BBox;
use crate::geom::path::{ContourBuf, PathCmd, contours_to_kurbo, stroke_to_fill};
use crate::geom::point::Point;
use crate::geom::store::{Path, PathArena};
use crate::geom::style::{FillRule, Paint, Polarity};
use crate::geom::tol;

/// A closed polygon boundary, flattened to line segments.
pub type Ring = Vec<[f64; 2]>;

/// One connected polygon: an outer ring plus hole rings.
pub type Shape = Vec<Ring>;

/// Flatten contours to polygon rings using the shared chord tolerance.
pub fn rings_from_contours(contours: &[ContourBuf]) -> Vec<Ring> {
    let bez_path = contours_to_kurbo(contours);
    let mut rings = Vec::new();
    let mut current = Vec::new();
    kurbo::flatten(bez_path, tol::FLATTEN_MM, |element| match element {
        kurbo::PathEl::MoveTo(point) => {
            push_ring(&mut rings, &mut current);
            current.push([point.x, point.y]);
        }
        kurbo::PathEl::LineTo(point) => current.push([point.x, point.y]),
        kurbo::PathEl::ClosePath => push_ring(&mut rings, &mut current),
        kurbo::PathEl::QuadTo(..) | kurbo::PathEl::CurveTo(..) => {
            unreachable!("kurbo::flatten emits lines")
        }
    });
    push_ring(&mut rings, &mut current);
    rings
}

/// Convert polygon rings back into closed line contours.
pub fn rings_to_contours(rings: Vec<Ring>) -> Vec<ContourBuf> {
    rings.into_iter().filter_map(ring_to_contour).collect()
}

/// Regularize rings under the given fill rule into non-overlapping shapes.
pub fn simplify_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    flatten_shapes(simplify_shapes(rings, fill_rule))
}

/// Regularize rings keeping the connected-shape structure: each shape is its
/// outer ring followed by its holes, wound opposite.
pub fn simplify_shapes(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Shape> {
    rings.simplify_shape(overlay_fill_rule(fill_rule))
}

pub fn union_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    simplify_rings(rings, fill_rule)
}

pub fn difference_rings(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Ring> {
    flatten_shapes(difference_shapes(subject, cutters))
}

pub fn intersection_rings(subject: Vec<Ring>, clip: Vec<Ring>) -> Vec<Ring> {
    if subject.is_empty() || clip.is_empty() {
        return Vec::new();
    }
    flatten_shapes(subject.overlay(&clip, OverlayRule::Intersect, OverlayFillRule::NonZero))
}

/// Difference keeping the connected-shape structure of the result.
pub fn difference_shapes(subject: Vec<Ring>, cutters: Vec<Ring>) -> Vec<Shape> {
    if subject.is_empty() || cutters.is_empty() {
        return subject.simplify_shape(OverlayFillRule::NonZero);
    }
    subject.overlay(&cutters, OverlayRule::Difference, OverlayFillRule::NonZero)
}

pub fn rings_bbox(rings: &[Ring]) -> BBox {
    rings
        .iter()
        .flat_map(|ring| ring.iter())
        .fold(BBox::empty(), |mut bbox, &[x, y]| {
            bbox.include_point(Point::new(x, y));
            bbox
        })
}

/// Signed area of one ring (positive when counter-clockwise).
pub fn ring_signed_area(ring: &Ring) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for index in 0..ring.len() {
        let [x0, y0] = ring[index];
        let [x1, y1] = ring[(index + 1) % ring.len()];
        area += x0 * y1 - x1 * y0;
    }
    area / 2.0
}

/// Net enclosed area of a regularized ring set (holes are wound opposite the
/// outer boundary, so summing signed areas subtracts them).
pub fn rings_area(rings: &[Ring]) -> f64 {
    rings.iter().map(ring_signed_area).sum::<f64>().abs()
}

/// Regularized filled planar point set.
///
/// A `ContourSet` is always in canonical form: rings are regularized
/// (non-overlapping, holes wound opposite their outer boundary) and contours
/// smaller than `tolerance²` in area are discarded. The winding/fill rule of
/// the *source* geometry matters only at construction; every subsequent
/// operation is a regularized set operation.
#[derive(Debug, Clone)]
pub struct ContourSet {
    pub bbox: BBox,
    pub rings: Vec<Ring>,
    pub tolerance: f64,
}

/// Result of enforcing a minimum width for every two-sided void gap.
#[derive(Debug, Clone)]
pub struct DiskGapRegularization {
    /// Input material retained after local gap trimming and disk opening.
    pub kept: ContourSet,
    /// Two-sided components of `close(source, disk(radius)) \ source`.
    pub narrow_voids: ContourSet,
    /// Initial medial-axis tube plus any directly swept certificate residuals.
    pub separator_keep_out: ContourSet,
    /// `source \ kept`.
    pub removed: ContourSet,
}

/// Failure to construct a narrow void's medial axis for gap regularization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapRegularizationError(String);

impl fmt::Display for GapRegularizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GapRegularizationError {}

impl ContourSet {
    pub fn new(rings: Vec<Ring>, fill_rule: FillRule, tolerance: f64) -> Self {
        let rings = filter_significant_rings(simplify_rings(rings, fill_rule), tolerance);
        Self {
            bbox: rings_bbox(&rings),
            rings,
            tolerance,
        }
    }

    pub fn empty(tolerance: f64) -> Self {
        Self {
            bbox: BBox::empty(),
            rings: Vec::new(),
            tolerance,
        }
    }

    pub fn from_contours(contours: &[ContourBuf], fill_rule: FillRule, tolerance: f64) -> Self {
        Self::new(rings_from_contours(contours), fill_rule, tolerance)
    }

    /// Build the union of independently filled contours.
    ///
    /// Each contour is filled on its own (even-odd, so nesting makes holes and
    /// winding direction is irrelevant), then the contours are unioned. Use
    /// this when sibling contours are separate features; applying even-odd
    /// across the whole list would XOR duplicated geometry away.
    pub fn from_filled_contours(contours: &[ContourBuf], tolerance: f64) -> Self {
        let rings = contours
            .iter()
            .flat_map(|contour| {
                simplify_rings(
                    rings_from_contours(std::slice::from_ref(contour)),
                    FillRule::EvenOdd,
                )
            })
            .collect();
        Self::new(rings, FillRule::NonZero, tolerance)
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
        tolerance: f64,
    ) -> Self {
        let mut rings = Vec::new();
        for path in paths {
            let contours = arena.path_contours(path);
            let path_rings = match path.paint {
                Paint::Fill { rule } => simplify_rings(rings_from_contours(&contours), rule),
                Paint::Stroke(stroke) => stroke_to_fill(&contours, stroke.into())
                    .map(|outline| simplify_rings(rings_from_contours(&outline), FillRule::NonZero))
                    .unwrap_or_default(),
                Paint::None => Vec::new(),
            };
            rings.extend(path_rings);
        }
        Self::new(rings, FillRule::NonZero, tolerance)
    }

    pub fn rectangle(bbox: BBox, tolerance: f64) -> Self {
        if bbox.is_empty() {
            return Self::empty(tolerance);
        }
        let ring = vec![
            [bbox.min.x, bbox.min.y],
            [bbox.max.x, bbox.min.y],
            [bbox.max.x, bbox.max.y],
            [bbox.min.x, bbox.max.y],
        ];
        Self::new(vec![ring], FillRule::NonZero, tolerance)
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// Net enclosed area.
    pub fn area(&self) -> f64 {
        rings_area(&self.rings)
    }

    /// Regularized union: `self ∪ other`.
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        Self::new(
            flatten_shapes(self.rings.overlay(
                &other.rings,
                OverlayRule::Union,
                OverlayFillRule::NonZero,
            )),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    pub fn union_assign(&mut self, other: &Self) {
        *self = self.union(other);
    }

    /// Regularized difference: `self \ cutters`.
    pub fn difference(&self, cutters: &Self) -> Self {
        Self::new(
            difference_rings(self.rings.clone(), cutters.rings.clone()),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    /// Regularized intersection: `self ∩ clip`.
    pub fn intersection(&self, clip: &Self) -> Self {
        Self::new(
            intersection_rings(self.rings.clone(), clip.rings.clone()),
            FillRule::NonZero,
            self.tolerance,
        )
    }

    /// Connected components, each retaining its own hole rings.
    pub fn connected_components(&self) -> Vec<Self> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(|shape| Self::new(shape, FillRule::NonZero, self.tolerance))
            .collect()
    }

    /// Whether the regularized region contains the point, including its boundary.
    pub fn contains_point(&self, point: Point) -> bool {
        if self.is_empty()
            || point.x < self.bbox.min.x
            || point.x > self.bbox.max.x
            || point.y < self.bbox.min.y
            || point.y > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance.max(tol::EPSILON_MM);
        if self
            .rings
            .iter()
            .any(|ring| ring_boundary_distance(ring, point) <= epsilon)
        {
            return true;
        }

        // Regularized boolean output nests pairwise-disjoint rings, so
        // even-odd parity across rings coincides with the nonzero fill rule.
        self.rings.iter().fold(false, |inside, ring| {
            inside ^ ring_contains_point(ring, point)
        })
    }

    /// Whether a closed disk is fully contained in the regularized region.
    ///
    /// The boundary check is exact for the flattened representation used by
    /// `ContourSet` boolean operations.
    pub fn contains_disk(&self, center: Point, radius: f64) -> bool {
        if !radius.is_finite() || radius < 0.0 || !center.is_finite() {
            return false;
        }
        if !self.contains_point(center) {
            return false;
        }
        if radius == 0.0 {
            return true;
        }
        if center.x - radius < self.bbox.min.x
            || center.x + radius > self.bbox.max.x
            || center.y - radius < self.bbox.min.y
            || center.y + radius > self.bbox.max.y
        {
            return false;
        }

        let epsilon = self.tolerance.max(tol::EPSILON_MM);
        self.rings
            .iter()
            .all(|ring| ring_boundary_distance(ring, center) + epsilon >= radius)
    }

    /// Minkowski sum with a disk: `self ⊕ D_radius`. This is the standard
    /// "buffer out" operation used for manufacturability checks. The
    /// topology-aware offset expands outer contours, contracts holes, and
    /// regularizes all resulting contours together.
    pub fn disk_dilate(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        self.disk_offset(radius)
    }

    /// Minkowski erosion by a disk: `self ⊖ D_radius`.
    ///
    /// This removes the radius-wide interior band swept by every boundary
    /// ring. It therefore contracts outer boundaries, expands holes, removes
    /// necks narrower than twice the radius, and may split or erase connected
    /// components.
    pub fn disk_erode(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        self.disk_offset(-radius)
    }

    /// Morphological opening by a disk: `(self ⊖ D_radius) ⊕ D_radius`.
    ///
    /// Equivalently, this is the union of every radius-sized disk contained in
    /// the source region. It is therefore a subset of the source that removes
    /// tips and islands too small to accommodate the disk while rounding the
    /// surviving outward corners.
    pub fn disk_open(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        // Round-offset tessellation approximates the mathematical disks. Clip
        // the regrown result to the source so the operation retains its
        // defining anti-extensive property at polygon tolerance.
        self.disk_erode(radius)
            .disk_dilate(radius)
            .intersection(self)
    }

    /// Morphological closing by a disk: `(self ⊕ D_radius) ⊖ D_radius`.
    ///
    /// Equivalently, the complement of the result is the union of every
    /// radius-sized disk contained in the source complement. Closing therefore
    /// fills void tips and gaps too small to accommodate the disk while
    /// rounding the surviving inward corners.
    pub fn disk_close(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        // Round-offset tessellation approximates the mathematical disks. Union
        // the contracted result with the source so the operation retains its
        // defining extensive property at polygon tolerance.
        self.disk_dilate(radius).disk_erode(radius).union(self)
    }

    /// Diagnose distinct components whose Euclidean separation is less than
    /// the disk diameter `2 * radius`.
    ///
    /// For connected components `C_i`, returns
    /// `⋃_{i<j} (((C_i ⊕ D_r) ∩ (C_j ⊕ D_r)) \ self)` for pairs with
    /// `distance(C_i, C_j) < 2r`. Subtracting `self` localizes the diagnostic
    /// to the intervening void. Bounding-box pruning and segment distance avoid
    /// constructing dilations for non-conflicting pairs.
    ///
    /// Verification-only diagnostic: production gap analysis goes through
    /// [`ContourSet::disk_gap_violations`], which also covers gaps within one
    /// connected component.
    #[cfg(test)]
    pub(crate) fn disk_inter_component_gap_violations(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return Self::empty(self.tolerance);
        }

        let components = self.connected_components();
        let mut violations = Self::empty(self.tolerance);
        for (index, left) in components.iter().enumerate() {
            for right in &components[index + 1..] {
                if !regions_within_distance(left, right, 2.0 * radius) {
                    continue;
                }
                let overlap = left
                    .disk_dilate(radius)
                    .intersection(&right.disk_dilate(radius))
                    .difference(self);
                violations = violations.union(&overlap);
            }
        }
        violations
    }

    /// Enforce a diameter-`2 * gap_radius` minimum for every two-sided void gap.
    ///
    /// Every pass isolates the narrow complement phase at the nominal radius,
    /// removes a guard-widened tube around the boundary medial axis inside
    /// that phase, and reopens with the filled-region disk:
    ///
    /// ```text
    /// S₀     = self
    /// Nₖ     = G_gap_radius(Sₖ)
    /// Sₖ₊₁   = open(Sₖ \ (Γ_Nₖ ⊕ disk(gap_radius + guard)), disk(filled_radius)) ∩ Sₖ
    /// ```
    ///
    /// `G_r(X)` is the two-sided part of `close(X, disk(r)) \ X`, and `Γ_N` is
    /// the boundary medial axis inside the narrow phase — the least local cut,
    /// trimming both sides of every narrow gap without widening one-sided edge
    /// clearance. The guard keeps every checked quantity strictly separated
    /// from every constructed one: a cut leaves a `2 (gap_radius + guard)`
    /// void, so construction noise cannot push it back under the nominal test
    /// and the monotone decreasing sequence stops when `Nₖ` is empty. This
    /// covers distinct components, hairpins, notches, and internal voids with
    /// the same cut on every pass.
    pub fn disk_regularize_gaps(
        &self,
        gap_radius: f64,
        filled_radius: f64,
        guard: f64,
    ) -> Result<DiskGapRegularization, GapRegularizationError> {
        if !gap_radius.is_finite()
            || gap_radius <= 0.0
            || !filled_radius.is_finite()
            || filled_radius <= 0.0
        {
            return Err(GapRegularizationError(
                "gap and filled-region radii must be finite and positive".to_string(),
            ));
        }
        if !guard.is_finite() || guard < 0.0 {
            return Err(GapRegularizationError(
                "gap-regularization guard must be finite and non-negative".to_string(),
            ));
        }

        let tube_radius = gap_radius + guard;
        let mut kept = self.clone();
        let mut narrow_voids = Self::empty(self.tolerance);
        let mut separator_keep_out = Self::empty(self.tolerance);
        loop {
            let pass_narrow_voids = kept.disk_gap_violations(gap_radius);
            if pass_narrow_voids.is_empty() {
                break;
            }
            let axis_keep_out =
                narrow_void_medial_axis_keep_out(&kept, &pass_narrow_voids, tube_radius)?;
            // A void thinner than the axis stroke has no representable medial
            // axis. Sweep such components whole: they sit far below the
            // regularization scale, so even the blunt cut stays local, and
            // every pass is guaranteed to remove material.
            let axisless = pass_narrow_voids
                .connected_components()
                .into_iter()
                .filter(|component| component.intersection(&axis_keep_out).is_empty())
                .collect::<Vec<_>>();
            let pass_keep_out = axisless.into_iter().fold(axis_keep_out, |keep_out, thin| {
                keep_out.union(&thin.disk_dilate(tube_radius))
            });
            let next = kept
                .difference(&pass_keep_out)
                .disk_open(filled_radius)
                .intersection(&kept);
            narrow_voids = narrow_voids.union(&pass_narrow_voids);
            separator_keep_out = separator_keep_out.union(&pass_keep_out);

            if kept.difference(&next).area() <= self.tolerance * self.tolerance {
                return Err(GapRegularizationError(format!(
                    "gap regularization stalled with {:.9} mm² of void-gap violations",
                    pass_narrow_voids.area()
                )));
            }
            kept = next;
        }
        let removed = self.difference(&kept);
        Ok(DiskGapRegularization {
            kept,
            narrow_voids,
            separator_keep_out,
            removed,
        })
    }

    /// Unfilled material that violates the two-sided void-gap radius.
    ///
    /// The raw closing residual `close(self, disk(radius)) \ self` also contains
    /// the rounded bite at an isolated concave corner. A residual component is
    /// a gap when it contacts nonincident, separated source-boundary segments
    /// on distinct rings, or on one ring with opposing tangents — the latter
    /// distinguishes hairpins and notches from the bite of a single smooth
    /// concavity. An empty result proves no two facing boundary branches fail
    /// the rolling-disk test.
    pub fn disk_gap_violations(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return Self::empty(self.tolerance);
        }
        if !radius.is_finite() {
            return Self::empty(self.tolerance);
        }
        let closing_residual = self.disk_close(radius).difference(self);
        two_sided_gap_residual(self, &closing_residual)
    }

    pub fn to_contours(&self) -> Vec<ContourBuf> {
        rings_to_contours(self.rings.clone())
    }

    /// Convert to closed contours, re-fitting maximal circular arcs over the
    /// flattened boundaries. Arcs from source outlines and disk-swept tool
    /// paths that the boolean pipeline tessellated come back as `ArcTo`
    /// segments, within the shared chord tolerance of the polyline form.
    pub fn to_contours_with_arcs(&self) -> Vec<ContourBuf> {
        self.rings
            .iter()
            .map(|ring| crate::geom::arcfit::ring_to_contour_with_arcs(ring, tol::FLATTEN_MM))
            .collect()
    }

    /// Convert each connected component to one positive contour.
    ///
    /// Hole rings are connected to their outer ring with zero-width bridges,
    /// allowing formats without compound-polygon holes to carry the same
    /// local positive geometry without layer-wide clear features.
    pub fn to_bridged_contours_with_arcs(&self) -> Vec<ContourBuf> {
        simplify_shapes(self.rings.clone(), FillRule::NonZero)
            .into_iter()
            .map(crate::geom::bridge::bridge_shape)
            .filter(|ring| ring.len() >= 3)
            .map(|ring| crate::geom::arcfit::ring_to_contour_with_arcs(&ring, tol::FLATTEN_MM))
            .collect()
    }

    fn disk_offset(&self, offset: f64) -> Self {
        let radius = offset.abs();
        let max_sagitta = tol::STROKE_OUTLINE_MM.min(radius);
        // i_overlay rounds each join into floor(sweep / join_angle)
        // segments. Use half the central angle allowed by the sagitta budget
        // so that flooring cannot make the emitted segments too coarse.
        let join_angle = (1.0 - max_sagitta / radius)
            .acos()
            .clamp(0.01 * std::f64::consts::PI, 0.25 * std::f64::consts::PI);
        let style = OutlineStyle::new(offset).line_join(OutlineLineJoin::Round(join_angle));
        let shapes = self.rings.outline_as::<i64>(&style);
        Self::new(flatten_shapes(shapes), FillRule::NonZero, self.tolerance)
    }
}

/// Compose an ordered dark/clear paint stream into a final positive image.
///
/// Consecutive same-polarity pushes are batched into one boolean operation.
#[derive(Debug, Default)]
pub struct PaintComposer {
    image: Vec<Ring>,
    run: Vec<Ring>,
    run_polarity: Option<Polarity>,
}

impl PaintComposer {
    pub fn push(&mut self, polarity: Polarity, mut rings: Vec<Ring>) {
        if rings.is_empty() {
            return;
        }
        if self.run_polarity != Some(polarity) {
            self.flush_run();
            self.run_polarity = Some(polarity);
        }
        self.run.append(&mut rings);
    }

    pub fn finish(mut self) -> Vec<Ring> {
        self.flush_run();
        self.image
    }

    pub fn finish_set(self, tolerance: f64) -> ContourSet {
        ContourSet::new(self.finish(), FillRule::NonZero, tolerance)
    }

    fn flush_run(&mut self) {
        let Some(polarity) = self.run_polarity.take() else {
            return;
        };
        if self.run.is_empty() {
            return;
        }

        match polarity {
            Polarity::Dark => {
                let mut rings = std::mem::take(&mut self.image);
                rings.append(&mut self.run);
                self.image = union_rings(rings, FillRule::NonZero);
            }
            Polarity::Clear => {
                if self.image.is_empty() {
                    self.run.clear();
                } else {
                    let cutters = union_rings(std::mem::take(&mut self.run), FillRule::NonZero);
                    self.image = difference_rings(std::mem::take(&mut self.image), cutters);
                }
            }
        }
    }
}

pub(crate) fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

fn flatten_shapes(shapes: Vec<Shape>) -> Vec<Ring> {
    shapes.into_iter().flatten().collect()
}

fn filter_significant_rings(mut rings: Vec<Ring>, tolerance: f64) -> Vec<Ring> {
    if tolerance > 0.0 {
        let min_area = tolerance.powi(2);
        rings.retain(|ring| ring_signed_area(ring).abs() > min_area);
    }
    rings
}

fn push_ring(out: &mut Vec<Ring>, ring: &mut Ring) {
    if ring.first() == ring.last() {
        ring.pop();
    }
    if ring.len() >= 3 {
        out.push(std::mem::take(ring));
    } else {
        ring.clear();
    }
}

fn ring_to_contour(ring: Ring) -> Option<ContourBuf> {
    if ring.len() < 3 {
        return None;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(ring.len() + 1);
    for (index, [x, y]) in ring.into_iter().enumerate() {
        let point = Point::new(x, y);
        bbox.include_point(point);
        if index == 0 {
            cmds.push(PathCmd::move_to(point));
        } else {
            cmds.push(PathCmd::line_to(point));
        }
    }
    cmds.push(PathCmd::close());
    Some(ContourBuf::from_parts(bbox, cmds))
}

fn ring_contains_point(ring: &Ring, point: Point) -> bool {
    if ring.len() < 3 {
        return false;
    }

    let mut inside = false;
    for index in 0..ring.len() {
        let [x0, y0] = ring[index];
        let [x1, y1] = ring[(index + 1) % ring.len()];
        if (y0 > point.y) != (y1 > point.y) {
            let crossing_x = x0 + (point.y - y0) * (x1 - x0) / (y1 - y0);
            if point.x < crossing_x {
                inside = !inside;
            }
        }
    }
    inside
}

fn ring_boundary_distance(ring: &Ring, point: Point) -> f64 {
    if ring.is_empty() {
        return f64::INFINITY;
    }

    (0..ring.len())
        .map(|index| {
            let [x0, y0] = ring[index];
            let [x1, y1] = ring[(index + 1) % ring.len()];
            point_segment_distance(point, Point::new(x0, y0), Point::new(x1, y1))
        })
        .fold(f64::INFINITY, f64::min)
}

fn point_segment_distance(point: Point, start: Point, end: Point) -> f64 {
    let segment = end - start;
    let length_squared = segment.x * segment.x + segment.y * segment.y;
    if length_squared == 0.0 {
        return point.distance_to(start);
    }
    let offset = point - start;
    let t = ((offset.x * segment.x + offset.y * segment.y) / length_squared).clamp(0.0, 1.0);
    point.distance_to(start + segment * t)
}

const VORONOI_COORDINATES_PER_MM: f64 = 100_000.0;

#[derive(Debug, Clone, Copy)]
struct BoundarySegment {
    ring: usize,
    index: usize,
    ring_len: usize,
}

#[derive(Debug, Clone, Copy)]
struct OrientedBoundarySegment {
    topology: BoundarySegment,
    start: Point,
    end: Point,
    bbox: BBox,
}

fn two_sided_gap_residual(source: &ContourSet, residual: &ContourSet) -> ContourSet {
    let source_segments = source
        .rings
        .iter()
        .enumerate()
        .flat_map(|(ring_id, ring)| {
            (0..ring.len()).filter_map(move |index| {
                let [start_x, start_y] = ring[index];
                let [end_x, end_y] = ring[(index + 1) % ring.len()];
                let start = Point::new(start_x, start_y);
                let end = Point::new(end_x, end_y);
                (start.distance_to(end) > source.tolerance).then_some(OrientedBoundarySegment {
                    topology: BoundarySegment {
                        ring: ring_id,
                        index,
                        ring_len: ring.len(),
                    },
                    start,
                    end,
                    bbox: segment_bbox(start, end),
                })
            })
        })
        .collect::<Vec<_>>();
    let contact_tolerance = source.tolerance.max(residual.tolerance);

    let rings = residual
        .connected_components()
        .into_iter()
        .filter(|component| {
            let contacts = source_segments
                .iter()
                .filter(|segment| {
                    segment
                        .bbox
                        .expand(contact_tolerance)
                        .intersects(component.bbox)
                        && region_boundary_within_distance(
                            component,
                            segment.start,
                            segment.end,
                            contact_tolerance,
                        )
                })
                .collect::<Vec<_>>();
            contacts.iter().enumerate().any(|(index, left)| {
                contacts[index + 1..].iter().any(|right| {
                    let separation =
                        segment_separation(left.start, left.end, right.start, right.end);
                    // Contacts on distinct rings always face each other across
                    // void, whatever their relative angle. Same-ring pairs
                    // must additionally oppose so the rounded bite of one
                    // smooth concavity is not mistaken for a gap; walls at
                    // exactly 90° remain ambiguous there by construction.
                    !boundary_segments_are_incident(left.topology, right.topology)
                        && separation > contact_tolerance
                        && (left.topology.ring != right.topology.ring
                            || boundary_tangents_oppose(left, right))
                })
            })
        })
        .flat_map(|component| component.rings)
        .collect();
    ContourSet::new(rings, FillRule::NonZero, residual.tolerance)
}

fn region_boundary_within_distance(
    region: &ContourSet,
    start: Point,
    end: Point,
    distance: f64,
) -> bool {
    let expanded = segment_bbox(start, end).expand(distance);
    region.rings.iter().any(|ring| {
        (0..ring.len()).any(|index| {
            let [other_start_x, other_start_y] = ring[index];
            let [other_end_x, other_end_y] = ring[(index + 1) % ring.len()];
            let other_start = Point::new(other_start_x, other_start_y);
            let other_end = Point::new(other_end_x, other_end_y);
            expanded.intersects(segment_bbox(other_start, other_end))
                && segment_separation(start, end, other_start, other_end) <= distance
        })
    })
}

fn segment_separation(
    left_start: Point,
    left_end: Point,
    right_start: Point,
    right_end: Point,
) -> f64 {
    point_segment_distance(left_start, right_start, right_end)
        .min(point_segment_distance(left_end, right_start, right_end))
        .min(point_segment_distance(right_start, left_start, left_end))
        .min(point_segment_distance(right_end, left_start, left_end))
}

fn boundary_tangents_oppose(
    left: &OrientedBoundarySegment,
    right: &OrientedBoundarySegment,
) -> bool {
    let left_tangent = left.end - left.start;
    let right_tangent = right.end - right.start;
    left_tangent.x * right_tangent.x + left_tangent.y * right_tangent.y < 0.0
}

fn narrow_void_medial_axis_keep_out(
    source: &ContourSet,
    narrow_voids: &ContourSet,
    radius: f64,
) -> Result<ContourSet, GapRegularizationError> {
    if narrow_voids.is_empty() {
        return Ok(ContourSet::empty(source.tolerance));
    }
    let origin = Point::new(source.bbox.min.x, source.bbox.min.y);
    let mut segments = Vec::<VoronoiLine<i32>>::new();
    let mut boundary_segments = Vec::new();
    for (ring_id, ring) in source.rings.iter().enumerate() {
        for index in 0..ring.len() {
            let [start_x, start_y] = ring[index];
            let [end_x, end_y] = ring[(index + 1) % ring.len()];
            if (end_x - start_x).hypot(end_y - start_y) <= source.tolerance {
                continue;
            }
            let start = quantize_voronoi_point(ring[index], origin)?;
            let end = quantize_voronoi_point(ring[(index + 1) % ring.len()], origin)?;
            if start == end {
                continue;
            }
            segments.push(VoronoiLine::new(start, end));
            boundary_segments.push(BoundarySegment {
                ring: ring_id,
                index,
                ring_len: ring.len(),
            });
        }
    }

    let diagram = VoronoiBuilder::<i32>::default()
        .with_segments(segments.iter())
        .and_then(VoronoiBuilder::build)
        .map_err(|error| {
            GapRegularizationError(format!(
                "could not construct boundary Voronoi diagram: {error}"
            ))
        })?;
    let mut contours = Vec::new();
    for edge in diagram.edges() {
        let twin = edge.twin().map_err(gap_regularization_error)?;
        if edge.id() > twin || !edge.is_primary() {
            continue;
        }
        let left = diagram
            .cell(edge.cell().map_err(gap_regularization_error)?)
            .map_err(gap_regularization_error)?;
        let right = diagram
            .cell(
                diagram
                    .edge(twin)
                    .and_then(|edge| edge.cell())
                    .map_err(gap_regularization_error)?,
            )
            .map_err(gap_regularization_error)?;
        let left_boundary = boundary_segments
            .get(left.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        let right_boundary = boundary_segments
            .get(right.source_index().usize())
            .ok_or_else(|| {
                GapRegularizationError(
                    "Voronoi cell references an unknown boundary segment".to_string(),
                )
            })?;
        if boundary_segments_are_incident(*left_boundary, *right_boundary) {
            continue;
        }
        let samples = voronoi_edge_samples(&diagram, edge.id(), &segments, source, radius)?;
        let mut commands = Vec::with_capacity(samples.len());
        for sample in samples {
            let point = Point::new(
                sample[0] / VORONOI_COORDINATES_PER_MM + origin.x,
                sample[1] / VORONOI_COORDINATES_PER_MM + origin.y,
            );
            if commands
                .last()
                .and_then(|command: &PathCmd| command.end_point())
                .is_some_and(|previous| previous.distance_to(point) <= tol::EPSILON_MM)
            {
                continue;
            }
            commands.push(if commands.is_empty() {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        if commands.len() >= 2 {
            contours.push(ContourBuf::new(commands));
        }
    }

    if contours.is_empty() {
        return Ok(ContourSet::empty(source.tolerance));
    }
    // A narrow filled stroke makes the one-dimensional axis available to the
    // existing set algebra. Intersecting with a slightly eroded narrow-void
    // phase removes the finite stroke's boundary fringe and the exterior axis.
    let axis_stroke_radius = tol::REGION_MM;
    let mut arena = PathArena::default();
    let path = arena.push_path(
        Paint::Stroke(crate::geom::StrokeStyle::round(2.0 * axis_stroke_radius)),
        contours,
    );
    let medial_axis = ContourSet::from_painted_paths(
        &arena,
        std::iter::once(&arena.paths[path as usize]),
        source.tolerance,
    );
    let interior_axis =
        medial_axis.intersection(&narrow_voids.disk_erode(2.0 * axis_stroke_radius));
    let keep_out = interior_axis.disk_dilate(radius);
    Ok(keep_out.intersection(&source.disk_dilate(radius)))
}

fn boundary_segments_are_incident(left: BoundarySegment, right: BoundarySegment) -> bool {
    if left.ring != right.ring {
        return false;
    }
    let distance = left.index.abs_diff(right.index);
    distance.min(left.ring_len - distance) <= 1
}

fn quantize_voronoi_point(
    [x, y]: [f64; 2],
    origin: Point,
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    fn coordinate(value: f64, origin: f64) -> Result<i32, GapRegularizationError> {
        let scaled = ((value - origin) * VORONOI_COORDINATES_PER_MM).round();
        if !scaled.is_finite() || scaled < i32::MIN as f64 || scaled > i32::MAX as f64 {
            return Err(GapRegularizationError(
                "component geometry exceeds the Voronoi coordinate range".to_string(),
            ));
        }
        Ok(scaled as i32)
    }

    Ok(VoronoiPoint::new(
        coordinate(x, origin.x)?,
        coordinate(y, origin.y)?,
    ))
}

fn gap_regularization_error(error: boostvoronoi::BvError) -> GapRegularizationError {
    GapRegularizationError(format!("invalid boundary Voronoi diagram: {error}"))
}

fn voronoi_edge_samples(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let affine = SimpleAffine::default();
    let mut samples = if let (Some(start), Some(end)) = (
        edge.vertex0(),
        diagram
            .edge_get_vertex1(edge_id)
            .map_err(gap_regularization_error)?,
    ) {
        let start = diagram.vertex(start).map_err(gap_regularization_error)?;
        let end = diagram.vertex(end).map_err(gap_regularization_error)?;
        vec![
            affine.transform(start.x(), start.y()),
            affine.transform(end.x(), end.y()),
        ]
    } else {
        clip_infinite_voronoi_edge(diagram, edge_id, segments, region, radius)?
    };

    if edge.is_curved() {
        let cell = edge.cell().map_err(gap_regularization_error)?;
        let twin_cell = diagram
            .edge(edge.twin().map_err(gap_regularization_error)?)
            .and_then(|edge| edge.cell())
            .map_err(gap_regularization_error)?;
        let (point_cell, segment_cell) = if diagram
            .cell(cell)
            .map_err(gap_regularization_error)?
            .contains_point()
        {
            (cell, twin_cell)
        } else {
            (twin_cell, cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        VoronoiVisualUtils::discretize(
            &point,
            segment,
            tol::FLATTEN_MM * VORONOI_COORDINATES_PER_MM,
            &affine,
            &mut samples,
        );
    }
    Ok(samples)
}

fn clip_infinite_voronoi_edge(
    diagram: &VoronoiDiagram,
    edge_id: VoronoiEdgeIndex,
    segments: &[VoronoiLine<i32>],
    region: &ContourSet,
    radius: f64,
) -> Result<Vec<[f64; 2]>, GapRegularizationError> {
    let edge = diagram.edge(edge_id).map_err(gap_regularization_error)?;
    let cell = edge.cell().map_err(gap_regularization_error)?;
    let twin_cell = diagram
        .edge(edge.twin().map_err(gap_regularization_error)?)
        .and_then(|edge| edge.cell())
        .map_err(gap_regularization_error)?;
    let left = diagram.cell(cell).map_err(gap_regularization_error)?;
    let right = diagram.cell(twin_cell).map_err(gap_regularization_error)?;
    let (origin, direction) = if left.contains_point() && right.contains_point() {
        let left = voronoi_cell_point(diagram, cell, segments)?;
        let right = voronoi_cell_point(diagram, twin_cell, segments)?;
        (
            [
                (left.x as f64 + right.x as f64) * 0.5,
                (left.y as f64 + right.y as f64) * 0.5,
            ],
            [
                left.y as f64 - right.y as f64,
                right.x as f64 - left.x as f64,
            ],
        )
    } else {
        let (point_cell, segment_cell) = if left.contains_segment() {
            (twin_cell, cell)
        } else {
            (cell, twin_cell)
        };
        let point = voronoi_cell_point(diagram, point_cell, segments)?;
        let segment = voronoi_cell_segment(diagram, segment_cell, segments)?;
        let origin = [point.x as f64, point.y as f64];
        let dx = segment.end.x - segment.start.x;
        let dy = segment.end.y - segment.start.y;
        let direction = if ([segment.start.x as f64, segment.start.y as f64] == origin)
            ^ left.contains_point()
        {
            [dy as f64, -dx as f64]
        } else {
            [-dy as f64, dx as f64]
        };
        (origin, direction)
    };
    let reach =
        (region.bbox.width().max(region.bbox.height()) + 4.0 * radius) * VORONOI_COORDINATES_PER_MM;
    let direction_scale = direction[0].abs().max(direction[1].abs());
    if direction_scale == 0.0 {
        return Err(GapRegularizationError(
            "infinite Voronoi edge has no direction".to_string(),
        ));
    }
    let coefficient = reach / direction_scale;
    let affine = SimpleAffine::default();
    let start = edge
        .vertex0()
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] - direction[0] * coefficient,
            origin[1] - direction[1] * coefficient,
        ]);
    let end = diagram
        .edge_get_vertex1(edge_id)
        .map_err(gap_regularization_error)?
        .map(|vertex| {
            diagram
                .vertex(vertex)
                .map(|vertex| affine.transform(vertex.x(), vertex.y()))
        })
        .transpose()
        .map_err(gap_regularization_error)?
        .unwrap_or([
            origin[0] + direction[0] * coefficient,
            origin[1] + direction[1] * coefficient,
        ]);
    Ok(vec![start, end])
}

fn voronoi_cell_point(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &[VoronoiLine<i32>],
) -> Result<VoronoiPoint<i32>, GapRegularizationError> {
    let cell = diagram.cell(cell).map_err(gap_regularization_error)?;
    let segment = segments.get(cell.source_index().usize()).ok_or_else(|| {
        GapRegularizationError("Voronoi point references an unknown segment".to_string())
    })?;
    Ok(match cell.source_category() {
        SourceCategory::SegmentStart => segment.start,
        SourceCategory::Segment | SourceCategory::SegmentEnd => segment.end,
        SourceCategory::SinglePoint => {
            return Err(GapRegularizationError(
                "unexpected standalone point in component Voronoi diagram".to_string(),
            ));
        }
    })
}

fn voronoi_cell_segment<'a>(
    diagram: &VoronoiDiagram,
    cell: VoronoiCellIndex,
    segments: &'a [VoronoiLine<i32>],
) -> Result<&'a VoronoiLine<i32>, GapRegularizationError> {
    let index = diagram
        .cell(cell)
        .map_err(gap_regularization_error)?
        .source_index()
        .usize();
    segments.get(index).ok_or_else(|| {
        GapRegularizationError("Voronoi cell references an unknown segment".to_string())
    })
}

#[cfg(test)]
fn regions_within_distance(left: &ContourSet, right: &ContourSet, distance: f64) -> bool {
    if !left.bbox.expand(distance).intersects(right.bbox) {
        return false;
    }
    let threshold = distance + left.tolerance.max(right.tolerance);
    for left_ring in &left.rings {
        for left_index in 0..left_ring.len() {
            let [left_start_x, left_start_y] = left_ring[left_index];
            let [left_end_x, left_end_y] = left_ring[(left_index + 1) % left_ring.len()];
            let left_start = Point::new(left_start_x, left_start_y);
            let left_end = Point::new(left_end_x, left_end_y);
            let left_bbox = segment_bbox(left_start, left_end).expand(threshold);
            for right_ring in &right.rings {
                for right_index in 0..right_ring.len() {
                    let [right_start_x, right_start_y] = right_ring[right_index];
                    let [right_end_x, right_end_y] =
                        right_ring[(right_index + 1) % right_ring.len()];
                    let right_start = Point::new(right_start_x, right_start_y);
                    let right_end = Point::new(right_end_x, right_end_y);
                    if !left_bbox.intersects(segment_bbox(right_start, right_end)) {
                        continue;
                    }
                    let separation = point_segment_distance(left_start, right_start, right_end)
                        .min(point_segment_distance(left_end, right_start, right_end))
                        .min(point_segment_distance(right_start, left_start, left_end))
                        .min(point_segment_distance(right_end, left_start, left_end));
                    if separation <= threshold {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn segment_bbox(start: Point, end: Point) -> BBox {
    BBox::new(
        Point::new(start.x.min(end.x), start.y.min(end.y)),
        Point::new(start.x.max(end.x), start.y.max(end.y)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::shapes;

    #[test]
    fn contour_set_composes_region_operations() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let inner = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let clip = ContourSet::rectangle(rect(5.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        let ring = outer.difference(&inner);
        let clipped = ring.intersection(&clip);
        let expanded = clipped.disk_dilate(0.5);

        assert!(!expanded.is_empty());
        assert!((expanded.bbox.min.x - 4.5).abs() <= 1e-9);
        assert!((expanded.bbox.max.x - 10.5).abs() <= 1e-9);
    }

    #[test]
    fn filled_contour_region_is_winding_insensitive() {
        let clockwise = rectangle_contour(0.0, 0.0, 10.0, 5.0);
        let counter_clockwise = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 5.0)),
            PathCmd::line_to(Point::new(10.0, 5.0)),
            PathCmd::line_to(Point::new(10.0, 0.0)),
            PathCmd::line_to(Point::new(0.0, 0.0)),
            PathCmd::close(),
        ]);

        let a = ContourSet::from_filled_contours(std::slice::from_ref(&clockwise), tol::REGION_MM);
        let b = ContourSet::from_filled_contours(
            std::slice::from_ref(&counter_clockwise),
            tol::REGION_MM,
        );
        let unioned =
            ContourSet::from_filled_contours(&[clockwise, counter_clockwise], tol::REGION_MM);

        assert!(!a.is_empty());
        assert!((a.area() - b.area()).abs() <= 1e-9);
        assert!((unioned.area() - 50.0).abs() <= 1e-6);
    }

    #[test]
    fn area_subtracts_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        let inner = ContourSet::rectangle(rect(1.0, 1.0, 3.0, 3.0), tol::REGION_MM);

        let ring = outer.difference(&inner);

        assert!((ring.area() - 12.0).abs() <= 1e-6);
    }

    #[test]
    fn containment_observes_boundaries_and_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);
        let region = outer.difference(&hole);

        assert!(region.contains_point(Point::new(2.0, 2.0)));
        assert!(region.contains_point(Point::new(0.0, 5.0)));
        assert!(!region.contains_point(Point::new(5.0, 5.0)));
        assert!(region.contains_disk(Point::new(2.0, 2.0), 2.0));
        assert!(!region.contains_disk(Point::new(2.0, 2.0), 2.01));
        assert!(!region.contains_disk(Point::new(3.5, 5.0), 0.6));
    }

    #[test]
    fn bridged_contour_preserves_local_holes_without_clear_polarity() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let circle = shapes::circle(2.0).unwrap();
        let circle = crate::geom::path::transform_cmds(
            circle.cmds,
            crate::geom::Affine2::translation(Point::new(5.0, 5.0)),
        );
        let hole = ContourSet::from_filled_contours(&[circle], tol::REGION_MM);
        let region = outer.difference(&hole);

        let contours = region.to_bridged_contours_with_arcs();
        let round_trip = ContourSet::from_contours(&contours, FillRule::NonZero, tol::REGION_MM);

        assert_eq!(contours.len(), 1);
        assert!(
            contours[0]
                .cmds
                .iter()
                .any(|cmd| cmd.op == crate::geom::PathOp::ArcTo)
        );
        assert!(
            (round_trip.area() - region.area()).abs() <= 0.01,
            "bridged area {}, source area {}",
            round_trip.area(),
            region.area()
        );
        assert!(!round_trip.contains_point(Point::new(5.0, 5.0)));
    }

    #[test]
    fn erodes_outer_boundaries_and_expands_holes() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);

        let eroded = outer.difference(&hole).disk_erode(0.5);

        assert!((eroded.bbox.min.x - 0.5).abs() <= 1e-9);
        assert!((eroded.bbox.min.y - 0.5).abs() <= 1e-9);
        assert!((eroded.bbox.max.x - 9.5).abs() <= 1e-9);
        assert!((eroded.bbox.max.y - 9.5).abs() <= 1e-9);
        let area = eroded.area();
        assert!(
            (area - 72.214601837).abs() <= 2e-2,
            "unexpected eroded area {area}"
        );
    }

    #[test]
    fn erosion_can_remove_an_entire_region() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 0.5, 0.5), tol::REGION_MM);

        let eroded = region.disk_erode(0.5);

        assert!(eroded.is_empty());
    }

    #[test]
    fn disk_opening_rounds_corners_and_stays_inside_source() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        let opened = region.disk_open(0.5);

        assert!(opened.difference(&region).is_empty());
        assert!((opened.bbox.min.x - 0.0).abs() <= 1e-9);
        assert!((opened.bbox.min.y - 0.0).abs() <= 1e-9);
        assert!((opened.bbox.max.x - 10.0).abs() <= 1e-9);
        assert!((opened.bbox.max.y - 10.0).abs() <= 1e-9);
        assert!((opened.area() - (99.0 + std::f64::consts::PI / 4.0)).abs() <= 2e-2);
        assert!(
            opened
                .to_contours_with_arcs()
                .iter()
                .flat_map(|contour| &contour.cmds)
                .any(|cmd| cmd.op == crate::geom::PathOp::ArcTo)
        );
    }

    #[test]
    fn disk_opening_removes_sub_diameter_slivers_and_small_islands() {
        let body = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let sliver = ContourSet::rectangle(rect(12.0, 0.0, 20.0, 0.8), tol::REGION_MM);
        let island = ContourSet::rectangle(rect(22.0, 0.0, 22.8, 0.8), tol::REGION_MM);
        let region = body.union(&sliver).union(&island);

        let opened = region.disk_open(0.5);

        assert_eq!(opened.connected_components().len(), 1);
        assert!(opened.intersection(&body).area() > 99.0);
        assert!(opened.intersection(&sliver).is_empty());
        assert!(opened.intersection(&island).is_empty());
    }

    #[test]
    fn disk_opening_is_idempotent_within_offset_tolerance() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let notch = ContourSet::rectangle(rect(4.0, 8.0, 6.0, 10.0), tol::REGION_MM);
        let region = outer.difference(&notch);

        let once = region.disk_open(0.5);
        let twice = once.disk_open(0.5);
        let symmetric_difference = once.difference(&twice).union(&twice.difference(&once));

        assert!(
            symmetric_difference.area() <= 2e-2,
            "opening changed by {:.9} mm² on repetition",
            symmetric_difference.area()
        );
    }

    #[test]
    fn disk_closing_fills_sub_diameter_gaps_and_stays_outside_source() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let closed = region.disk_close(0.5);
        let middle = ContourSet::rectangle(rect(4.0, 1.0, 4.8, 9.0), tol::REGION_MM);

        assert!(region.difference(&closed).is_empty());
        assert!(closed.intersection(&middle).area() > 6.3);
    }

    #[test]
    fn disk_closing_preserves_wide_gaps() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let closed = region.disk_close(0.5);
        let middle = ContourSet::rectangle(rect(4.0, 1.0, 5.2, 9.0), tol::REGION_MM);

        assert!(closed.intersection(&middle).is_empty());
    }

    #[test]
    fn disk_gap_violations_report_close_distinct_components() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let close = ContourSet::rectangle(rect(4.8, 0.0, 10.0, 10.0), tol::REGION_MM);
        let wide = ContourSet::rectangle(rect(5.2, 0.0, 10.0, 10.0), tol::REGION_MM);

        let close_violations = left.union(&close).disk_gap_violations(0.5);
        let wide_violations = left.union(&wide).disk_gap_violations(0.5);

        assert!(close_violations.area() > 1.5);
        assert!(wide_violations.is_empty());
        assert!(left.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_sweeps_a_void_thinner_than_the_axis_stroke() {
        // A 3 µm gap is two-sided but too thin to carry a medial-axis stroke;
        // the whole-component sweep must still make progress instead of
        // stalling into an error.
        let left = ContourSet::rectangle(rect(0.0, 0.0, 5.0, 6.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(5.003, 0.0, 10.0, 6.0), tol::REGION_MM);
        let region = left.union(&right);
        assert!(!region.disk_gap_violations(0.5).is_empty());

        let regularization = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert!(regularization.kept.disk_gap_violations(0.5).is_empty());
        assert!(regularization.removed.area() > 0.0);
    }

    #[test]
    fn disk_gap_violations_flag_a_diagonal_corner_to_corner_approach() {
        // The facing contacts here include perpendicular wall pairs; distinct
        // rings are classified as a gap without any tangent condition.
        let lower = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        let upper = ContourSet::rectangle(rect(4.3, 4.3, 8.0, 8.0), tol::REGION_MM);

        let violations = lower.union(&upper).disk_gap_violations(1.0);

        assert!(!violations.is_empty());
        assert!(violations.contains_point(Point::new(4.15, 4.15)));
    }

    #[test]
    fn disk_gap_violations_exclude_isolated_void_corners() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let wide_hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let region = outer.difference(&wide_hole);
        let raw_closing_residual = region.disk_close(0.5).difference(&region);

        assert!(raw_closing_residual.area() > 0.1);
        assert!(region.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_rejects_invalid_scales() {
        let region = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);

        assert!(region.disk_regularize_gaps(0.0, 0.5, 0.025).is_err());
        assert!(region.disk_regularize_gaps(0.5, f64::NAN, 0.025).is_err());
        assert!(region.disk_regularize_gaps(0.5, 0.5, -0.025).is_err());
    }

    #[test]
    fn disk_gap_regularization_widens_a_gap_thinner_than_the_guard() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(4.01, 0.0, 10.0, 10.0), tol::REGION_MM);
        let region = left.union(&right);

        let before = region.disk_gap_violations(0.5);
        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert!(before.area() > 0.05);
        assert!(before.disk_open(0.025).is_empty());
        assert!(result.removed.area() > 0.0);
        assert!(result.kept.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_trims_a_close_pair_locally() {
        let left = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let right = ContourSet::rectangle(rect(10.8, 0.0, 20.8, 10.0), tol::REGION_MM);
        let distant = ContourSet::rectangle(rect(24.0, 0.0, 30.0, 10.0), tol::REGION_MM);
        let region = left.union(&right).union(&distant);

        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert_eq!(result.kept.connected_components().len(), 3);
        assert!(result.kept.difference(&region).is_empty());
        assert!(
            result.removed.area() < 4.0,
            "removed {:.9} mm²",
            result.removed.area()
        );
        assert!(result.removed.intersection(&distant).area() <= 0.25);
        assert!(result.kept.disk_gap_violations(0.5).is_empty());
    }

    #[test]
    fn disk_gap_regularization_is_symmetric_at_a_three_way_conflict() {
        let lower_left = ContourSet::rectangle(rect(0.0, 0.0, 4.0, 4.0), tol::REGION_MM);
        let lower_right = ContourSet::rectangle(rect(4.8, 0.0, 8.8, 4.0), tol::REGION_MM);
        let upper = ContourSet::rectangle(rect(2.4, 4.8, 6.4, 8.8), tol::REGION_MM);
        let region = lower_left.union(&lower_right).union(&upper);

        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();

        assert_eq!(result.kept.connected_components().len(), 3);
        for source in [&lower_left, &lower_right, &upper] {
            assert!(result.kept.intersection(source).area() > 13.0);
        }
        let violations = result.kept.disk_gap_violations(0.5);
        assert!(
            violations.is_empty(),
            "remaining void-gap violation area {:.9} mm²",
            violations.area(),
        );
    }

    #[test]
    fn disk_gap_regularization_widens_a_same_component_hairpin() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let narrow_notch = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 10.0), tol::REGION_MM);
        let hairpin = outer.difference(&narrow_notch);

        let before = hairpin.disk_gap_violations(0.5);
        let result = hairpin.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
        let after = result.kept.disk_gap_violations(0.5);

        assert_eq!(hairpin.connected_components().len(), 1);
        assert!(before.area() > 1.0);
        assert!(result.removed.area() > 1.0);
        assert!(
            after.is_empty(),
            "remaining hairpin gap {:.9} mm²",
            after.area()
        );
    }

    #[test]
    fn disk_gap_regularization_widens_a_narrow_internal_void() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let narrow_hole = ContourSet::rectangle(rect(4.6, 3.0, 5.4, 7.0), tol::REGION_MM);
        let region = outer.difference(&narrow_hole);

        let before = region.disk_gap_violations(0.5);
        let result = region.disk_regularize_gaps(0.5, 0.5, 0.025).unwrap();
        let after = result.kept.disk_gap_violations(0.5);

        assert!(before.area() > 3.0);
        assert!(result.removed.area() > 1.0);
        assert!(result.kept.contains_point(Point::new(2.0, 5.0)));
        assert!(
            after.is_empty(),
            "remaining internal-void gap {:.9} mm²",
            after.area(),
        );
    }

    #[test]
    fn dilation_shrinks_but_preserves_a_large_hole() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);

        let dilated = outer.difference(&hole).disk_dilate(0.5);
        let expected_hole = ContourSet::rectangle(rect(3.5, 3.5, 6.5, 6.5), tol::REGION_MM);

        assert!(dilated.intersection(&expected_hole).is_empty());
        let area = dilated.area();
        assert!(
            (area - 111.785398163).abs() <= 2e-2,
            "unexpected dilated area {area}"
        );
    }

    #[test]
    fn union_contains_both_regions_when_a_hole_overlaps_filled_material() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), tol::REGION_MM);
        let hole = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), tol::REGION_MM);
        let frame = outer.difference(&hole);
        let plug = ContourSet::rectangle(rect(4.0, 4.0, 6.0, 6.0), tol::REGION_MM);

        let union = frame.union(&plug);

        assert!(frame.difference(&union).is_empty());
        assert!(plug.difference(&union).is_empty());
        assert!((union.area() - 88.0).abs() <= 1e-6);
    }

    /// Reduced chain of narrow complement pockets from the ControlHub A5
    /// rounded-board/V-score corner. A positive offset must contain every
    /// source point even when all of these holes collapse.
    #[test]
    fn dilation_is_monotone_for_a5_corner_hole_chain() {
        let outer =
            ContourSet::rectangle(rect(22.8473, 110.5175, 27.8973, 115.5675), tol::REGION_MM);
        let holes = ContourSet::from_filled_contours(
            &[
                contour_from_vertices(&[
                    [22.947299957275405, 111.0119310617447],
                    [22.947299957275405, 111.0245145559311],
                    [22.95425641536714, 111.06290817260744],
                ]),
                contour_from_vertices(&[
                    [23.024561643600478, 111.42717397212984],
                    [23.034708142280593, 111.47974538803102],
                    [23.044173359870925, 111.51395440101625],
                ]),
                contour_from_vertices(&[
                    [23.121961593627944, 111.79509580135347],
                    [23.14569699764253, 111.88088023662569],
                    [23.17426168918611, 111.95898854732515],
                ]),
                contour_from_vertices(&[
                    [23.255928039550795, 112.18230032920839],
                    [23.288409471511855, 112.27111899852754],
                    [23.340039849281325, 112.38374710083009],
                ]),
                contour_from_vertices(&[
                    [23.42500483989717, 112.56909239292146],
                    [23.46326601505281, 112.65255665779115],
                    [23.54809403419496, 112.80427730083467],
                ]),
                contour_from_vertices(&[
                    [23.626791238784804, 112.94503259658815],
                    [23.66619455814363, 113.01550805568696],
                    [23.791095137596145, 113.20204389095308],
                ]),
                contour_from_vertices(&[
                    [23.864429831504836, 113.31156766414644],
                    [23.89930665493013, 113.36365520954134],
                    [24.080685615539565, 113.59284710884096],
                ]),
                contour_from_vertices(&[
                    [24.131775259971633, 113.65740430355073],
                    [24.158974766731276, 113.69177389144899],
                    [24.444096326828017, 113.99821996688844],
                    [24.75268936157228, 114.28101646900178],
                    [25.08276188373567, 114.53819620609285],
                    [25.115122795104995, 114.55951189994813],
                    [24.76722896099092, 114.29204106330873],
                    [24.431027054786696, 113.98377299308778],
                ]),
                contour_from_vertices(&[
                    [25.20675671100618, 114.61986958980562],
                    [25.432661533355727, 114.76866948604585],
                    [25.4813165664673, 114.795392036438],
                ]),
                contour_from_vertices(&[
                    [25.624097824096694, 114.87381088733675],
                    [25.797136902809157, 114.96884799003602],
                    [25.857525467872634, 114.99598038196565],
                ]),
                contour_from_vertices(&[
                    [26.056700825691237, 115.08546924591066],
                    [26.179885625839248, 115.1408157348633],
                    [26.24467504024507, 115.16395568847658],
                ]),
                contour_from_vertices(&[
                    [26.486665248870864, 115.25038421154024],
                    [26.5711922645569, 115.2805736064911],
                    [26.634812474250808, 115.29765975475313],
                ]),
                contour_from_vertices(&[
                    [26.93655109405519, 115.37869584560396],
                    [26.973154783248916, 115.38852632045747],
                    [27.011330246925368, 115.39559543132783],
                ]),
                contour_from_vertices(&[
                    [27.390587329864516, 115.46582400798799],
                    [27.396905422210708, 115.46750009059907],
                    [27.402869582176223, 115.46750009059907],
                ]),
            ],
            tol::REGION_MM,
        );
        let source = outer.difference(&holes);

        let dilated = source.disk_dilate(0.525);
        let removed_source = source.difference(&dilated);

        assert!(
            removed_source.is_empty(),
            "dilation removed {:.9} mm² from its source",
            removed_source.area()
        );
    }

    #[test]
    fn painted_path_region_unions_fills_and_native_strokes() {
        let mut arena = PathArena::default();
        let filled = arena.push_path(
            Paint::Fill {
                rule: FillRule::EvenOdd,
            },
            [rectangle_contour(0.0, 0.0, 1.0, 1.0)],
        );
        let stroked = arena.push_path(
            Paint::Stroke(crate::geom::StrokeStyle::round(1.0)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(2.0, 0.5)),
                PathCmd::line_to(Point::new(4.0, 0.5)),
            ])],
        );
        let unpainted = arena.push_path(Paint::None, [rectangle_contour(10.0, 10.0, 20.0, 20.0)]);

        let region = ContourSet::from_painted_paths(
            &arena,
            [filled, stroked, unpainted]
                .iter()
                .map(|&index| arena.path(index)),
            tol::REGION_MM,
        );

        assert!((region.bbox.min.x - 0.0).abs() <= 1e-9);
        assert!((region.bbox.min.y - 0.0).abs() <= 1e-9);
        assert!((region.bbox.max.x - 4.5).abs() <= 1e-9);
        assert!((region.bbox.max.y - 1.0).abs() <= 1e-9);
        assert!(region.area() > 3.5);
        assert!(region.area() < 4.0);
    }

    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox::new(Point::new(min_x, min_y), Point::new(max_x, max_y))
    }

    fn rectangle_contour(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(min_x, min_y)),
            PathCmd::line_to(Point::new(max_x, min_y)),
            PathCmd::line_to(Point::new(max_x, max_y)),
            PathCmd::line_to(Point::new(min_x, max_y)),
            PathCmd::close(),
        ])
    }

    fn contour_from_vertices(vertices: &[[f64; 2]]) -> ContourBuf {
        let mut cmds = Vec::with_capacity(vertices.len() + 1);
        for (index, &[x, y]) in vertices.iter().enumerate() {
            let point = Point::new(x, y);
            cmds.push(if index == 0 {
                PathCmd::move_to(point)
            } else {
                PathCmd::line_to(point)
            });
        }
        cmds.push(PathCmd::close());
        ContourBuf::new(cmds)
    }

    /// Regression: V-score relief tool-center region from a real board whose
    /// boolean output carried sub-micrometer float-debris segments. Dilation
    /// must handle it without panicking or losing the region.
    #[test]
    fn dilates_boolean_debris_with_submicron_segments() {
        let contour = contour_from_vertices(&[
            [38.0, 160.0],
            [38.0, 156.894598],
            [38.0171578, 156.764663],
            [38.0503974, 156.684556],
            [38.0504384, 156.684832],
            [38.1270673, 157.070071],
            [38.1389899, 157.117669],
            [38.2530098, 157.493541],
            [38.2695398, 157.539739],
            [38.419852, 157.902626],
            [38.4408314, 157.946984],
            [38.6259894, 158.29339],
            [38.6512156, 158.335477],
            [38.8694365, 158.662066],
            [38.8986657, 158.701478],
            [39.1478457, 159.005105],
            [39.1807983, 159.041462],
            [39.4585402, 159.319203],
            [39.4948957, 159.352154],
            [39.7985227, 159.601335],
            [39.8379354, 159.630565],
            [40.1645255, 159.848785],
            [40.2066116, 159.874011],
            [40.5530176, 160.059169],
            [40.5973749, 160.080148],
            [40.9602618, 160.23046],
            [41.0064602, 160.24699],
            [41.3823323, 160.36101],
            [41.4299297, 160.372933],
            [41.8151686, 160.449562],
            [41.8154452, 160.449603],
            [41.735338, 160.482842],
            [41.6054032, 160.5],
            [38.5, 160.5],
            [38.3675704, 160.482272],
            [38.2503393, 160.433321],
            [38.1464467, 160.353553],
            [38.0666795, 160.249661],
            [38.0177281, 160.13243],
        ]);
        let region = ContourSet::from_filled_contours(&[contour], tol::REGION_MM);

        let grown = region.disk_dilate(0.5);

        assert!(grown.area() > region.area());
    }

    /// Regression: minimal boundary fragment from a real board that crashed
    /// an arc-preserving offset library's slice stitching when grown by the
    /// route-tool radius.
    #[test]
    fn dilates_relief_boundary_fragment() {
        let contour = contour_from_vertices(&[
            [31.901232957840, 63.057707951027],
            [31.859204053879, 63.115636036354],
            [31.806460976601, 63.248603985268],
            [31.793315052986, 63.391045973259],
            [32.526947975159, 63.811510965782],
            [32.643206000328, 63.728166029411],
            [32.689244031906, 63.673370048958],
            [33.861821055412, 62.191123053986],
        ]);
        let region = ContourSet::from_filled_contours(&[contour], tol::REGION_MM);

        let grown = region.disk_dilate(0.5);

        assert!(grown.area() > region.area());
    }
}
