use crate::geom::pattern::{StrokePatternMark, stroke_pattern_marks};
use crate::geom::{AccuracyError, GeometryAccuracy};
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

use crate::geom::affine::Affine2;
use crate::geom::arc::{Arc, EllipticalArc};
use crate::geom::bbox::BBox;
use crate::geom::point::Point;
use crate::geom::style::{LineCap, LineJoin, LinePattern};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathOp {
    #[default]
    MoveTo,
    LineTo,
    ArcTo,
    EllipseTo,
    CubicTo,
    Close,
}

/// One fat path command. Which points are meaningful depends on `op`:
///
/// - `MoveTo`/`LineTo`: `p0` is the target point.
/// - `ArcTo`: `p0` is the arc end, `p1` the center, `clockwise` the direction.
/// - `EllipseTo`: `p0` is the arc end, `p1` the center, `p2` and `p3` the
///   images of the unit x and y axes, `clockwise` the direction. This is the
///   affine image of a circular arc; see [`EllipticalArc`].
/// - `CubicTo`: `p0`/`p1` are control points, `p2` the end point.
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

    pub fn cubic_to(p1: Point, p2: Point, p3: Point) -> Self {
        Self {
            op: PathOp::CubicTo,
            p0: p1,
            p1: p2,
            p2: p3,
            ..Self::default()
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
            PathOp::CubicTo => Some(self.p2),
            PathOp::Close => None,
        }
    }

    /// Whether this command is a curve rather than a line or a move.
    pub fn is_curve(self) -> bool {
        matches!(self.op, PathOp::ArcTo | PathOp::EllipseTo | PathOp::CubicTo)
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

    /// Exact image under an affine transform, given the current point.
    fn transformed(self, transform: Affine2, start: Point) -> Self {
        match self.op {
            PathOp::MoveTo | PathOp::LineTo => Self {
                p0: transform.transform_point(self.p0),
                ..self
            },
            PathOp::CubicTo => Self {
                p0: transform.transform_point(self.p0),
                p1: transform.transform_point(self.p1),
                p2: transform.transform_point(self.p2),
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

    /// Exact image under an affine transform. Circular arcs stay circular
    /// under similarities and become elliptical arcs otherwise; nothing is
    /// approximated. Prior uncertainty scales with the transform.
    pub fn transformed(self, transform: Affine2) -> Self {
        let scale = transform.max_scale();
        let mut current = Point::default();
        let cmds = self
            .cmds
            .into_iter()
            .map(|cmd| {
                let transformed = cmd.transformed(transform, current);
                current = cmd.end_point().unwrap_or(current);
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

    /// The same contour with elliptical arcs and cubics replaced by chords
    /// within `accuracy`, for writers that only carry lines and circular arcs.
    pub fn flattened_curves(&self, accuracy: GeometryAccuracy) -> Result<Self, AccuracyError> {
        let allowance = accuracy.allowance(self.uncertainty_mm)?;
        let mut cmds = Vec::with_capacity(self.cmds.len());
        let mut current = Point::default();
        let mut added: f64 = 0.0;
        for cmd in &self.cmds {
            match cmd.op {
                PathOp::EllipseTo | PathOp::CubicTo => {
                    let start = current;
                    let segment = if cmd.op == PathOp::EllipseTo {
                        Segment::Ellipse(cmd.elliptical_arc(start))
                    } else {
                        Segment::Cubic {
                            start,
                            c1: cmd.p0,
                            c2: cmd.p1,
                            end: cmd.p2,
                        }
                    };
                    let (points, error) = segment.chords(allowance)?;
                    added = added.max(error);
                    cmds.extend(points.into_iter().map(PathCmd::line_to));
                }
                _ => cmds.push(*cmd),
            }
            current = cmd.end_point().unwrap_or(current);
        }
        Ok(Self::new(cmds).with_uncertainty(self.uncertainty_mm + added))
    }
}

/// A resolved geometric segment of a contour, with explicit start points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Line {
        start: Point,
        end: Point,
    },
    Arc(Arc),
    Ellipse(EllipticalArc),
    Cubic {
        start: Point,
        c1: Point,
        c2: Point,
        end: Point,
    },
}

impl Segment {
    pub fn start(&self) -> Point {
        match *self {
            Self::Line { start, .. } | Self::Cubic { start, .. } => start,
            Self::Arc(arc) => arc.start,
            Self::Ellipse(arc) => arc.start,
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Self::Line { end, .. } | Self::Cubic { end, .. } => end,
            Self::Arc(arc) => arc.end,
            Self::Ellipse(arc) => arc.end,
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
            Self::Cubic { start, c1, c2, end } => {
                let mut bbox = BBox::from_point(start);
                bbox.include_point(c1);
                bbox.include_point(c2);
                bbox.include_point(end);
                bbox
            }
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
            Self::Cubic { start, c1, c2, end } => {
                let u = 1.0 - t;
                start * (u * u * u)
                    + c1 * (3.0 * u * u * t)
                    + c2 * (3.0 * u * t * t)
                    + end * (t * t * t)
            }
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

    /// Chord endpoints (excluding the start) approximating a curved segment
    /// within `max_error_mm`, and the error actually incurred. Lines and
    /// circular arcs are returned as their own end point with no error.
    pub fn chords(&self, max_error_mm: f64) -> Result<(Vec<Point>, f64), AccuracyError> {
        match *self {
            Self::Line { end, .. } | Self::Arc(Arc { end, .. }) => Ok((vec![end], 0.0)),
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
            Self::Cubic { start, c1, c2, end } => {
                let mut path = BezPath::new();
                path.move_to(kurbo_point(start));
                path.curve_to(kurbo_point(c1), kurbo_point(c2), kurbo_point(end));
                let mut points = Vec::new();
                kurbo::flatten(path, max_error_mm.max(f64::MIN_POSITIVE), |el| {
                    if let PathEl::LineTo(point) = el {
                        points.push(ir_point(point));
                    }
                });
                if points.len() > 1_000_000 {
                    return Err(AccuracyError::SubdivisionLimit);
                }
                Ok((points, max_error_mm))
            }
        }
    }
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
        Segment::Cubic { start, c1, c2, end } => {
            // Integrate x·dy - y·dx over the cubic in power-basis form.
            let d = start;
            let c = (c1 - start) * 3.0;
            let b = (c2 - c1 * 2.0 + start) * 3.0;
            let a = end - c2 * 3.0 + c1 * 3.0 - start;
            cross(d, c) + cross(d, b) + (cross(d, a) * 3.0 + cross(c, b)) / 3.0 + cross(c, a) / 2.0
                - cross(a, b) / 5.0
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
                PathOp::LineTo => {
                    let start = self.current.unwrap_or(cmd.p0);
                    self.current = Some(cmd.p0);
                    return Some(Segment::Line { start, end: cmd.p0 });
                }
                PathOp::ArcTo => {
                    let start = self.current.unwrap_or(cmd.p0);
                    self.current = Some(cmd.p0);
                    return Some(Segment::Arc(Arc::new(start, cmd.p0, cmd.p1, cmd.clockwise)));
                }
                PathOp::EllipseTo => {
                    let start = self.current.unwrap_or(cmd.p0);
                    self.current = Some(cmd.p0);
                    return Some(Segment::Ellipse(cmd.elliptical_arc(start)));
                }
                PathOp::CubicTo => {
                    let start = self.current.unwrap_or(cmd.p2);
                    self.current = Some(cmd.p2);
                    return Some(Segment::Cubic {
                        start,
                        c1: cmd.p0,
                        c2: cmd.p1,
                        end: cmd.p2,
                    });
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

pub fn contour_bbox(cmds: &[PathCmd]) -> BBox {
    let mut bbox = BBox::empty();
    let mut current = Point::default();
    for cmd in cmds {
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
            PathOp::CubicTo => {
                bbox.include_point(cmd.p0);
                bbox.include_point(cmd.p1);
                bbox.include_point(cmd.p2);
                current = cmd.p2;
            }
            PathOp::Close => {}
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

/// Style for converting a stroked centerline into filled geometry.
///
/// Geometrically this is the Minkowski sum of the source path and the stroke
/// aperture implied by the style. For the normal PCB/Gerber case that aperture
/// is a disk with radius `width / 2`, with caps and joins controlling endpoint
/// and vertex treatment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeToFillStyle {
    pub width: f64,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub pattern: LinePattern,
}

impl StrokeToFillStyle {
    pub fn new(width: f64, line_cap: LineCap, line_join: LineJoin) -> Self {
        Self {
            width,
            line_cap,
            line_join,
            pattern: LinePattern::Solid,
        }
    }
}

impl From<crate::geom::style::StrokeStyle> for StrokeToFillStyle {
    fn from(stroke: crate::geom::style::StrokeStyle) -> Self {
        Self {
            width: stroke.width,
            line_cap: stroke.cap,
            line_join: stroke.join,
            pattern: stroke.pattern,
        }
    }
}

/// Convert stroked centerlines/arcs into filled contours within `accuracy`.
///
/// This is a preparation step: the outline is approximated once, here, and
/// the returned contours record what it cost. Use it for boolean
/// composition, comparison, and targets that cannot represent native
/// strokes. Gerber export should prefer native draw/arc objects.
pub fn stroke_to_fill(
    contours: &[ContourBuf],
    style: StrokeToFillStyle,
    accuracy: GeometryAccuracy,
) -> Result<Option<Vec<ContourBuf>>, AccuracyError> {
    if !style.width.is_finite()
        || contours
            .iter()
            .any(|c| !c.bbox.is_valid() || !c.uncertainty_mm.is_finite() || c.uncertainty_mm < 0.0)
    {
        return Err(AccuracyError::InvalidGeometry("invalid stroke geometry"));
    }
    if style.width <= 0.0 {
        return Ok(None);
    }
    if !matches!(style.pattern, LinePattern::Solid | LinePattern::Erase) {
        let solid_style = StrokeToFillStyle {
            pattern: LinePattern::Solid,
            ..style
        };
        let mut out = Vec::new();
        for contour in contours {
            accuracy.check(contour.uncertainty_mm)?;
            if contour
                .cmds
                .iter()
                .any(|cmd| matches!(cmd.op, PathOp::CubicTo | PathOp::EllipseTo))
            {
                return Err(AccuracyError::InvalidGeometry(
                    "pattern placement requires lines or circular arcs",
                ));
            }
            let segments = contour.segments().collect::<Vec<_>>();
            for mark in stroke_pattern_marks(&segments, style.pattern, style.width) {
                match mark {
                    StrokePatternMark::Dash(segments) => {
                        if let Some(dash) = contour_from_segments(&segments) {
                            out.extend(
                                stroke_to_fill(
                                    &[dash.with_uncertainty(contour.uncertainty_mm)],
                                    solid_style,
                                    accuracy,
                                )?
                                .unwrap_or_default(),
                            );
                        }
                    }
                    StrokePatternMark::Dot(at) => {
                        let dot = crate::geom::shapes::circle(style.width)
                            .expect("positive finite stroke width");
                        out.push(
                            dot.with_uncertainty(contour.uncertainty_mm)
                                .transformed(Affine2::translation(at)),
                        );
                    }
                }
            }
        }
        return Ok((!out.is_empty()).then_some(out));
    }
    let prior = contours
        .iter()
        .map(|c| c.uncertainty_mm)
        .fold(0.0, f64::max);
    let bbox = contours
        .iter()
        .fold(BBox::empty(), |bbox, c| bbox.union(c.bbox))
        .expand(style.width.max(0.0));
    let numeric = crate::geom::accuracy::numerical_error(bbox);
    let remaining = accuracy.allowance(prior + stroke_rounding_error(style) + numeric)?;
    if remaining < f64::EPSILON
        || (bbox.width().max(bbox.height()) / remaining).sqrt() > 1_000_000.0
    {
        return Err(AccuracyError::SubdivisionLimit);
    }
    let mut out = solid_stroke_to_fill(contours, style, remaining / 2.0);
    if let Some(out) = &mut out {
        for contour in out {
            contour.uncertainty_mm += numeric;
            accuracy.check(contour.uncertainty_mm)?;
        }
    }
    Ok(out)
}

// Kurbo's round caps/joins use a fixed unit-circle tolerance, independently
// of the tolerance passed to stroke().
fn stroke_rounding_error(style: StrokeToFillStyle) -> f64 {
    if matches!(style.line_cap, LineCap::Round) || matches!(style.line_join, LineJoin::Round) {
        0.0004 * style.width.max(0.0) / 2.0
    } else {
        0.0
    }
}

fn solid_stroke_to_fill(
    contours: &[ContourBuf],
    style: StrokeToFillStyle,
    accuracy: f64,
) -> Option<Vec<ContourBuf>> {
    let (source, conversion_error) = contours_to_kurbo(contours, accuracy);
    if source.elements().is_empty() {
        return None;
    }
    let stroke = Stroke::new(style.width)
        .with_join(kurbo_join(style.line_join))
        .with_caps(kurbo_cap(style.line_cap));
    let outline = kurbo::stroke(source, &stroke, &StrokeOpts::default(), accuracy);
    let mut out = kurbo_path_to_contours(&outline);
    for contour in &mut out {
        contour.uncertainty_mm = contours
            .iter()
            .map(|c| c.uncertainty_mm)
            .fold(0.0, f64::max)
            + conversion_error
            + stroke_rounding_error(style)
            + accuracy;
        if contour
            .cmds
            .last()
            .is_none_or(|cmd| cmd.op != PathOp::Close)
        {
            contour.cmds.push(PathCmd::close());
            contour.bbox = contour_bbox(&contour.cmds);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn contour_from_segments(segments: &[Segment]) -> Option<ContourBuf> {
    let first = segments.first()?;
    let mut current = first.start();
    let mut cmds = vec![PathCmd::move_to(current)];
    for segment in segments {
        if current != segment.start() {
            current = segment.start();
            cmds.push(PathCmd::move_to(current));
        }
        match *segment {
            Segment::Line { end, .. } => cmds.push(PathCmd::line_to(end)),
            Segment::Arc(arc) => {
                cmds.push(PathCmd::arc_to(arc.end, arc.center, arc.clockwise));
            }
            Segment::Ellipse(arc) => cmds.push(PathCmd::from_elliptical_arc(arc)),
            Segment::Cubic { c1, c2, end, .. } => {
                cmds.push(PathCmd::cubic_to(c1, c2, end));
            }
        }
        current = segment.end();
    }
    Some(ContourBuf::new(cmds))
}

/// Convert contours to a kurbo path, approximating circular and elliptical
/// arcs by cubics whose radial error stays within `accuracy`. Returns the
/// largest conversion error incurred.
pub(crate) fn contours_to_kurbo(contours: &[ContourBuf], accuracy: f64) -> (BezPath, f64) {
    let mut out = BezPath::new();
    let mut current = Point::default();
    let mut conversion_error: f64 = 0.0;
    for contour in contours {
        for cmd in &contour.cmds {
            match cmd.op {
                PathOp::MoveTo => {
                    current = cmd.p0;
                    out.move_to(kurbo_point(cmd.p0));
                }
                PathOp::LineTo => {
                    current = cmd.p0;
                    out.line_to(kurbo_point(cmd.p0));
                }
                PathOp::ArcTo => {
                    let arc = Arc::new(current, cmd.p0, cmd.p1, cmd.clockwise);
                    let radius_mismatch = (arc.radius() - cmd.p0.distance_to(cmd.p1)).abs();
                    conversion_error = conversion_error.max(
                        append_arc_to_kurbo(&mut out, arc.to_elliptical(), accuracy)
                            + radius_mismatch,
                    );
                    current = cmd.p0;
                }
                PathOp::EllipseTo => {
                    conversion_error = conversion_error.max(append_arc_to_kurbo(
                        &mut out,
                        cmd.elliptical_arc(current),
                        accuracy,
                    ));
                    current = cmd.p0;
                }
                PathOp::CubicTo => {
                    current = cmd.p2;
                    out.curve_to(
                        kurbo_point(cmd.p0),
                        kurbo_point(cmd.p1),
                        kurbo_point(cmd.p2),
                    );
                }
                PathOp::Close => out.close_path(),
            }
        }
    }
    (out, conversion_error)
}

/// Append an elliptical arc as tangent-matched cubics and return the radial
/// error bound. A cubic over a unit-circle angle `δ ≤ π/2` errs by at most
/// `δ⁶ / 40000`, and the axis basis scales that by its largest singular
/// value, so the sweep is subdivided until that bound meets `accuracy`.
fn append_arc_to_kurbo(out: &mut BezPath, arc: EllipticalArc, accuracy: f64) -> f64 {
    let scale = arc.max_scale();
    if arc.is_degenerate() || scale == 0.0 {
        out.line_to(kurbo_point(arc.end));
        return 0.0;
    }
    let signed_sweep = arc.signed_sweep_radians();
    let max_angle = (accuracy * 40_000.0 / scale)
        .powf(1.0 / 6.0)
        .min(std::f64::consts::FRAC_PI_2);
    let segment_count = (signed_sweep.abs() / max_angle).ceil().max(1.0) as usize;
    let delta = signed_sweep / segment_count as f64;
    let mut angle = arc.start_angle();
    // Tangent at parameter θ is −x_axis·sin θ + y_axis·cos θ.
    let tangent = |angle: f64| arc.y_axis * angle.cos() - arc.x_axis * angle.sin();

    for _ in 0..segment_count {
        let next_angle = angle + delta;
        let k = 4.0 / 3.0 * (delta / 4.0).tan();
        let p0 = arc.point_at(angle);
        let p3 = arc.point_at(next_angle);
        let c1 = p0 + tangent(angle) * k;
        let c2 = p3 - tangent(next_angle) * k;
        out.curve_to(kurbo_point(c1), kurbo_point(c2), kurbo_point(p3));
        angle = next_angle;
    }
    scale * delta.abs().powi(6) / 40_000.0
}

fn kurbo_path_to_contours(path: &BezPath) -> Vec<ContourBuf> {
    let mut contours = Vec::new();
    let mut cmds = Vec::new();
    let mut bbox = BBox::empty();
    let mut current = Point::default();

    for element in path.iter() {
        match element {
            PathEl::MoveTo(point) => {
                push_kurbo_contour(&mut contours, &mut bbox, &mut cmds);
                current = ir_point(point);
                bbox.include_point(current);
                cmds.push(PathCmd::move_to(current));
            }
            PathEl::LineTo(point) => {
                current = ir_point(point);
                bbox.include_point(current);
                cmds.push(PathCmd::line_to(current));
            }
            PathEl::QuadTo(p1, p2) => {
                let p1 = ir_point(p1);
                let p2 = ir_point(p2);
                let c1 = current + (p1 - current) * (2.0 / 3.0);
                let c2 = p2 + (p1 - p2) * (2.0 / 3.0);
                bbox.include_point(c1);
                bbox.include_point(c2);
                bbox.include_point(p2);
                cmds.push(PathCmd::cubic_to(c1, c2, p2));
                current = p2;
            }
            PathEl::CurveTo(p1, p2, p3) => {
                let p1 = ir_point(p1);
                let p2 = ir_point(p2);
                let p3 = ir_point(p3);
                bbox.include_point(p1);
                bbox.include_point(p2);
                bbox.include_point(p3);
                cmds.push(PathCmd::cubic_to(p1, p2, p3));
                current = p3;
            }
            PathEl::ClosePath => cmds.push(PathCmd::close()),
        }
    }
    push_kurbo_contour(&mut contours, &mut bbox, &mut cmds);
    contours
}

fn push_kurbo_contour(contours: &mut Vec<ContourBuf>, bbox: &mut BBox, cmds: &mut Vec<PathCmd>) {
    if cmds.is_empty() {
        return;
    }
    contours.push(ContourBuf::from_parts(*bbox, std::mem::take(cmds)));
    *bbox = BBox::empty();
}

fn kurbo_cap(line_cap: LineCap) -> Cap {
    match line_cap {
        LineCap::Round => Cap::Round,
        LineCap::Square => Cap::Square,
        LineCap::Butt => Cap::Butt,
    }
}

fn kurbo_join(line_join: LineJoin) -> Join {
    match line_join {
        LineJoin::Round => Join::Round,
        LineJoin::Miter => Join::Miter,
        LineJoin::Bevel => Join::Bevel,
    }
}

pub(crate) fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(point.x, point.y)
}

pub(crate) fn ir_point(point: kurbo::Point) -> Point {
    Point::new(point.x, point.y)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let curved = ContourBuf::new(vec![
            PathCmd::move_to(Point::ZERO),
            PathCmd::cubic_to(
                Point::new(0.0, 1.0),
                Point::new(1.0, 1.0),
                Point::new(1.0, 0.0),
            ),
            PathCmd::close(),
        ]);
        assert!((curved.signed_area() + 0.6).abs() <= 1e-12);
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
    fn stroke_to_fill_rejects_non_positive_width() {
        let accuracy = GeometryAccuracy::default();

        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(1.0, 0.0))];

        assert!(
            stroke_to_fill(
                &source,
                StrokeToFillStyle::new(0.0, LineCap::Round, LineJoin::Round),
                accuracy
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn stroke_to_fill_expands_centerline_by_half_width() {
        let accuracy = GeometryAccuracy::default();

        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(10.0, 0.0))];
        let fill = stroke_to_fill(
            &source,
            StrokeToFillStyle::new(2.0, LineCap::Butt, LineJoin::Round),
            accuracy,
        )
        .unwrap()
        .expect("stroke should expand to fill geometry");
        let bbox = fill
            .iter()
            .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox));

        assert!((bbox.min.x - 0.0).abs() <= 1e-9);
        assert!((bbox.min.y + 1.0).abs() <= 1e-9);
        assert!((bbox.max.x - 10.0).abs() <= 1e-9);
        assert!((bbox.max.y - 1.0).abs() <= 1e-9);
        assert!(fill.iter().all(|contour| {
            contour
                .cmds
                .last()
                .is_some_and(|cmd| cmd.op == PathOp::Close)
        }));
    }

    #[test]
    fn patterned_strokes_preserve_input_error() {
        let source = line_contour(Point::ZERO, Point::new(20.0, 0.0)).with_uncertainty(0.001);
        let mut style = StrokeToFillStyle::new(1.0, LineCap::Round, LineJoin::Round);
        style.pattern = LinePattern::Phantom;
        let fill = stroke_to_fill(&[source], style, GeometryAccuracy::default())
            .unwrap()
            .unwrap();
        assert!(fill.iter().all(|contour| contour.uncertainty_mm >= 0.001));
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

    fn line_contour(start: Point, end: Point) -> ContourBuf {
        ContourBuf::new(vec![PathCmd::move_to(start), PathCmd::line_to(end)])
    }
}
