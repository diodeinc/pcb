use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

use crate::geom::arc::Arc;
use crate::geom::bbox::BBox;
use crate::geom::point::Point;
use crate::geom::style::{LineCap, LineJoin};
use crate::geom::tol;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathOp {
    #[default]
    MoveTo,
    LineTo,
    ArcTo,
    CubicTo,
    Close,
}

/// One fat path command. Which points are meaningful depends on `op`:
///
/// - `MoveTo`/`LineTo`: `p0` is the target point.
/// - `ArcTo`: `p0` is the arc end, `p1` the center, `clockwise` the direction.
/// - `CubicTo`: `p0`/`p1` are control points, `p2` the end point.
/// - `Close`: no points.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PathCmd {
    pub op: PathOp,
    pub p0: Point,
    pub p1: Point,
    pub p2: Point,
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
            PathOp::MoveTo | PathOp::LineTo | PathOp::ArcTo => Some(self.p0),
            PathOp::CubicTo => Some(self.p2),
            PathOp::Close => None,
        }
    }
}

/// An owned contour: a command list plus its bounding box.
///
/// This is the detached form of an arena [`crate::geom::Contour`] record, used
/// to move contours between documents and geometry passes.
#[derive(Debug, Clone, Default)]
pub struct ContourBuf {
    pub bbox: BBox,
    pub cmds: Vec<PathCmd>,
}

impl ContourBuf {
    /// Build from commands, computing the bounding box.
    pub fn new(cmds: Vec<PathCmd>) -> Self {
        Self {
            bbox: contour_bbox(&cmds),
            cmds,
        }
    }

    /// Build from commands with a precomputed bounding box.
    pub fn from_parts(bbox: BBox, cmds: Vec<PathCmd>) -> Self {
        Self { bbox, cmds }
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    pub fn segments(&self) -> Segments<'_> {
        segments(&self.cmds)
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
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Self::Line { end, .. } | Self::Cubic { end, .. } => end,
            Self::Arc(arc) => arc.end,
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
            Self::Cubic { start, c1, c2, end } => {
                let mut bbox = BBox::from_point(start);
                bbox.include_point(c1);
                bbox.include_point(c2);
                bbox.include_point(end);
                bbox
            }
        }
    }

    /// Sample the segment at `count` evenly spaced parameters, excluding the
    /// start point and including the end point. Lines and arcs are exact;
    /// cubics are evaluated on the Bezier polynomial.
    pub fn sample_points(&self, count: usize, out: &mut Vec<Point>) {
        match *self {
            Self::Line { end, .. } => out.push(end),
            Self::Arc(arc) => {
                let sweep = arc.sweep_radians();
                let signed = if arc.clockwise { -sweep } else { sweep };
                let start_angle = arc.start.angle_from(arc.center);
                for step in 1..=count {
                    let t = step as f64 / count as f64;
                    if step == count {
                        out.push(arc.end);
                    } else {
                        out.push(arc.point_at(start_angle + signed * t));
                    }
                }
            }
            Self::Cubic { start, c1, c2, end } => {
                for step in 1..=count {
                    let t = step as f64 / count as f64;
                    if step == count {
                        out.push(end);
                    } else {
                        let u = 1.0 - t;
                        let point = start * (u * u * u)
                            + c1 * (3.0 * u * u * t)
                            + c2 * (3.0 * u * t * t)
                            + end * (t * t * t);
                        out.push(point);
                    }
                }
            }
        }
    }
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

pub fn transform_cmds(
    cmds: impl IntoIterator<Item = PathCmd>,
    transform: crate::geom::affine::Affine2,
) -> ContourBuf {
    let mut bbox = BBox::empty();
    let mut current = Point::default();
    let mut transformed_cmds = Vec::new();

    for cmd in cmds {
        let start = current;
        let mut transformed = cmd;
        transformed.p0 = transform.transform_point(cmd.p0);
        transformed.p1 = transform.transform_point(cmd.p1);
        if cmd.op != PathOp::ArcTo {
            transformed.p2 = transform.transform_point(cmd.p2);
        } else if transform.determinant() < 0.0 {
            transformed.clockwise = !cmd.clockwise;
        }

        match cmd.op {
            PathOp::MoveTo | PathOp::LineTo => {
                current = cmd.p0;
                bbox.include_point(transformed.p0);
            }
            PathOp::ArcTo => {
                bbox = bbox.union(
                    Arc::new(
                        transform.transform_point(start),
                        transformed.p0,
                        transformed.p1,
                        transformed.clockwise,
                    )
                    .bbox(),
                );
                current = cmd.p0;
            }
            PathOp::CubicTo => {
                bbox.include_point(transformed.p0);
                bbox.include_point(transformed.p1);
                bbox.include_point(transformed.p2);
                current = cmd.p2;
            }
            PathOp::Close => {}
        }

        transformed_cmds.push(transformed);
    }

    ContourBuf::from_parts(bbox, transformed_cmds)
}

pub(crate) fn validate_cmd_points(name: &str, cmds: &[PathCmd]) -> Result<(), String> {
    for (index, cmd) in cmds.iter().enumerate() {
        if !cmd.p0.is_finite() || !cmd.p1.is_finite() || !cmd.p2.is_finite() {
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
}

impl StrokeToFillStyle {
    pub fn new(width: f64, line_cap: LineCap, line_join: LineJoin) -> Self {
        Self {
            width,
            line_cap,
            line_join,
        }
    }
}

impl From<crate::geom::style::StrokeStyle> for StrokeToFillStyle {
    fn from(stroke: crate::geom::style::StrokeStyle) -> Self {
        Self::new(stroke.width, stroke.cap, stroke.join)
    }
}

/// Convert stroked centerlines/arcs into filled contours.
///
/// Use this for rendering, boolean composition, comparison, and fallback
/// targets that cannot represent native strokes. Gerber export should prefer
/// native draw/arc objects where possible.
pub fn stroke_to_fill(
    contours: &[ContourBuf],
    style: StrokeToFillStyle,
) -> Option<Vec<ContourBuf>> {
    if style.width <= 0.0 {
        return None;
    }
    let source = contours_to_kurbo(contours);
    if source.elements().is_empty() {
        return None;
    }
    let stroke = Stroke::new(style.width)
        .with_join(kurbo_join(style.line_join))
        .with_caps(kurbo_cap(style.line_cap));
    let outline = kurbo::stroke(
        source,
        &stroke,
        &StrokeOpts::default(),
        tol::STROKE_OUTLINE_MM,
    );
    let mut out = kurbo_path_to_contours(&outline);
    for contour in &mut out {
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

pub(crate) fn contours_to_kurbo(contours: &[ContourBuf]) -> BezPath {
    let mut out = BezPath::new();
    let mut current = Point::default();
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
                    append_arc_to_kurbo(&mut out, current, cmd.p0, cmd.p1, cmd.clockwise);
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
    out
}

fn append_arc_to_kurbo(
    out: &mut BezPath,
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
) {
    let arc = Arc::new(start, end, center, clockwise);
    let radius = arc.radius();
    if radius == 0.0 {
        out.line_to(kurbo_point(end));
        return;
    }

    let sweep = arc.sweep_radians();
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let segment_count = (signed_sweep.abs() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    let delta = signed_sweep / segment_count as f64;
    let mut angle = start.angle_from(center);

    for _ in 0..segment_count {
        let next_angle = angle + delta;
        let k = 4.0 / 3.0 * (delta / 4.0).tan();
        let p0 = arc.point_at(angle);
        let p3 = arc.point_at(next_angle);
        let c1 = Point::new(
            p0.x - radius * angle.sin() * k,
            p0.y + radius * angle.cos() * k,
        );
        let c2 = Point::new(
            p3.x + radius * next_angle.sin() * k,
            p3.y - radius * next_angle.cos() * k,
        );
        out.curve_to(kurbo_point(c1), kurbo_point(c2), kurbo_point(p3));
        angle = next_angle;
    }
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
    fn stroke_to_fill_rejects_non_positive_width() {
        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(1.0, 0.0))];

        assert!(
            stroke_to_fill(
                &source,
                StrokeToFillStyle::new(0.0, LineCap::Round, LineJoin::Round)
            )
            .is_none()
        );
    }

    #[test]
    fn stroke_to_fill_expands_centerline_by_half_width() {
        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(10.0, 0.0))];
        let fill = stroke_to_fill(
            &source,
            StrokeToFillStyle::new(2.0, LineCap::Butt, LineJoin::Round),
        )
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
