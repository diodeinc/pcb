//! Regularized planar regions and boolean composition.
//!
//! The flattened polygon form used for boolean set operations is a list of
//! [`Ring`]s (closed polygon boundaries). [`ContourSet`] is the regularized
//! region type built on top: union, difference, intersection, and disk
//! dilation over filled point sets, shared by every dialect so IPC, Gerber,
//! SVG, and comparison all use the same geometry semantics.

use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;

use crate::geom::bbox::BBox;
use crate::geom::path::{
    ContourBuf, PathCmd, StrokeToFillStyle, contours_to_kurbo, stroke_to_fill,
};
use crate::geom::point::Point;
use crate::geom::style::{FillRule, LineCap, LineJoin, Polarity};
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
        let mut rings = self.rings.clone();
        rings.extend(other.rings.iter().cloned());
        Self::new(rings, FillRule::NonZero, self.tolerance)
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

    /// Minkowski sum with a disk: `self ⊕ D_radius`. This is the standard
    /// "buffer out" operation used for manufacturability checks, computed as
    /// a round-join parallel offset of the region boundary.
    pub fn disk_dilate(&self, radius: f64) -> Self {
        if self.is_empty() || radius <= 0.0 {
            return self.clone();
        }

        let mut dilated = self.rings.clone();
        let boundary = rings_to_contours(self.rings.clone());
        if let Some(stroke) = stroke_to_fill(
            &boundary,
            StrokeToFillStyle::new(2.0 * radius, LineCap::Round, LineJoin::Round),
        ) {
            dilated.extend(rings_from_contours(&stroke));
        }
        Self::new(dilated, FillRule::NonZero, self.tolerance)
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

#[cfg(test)]
mod tests {
    use super::*;

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
