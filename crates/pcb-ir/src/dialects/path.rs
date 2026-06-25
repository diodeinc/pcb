use crate::common::*;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::simplify::SimplifyShape;
use i_overlay::float::single::SingleFloatOverlay;
use kurbo::{BezPath, Cap, Join, PathEl, Stroke, StrokeOpts};

pub type PolygonContour = Vec<[f64; 2]>;
pub type PolygonShape = Vec<PolygonContour>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintOp {
    Dark,
    Clear,
}

#[derive(Debug, Clone, Default)]
pub struct ContourImage {
    pub bbox: BBox,
    pub contours: Vec<PolygonContour>,
}

impl ContourImage {
    pub fn new(contours: Vec<PolygonContour>) -> Self {
        Self {
            bbox: polygon_contours_bbox(&contours),
            contours,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }
}

/// Filled planar region plus the geometric assumptions needed to operate on
/// it. This keeps boolean/dilation code at the path dialect level instead of
/// spreading raw contour plumbing through format-specific code.
#[derive(Debug, Clone)]
pub struct ContourSet {
    pub bbox: BBox,
    pub contours: Vec<PolygonContour>,
    pub fill_rule: FillRule,
    pub tolerance: f64,
}

impl ContourSet {
    pub fn new(contours: Vec<PolygonContour>, fill_rule: FillRule, tolerance: f64) -> Self {
        let contours =
            filter_significant_contours(simplify_polygon_contours(contours, fill_rule), tolerance);
        Self {
            bbox: polygon_contours_bbox(&contours),
            contours,
            fill_rule,
            tolerance,
        }
    }

    pub fn empty(fill_rule: FillRule, tolerance: f64) -> Self {
        Self::new(Vec::new(), fill_rule, tolerance)
    }

    pub fn from_payloads(payloads: &[PathPayload], fill_rule: FillRule, tolerance: f64) -> Self {
        Self::new(payloads_to_polygon_contours(payloads), fill_rule, tolerance)
    }

    pub fn rectangle(bbox: BBox, fill_rule: FillRule, tolerance: f64) -> Self {
        if bbox.is_empty() {
            return Self::empty(fill_rule, tolerance);
        }
        Self::from_payloads(&[rectangle_payload(bbox)], fill_rule, tolerance)
    }

    pub fn is_empty(&self) -> bool {
        self.contours.is_empty()
    }

    pub fn union(mut self, other: &Self) -> Self {
        debug_assert_eq!(self.fill_rule, other.fill_rule);
        self.contours.extend(other.contours.clone());
        Self::new(self.contours, self.fill_rule, self.tolerance)
    }

    pub fn difference(self, cutters: &Self) -> Self {
        debug_assert_eq!(self.fill_rule, cutters.fill_rule);
        Self::new(
            difference_contours(self.contours, cutters.contours.clone()),
            self.fill_rule,
            self.tolerance,
        )
    }

    pub fn intersection(self, clip: &Self) -> Self {
        debug_assert_eq!(self.fill_rule, clip.fill_rule);
        Self::new(
            intersection_contours(self.contours, clip.contours.clone()),
            self.fill_rule,
            self.tolerance,
        )
    }

    pub fn disk_dilate(self, radius: f64) -> Self {
        Self::new(
            disk_dilate_contours(self.contours, radius, self.fill_rule),
            self.fill_rule,
            self.tolerance,
        )
    }

    pub fn to_payloads(&self) -> Vec<PathPayload> {
        polygon_contours_to_payloads(self.contours.clone())
    }
}

#[derive(Debug, Default)]
pub struct PaintComposer {
    image: Vec<PolygonContour>,
    run: Vec<PolygonContour>,
    run_op: Option<PaintOp>,
}

impl PaintComposer {
    pub fn push(&mut self, op: PaintOp, mut contours: Vec<PolygonContour>) {
        if contours.is_empty() {
            return;
        }
        if self.run_op != Some(op) {
            self.flush_run();
            self.run_op = Some(op);
        }
        self.run.append(&mut contours);
    }

    pub fn finish(mut self) -> Vec<PolygonContour> {
        self.flush_run();
        self.image
    }

    pub fn finish_image(self) -> ContourImage {
        ContourImage::new(self.finish())
    }

    fn flush_run(&mut self) {
        let Some(op) = self.run_op.take() else {
            return;
        };
        if self.run.is_empty() {
            return;
        }

        match op {
            PaintOp::Dark => {
                let mut contours = std::mem::take(&mut self.image);
                contours.append(&mut self.run);
                self.image = union_contours(contours, FillRule::NonZero);
            }
            PaintOp::Clear => {
                if self.image.is_empty() {
                    self.run.clear();
                } else {
                    let cutters = union_contours(std::mem::take(&mut self.run), FillRule::NonZero);
                    self.image = difference_contours(std::mem::take(&mut self.image), cutters);
                }
            }
        }
    }
}

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

pub fn transform_cmds(
    cmds: impl IntoIterator<Item = PathCmd>,
    transform: Affine2,
) -> (BBox, Vec<PathCmd>) {
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
                bbox.include_circular_arc(
                    transform.transform_point(start),
                    transformed.p0,
                    transformed.p1,
                    transformed.clockwise,
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

    (bbox, transformed_cmds)
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

/// Convert stroked centerlines/arcs into filled contours.
///
/// Use this for rendering, boolean composition, comparison, and fallback
/// targets that cannot represent native strokes. Gerber export should prefer
/// native draw/arc objects where possible.
pub fn stroke_to_fill(
    payloads: &[PathPayload],
    style: StrokeToFillStyle,
) -> Option<Vec<PathPayload>> {
    if style.width <= 0.0 {
        return None;
    }
    let source = payloads_to_kurbo(payloads);
    if source.elements().is_empty() {
        return None;
    }
    let stroke = Stroke::new(style.width)
        .with_join(kurbo_join(style.line_join))
        .with_caps(kurbo_cap(style.line_cap));
    let outline = kurbo::stroke(source, &stroke, &StrokeOpts::default(), 0.01);
    let mut contours = kurbo_path_to_payloads(&outline);
    for contour in &mut contours {
        if contour
            .cmds
            .last()
            .is_none_or(|cmd| cmd.op != PathOp::Close)
        {
            contour.cmds.push(PathCmd::close());
            contour.bbox = contour_bbox(&contour.cmds);
        }
    }
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
    polygon_shapes_to_polygon_contours(difference_contour_shapes(subject, cutters))
}

pub fn intersection_contours(
    subject: Vec<PolygonContour>,
    clip: Vec<PolygonContour>,
) -> Vec<PolygonContour> {
    if subject.is_empty() || clip.is_empty() {
        return Vec::new();
    }
    polygon_shapes_to_polygon_contours(subject.overlay(
        &clip,
        OverlayRule::Intersect,
        OverlayFillRule::NonZero,
    ))
}

pub fn difference_contour_shapes(
    subject: Vec<PolygonContour>,
    cutters: Vec<PolygonContour>,
) -> Vec<PolygonShape> {
    if subject.is_empty() || cutters.is_empty() {
        return subject.simplify_shape(OverlayFillRule::NonZero);
    }
    subject.overlay(&cutters, OverlayRule::Difference, OverlayFillRule::NonZero)
}

/// Approximate the Minkowski sum of filled contours and a disk.
///
/// This is the standard "buffer out" operation used for manufacturability
/// checks. It unions the original filled region with a round stroke around its
/// boundary, then simplifies with the requested fill rule.
pub fn disk_dilate_contours(
    contours: Vec<PolygonContour>,
    radius: f64,
    fill_rule: FillRule,
) -> Vec<PolygonContour> {
    let contours = simplify_polygon_contours(contours, fill_rule);
    if contours.is_empty() || radius <= 0.0 {
        return contours;
    }

    let mut dilated = contours.clone();
    let boundary = polygon_contours_to_payloads(contours);
    if let Some(stroke) = stroke_to_fill(
        &boundary,
        StrokeToFillStyle::new(2.0 * radius, LineCap::Round, LineJoin::Round),
    ) {
        dilated.extend(payloads_to_polygon_contours(&stroke));
    }
    union_contours(dilated, fill_rule)
}

pub fn polygon_contours_to_payloads(contours: Vec<PolygonContour>) -> Vec<PathPayload> {
    contours
        .into_iter()
        .filter_map(polygon_contour_to_payload)
        .collect()
}

pub fn polygon_contours_bbox(contours: &[PolygonContour]) -> BBox {
    contours
        .iter()
        .flat_map(|contour| contour.iter())
        .fold(BBox::empty(), |mut bbox, &[x, y]| {
            bbox.include_point(Point::new(x, y));
            bbox
        })
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

fn filter_significant_contours(
    mut contours: Vec<PolygonContour>,
    tolerance: f64,
) -> Vec<PolygonContour> {
    if tolerance > 0.0 {
        let min_area = tolerance.powi(2);
        contours.retain(|contour| polygon_contour_area(contour).abs() > min_area);
    }
    contours
}

fn polygon_contour_area(contour: &PolygonContour) -> f64 {
    if contour.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for index in 0..contour.len() {
        let [x0, y0] = contour[index];
        let [x1, y1] = contour[(index + 1) % contour.len()];
        area += x0 * y1 - x1 * y0;
    }
    area / 2.0
}

fn rectangle_payload(bbox: BBox) -> PathPayload {
    let cmds = vec![
        PathCmd::move_to(Point::new(bbox.min.x, bbox.min.y)),
        PathCmd::line_to(Point::new(bbox.max.x, bbox.min.y)),
        PathCmd::line_to(Point::new(bbox.max.x, bbox.max.y)),
        PathCmd::line_to(Point::new(bbox.min.x, bbox.max.y)),
        PathCmd::close(),
    ];
    PathPayload { bbox, cmds }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_to_fill_rejects_non_positive_width() {
        let source = vec![line_payload(Point::new(0.0, 0.0), Point::new(1.0, 0.0))];

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
        let source = vec![line_payload(Point::new(0.0, 0.0), Point::new(10.0, 0.0))];
        let fill = stroke_to_fill(
            &source,
            StrokeToFillStyle::new(2.0, LineCap::Butt, LineJoin::Round),
        )
        .expect("stroke should expand to fill geometry");
        let bbox = fill
            .iter()
            .fold(BBox::empty(), |bbox, payload| bbox.union(payload.bbox));

        assert_close(bbox.min.x, 0.0);
        assert_close(bbox.min.y, -1.0);
        assert_close(bbox.max.x, 10.0);
        assert_close(bbox.max.y, 1.0);
        assert!(fill.iter().all(|payload| {
            payload
                .cmds
                .last()
                .is_some_and(|cmd| cmd.op == PathOp::Close)
        }));
    }

    #[test]
    fn contour_set_composes_region_operations() {
        let outer = ContourSet::rectangle(rect(0.0, 0.0, 10.0, 10.0), FillRule::NonZero, 0.001);
        let inner = ContourSet::rectangle(rect(3.0, 3.0, 7.0, 7.0), FillRule::NonZero, 0.001);
        let clip = ContourSet::rectangle(rect(5.0, 0.0, 10.0, 10.0), FillRule::NonZero, 0.001);

        let ring = outer.difference(&inner);
        let clipped = ring.intersection(&clip);
        let expanded = clipped.disk_dilate(0.5);

        assert!(!expanded.is_empty());
        assert_close(expanded.bbox.min.x, 4.5);
        assert_close(expanded.bbox.max.x, 10.5);
    }

    fn line_payload(start: Point, end: Point) -> PathPayload {
        let mut bbox = BBox::from_point(start);
        bbox.include_point(end);
        PathPayload {
            bbox,
            cmds: vec![PathCmd::move_to(start), PathCmd::line_to(end)],
        }
    }

    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox {
            min: Point::new(min_x, min_y),
            max: Point::new(max_x, max_y),
        }
    }

    fn assert_close(left: f64, right: f64) {
        assert!(
            (left - right).abs() <= 1e-9,
            "expected {left} to be close to {right}"
        );
    }
}
