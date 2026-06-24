use std::fmt;

use crate::common::{FillRule, LineCap, LineJoin, Point};
use crate::dialects::ipc::{GeometryDocument, GeometryPathPaintClass};
use crate::dialects::path::{
    PathCmd, PathOp, PathPayload, StrokeToFillStyle, contour_bbox, payloads_to_polygon_contours,
    polygon_contours_to_payloads, simplify_polygon_contours, stroke_to_fill,
};

pub const DEFAULT_ROUTE_TOOL_DIAMETER_MM: f64 = 1.0;
pub const DEFAULT_RELIEF_TOLERANCE_MM: f64 = 0.01;

#[derive(Debug, Clone)]
pub struct VScoreReliefInput {
    pub board_boundary: Vec<PathPayload>,
    pub score_lines: Vec<VScoreLine>,
    pub tool_diameter_mm: f64,
    pub tolerance_mm: f64,
}

impl VScoreReliefInput {
    pub fn new(board_boundary: Vec<PathPayload>, score_lines: Vec<VScoreLine>) -> Self {
        Self {
            board_boundary,
            score_lines,
            tool_diameter_mm: DEFAULT_ROUTE_TOOL_DIAMETER_MM,
            tolerance_mm: DEFAULT_RELIEF_TOLERANCE_MM,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VScoreLine {
    pub start: Point,
    pub end: Point,
    pub width: f64,
}

#[derive(Debug, Clone)]
pub struct RouteRelief {
    pub boundary_path: PathPayload,
    pub toolpath: PathPayload,
    pub contours: Vec<PathPayload>,
    pub tool_diameter_mm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VScoreReliefError {
    EmptyScoreLines,
    InvalidToolDiameter(f64),
    InvalidTolerance(f64),
    EmptyBoundary,
    InvalidBoundary(&'static str),
}

impl fmt::Display for VScoreReliefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScoreLines => write!(f, "V-score relief score lines are empty"),
            Self::InvalidToolDiameter(value) => {
                write!(
                    f,
                    "V-score relief tool diameter must be positive; got {value}"
                )
            }
            Self::InvalidTolerance(value) => {
                write!(f, "V-score relief tolerance must be positive; got {value}")
            }
            Self::EmptyBoundary => write!(f, "V-score relief boundary is empty"),
            Self::InvalidBoundary(message) => {
                write!(f, "V-score relief boundary is invalid: {message}")
            }
        }
    }
}

impl std::error::Error for VScoreReliefError {}

pub fn vscore_route_reliefs(
    input: &VScoreReliefInput,
) -> Result<Vec<RouteRelief>, VScoreReliefError> {
    if input.score_lines.is_empty() {
        return Err(VScoreReliefError::EmptyScoreLines);
    }
    if !input.tool_diameter_mm.is_finite() || input.tool_diameter_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidToolDiameter(
            input.tool_diameter_mm,
        ));
    }
    if !input.tolerance_mm.is_finite() || input.tolerance_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidTolerance(input.tolerance_mm));
    }
    if input.board_boundary.is_empty() {
        return Err(VScoreReliefError::EmptyBoundary);
    }

    let mut reliefs = Vec::new();
    for boundary in &input.board_boundary {
        append_boundary_reliefs(boundary, input, &mut reliefs)?;
    }

    Ok(reliefs)
}

pub fn vscore_lines_for<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
) -> Vec<VScoreLine> {
    let mut lines = Vec::new();
    for feature in doc.features.iter().filter(|feature| feature.is_vcut()) {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            if path.paint_class().ok().flatten() != Some(GeometryPathPaintClass::Stroked) {
                continue;
            }
            let line_start = lines.len();
            append_path_line_segments(doc, path.contour_start, path.contour_count, &mut lines);
            if feature.stroke_width > 0.0 {
                for line in &mut lines[line_start..] {
                    line.width = feature.stroke_width;
                }
            }
        }
    }
    lines
}

fn append_path_line_segments<Symbol, LayerFunction>(
    doc: &GeometryDocument<Symbol, LayerFunction>,
    contour_start: u32,
    contour_count: u32,
    lines: &mut Vec<VScoreLine>,
) {
    for contour in &doc.contours[contour_start as usize..(contour_start + contour_count) as usize] {
        let cmds = &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize];
        append_contour_line_segments(cmds, lines);
    }
}

fn append_contour_line_segments(cmds: &[PathCmd], lines: &mut Vec<VScoreLine>) {
    let mut first = None;
    let mut current = None;
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                first = Some(cmd.p0);
                current = Some(cmd.p0);
            }
            PathOp::LineTo => {
                if let Some(start) = current {
                    lines.push(VScoreLine {
                        start,
                        end: cmd.p0,
                        width: 0.0,
                    });
                }
                current = Some(cmd.p0);
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (first, current)
                    && start.distance_to(end) > 0.0
                {
                    lines.push(VScoreLine {
                        start: end,
                        end: start,
                        width: 0.0,
                    });
                }
                current = first;
            }
            PathOp::ArcTo | PathOp::CubicTo => current = cmd.end_point(),
        }
    }
}

fn append_boundary_reliefs(
    boundary: &PathPayload,
    input: &VScoreReliefInput,
    reliefs: &mut Vec<RouteRelief>,
) -> Result<(), VScoreReliefError> {
    let side = boundary_outward_side(boundary)?;
    let mut current = None;
    let mut first = None;
    let mut route_cmds = Vec::new();

    for &cmd in &boundary.cmds {
        match cmd.op {
            PathOp::MoveTo => {
                flush_route(
                    &mut route_cmds,
                    side,
                    input.tolerance_mm,
                    input.tool_diameter_mm,
                    reliefs,
                )?;
                current = Some(cmd.p0);
                first = Some(cmd.p0);
            }
            PathOp::LineTo => {
                let start = current.ok_or(VScoreReliefError::InvalidBoundary(
                    "line command appears before move command",
                ))?;
                append_segment_relief(start, cmd, input, side, &mut route_cmds, reliefs)?;
                current = Some(cmd.p0);
            }
            PathOp::ArcTo | PathOp::CubicTo => {
                let start = current.ok_or(VScoreReliefError::InvalidBoundary(
                    "curve command appears before move command",
                ))?;
                append_route_cmd(start, cmd, &mut route_cmds);
                current = cmd.end_point();
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (first, current)
                    && start.distance_to(end) > input.tolerance_mm
                {
                    append_segment_relief(
                        end,
                        PathCmd::line_to(start),
                        input,
                        side,
                        &mut route_cmds,
                        reliefs,
                    )?;
                }
                flush_route(
                    &mut route_cmds,
                    side,
                    input.tolerance_mm,
                    input.tool_diameter_mm,
                    reliefs,
                )?;
                current = first;
            }
        }
    }
    flush_route(
        &mut route_cmds,
        side,
        input.tolerance_mm,
        input.tool_diameter_mm,
        reliefs,
    )?;
    Ok(())
}

fn append_segment_relief(
    start: Point,
    cmd: PathCmd,
    input: &VScoreReliefInput,
    side: OutwardSide,
    route_cmds: &mut Vec<PathCmd>,
    reliefs: &mut Vec<RouteRelief>,
) -> Result<(), VScoreReliefError> {
    if start.distance_to(cmd.p0) <= input.tolerance_mm {
        return Ok(());
    }
    if segment_lies_on_any_score_line(start, cmd.p0, &input.score_lines, input.tolerance_mm) {
        flush_route(
            route_cmds,
            side,
            input.tolerance_mm,
            input.tool_diameter_mm,
            reliefs,
        )?;
    } else {
        append_route_cmd(start, cmd, route_cmds);
    }
    Ok(())
}

fn append_route_cmd(start: Point, cmd: PathCmd, route_cmds: &mut Vec<PathCmd>) {
    if route_cmds.is_empty() {
        route_cmds.push(PathCmd::move_to(start));
    }
    route_cmds.push(cmd);
}

fn flush_route(
    route_cmds: &mut Vec<PathCmd>,
    side: OutwardSide,
    tolerance: f64,
    tool_diameter_mm: f64,
    reliefs: &mut Vec<RouteRelief>,
) -> Result<(), VScoreReliefError> {
    if route_cmds.len() < 2 {
        route_cmds.clear();
        return Ok(());
    }
    let cmds = std::mem::take(route_cmds);
    let bbox = contour_bbox(&cmds);
    if bbox.is_empty() {
        return Ok(());
    }

    let boundary_path = PathPayload { bbox, cmds };
    let tool_radius = tool_diameter_mm / 2.0;
    let toolpath = offset_path(&boundary_path, side, tool_radius, tolerance)?;
    let contours = route_relief_contours(&toolpath, tool_diameter_mm)?;
    reliefs.push(RouteRelief {
        boundary_path,
        toolpath,
        contours,
        tool_diameter_mm,
    });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutwardSide {
    Left,
    Right,
}

fn route_relief_contours(
    toolpath: &PathPayload,
    tool_diameter_mm: f64,
) -> Result<Vec<PathPayload>, VScoreReliefError> {
    // The routed relief is the router sweep: offset the centerline outside the
    // finished edge, then stroke that centerline with the round cutter diameter.
    let sweep = stroke_to_fill(
        std::slice::from_ref(toolpath),
        StrokeToFillStyle::new(tool_diameter_mm, LineCap::Round, LineJoin::Round),
    )
    .ok_or(VScoreReliefError::InvalidBoundary(
        "route relief contour has empty bounds",
    ))?;
    let contours = payloads_to_polygon_contours(&sweep);
    let contours = simplify_polygon_contours(contours, FillRule::NonZero);
    let payloads = polygon_contours_to_payloads(contours);
    if payloads.is_empty() {
        Err(VScoreReliefError::InvalidBoundary(
            "route relief contour has empty bounds",
        ))
    } else {
        Ok(payloads)
    }
}

fn offset_path(
    path: &PathPayload,
    side: OutwardSide,
    offset: f64,
    tolerance: f64,
) -> Result<PathPayload, VScoreReliefError> {
    let mut current = None;
    let mut out = Vec::new();
    let mut current_out = None;

    for cmd in &path.cmds {
        match cmd.op {
            PathOp::MoveTo => current = Some(cmd.p0),
            PathOp::LineTo => {
                let start = current.ok_or(VScoreReliefError::InvalidBoundary(
                    "line command appears before move command",
                ))?;
                let (offset_start, offset_end) = offset_line_segment(start, cmd.p0, side, offset)?;
                append_offset_segment(
                    &mut out,
                    &mut current_out,
                    offset_start,
                    PathCmd::line_to(offset_end),
                    tolerance,
                );
                current = Some(cmd.p0);
            }
            PathOp::ArcTo => {
                let start = current.ok_or(VScoreReliefError::InvalidBoundary(
                    "arc command appears before move command",
                ))?;
                let (offset_start, offset_end, offset_center) =
                    offset_arc_segment(start, cmd.p0, cmd.p1, cmd.clockwise, side, offset)?;
                append_offset_segment(
                    &mut out,
                    &mut current_out,
                    offset_start,
                    PathCmd::arc_to(offset_end, offset_center, cmd.clockwise),
                    tolerance,
                );
                current = Some(cmd.p0);
            }
            PathOp::CubicTo => {
                let start = current.ok_or(VScoreReliefError::InvalidBoundary(
                    "cubic command appears before move command",
                ))?;
                let mut segment_start = start;
                for index in 1..=16 {
                    let segment_end =
                        cubic_point(start, cmd.p0, cmd.p1, cmd.p2, index as f64 / 16.0);
                    let (offset_start, offset_end) =
                        offset_line_segment(segment_start, segment_end, side, offset)?;
                    append_offset_segment(
                        &mut out,
                        &mut current_out,
                        offset_start,
                        PathCmd::line_to(offset_end),
                        tolerance,
                    );
                    segment_start = segment_end;
                }
                current = Some(cmd.p2);
            }
            PathOp::Close => {}
        }
    }

    if out.len() < 2 {
        return Err(VScoreReliefError::InvalidBoundary(
            "route relief toolpath is empty",
        ));
    }
    let bbox = contour_bbox(&out);
    if bbox.is_empty() {
        return Err(VScoreReliefError::InvalidBoundary(
            "route relief toolpath has empty bounds",
        ));
    }
    Ok(PathPayload { bbox, cmds: out })
}

fn append_offset_segment(
    out: &mut Vec<PathCmd>,
    current_out: &mut Option<Point>,
    start: Point,
    cmd: PathCmd,
    tolerance: f64,
) {
    match current_out {
        None => out.push(PathCmd::move_to(start)),
        Some(current) if current.distance_to(start) > tolerance => {
            out.push(PathCmd::line_to(start))
        }
        Some(_) => {}
    }
    *current_out = cmd.end_point();
    out.push(cmd);
}

fn boundary_outward_side(boundary: &PathPayload) -> Result<OutwardSide, VScoreReliefError> {
    let contours = payloads_to_polygon_contours(std::slice::from_ref(boundary));
    let signed_area = contours
        .iter()
        .map(|contour| signed_area(contour))
        .sum::<f64>();
    if signed_area > 0.0 {
        Ok(OutwardSide::Right)
    } else if signed_area < 0.0 {
        Ok(OutwardSide::Left)
    } else {
        Err(VScoreReliefError::InvalidBoundary(
            "boundary has zero signed area",
        ))
    }
}

fn signed_area(contour: &[[f64; 2]]) -> f64 {
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

fn offset_line_segment(
    start: Point,
    end: Point,
    side: OutwardSide,
    offset: f64,
) -> Result<(Point, Point), VScoreReliefError> {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return Err(VScoreReliefError::InvalidBoundary(
            "route relief contains a zero-length line",
        ));
    }
    let normal = match side {
        OutwardSide::Left => Point::new(-dy / len, dx / len),
        OutwardSide::Right => Point::new(dy / len, -dx / len),
    };
    let delta = Point::new(normal.x * offset, normal.y * offset);
    Ok((add(start, delta), add(end, delta)))
}

fn offset_arc_segment(
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
    side: OutwardSide,
    offset: f64,
) -> Result<(Point, Point, Point), VScoreReliefError> {
    let radius = start.distance_to(center);
    if radius <= 0.0 {
        return Err(VScoreReliefError::InvalidBoundary(
            "route relief contains a zero-radius arc",
        ));
    }
    let offset_away_from_center = match (side, clockwise) {
        (OutwardSide::Right, false) | (OutwardSide::Left, true) => true,
        (OutwardSide::Right, true) | (OutwardSide::Left, false) => false,
    };
    let offset_radius = if offset_away_from_center {
        radius + offset
    } else {
        radius - offset
    };
    if offset_radius <= 0.0 {
        return Err(VScoreReliefError::InvalidBoundary(
            "route relief arc offset collapses",
        ));
    }
    Ok((
        scale_from_center(center, start, offset_radius / radius),
        scale_from_center(center, end, offset_radius / radius),
        center,
    ))
}

fn cubic_point(start: Point, c1: Point, c2: Point, end: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt.powi(3) * start.x
            + 3.0 * mt.powi(2) * t * c1.x
            + 3.0 * mt * t.powi(2) * c2.x
            + t.powi(3) * end.x,
        mt.powi(3) * start.y
            + 3.0 * mt.powi(2) * t * c1.y
            + 3.0 * mt * t.powi(2) * c2.y
            + t.powi(3) * end.y,
    )
}

fn scale_from_center(center: Point, point: Point, scale: f64) -> Point {
    Point::new(
        center.x + (point.x - center.x) * scale,
        center.y + (point.y - center.y) * scale,
    )
}

fn add(point: Point, delta: Point) -> Point {
    Point::new(point.x + delta.x, point.y + delta.y)
}

fn segment_lies_on_any_score_line(
    start: Point,
    end: Point,
    score_lines: &[VScoreLine],
    tolerance: f64,
) -> bool {
    score_lines
        .iter()
        .any(|line| segment_lies_on_score_line(start, end, *line, tolerance))
}

fn segment_lies_on_score_line(start: Point, end: Point, line: VScoreLine, tolerance: f64) -> bool {
    point_lies_on_segment(start, line.start, line.end, tolerance)
        && point_lies_on_segment(end, line.start, line.end, tolerance)
}

fn point_lies_on_segment(point: Point, start: Point, end: Point, tolerance: f64) -> bool {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = dx.hypot(dy);
    if len <= tolerance {
        return false;
    }
    let px = point.x - start.x;
    let py = point.y - start.y;
    let distance = (dx * py - dy * px).abs() / len;
    if distance > tolerance {
        return false;
    }
    let along = (px * dx + py * dy) / len;
    along >= -tolerance && along <= len + tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(cmds: Vec<PathCmd>) -> Vec<PathPayload> {
        vec![PathPayload {
            bbox: contour_bbox(&cmds),
            cmds,
        }]
    }

    fn rectangle_score_lines(width: f64, height: f64) -> Vec<VScoreLine> {
        vec![
            VScoreLine {
                start: Point::new(0.0, 0.0),
                end: Point::new(width, 0.0),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(width, 0.0),
                end: Point::new(width, height),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(width, height),
                end: Point::new(0.0, height),
                width: 0.025,
            },
            VScoreLine {
                start: Point::new(0.0, height),
                end: Point::new(0.0, 0.0),
                width: 0.025,
            },
        ]
    }

    #[test]
    fn rectangle_boundary_needs_no_reliefs() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );

        assert!(vscore_route_reliefs(&input).unwrap().is_empty());
    }

    #[test]
    fn inset_boundary_segments_become_route_reliefs() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(6.0, 5.0)),
                PathCmd::line_to(Point::new(5.0, 3.0)),
                PathCmd::line_to(Point::new(4.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 5.0),
        );

        let reliefs = vscore_route_reliefs(&input).unwrap();

        assert_eq!(reliefs.len(), 1);
        assert_eq!(reliefs[0].boundary_path.cmds.len(), 3);
        assert!(
            reliefs
                .iter()
                .all(|relief| !relief.boundary_path.bbox.is_empty())
        );
        assert!(
            reliefs
                .iter()
                .all(|relief| !relief.toolpath.bbox.is_empty())
        );
        assert!(reliefs.iter().all(|relief| !relief.contours.is_empty()));
        assert!(
            reliefs
                .iter()
                .flat_map(|relief| &relief.contours)
                .all(|contour| contour
                    .cmds
                    .last()
                    .is_some_and(|cmd| cmd.op == PathOp::Close))
        );
        assert!(reliefs.iter().all(|relief| relief.tool_diameter_mm == 1.0));
    }

    #[test]
    fn route_relief_contour_uses_outside_offset_toolpath() {
        let mut score_lines = rectangle_score_lines(10.0, 5.0);
        score_lines.remove(2);
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ]),
            score_lines,
        );

        let reliefs = vscore_route_reliefs(&input).unwrap();

        assert_eq!(reliefs.len(), 1);
        assert!(reliefs[0].toolpath.bbox.min.y > 5.49);
        let bbox = reliefs[0]
            .contours
            .iter()
            .fold(crate::common::BBox::empty(), |bbox, contour| {
                bbox.union(contour.bbox)
            });
        assert!(bbox.min.y >= 5.0 - DEFAULT_RELIEF_TOLERANCE_MM);
        assert!(bbox.max.y > 5.99);
    }

    #[test]
    fn curved_boundary_relief_preserves_arc() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 10.0)),
                PathCmd::line_to(Point::new(2.0, 10.0)),
                PathCmd::arc_to(Point::new(0.0, 8.0), Point::new(2.0, 8.0), false),
                PathCmd::line_to(Point::new(0.0, 0.0)),
                PathCmd::close(),
            ]),
            rectangle_score_lines(10.0, 10.0),
        );

        let reliefs = vscore_route_reliefs(&input).unwrap();

        assert_eq!(reliefs.len(), 1);
        assert!(
            reliefs[0]
                .boundary_path
                .cmds
                .iter()
                .any(|cmd| cmd.op == PathOp::ArcTo)
        );
        assert!(
            reliefs[0]
                .toolpath
                .cmds
                .iter()
                .any(|cmd| cmd.op == PathOp::ArcTo)
        );
        assert!(!reliefs[0].contours.is_empty());
    }

    #[test]
    fn no_score_lines_errors_instead_of_inferring_bbox_scores() {
        let input = VScoreReliefInput::new(
            path(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
            ]),
            Vec::new(),
        );

        assert_eq!(
            vscore_route_reliefs(&input).unwrap_err(),
            VScoreReliefError::EmptyScoreLines
        );
    }
}
