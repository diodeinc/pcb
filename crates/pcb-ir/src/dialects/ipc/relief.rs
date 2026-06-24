use std::fmt;

use crate::common::{BBox, Point};

pub const DEFAULT_ROUTE_TOOL_DIAMETER_MM: f64 = 1.0;
pub const DEFAULT_RELIEF_TOLERANCE_MM: f64 = 0.01;

#[derive(Debug, Clone)]
pub struct VScoreReliefInput {
    pub board_boundary: Vec<Point>,
    pub score_bbox: BBox,
    pub tool_diameter_mm: f64,
    pub tolerance_mm: f64,
}

impl VScoreReliefInput {
    pub fn new(board_boundary: Vec<Point>, score_bbox: BBox) -> Self {
        Self {
            board_boundary,
            score_bbox,
            tool_diameter_mm: DEFAULT_ROUTE_TOOL_DIAMETER_MM,
            tolerance_mm: DEFAULT_RELIEF_TOLERANCE_MM,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteRelief {
    pub start: Point,
    pub end: Point,
    pub tool_diameter_mm: f64,
    pub bbox: BBox,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VScoreReliefError {
    EmptyScoreBox,
    InvalidToolDiameter(f64),
    InvalidTolerance(f64),
    TooFewBoundaryPoints(usize),
    DegenerateBoundary,
}

impl fmt::Display for VScoreReliefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyScoreBox => write!(f, "V-score relief score box is empty"),
            Self::InvalidToolDiameter(value) => {
                write!(
                    f,
                    "V-score relief tool diameter must be positive; got {value}"
                )
            }
            Self::InvalidTolerance(value) => {
                write!(f, "V-score relief tolerance must be positive; got {value}")
            }
            Self::TooFewBoundaryPoints(count) => {
                write!(
                    f,
                    "V-score relief boundary needs at least 3 points; got {count}"
                )
            }
            Self::DegenerateBoundary => write!(f, "V-score relief boundary has zero area"),
        }
    }
}

impl std::error::Error for VScoreReliefError {}

pub fn vscore_route_reliefs(
    input: &VScoreReliefInput,
) -> Result<Vec<RouteRelief>, VScoreReliefError> {
    if input.score_bbox.is_empty() {
        return Err(VScoreReliefError::EmptyScoreBox);
    }
    if !input.tool_diameter_mm.is_finite() || input.tool_diameter_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidToolDiameter(
            input.tool_diameter_mm,
        ));
    }
    if !input.tolerance_mm.is_finite() || input.tolerance_mm <= 0.0 {
        return Err(VScoreReliefError::InvalidTolerance(input.tolerance_mm));
    }

    let boundary = normalized_boundary(&input.board_boundary, input.tolerance_mm)?;
    let signed_area = signed_area(&boundary);
    if signed_area.abs() <= input.tolerance_mm * input.tolerance_mm {
        return Err(VScoreReliefError::DegenerateBoundary);
    }

    let outward_offset = input.tool_diameter_mm / 2.0;
    let outward_side = if signed_area > 0.0 {
        OutwardSide::Right
    } else {
        OutwardSide::Left
    };
    let mut reliefs = Vec::new();
    for index in 0..boundary.len() {
        let start = boundary[index];
        let end = boundary[(index + 1) % boundary.len()];
        if start.distance_to(end) <= input.tolerance_mm {
            continue;
        }

        if !segment_lies_on_score_line(start, end, input.score_bbox, input.tolerance_mm) {
            reliefs.push(route_relief_for_segment(
                start,
                end,
                input.tool_diameter_mm,
                outward_offset,
                outward_side,
            ));
        }
    }

    Ok(reliefs)
}

fn normalized_boundary(points: &[Point], tolerance: f64) -> Result<Vec<Point>, VScoreReliefError> {
    let mut out = Vec::new();
    for &point in points {
        if out
            .last()
            .is_none_or(|previous: &Point| previous.distance_to(point) > tolerance)
        {
            out.push(point);
        }
    }
    if out.len() > 1 && out[0].distance_to(*out.last().unwrap()) <= tolerance {
        out.pop();
    }
    if out.len() < 3 {
        return Err(VScoreReliefError::TooFewBoundaryPoints(out.len()));
    }
    Ok(out)
}

fn segment_lies_on_score_line(start: Point, end: Point, bbox: BBox, tolerance: f64) -> bool {
    let x_on_left = close(start.x, bbox.min.x, tolerance) && close(end.x, bbox.min.x, tolerance);
    let x_on_right = close(start.x, bbox.max.x, tolerance) && close(end.x, bbox.max.x, tolerance);
    let y_on_bottom = close(start.y, bbox.min.y, tolerance) && close(end.y, bbox.min.y, tolerance);
    let y_on_top = close(start.y, bbox.max.y, tolerance) && close(end.y, bbox.max.y, tolerance);

    (x_on_left || x_on_right)
        && in_range(start.y, bbox.min.y, bbox.max.y, tolerance)
        && in_range(end.y, bbox.min.y, bbox.max.y, tolerance)
        || (y_on_bottom || y_on_top)
            && in_range(start.x, bbox.min.x, bbox.max.x, tolerance)
            && in_range(end.x, bbox.min.x, bbox.max.x, tolerance)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutwardSide {
    Left,
    Right,
}

fn route_relief_for_segment(
    start: Point,
    end: Point,
    tool_diameter_mm: f64,
    outward_offset: f64,
    outward_side: OutwardSide,
) -> RouteRelief {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = start.distance_to(end);
    let (normal_x, normal_y) = match outward_side {
        OutwardSide::Left => (-dy / length, dx / length),
        OutwardSide::Right => (dy / length, -dx / length),
    };
    let offset = Point::new(normal_x * outward_offset, normal_y * outward_offset);
    let start = Point::new(start.x + offset.x, start.y + offset.y);
    let end = Point::new(end.x + offset.x, end.y + offset.y);
    let bbox = swept_segment_bbox(start, end, tool_diameter_mm / 2.0);
    RouteRelief {
        start,
        end,
        tool_diameter_mm,
        bbox,
    }
}

fn swept_segment_bbox(start: Point, end: Point, radius: f64) -> BBox {
    let mut bbox = BBox::from_point(start);
    bbox.include_point(end);
    bbox.include_point(Point::new(start.x - radius, start.y - radius));
    bbox.include_point(Point::new(start.x + radius, start.y + radius));
    bbox.include_point(Point::new(end.x - radius, end.y - radius));
    bbox.include_point(Point::new(end.x + radius, end.y + radius));
    bbox
}

fn signed_area(points: &[Point]) -> f64 {
    let mut area = 0.0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    area / 2.0
}

fn close(left: f64, right: f64, tolerance: f64) -> bool {
    (left - right).abs() <= tolerance
}

fn in_range(value: f64, min: f64, max: f64, tolerance: f64) -> bool {
    value >= min - tolerance && value <= max + tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_boundary_needs_no_reliefs() {
        let input = VScoreReliefInput::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 5.0),
                Point::new(0.0, 5.0),
            ],
            BBox {
                min: Point::new(0.0, 0.0),
                max: Point::new(10.0, 5.0),
            },
        );

        assert!(vscore_route_reliefs(&input).unwrap().is_empty());
    }

    #[test]
    fn inset_boundary_segments_become_route_reliefs() {
        let input = VScoreReliefInput::new(
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 5.0),
                Point::new(6.0, 5.0),
                Point::new(5.0, 3.0),
                Point::new(4.0, 5.0),
                Point::new(0.0, 5.0),
            ],
            BBox {
                min: Point::new(0.0, 0.0),
                max: Point::new(10.0, 5.0),
            },
        );

        let reliefs = vscore_route_reliefs(&input).unwrap();

        assert_eq!(reliefs.len(), 2);
        assert!(reliefs.iter().all(|relief| !relief.bbox.is_empty()));
        assert!(reliefs.iter().all(|relief| relief.tool_diameter_mm == 1.0));
    }
}
