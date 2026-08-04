//! Source-independent expansion of symbolic IPC line patterns.
//!
//! IPC-2581 defines pattern dimensions as multiples of the line width. This
//! module converts those semantic patterns into explicit dash segments and
//! dot centers so format writers do not need to guess at source-specific
//! rendering conventions.

use crate::geom::{Arc, LinePattern, Point, Segment};

const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, PartialEq)]
pub enum StrokePatternMark {
    /// One continuous visible dash, possibly spanning source segments.
    Dash(Vec<Segment>),
    Dot(Point),
}

/// Expand a line pattern over one continuous contour.
///
/// Pattern phase is continuous across segment boundaries and restarts for each
/// contour. `Solid` and IPC `Erase` lines are returned as uninterrupted dash
/// segments. Dots consume one line width along the contour and are returned at
/// the center of that interval, matching IPC-2581C's one-line-width diameter.
pub fn stroke_pattern_marks(
    segments: &[Segment],
    pattern: LinePattern,
    line_width: f64,
) -> Vec<StrokePatternMark> {
    if segments.is_empty() || !line_width.is_finite() || line_width <= 0.0 {
        return Vec::new();
    }
    if matches!(pattern, LinePattern::Solid | LinePattern::Erase) {
        return vec![StrokePatternMark::Dash(segments.to_vec())];
    }

    let measured = measured_segments(segments);
    let total_length = measured.last().map_or(0.0, |segment| segment.end);
    if total_length <= EPSILON {
        return Vec::new();
    }

    let elements = pattern_elements(pattern);
    let mut marks = Vec::new();
    let mut cursor = 0.0;
    let mut element_index = 0;
    while cursor < total_length - EPSILON {
        let element = elements[element_index];
        let length = element.widths * line_width;
        let next = cursor + length;
        if next <= cursor {
            break;
        }
        let end = next.min(total_length);
        match element.kind {
            PatternElementKind::Dash => {
                let dash = slice_segments(&measured, cursor, end);
                if !dash.is_empty() {
                    marks.push(StrokePatternMark::Dash(dash));
                }
            }
            PatternElementKind::Dot => {
                let center = cursor + length / 2.0;
                if center <= total_length + EPSILON
                    && let Some(point) = point_at_distance(&measured, center.min(total_length))
                {
                    marks.push(StrokePatternMark::Dot(point));
                }
            }
            PatternElementKind::Gap => {}
        }
        cursor = next;
        element_index = (element_index + 1) % elements.len();
    }
    marks
}

#[derive(Debug, Clone, Copy)]
struct MeasuredSegment {
    segment: Segment,
    start: f64,
    end: f64,
}

fn measured_segments(segments: &[Segment]) -> Vec<MeasuredSegment> {
    let mut measured = Vec::with_capacity(segments.len());
    let mut cursor = 0.0;
    for &segment in segments {
        if let Segment::Cubic { start, .. } = segment {
            const STEPS: usize = 32;
            let mut points = Vec::with_capacity(STEPS);
            segment.sample_points(STEPS, &mut points);
            let mut current = start;
            for point in points {
                push_measured_segment(
                    &mut measured,
                    &mut cursor,
                    Segment::Line {
                        start: current,
                        end: point,
                    },
                );
                current = point;
            }
        } else {
            push_measured_segment(&mut measured, &mut cursor, segment);
        }
    }
    measured
}

fn push_measured_segment(measured: &mut Vec<MeasuredSegment>, cursor: &mut f64, segment: Segment) {
    let length = segment_length(segment);
    if length <= EPSILON || !length.is_finite() {
        return;
    }
    measured.push(MeasuredSegment {
        segment,
        start: *cursor,
        end: *cursor + length,
    });
    *cursor += length;
}

fn segment_length(segment: Segment) -> f64 {
    match segment {
        Segment::Line { start, end } => start.distance_to(end),
        Segment::Arc(arc) => arc.radius() * arc.sweep_radians(),
        Segment::Cubic { .. } => unreachable!("cubic segments are flattened before measurement"),
    }
}

fn slice_segments(measured: &[MeasuredSegment], start: f64, end: f64) -> Vec<Segment> {
    if end <= start + EPSILON {
        return Vec::new();
    }
    measured
        .iter()
        .filter_map(|entry| {
            let local_start = start.max(entry.start);
            let local_end = end.min(entry.end);
            if local_end <= local_start + EPSILON {
                return None;
            }
            let length = entry.end - entry.start;
            Some(segment_slice(
                entry.segment,
                (local_start - entry.start) / length,
                (local_end - entry.start) / length,
            ))
        })
        .collect()
}

fn point_at_distance(measured: &[MeasuredSegment], distance: f64) -> Option<Point> {
    let entry = measured
        .iter()
        .find(|entry| distance <= entry.end + EPSILON)
        .or_else(|| measured.last())?;
    let t = ((distance - entry.start) / (entry.end - entry.start)).clamp(0.0, 1.0);
    Some(segment_point(entry.segment, t))
}

fn segment_slice(segment: Segment, start_t: f64, end_t: f64) -> Segment {
    match segment {
        Segment::Line { .. } => Segment::Line {
            start: segment_point(segment, start_t),
            end: segment_point(segment, end_t),
        },
        Segment::Arc(arc) => Segment::Arc(Arc::new(
            segment_point(segment, start_t),
            segment_point(segment, end_t),
            arc.center,
            arc.clockwise,
        )),
        Segment::Cubic { .. } => Segment::Line {
            start: segment_point(segment, start_t),
            end: segment_point(segment, end_t),
        },
    }
}

fn segment_point(segment: Segment, t: f64) -> Point {
    match segment {
        Segment::Line { start, end } => start + (end - start) * t,
        Segment::Arc(arc) => {
            let start_angle = arc.start.angle_from(arc.center);
            let signed_sweep = if arc.clockwise {
                -arc.sweep_radians()
            } else {
                arc.sweep_radians()
            };
            arc.point_at(start_angle + signed_sweep * t)
        }
        Segment::Cubic { start, c1, c2, end } => {
            let u = 1.0 - t;
            start * (u * u * u)
                + c1 * (3.0 * u * u * t)
                + c2 * (3.0 * u * t * t)
                + end * (t * t * t)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PatternElement {
    kind: PatternElementKind,
    widths: f64,
}

#[derive(Debug, Clone, Copy)]
enum PatternElementKind {
    Dash,
    Dot,
    Gap,
}

const fn element(kind: PatternElementKind, widths: f64) -> PatternElement {
    PatternElement { kind, widths }
}

const DOTTED_PATTERN: [PatternElement; 2] = [
    element(PatternElementKind::Dot, 1.0),
    element(PatternElementKind::Gap, 2.0),
];
const DASHED_PATTERN: [PatternElement; 2] = [
    element(PatternElementKind::Dash, 3.0),
    element(PatternElementKind::Gap, 3.0),
];
const CENTER_PATTERN: [PatternElement; 4] = [
    element(PatternElementKind::Dash, 6.0),
    element(PatternElementKind::Gap, 2.0),
    element(PatternElementKind::Dot, 1.0),
    element(PatternElementKind::Gap, 2.0),
];
const PHANTOM_PATTERN: [PatternElement; 6] = [
    element(PatternElementKind::Dash, 6.0),
    element(PatternElementKind::Gap, 2.0),
    element(PatternElementKind::Dot, 1.0),
    element(PatternElementKind::Gap, 2.0),
    element(PatternElementKind::Dot, 1.0),
    element(PatternElementKind::Gap, 2.0),
];

fn pattern_elements(pattern: LinePattern) -> &'static [PatternElement] {
    match pattern {
        LinePattern::Dotted => &DOTTED_PATTERN,
        LinePattern::Dashed => &DASHED_PATTERN,
        LinePattern::Center => &CENTER_PATTERN,
        LinePattern::Phantom => &PHANTOM_PATTERN,
        LinePattern::Solid | LinePattern::Erase => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: Point, end: Point) -> Segment {
        Segment::Line { start, end }
    }

    #[test]
    fn phantom_uses_ipc_2581c_width_ratios() {
        let marks = stroke_pattern_marks(
            &[line(Point::new(0.0, 0.0), Point::new(20.0, 0.0))],
            LinePattern::Phantom,
            1.0,
        );

        assert_eq!(
            marks,
            vec![
                StrokePatternMark::Dash(vec![line(Point::new(0.0, 0.0), Point::new(6.0, 0.0),)]),
                StrokePatternMark::Dot(Point::new(8.5, 0.0)),
                StrokePatternMark::Dot(Point::new(11.5, 0.0)),
                StrokePatternMark::Dash(vec![line(Point::new(14.0, 0.0), Point::new(20.0, 0.0),)]),
            ]
        );
    }

    #[test]
    fn pattern_phase_continues_across_contour_segments() {
        let marks = stroke_pattern_marks(
            &[
                line(Point::new(0.0, 0.0), Point::new(5.0, 0.0)),
                line(Point::new(5.0, 0.0), Point::new(5.0, 5.0)),
            ],
            LinePattern::Dashed,
            1.0,
        );

        assert_eq!(
            marks,
            vec![
                StrokePatternMark::Dash(vec![line(Point::new(0.0, 0.0), Point::new(3.0, 0.0),)]),
                StrokePatternMark::Dash(vec![line(Point::new(5.0, 1.0), Point::new(5.0, 4.0),)]),
            ]
        );
    }

    #[test]
    fn one_dash_remains_continuous_across_source_segments() {
        let marks = stroke_pattern_marks(
            &[
                line(Point::new(0.0, 0.0), Point::new(2.0, 0.0)),
                line(Point::new(2.0, 0.0), Point::new(2.0, 4.0)),
            ],
            LinePattern::Dashed,
            1.0,
        );

        assert_eq!(
            marks[0],
            StrokePatternMark::Dash(vec![
                line(Point::new(0.0, 0.0), Point::new(2.0, 0.0)),
                line(Point::new(2.0, 0.0), Point::new(2.0, 1.0)),
            ])
        );
    }

    #[test]
    fn dotted_pattern_emits_one_width_diameter_dot_centers() {
        let marks = stroke_pattern_marks(
            &[line(Point::new(0.0, 0.0), Point::new(8.0, 0.0))],
            LinePattern::Dotted,
            1.0,
        );

        assert_eq!(
            marks,
            vec![
                StrokePatternMark::Dot(Point::new(0.5, 0.0)),
                StrokePatternMark::Dot(Point::new(3.5, 0.0)),
                StrokePatternMark::Dot(Point::new(6.5, 0.0)),
            ]
        );
    }
}
