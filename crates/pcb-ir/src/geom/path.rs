use crate::geom::{AccuracyError, GeometryAccuracy};

use crate::geom::affine::Affine2;
use crate::geom::arc::{Arc, EllipticalArc};
use crate::geom::bbox::BBox;
use crate::geom::point::Point;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathOp {
    #[default]
    MoveTo,
    LineTo,
    ArcTo,
    EllipseTo,
    Close,
}

/// One fat path command. Which points are meaningful depends on `op`:
///
/// - `MoveTo`/`LineTo`: `p0` is the target point.
/// - `ArcTo`: `p0` is the arc end, `p1` the center, `clockwise` the direction.
/// - `EllipseTo`: `p0` is the arc end, `p1` the center, `p2` and `p3` the
///   images of the unit x and y axes, `clockwise` the direction. This is the
///   affine image of a circular arc; see [`EllipticalArc`].
/// - `Close`: no points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PathCmd {
    pub op: PathOp,
    pub p0: Point,
    pub p1: Point,
    pub p2: Point,
    pub p3: Point,
    pub clockwise: bool,
}

impl PathCmd {
    pub fn move_to(p: Point) -> Self {
        Self {
            op: PathOp::MoveTo,
            p0: p,
            ..Self::default()
        }
    }

    pub fn line_to(p: Point) -> Self {
        Self {
            op: PathOp::LineTo,
            p0: p,
            ..Self::default()
        }
    }

    pub fn arc_to(end: Point, center: Point, clockwise: bool) -> Self {
        Self {
            op: PathOp::ArcTo,
            p0: end,
            p1: center,
            clockwise,
            ..Self::default()
        }
    }

    /// An arc of the ellipse `center + x_axis·cos θ + y_axis·sin θ`.
    pub fn ellipse_to(
        end: Point,
        center: Point,
        x_axis: Point,
        y_axis: Point,
        clockwise: bool,
    ) -> Self {
        Self {
            op: PathOp::EllipseTo,
            p0: end,
            p1: center,
            p2: x_axis,
            p3: y_axis,
            clockwise,
        }
    }

    pub fn close() -> Self {
        Self {
            op: PathOp::Close,
            ..Self::default()
        }
    }

    pub fn end_point(self) -> Option<Point> {
        match self.op {
            PathOp::MoveTo | PathOp::LineTo | PathOp::ArcTo | PathOp::EllipseTo => Some(self.p0),
            PathOp::Close => None,
        }
    }

    pub fn is_finite(self) -> bool {
        self.p0.is_finite() && self.p1.is_finite() && self.p2.is_finite() && self.p3.is_finite()
    }

    /// Whether this command is a curve rather than a line or a move.
    pub fn is_curve(self) -> bool {
        matches!(self.op, PathOp::ArcTo | PathOp::EllipseTo)
    }

    /// The elliptical arc of an `EllipseTo` command starting at `start`.
    fn elliptical_arc(self, start: Point) -> EllipticalArc {
        EllipticalArc {
            start,
            end: self.p0,
            center: self.p1,
            x_axis: self.p2,
            y_axis: self.p3,
            clockwise: self.clockwise,
        }
    }

    fn from_elliptical_arc(arc: EllipticalArc) -> Self {
        Self::ellipse_to(arc.end, arc.center, arc.x_axis, arc.y_axis, arc.clockwise)
    }

    /// The segment this command draws from the current point. A path that
    /// draws before its first move starts at the command's own end point.
    pub(crate) fn segment_from(self, current: Option<Point>) -> Option<Segment> {
        let end = self.end_point()?;
        let start = current.unwrap_or(end);
        Some(match self.op {
            PathOp::MoveTo | PathOp::Close => return None,
            PathOp::LineTo => Segment::Line { start, end },
            PathOp::ArcTo => Segment::Arc(Arc::new(start, end, self.p1, self.clockwise)),
            PathOp::EllipseTo => Segment::Ellipse(self.elliptical_arc(start)),
        })
    }

    /// Exact image under an affine transform, given the current point.
    pub(crate) fn transformed(self, transform: Affine2, start: Point) -> Self {
        match self.op {
            PathOp::MoveTo | PathOp::LineTo => Self {
                p0: transform.transform_point(self.p0),
                ..self
            },
            PathOp::ArcTo => {
                if transform.preserves_circles(1e-12 * transform.max_scale().powi(2)) {
                    Self {
                        p0: transform.transform_point(self.p0),
                        p1: transform.transform_point(self.p1),
                        clockwise: self.clockwise != (transform.determinant() < 0.0),
                        ..self
                    }
                } else {
                    Self::from_elliptical_arc(
                        Arc::new(start, self.p0, self.p1, self.clockwise)
                            .to_elliptical()
                            .transformed(transform),
                    )
                }
            }
            PathOp::EllipseTo => {
                Self::from_elliptical_arc(self.elliptical_arc(start).transformed(transform))
            }
            PathOp::Close => self,
        }
    }
}

/// An owned contour: a command list plus its bounding box.
///
/// This is the detached form of an arena [`crate::geom::Contour`] record, used
/// to move contours between documents and geometry passes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContourBuf {
    pub bbox: BBox,
    pub cmds: Vec<PathCmd>,
    /// Approximation already present in these commands. Zero means the
    /// commands themselves are the source geometry, not that they are lines.
    pub uncertainty_mm: f64,
}

impl ContourBuf {
    /// Build from commands, computing the bounding box.
    pub fn new(cmds: Vec<PathCmd>) -> Self {
        Self {
            bbox: contour_bbox(&cmds),
            cmds,
            uncertainty_mm: 0.0,
        }
    }

    /// Build from commands with a precomputed bounding box.
    pub fn from_parts(bbox: BBox, cmds: Vec<PathCmd>) -> Self {
        Self {
            bbox,
            cmds,
            uncertainty_mm: 0.0,
        }
    }

    /// Record the approximation already present in these commands.
    pub fn with_uncertainty(mut self, uncertainty_mm: f64) -> Self {
        self.uncertainty_mm = uncertainty_mm;
        self
    }

    /// The same contour with every arc one circle. A source may state a
    /// centre its two ends are not equidistant from; neighbours meet at the
    /// ends, so those stay and the centre moves to the nearest point of the
    /// chord's bisector, which keeps it on its side of the chord and so keeps
    /// the sweep. What the radii disagreed by is charged to the uncertainty.
    pub fn with_consistent_arcs(self) -> Self {
        let mut current = Point::default();
        let mut start = current;
        let mut disagreement: f64 = 0.0;
        let cmds = self
            .cmds
            .into_iter()
            .map(|mut cmd| {
                if cmd.op == PathOp::ArcTo {
                    let chord = cmd.p0 - current;
                    let radii = current.distance_to(cmd.p1) - cmd.p0.distance_to(cmd.p1);
                    if radii.abs() > crate::geom::tol::ARC_RADIUS_MM && chord.length() > 0.0 {
                        let along = chord / chord.length();
                        let to_middle = (current + cmd.p0) * 0.5 - cmd.p1;
                        cmd.p1 = cmd.p1 + along * (to_middle.x * along.x + to_middle.y * along.y);
                        disagreement = disagreement.max(radii.abs());
                    }
                }
                if cmd.op == PathOp::MoveTo {
                    start = cmd.p0;
                }
                current = cmd.end_point().unwrap_or(start);
                cmd
            })
            .collect();
        Self::new(cmds).with_uncertainty(self.uncertainty_mm + disagreement)
    }

    /// Exact image under an affine transform. Circular arcs stay circular
    /// under similarities and become elliptical arcs otherwise; nothing is
    /// approximated. Prior uncertainty scales with the transform.
    pub fn transformed(self, transform: Affine2) -> Self {
        let scale = transform.max_scale();
        let mut current = Point::default();
        let mut start = current;
        let cmds = self
            .cmds
            .into_iter()
            .map(|cmd| {
                let transformed = cmd.transformed(transform, current);
                if cmd.op == PathOp::MoveTo {
                    start = cmd.p0;
                }
                current = cmd.end_point().unwrap_or(start);
                transformed
            })
            .collect::<Vec<_>>();
        Self {
            bbox: contour_bbox(&cmds),
            cmds,
            uncertainty_mm: self.uncertainty_mm * scale,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    pub fn segments(&self) -> Segments<'_> {
        segments(&self.cmds)
    }

    /// Signed area enclosed by this contour, preserving arc and cubic
    /// segments without flattening. Counter-clockwise contours are positive
    /// and clockwise contours are negative.
    pub fn signed_area(&self) -> f64 {
        self.segments().map(segment_signed_double_area).sum::<f64>() / 2.0
    }

    /// The same contour with elliptical arcs replaced by chords within
    /// `accuracy`, for writers that only carry lines and circular arcs.
    pub fn flattened_curves(&self, accuracy: GeometryAccuracy) -> Result<Self, AccuracyError> {
        let allowance = accuracy.allowance(self.uncertainty_mm)?;
        let mut cmds = Vec::with_capacity(self.cmds.len());
        let mut current = Point::default();
        let mut start = current;
        let mut added: f64 = 0.0;
        for cmd in &self.cmds {
            match cmd.op {
                PathOp::EllipseTo => {
                    let segment = Segment::Ellipse(cmd.elliptical_arc(current));
                    let (points, error) = segment.chords(allowance)?;
                    added = added.max(error);
                    cmds.extend(points.into_iter().map(PathCmd::line_to));
                }
                _ => cmds.push(*cmd),
            }
            if cmd.op == PathOp::MoveTo {
                start = cmd.p0;
            }
            current = cmd.end_point().unwrap_or(start);
        }
        Ok(Self::new(cmds).with_uncertainty(self.uncertainty_mm + added))
    }
}

/// A resolved geometric segment of a contour, with explicit start points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Line { start: Point, end: Point },
    Arc(Arc),
    Ellipse(EllipticalArc),
}

impl Segment {
    pub fn start(&self) -> Point {
        match *self {
            Self::Line { start, .. } => start,
            Self::Arc(arc) => arc.start,
            Self::Ellipse(arc) => arc.start,
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Self::Line { end, .. } => end,
            Self::Arc(arc) => arc.end,
            Self::Ellipse(arc) => arc.end,
        }
    }

    /// Command continuing a path from this segment's start (or end when reversed).
    /// Reversal preserves curves, changing arc direction.
    pub fn to_path_cmd(self, reverse: bool) -> PathCmd {
        let end = if reverse { self.start() } else { self.end() };
        match self {
            Self::Line { .. } => PathCmd::line_to(end),
            Self::Arc(arc) => PathCmd::arc_to(end, arc.center, arc.clockwise ^ reverse),
            Self::Ellipse(arc) => PathCmd::ellipse_to(
                end,
                arc.center,
                arc.x_axis,
                arc.y_axis,
                arc.clockwise ^ reverse,
            ),
        }
    }

    pub fn bbox(&self) -> BBox {
        match *self {
            Self::Line { start, end } => {
                let mut bbox = BBox::from_point(start);
                bbox.include_point(end);
                bbox
            }
            Self::Arc(arc) => arc.bbox(),
            Self::Ellipse(arc) => arc.bbox(),
        }
    }

    /// The point at parameter `t` in `[0, 1]` along the segment.
    pub fn point_at(&self, t: f64) -> Point {
        match *self {
            Self::Line { start, end } => start + (end - start) * t,
            Self::Arc(arc) => {
                let start_angle = arc.start.angle_from(arc.center);
                let signed_sweep = if arc.clockwise {
                    -arc.sweep_radians()
                } else {
                    arc.sweep_radians()
                };
                arc.point_at(start_angle + signed_sweep * t)
            }
            Self::Ellipse(arc) => arc.point_at(arc.start_angle() + arc.signed_sweep_radians() * t),
        }
    }

    /// Sample the segment at `count` evenly spaced parameters, excluding the
    /// start point and including the end point.
    pub fn sample_points(&self, count: usize, out: &mut Vec<Point>) {
        match *self {
            Self::Line { end, .. } => out.push(end),
            _ => {
                for step in 1..=count {
                    if step == count {
                        out.push(self.end());
                    } else {
                        out.push(self.point_at(step as f64 / count as f64));
                    }
                }
            }
        }
    }

    /// Chord endpoints (excluding the start) approximating the segment within
    /// `max_error_mm`, and the error actually incurred.
    pub fn chords(&self, max_error_mm: f64) -> Result<(Vec<Point>, f64), AccuracyError> {
        match *self {
            Self::Line { end, .. } => Ok((vec![end], 0.0)),
            Self::Arc(arc) => {
                if !tolerance_reaches(arc.radius(), max_error_mm) {
                    return Err(AccuracyError::SubdivisionLimit);
                }
                Ok(arc.chords(max_error_mm))
            }
            Self::Ellipse(arc) => {
                let scale = arc.max_scale();
                let sweep = arc.sweep_radians();
                // A chord of parametric angle δ on the unit circle has
                // sagitta 2·sin²(δ/4); the basis scales it by at most `scale`.
                let ratio = (max_error_mm / (2.0 * scale)).min(1.0);
                let step = 4.0 * ratio.sqrt().asin();
                let count = (sweep / step).ceil().max(1.0);
                if !count.is_finite() || count > 1_000_000.0 {
                    return Err(AccuracyError::SubdivisionLimit);
                }
                let count = count as usize;
                let mut points = Vec::with_capacity(count);
                self.sample_points(count, &mut points);
                let error = 2.0 * scale * (sweep / count as f64 / 4.0).sin().powi(2);
                Ok((points, error))
            }
        }
    }
}

/// Whether a curve of this extent flattens to `tolerance` in a sane number
/// of chords; the count grows with the square root of their ratio.
fn tolerance_reaches(extent: f64, tolerance: f64) -> bool {
    tolerance > 0.0 && (extent / tolerance).sqrt() <= 1_000_000.0
}

fn segment_signed_double_area(segment: Segment) -> f64 {
    match segment {
        Segment::Line { start, end } => cross(start, end),
        Segment::Arc(arc) => {
            let sweep = if arc.clockwise {
                -arc.sweep_radians()
            } else {
                arc.sweep_radians()
            };
            cross(arc.center, arc.end - arc.start) + arc.radius().powi(2) * sweep
        }
        Segment::Ellipse(arc) => {
            cross(arc.center, arc.end - arc.start)
                + cross(arc.x_axis, arc.y_axis) * arc.signed_sweep_radians()
        }
    }
}

fn cross(left: Point, right: Point) -> f64 {
    left.x * right.y - left.y * right.x
}

/// Iterate the geometric segments of a command stream, resolving the current
/// point and closing subpaths back to their start.
pub fn segments(cmds: &[PathCmd]) -> Segments<'_> {
    Segments {
        cmds: cmds.iter(),
        first: None,
        current: None,
    }
}

pub struct Segments<'a> {
    cmds: std::slice::Iter<'a, PathCmd>,
    first: Option<Point>,
    current: Option<Point>,
}

impl Iterator for Segments<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        loop {
            let cmd = self.cmds.next()?;
            match cmd.op {
                PathOp::MoveTo => {
                    self.first = Some(cmd.p0);
                    self.current = Some(cmd.p0);
                }
                PathOp::LineTo | PathOp::ArcTo | PathOp::EllipseTo => {
                    let segment = cmd.segment_from(self.current)?;
                    self.current = Some(segment.end());
                    return Some(segment);
                }
                PathOp::Close => {
                    let (Some(start), Some(end)) = (self.current, self.first) else {
                        continue;
                    };
                    self.current = self.first;
                    if start.distance_to(end) > 0.0 {
                        return Some(Segment::Line { start, end });
                    }
                }
            }
        }
    }
}

pub use crate::geom::stroke::stroke_to_fill;

pub fn contour_bbox(cmds: &[PathCmd]) -> BBox {
    let mut bbox = BBox::empty();
    let mut current = Point::default();
    let mut start = current;
    for cmd in cmds {
        if cmd.op == PathOp::MoveTo {
            start = cmd.p0;
        }
        match cmd.op {
            PathOp::MoveTo | PathOp::LineTo => {
                current = cmd.p0;
                bbox.include_point(cmd.p0);
            }
            PathOp::ArcTo => {
                bbox = bbox.union(Arc::new(current, cmd.p0, cmd.p1, cmd.clockwise).bbox());
                current = cmd.p0;
            }
            PathOp::EllipseTo => {
                bbox = bbox.union(cmd.elliptical_arc(current).bbox());
                current = cmd.p0;
            }
            PathOp::Close => current = start,
        }
    }
    bbox
}

pub(crate) fn validate_cmd_points(name: &str, cmds: &[PathCmd]) -> Result<(), String> {
    for (index, cmd) in cmds.iter().enumerate() {
        if !cmd.p0.is_finite() || !cmd.p1.is_finite() || !cmd.p2.is_finite() || !cmd.p3.is_finite()
        {
            return Err(format!(
                "{name} path command {index} contains non-finite point"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_commands_preserve_forward_and_reversed_curves() {
        for command in [
            PathCmd::line_to(Point::new(-2.0, 1.0)),
            PathCmd::arc_to(Point::new(0.0, 3.0), Point::ZERO, false),
            PathCmd::ellipse_to(
                Point::new(0.0, 1.0),
                Point::ZERO,
                Point::new(3.0, 0.0),
                Point::new(0.0, 1.0),
                false,
            ),
        ] {
            let source = ContourBuf::new(vec![PathCmd::move_to(Point::new(3.0, 0.0)), command]);
            let segment = source.segments().next().unwrap();
            for reverse in [false, true] {
                let rebuilt = ContourBuf::new(vec![
                    PathCmd::move_to(if reverse {
                        segment.end()
                    } else {
                        segment.start()
                    }),
                    segment.to_path_cmd(reverse),
                ]);
                let rebuilt = rebuilt.segments().next().unwrap();
                for t in [0.0, 0.17, 0.63, 1.0] {
                    assert!(
                        rebuilt
                            .point_at(t)
                            .distance_to(segment.point_at(if reverse { 1.0 - t } else { t }))
                            < 1e-12
                    );
                }
            }
        }
    }

    #[test]
    fn closed_arc_continuations_match_explicit_moves_through_preparation() {
        use crate::geom::{ContourSet, Resolution};
        let transform = Affine2 {
            m00: 1.5,
            m01: 0.3,
            m10: -0.2,
            m11: 0.8,
            m02: 10.0,
            m12: -3.0,
        };
        for (start, arc) in [
            (
                Point::new(1.0, 0.0),
                PathCmd::arc_to(Point::new(0.0, 1.0), Point::ZERO, false),
            ),
            (
                Point::new(2.0, 0.0),
                PathCmd::ellipse_to(
                    Point::new(0.0, 1.0),
                    Point::ZERO,
                    Point::new(2.0, 0.0),
                    Point::new(0.0, 1.0),
                    false,
                ),
            ),
        ] {
            let cmds = vec![
                PathCmd::move_to(start),
                PathCmd::line_to(Point::new(0.0, -2.0)),
                PathCmd::line_to(Point::new(-3.0, -2.0)),
                PathCmd::close(),
                arc,
                PathCmd::close(),
            ];
            let mut explicit = cmds.clone();
            explicit.insert(4, PathCmd::move_to(start));
            let actual = ContourBuf::new(cmds);
            let expected = ContourBuf::new(explicit);
            assert_eq!(actual.bbox, expected.bbox);
            let actual_transformed = actual.clone().transformed(transform);
            let expected_transformed = expected.clone().transformed(transform);
            assert_eq!(actual_transformed.cmds[4], expected_transformed.cmds[5]);
            for (actual, expected) in [
                (actual, expected),
                (actual_transformed, expected_transformed),
            ] {
                for flatten in [false, true] {
                    let prepare = |contour: &ContourBuf| {
                        let contour = if flatten {
                            contour
                                .flattened_curves(GeometryAccuracy::new(0.001).unwrap())
                                .unwrap()
                        } else {
                            contour.clone()
                        };
                        ContourSet::from_filled_contours(&[contour], Resolution::default().strict())
                            .unwrap()
                    };
                    assert_eq!(prepare(&actual).rings, prepare(&expected).rings);
                }
            }
        }
    }

    #[test]
    fn prepared_small_circles_have_no_numerical_join_edges() {
        use crate::geom::{ContourSet, Resolution};
        for (diameter, prior) in [0.381, 20.0]
            .into_iter()
            .flat_map(|diameter| [0.0, 0.0005, 0.0008, 0.001, 0.002].map(|prior| (diameter, prior)))
        {
            let source = crate::geom::shapes::circle(diameter)
                .unwrap()
                .with_uncertainty(prior);
            let region =
                ContourSet::from_filled_contours(&[source], Resolution::default().strict())
                    .unwrap();
            for ring in &region.rings {
                for (a, b) in crate::geom::region::ring_edges(ring) {
                    assert!(
                        a.distance_to(b) > crate::geom::tol::EPSILON_MM,
                        "prior={prior}: numerical join {a:?} -> {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn signed_area_preserves_arcs_ellipses_and_cubics() {
        let circle = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(1.0, 0.0), Point::ZERO, false),
            PathCmd::close(),
        ]);
        assert!((circle.signed_area() - std::f64::consts::PI).abs() <= 1e-12);

        let ellipse = circle.clone().transformed(Affine2 {
            m00: 2.0,
            m11: 0.5,
            ..Affine2::IDENTITY
        });
        assert!(ellipse.cmds[1].op == PathOp::EllipseTo);
        assert!((ellipse.signed_area() - std::f64::consts::PI).abs() <= 1e-12);
        let mirrored = circle.transformed(Affine2 {
            m00: -1.0,
            m11: 2.0,
            ..Affine2::IDENTITY
        });
        assert!((mirrored.signed_area() + 2.0 * std::f64::consts::PI).abs() <= 1e-12);
    }

    #[test]
    fn an_arc_whose_radii_disagree_is_recentred_between_its_ends() {
        // A half turn from (5, 0) to (-5.002, 0) about a centre stated at the
        // origin: the start is 5 away, the end 5.002.
        let end = Point::new(-5.002, 0.0);
        let contour = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(5.0, 0.0)),
            PathCmd::arc_to(end, Point::ZERO, false),
        ])
        .with_consistent_arcs();

        let arc = contour.cmds[1];
        assert_eq!(arc.p0, end, "neighbours meet at the ends");
        assert!((arc.p1.x + 0.001).abs() < 1e-12 && arc.p1.y == 0.0);
        assert!((contour.uncertainty_mm - 0.002).abs() < 1e-12);
        assert!(contour.bbox.max.y > 5.0, "still the upper half turn");

        let exact = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(5.0, 0.0)),
            PathCmd::arc_to(Point::new(0.0, 5.0), Point::ZERO, false),
        ]);
        assert_eq!(exact.clone().with_consistent_arcs(), exact);
    }

    #[test]
    fn similarity_transforms_keep_circular_arcs() {
        let circle = crate::geom::shapes::circle(2.0).unwrap();
        let placed = circle.transformed(Affine2::placement(
            Point::new(5.0, 5.0),
            30.0,
            crate::geom::point::Mirror::NONE,
            3.0,
        ));
        assert!(placed.cmds.iter().all(|cmd| cmd.op != PathOp::EllipseTo));
        assert!((placed.bbox.min.x - 2.0).abs() <= 1e-9);
        assert!((placed.bbox.max.y - 8.0).abs() <= 1e-9);
    }

    #[test]
    fn flattening_curves_stays_within_the_requested_error() {
        let ellipse = crate::geom::shapes::ellipse(4.0, 2.0).unwrap();
        for budget in [0.01, 0.001, 0.0001] {
            let flat = ellipse
                .flattened_curves(GeometryAccuracy::new(budget).unwrap())
                .unwrap();
            assert!(flat.cmds.iter().all(|cmd| !cmd.is_curve()));
            assert!(flat.uncertainty_mm <= budget);
            let worst = flat
                .segments()
                .map(|segment| {
                    let mid = segment.point_at(0.5);
                    let radial = ((mid.x / 2.0).powi(2) + mid.y.powi(2)).sqrt();
                    (1.0 - radial).abs()
                })
                .fold(0.0, f64::max);
            // Radial parameter error bounds the geometric error on this ellipse.
            assert!(
                worst <= flat.uncertainty_mm,
                "{worst} > {}",
                flat.uncertainty_mm
            );
        }
    }

    #[test]
    fn segments_resolve_current_point_and_close() {
        let cmds = vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(0.0, 1.0), Point::new(0.0, 0.0), false),
            PathCmd::close(),
        ];

        let segments = segments(&cmds).collect::<Vec<_>>();

        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments[0],
            Segment::Line {
                start: Point::new(0.0, 0.0),
                end: Point::new(1.0, 0.0)
            }
        );
        let Segment::Arc(arc) = segments[1] else {
            panic!("expected arc");
        };
        assert_eq!(arc.start, Point::new(1.0, 0.0));
        assert_eq!(arc.end, Point::new(0.0, 1.0));
        assert_eq!(
            segments[2],
            Segment::Line {
                start: Point::new(0.0, 1.0),
                end: Point::new(0.0, 0.0)
            }
        );
    }
}
