use crate::common::*;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

pub type PolygonContour = Vec<[f64; 2]>;

#[derive(Debug, Clone)]
pub struct PathPayload {
    pub bbox: BBox,
    pub cmds: Vec<PathCmd>,
}

impl From<(BBox, Vec<PathCmd>)> for PathPayload {
    fn from((bbox, cmds): (BBox, Vec<PathCmd>)) -> Self {
        Self { bbox, cmds }
    }
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PathOp {
    #[default]
    MoveTo,
    LineTo,
    ArcTo,
    CubicTo,
    Close,
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
                bbox.include_circular_arc(current, cmd.p0, cmd.p1, cmd.clockwise);
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

pub fn outline_stroke(
    payloads: &[PathPayload],
    width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
) -> Option<Vec<PathPayload>> {
    if width <= 0.0 {
        return None;
    }
    let source = payloads_to_kurbo(payloads);
    if source.elements().is_empty() {
        return None;
    }
    let stroke = Stroke::new(width)
        .with_join(kurbo_join(line_join))
        .with_caps(kurbo_cap(line_cap));
    let outline = kurbo::stroke(source, &stroke, &StrokeOpts::default(), 0.01);
    let contours = kurbo_path_to_payloads(&outline);
    (!contours.is_empty()).then_some(contours)
}

pub fn payloads_to_polygon_contours(payloads: &[PathPayload]) -> Vec<PolygonContour> {
    let bez_path = payloads_to_kurbo(payloads);
    let mut contours = Vec::new();
    let mut current = Vec::new();
    kurbo::flatten(bez_path, 0.005, |element| match element {
        PathEl::MoveTo(point) => {
            push_polygon_contour(&mut contours, &mut current);
            current.push([point.x, point.y]);
        }
        PathEl::LineTo(point) => current.push([point.x, point.y]),
        PathEl::ClosePath => push_polygon_contour(&mut contours, &mut current),
        PathEl::QuadTo(..) | PathEl::CurveTo(..) => unreachable!("kurbo::flatten emits lines"),
    });
    push_polygon_contour(&mut contours, &mut current);
    contours
}

pub fn simplify_polygon_contours(
    contours: Vec<PolygonContour>,
    fill_rule: FillRule,
) -> Vec<PolygonContour> {
    polygon_shapes_to_polygon_contours(contours.simplify_shape(overlay_fill_rule(fill_rule)))
}

pub fn union_contours(contours: Vec<PolygonContour>, fill_rule: FillRule) -> Vec<PolygonContour> {
    simplify_polygon_contours(contours, fill_rule)
}

pub fn difference_contours(
    subject: Vec<PolygonContour>,
    cutters: Vec<PolygonContour>,
) -> Vec<PolygonContour> {
    if subject.is_empty() || cutters.is_empty() {
        return subject;
    }
    polygon_shapes_to_polygon_contours(subject.overlay(
        &cutters,
        OverlayRule::Difference,
        OverlayFillRule::NonZero,
    ))
}

pub fn polygon_contours_to_payloads(contours: Vec<PolygonContour>) -> Vec<PathPayload> {
    contours
        .into_iter()
        .filter_map(polygon_contour_to_payload)
        .collect()
}

pub fn overlay_fill_rule(fill_rule: FillRule) -> OverlayFillRule {
    match fill_rule {
        FillRule::EvenOdd => OverlayFillRule::EvenOdd,
        FillRule::NonZero => OverlayFillRule::NonZero,
    }
}

pub fn polygon_shapes_to_polygon_contours(shapes: Vec<Vec<PolygonContour>>) -> Vec<PolygonContour> {
    shapes.into_iter().flatten().collect()
}

fn payloads_to_kurbo(payloads: &[PathPayload]) -> BezPath {
    let mut out = BezPath::new();
    let mut current = Point::default();
    for payload in payloads {
        for cmd in &payload.cmds {
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
    let radius = start.distance_to(center);
    if radius == 0.0 {
        out.line_to(kurbo_point(end));
        return;
    }

    let sweep = arc_sweep_radians(start, end, center, clockwise);
    let signed_sweep = if clockwise { -sweep } else { sweep };
    let segment_count = (signed_sweep.abs() / std::f64::consts::FRAC_PI_2)
        .ceil()
        .max(1.0) as usize;
    let delta = signed_sweep / segment_count as f64;
    let mut angle = start.angle_from(center);

    for _ in 0..segment_count {
        let next_angle = angle + delta;
        let k = 4.0 / 3.0 * (delta / 4.0).tan();
        let p0 = Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
        let p3 = Point::new(
            center.x + radius * next_angle.cos(),
            center.y + radius * next_angle.sin(),
        );
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

fn kurbo_path_to_payloads(path: &BezPath) -> Vec<PathPayload> {
    let mut contours = Vec::new();
    let mut cmds = Vec::new();
    let mut bbox = BBox::empty();
    let mut current = Point::default();

    for element in path.iter() {
        match element {
            PathEl::MoveTo(point) => {
                push_kurbo_payload(&mut contours, &mut bbox, &mut cmds);
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
                let c1 = Point::new(
                    current.x + (p1.x - current.x) * 2.0 / 3.0,
                    current.y + (p1.y - current.y) * 2.0 / 3.0,
                );
                let c2 = Point::new(
                    p2.x + (p1.x - p2.x) * 2.0 / 3.0,
                    p2.y + (p1.y - p2.y) * 2.0 / 3.0,
                );
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
    push_kurbo_payload(&mut contours, &mut bbox, &mut cmds);
    contours
}

fn push_kurbo_payload(contours: &mut Vec<PathPayload>, bbox: &mut BBox, cmds: &mut Vec<PathCmd>) {
    if cmds.is_empty() {
        return;
    }
    contours.push(PathPayload {
        bbox: *bbox,
        cmds: std::mem::take(cmds),
    });
    *bbox = BBox::empty();
}

fn push_polygon_contour(out: &mut Vec<PolygonContour>, contour: &mut PolygonContour) {
    if contour.first() == contour.last() {
        contour.pop();
    }
    if contour.len() >= 3 {
        out.push(std::mem::take(contour));
    } else {
        contour.clear();
    }
}

fn polygon_contour_to_payload(contour: PolygonContour) -> Option<PathPayload> {
    if contour.len() < 3 {
        return None;
    }
    let mut bbox = BBox::empty();
    let mut cmds = Vec::with_capacity(contour.len() + 1);
    for (index, [x, y]) in contour.into_iter().enumerate() {
        let point = Point::new(x, y);
        bbox.include_point(point);
        if index == 0 {
            cmds.push(PathCmd::move_to(point));
        } else {
            cmds.push(PathCmd::line_to(point));
        }
    }
    cmds.push(PathCmd::close());
    Some(PathPayload { bbox, cmds })
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

fn kurbo_point(point: Point) -> kurbo::Point {
    kurbo::Point::new(point.x, point.y)
}

fn ir_point(point: kurbo::Point) -> Point {
    Point::new(point.x, point.y)
}
