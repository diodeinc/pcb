//! Exact stroke outlines.
//!
//! A stroke is the Minkowski sum of its centerline and the stroke aperture.
//! The sum distributes over the centerline's segments, so the outline is
//! built one segment at a time: a line sweeps a rectangle, an arc sweeps an
//! annular sector, and a round end adds a half disk. Every piece is a simple
//! counter-clockwise contour of lines and circular arcs, so nothing is
//! approximated here; the pieces overlap and are meant to be unioned.
//!
//! Joins are round. The half disk closing a segment's end reaches every
//! direction within a quarter turn of its tangent, which is exactly the
//! wedge any turn onto the next segment opens, so a joint needs no piece of
//! its own.

use std::f64::consts::PI;

use crate::geom::accuracy::numerical_error;
use crate::geom::affine::Affine2;
use crate::geom::arc::Arc;
use crate::geom::path::{ContourBuf, PathCmd, PathOp, Segment};
use crate::geom::pattern::{StrokePatternMark, stroke_pattern_marks};
use crate::geom::point::Point;
use crate::geom::style::{LineCap, LinePattern, StrokeStyle};
use crate::geom::{AccuracyError, GeometryAccuracy, shapes};

/// Convert stroked centerlines into filled contours.
///
/// Lines and circular arcs outline exactly. Elliptical arcs have no
/// circular offset, so they are flattened within `accuracy` first and the
/// returned contours record what that cost. The pieces overlap: fill them
/// under the nonzero rule or union them.
pub fn stroke_to_fill(
    contours: &[ContourBuf],
    style: StrokeStyle,
    accuracy: GeometryAccuracy,
) -> Result<Option<Vec<ContourBuf>>, AccuracyError> {
    if !style.width.is_finite() || contours.iter().any(|contour| !contour.is_valid()) {
        return Err(AccuracyError::InvalidGeometry("invalid stroke geometry"));
    }
    if style.width <= 0.0 {
        return Ok(None);
    }
    let solid = matches!(style.pattern, LinePattern::Solid | LinePattern::Erase);
    let mut out = Vec::new();
    for contour in contours {
        let contour = contour.flattened_curves(accuracy)?;
        // Outline points are sums of source coordinates and offsets, which
        // round like any other arithmetic on them.
        let uncertainty =
            contour.uncertainty_mm + numerical_error(contour.bbox.expand(style.width));
        accuracy.check(uncertainty)?;
        let pieces = if solid {
            subpaths(&contour.cmds)
                .iter()
                .flat_map(|subpath| subpath_outline(subpath, style))
                .collect::<Vec<_>>()
        } else {
            let segments = contour.segments().collect::<Vec<_>>();
            stroke_pattern_marks(&segments, style.pattern, style.width)
                .into_iter()
                .flat_map(|mark| match mark {
                    StrokePatternMark::Dash(segments) => subpath_outline(
                        &Subpath {
                            segments,
                            closed: false,
                        },
                        style,
                    ),
                    StrokePatternMark::Dot(at) => dot(LineCap::Round, style.width, at),
                })
                .collect()
        };
        out.extend(
            pieces
                .into_iter()
                .map(|piece| piece.with_uncertainty(uncertainty)),
        );
    }
    Ok((!out.is_empty()).then_some(out))
}

/// One continuous run of segments, and whether it returns to its start.
struct Subpath {
    segments: Vec<Segment>,
    closed: bool,
}

fn subpaths(cmds: &[PathCmd]) -> Vec<Subpath> {
    let mut out = Vec::new();
    let mut segments = Vec::new();
    let mut first = None;
    let mut current = None;
    let mut flush = |segments: &mut Vec<Segment>, closed: bool| {
        if !segments.is_empty() {
            out.push(Subpath {
                segments: std::mem::take(segments),
                closed,
            });
        }
    };
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                flush(&mut segments, false);
                first = Some(cmd.p0);
                current = first;
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (current, first)
                    && start != end
                {
                    segments.push(Segment::Line { start, end });
                }
                // Drawing that continues after a close starts where the
                // closed run did.
                current = first;
                flush(&mut segments, true);
            }
            _ => {
                let Some(segment) = cmd.segment_from(current) else {
                    continue;
                };
                current = Some(segment.end());
                segments.push(segment);
            }
        }
    }
    flush(&mut segments, false);
    out
}

/// How one end of a segment's outline is finished.
#[derive(Clone, Copy, PartialEq)]
enum End {
    /// A half disk: a round cap, or the round join onto the next segment.
    Round,
    /// A straight edge pushed `extension` past the end point: a butt cap,
    /// a square cap, or the start of a segment whose predecessor joins it.
    Flat { extension: f64 },
}

impl End {
    fn cap(style: StrokeStyle) -> Self {
        match style.cap {
            LineCap::Round => Self::Round,
            LineCap::Butt => Self::Flat { extension: 0.0 },
            LineCap::Square => Self::Flat {
                extension: style.width / 2.0,
            },
        }
    }
}

fn subpath_outline(subpath: &Subpath, style: StrokeStyle) -> Vec<ContourBuf> {
    let drawn = subpath
        .segments
        .iter()
        .filter(|segment| !is_point(segment))
        .collect::<Vec<_>>();
    let Some(first) = subpath.segments.first() else {
        return Vec::new();
    };
    if drawn.is_empty() {
        // A stroke of no length still images its cap, as it does in SVG and
        // as a zero-length Gerber draw does.
        return dot(style.cap, style.width, first.start());
    }
    let joined = End::Flat { extension: 0.0 };
    let last = drawn.len() - 1;
    drawn
        .iter()
        .enumerate()
        .flat_map(|(index, segment)| {
            let start = if index == 0 && !subpath.closed {
                End::cap(style)
            } else {
                joined
            };
            let end = if index == last && !subpath.closed {
                End::cap(style)
            } else {
                End::Round
            };
            match **segment {
                Segment::Arc(arc) if arc.radius() > 0.0 => {
                    arc_outline(arc, style.width, start, end)
                }
                _ => line_outline(segment.start(), segment.end(), style.width, start, end)
                    .into_iter()
                    .collect(),
            }
        })
        .collect()
}

fn is_point(segment: &Segment) -> bool {
    match segment {
        Segment::Arc(arc) => arc.radius() <= 0.0 || arc.sweep_radians() <= 0.0,
        _ => segment.start() == segment.end(),
    }
}

fn dot(cap: LineCap, width: f64, at: Point) -> Vec<ContourBuf> {
    match cap {
        LineCap::Round => shapes::circle(width),
        LineCap::Square => shapes::rect(width, width),
        LineCap::Butt => None,
    }
    .map(|shape| shape.transformed(Affine2::translation(at)))
    .into_iter()
    .collect()
}

/// The rectangle a line sweeps, finished at each end.
fn line_outline(from: Point, to: Point, width: f64, start: End, end: End) -> Option<ContourBuf> {
    let length = from.distance_to(to);
    if length <= 0.0 {
        return None;
    }
    let half = width / 2.0;
    let along = (to - from) / length;
    let left = Point::new(-along.y, along.x) * half;
    let extension = |end: End| match end {
        End::Round => 0.0,
        End::Flat { extension } => extension,
    };
    let (from, to) = (from - along * extension(start), to + along * extension(end));

    let mut cmds = vec![PathCmd::move_to(from - left), PathCmd::line_to(to - left)];
    cmds.push(match end {
        End::Round => PathCmd::arc_to(to + left, to, false),
        End::Flat { .. } => PathCmd::line_to(to + left),
    });
    cmds.push(PathCmd::line_to(from + left));
    if start == End::Round {
        cmds.push(PathCmd::arc_to(from - left, from, false));
    }
    cmds.push(PathCmd::close());
    Some(ContourBuf::new(cmds))
}

/// The annular sector an arc sweeps, finished at each end. A stroke wider
/// than the arc's diameter has no inner edge: it sweeps the whole sector out
/// to the far side, and round ends become full disks.
fn arc_outline(arc: Arc, width: f64, start: End, end: End) -> Vec<ContourBuf> {
    // Every piece winds counter-clockwise so overlapping pieces never
    // cancel; the stroke of an arc does not depend on its direction.
    let (arc, start, end) = if arc.clockwise {
        (arc.reversed(), end, start)
    } else {
        (arc, start, end)
    };
    let half = width / 2.0;
    let radius = arc.radius();
    let sweep = arc.sweep_radians();
    let first_angle = arc.start.angle_from(arc.center);
    // Sectors of at most half a turn stay simple with their ends attached.
    let pieces = (sweep / PI).ceil().max(1.0) as usize;
    // The outer spokes point at the arc's own ends, so its outline meets
    // its neighbours' exactly.
    let spoke = |index: usize| {
        if index == 0 {
            (arc.start - arc.center) / radius
        } else if index == pieces {
            (arc.end - arc.center) / arc.end.distance_to(arc.center)
        } else {
            let angle = first_angle + sweep * index as f64 / pieces as f64;
            Point::new(angle.cos(), angle.sin())
        }
    };
    let joined = End::Flat { extension: 0.0 };

    (0..pieces)
        .flat_map(|index| {
            let (from, to) = (spoke(index), spoke(index + 1));
            let start = if index == 0 { start } else { joined };
            let end = if index + 1 == pieces { end } else { joined };
            sector_outline(arc.center, radius, half, (from, start), (to, end))
        })
        .collect()
}

/// One sector between two unit spokes, counter-clockwise from the first.
fn sector_outline(
    center: Point,
    radius: f64,
    half: f64,
    (from, start): (Point, End),
    (to, end): (Point, End),
) -> Vec<ContourBuf> {
    let tangent = |spoke: Point| Point::new(-spoke.y, spoke.x);
    let outer = radius + half;
    let inner = radius - half;
    let on = |spoke: Point, distance: f64| center + spoke * distance;

    if inner <= 0.0 {
        let sector = ContourBuf::new(vec![
            PathCmd::move_to(center),
            PathCmd::line_to(on(from, outer)),
            PathCmd::arc_to(on(to, outer), center, false),
            PathCmd::close(),
        ]);
        let disk = |spoke: Point, end: End| {
            (end == End::Round)
                .then(|| shapes::circle(2.0 * half))
                .flatten()
                .map(|disk| disk.transformed(Affine2::translation(on(spoke, radius))))
        };
        return [Some(sector), disk(from, start), disk(to, end)]
            .into_iter()
            .flatten()
            .collect();
    }

    let mut cmds = vec![
        PathCmd::move_to(on(from, outer)),
        PathCmd::arc_to(on(to, outer), center, false),
    ];
    match end {
        End::Round => cmds.push(PathCmd::arc_to(on(to, inner), on(to, radius), false)),
        End::Flat { extension } => {
            let push = tangent(to) * extension;
            if extension > 0.0 {
                cmds.push(PathCmd::line_to(on(to, outer) + push));
                cmds.push(PathCmd::line_to(on(to, inner) + push));
            }
            cmds.push(PathCmd::line_to(on(to, inner)));
        }
    }
    cmds.push(PathCmd::arc_to(on(from, inner), center, true));
    match start {
        End::Round => cmds.push(PathCmd::arc_to(on(from, outer), on(from, radius), false)),
        End::Flat { extension } if extension > 0.0 => {
            let push = tangent(from) * -extension;
            cmds.push(PathCmd::line_to(on(from, inner) + push));
            cmds.push(PathCmd::line_to(on(from, outer) + push));
        }
        End::Flat { .. } => {}
    }
    cmds.push(PathCmd::close());
    vec![ContourBuf::new(cmds)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{BBox, ContourSet, FillRule, Resolution};

    fn image(contours: &[ContourBuf], style: StrokeStyle) -> ContourSet {
        let fill = stroke_to_fill(contours, style, GeometryAccuracy::default())
            .unwrap()
            .expect("the stroke paints");
        ContourSet::from_contours(&fill, FillRule::NonZero, Resolution::default()).unwrap()
    }

    /// The image's area may fall short of the exact outline's by what
    /// flattening its arcs cost along the boundary.
    fn assert_area(image: &ContourSet, expected: f64) {
        let boundary = image
            .rings
            .iter()
            .flat_map(crate::geom::region::ring_edges)
            .map(|(start, end)| start.distance_to(end))
            .sum::<f64>();
        let area = image.area();
        assert!(
            (area - expected).abs() <= image.uncertainty_mm * boundary + 1e-9,
            "{area} is not {expected}"
        );
    }

    fn round(width: f64) -> StrokeStyle {
        StrokeStyle::new(width, LineCap::Round)
    }

    fn polyline(points: &[(f64, f64)]) -> ContourBuf {
        ContourBuf::new(
            points
                .iter()
                .enumerate()
                .map(|(index, &(x, y))| {
                    if index == 0 {
                        PathCmd::move_to(Point::new(x, y))
                    } else {
                        PathCmd::line_to(Point::new(x, y))
                    }
                })
                .collect(),
        )
    }

    #[test]
    fn outlines_are_exact_lines_and_arcs() {
        let source = [polyline(&[(0.0, 0.0), (10.0, 0.0)])];
        let fill = stroke_to_fill(&source, round(2.0), GeometryAccuracy::default())
            .unwrap()
            .unwrap();
        assert!(fill.iter().all(|contour| contour.uncertainty_mm < 1e-12));
        assert!(
            fill.iter()
                .flat_map(|contour| &contour.cmds)
                .all(|cmd| cmd.op != PathOp::EllipseTo)
        );
        // A stadium: the rectangle plus one disk.
        assert_area(&image(&source, round(2.0)), 20.0 + PI);
    }

    #[test]
    fn caps_finish_only_the_open_ends() {
        let source = [polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)])];
        let width = 2.0;
        let butt = image(&source, StrokeStyle::new(width, LineCap::Butt));
        let square = image(&source, StrokeStyle::new(width, LineCap::Square));
        // Two 10×2 rectangles overlapping in a unit square, plus the outer
        // quarter disk of the round join.
        let joined = 40.0 - 1.0 + PI / 4.0;
        assert_area(&butt, joined);
        assert_area(&square, joined + 2.0 * width);
        assert_eq!(butt.bbox.min, Point::new(0.0, -1.0));
        assert!((square.bbox.min.x + 1.0).abs() < 1e-9);
        assert!((square.bbox.max.y - 11.0).abs() < 1e-9);
    }

    #[test]
    fn closed_paths_join_every_vertex_and_cap_none() {
        let mut square = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        square.cmds.push(PathCmd::close());
        let butt = image(&[square], StrokeStyle::new(2.0, LineCap::Butt));
        // A 12×12 rounded square minus the 8×8 opening.
        let expected = 144.0 - (4.0 - PI) - 64.0;
        assert_area(&butt, expected);
    }

    #[test]
    fn arcs_sweep_annular_sectors_in_either_direction() {
        for clockwise in [false, true] {
            let (start, end) = if clockwise {
                (Point::new(0.0, 5.0), Point::new(5.0, 0.0))
            } else {
                (Point::new(5.0, 0.0), Point::new(0.0, 5.0))
            };
            let quarter = ContourBuf::new(vec![
                PathCmd::move_to(start),
                PathCmd::arc_to(end, Point::ZERO, clockwise),
            ]);
            let butt = image(&[quarter], StrokeStyle::new(2.0, LineCap::Butt));
            let expected = PI / 4.0 * (36.0 - 16.0);
            assert_area(&butt, expected);
        }
    }

    #[test]
    fn a_full_circle_strokes_to_an_annulus() {
        let circle = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(5.0, 0.0)),
            PathCmd::arc_to(Point::new(5.0, 0.0), Point::ZERO, false),
        ]);
        let ring = image(&[circle], round(2.0));
        assert_area(&ring, PI * (36.0 - 16.0));
        assert!(!ring.contains_point(Point::ZERO));
    }

    #[test]
    fn a_stroke_wider_than_its_arc_has_no_hole() {
        let circle = shapes::circle(1.0).unwrap();
        let disk = image(&[circle], round(2.0));
        assert_area(&disk, PI * 2.25);
        assert!(disk.contains_point(Point::ZERO));
    }

    #[test]
    fn a_stroke_of_no_length_images_its_cap() {
        let at = Point::new(1.0, 2.0);
        let source = [polyline(&[(at.x, at.y), (at.x, at.y)])];
        assert_area(&image(&source, round(0.2)), PI * 0.01);
        let square = image(&source, StrokeStyle::new(0.2, LineCap::Square));
        assert_eq!(
            square.bbox,
            BBox::new(Point::new(0.9, 1.9), Point::new(1.1, 2.1))
        );
        let butt = StrokeStyle::new(0.2, LineCap::Butt);
        assert!(
            stroke_to_fill(&source, butt, GeometryAccuracy::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn every_piece_is_simple_so_either_fill_rule_gives_the_same_image() {
        let mut path = polyline(&[(0.0, 0.0), (4.0, 0.0), (4.0, 0.5), (0.0, 0.5)]);
        path.cmds.push(PathCmd::arc_to(
            Point::new(0.0, 2.5),
            Point::new(0.0, 1.5),
            true,
        ));
        let fill = stroke_to_fill(&[path], round(1.0), GeometryAccuracy::default())
            .unwrap()
            .unwrap();
        let nonzero =
            ContourSet::from_contours(&fill, FillRule::NonZero, Resolution::default()).unwrap();
        let independent = ContourSet::from_filled_contours(&fill, Resolution::default()).unwrap();
        assert!((nonzero.area() - independent.area()).abs() < 1e-9);
    }

    #[test]
    fn stroke_to_fill_rejects_non_positive_width() {
        let accuracy = GeometryAccuracy::default();

        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(1.0, 0.0))];

        assert!(
            stroke_to_fill(&source, StrokeStyle::new(0.0, LineCap::Round), accuracy)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stroke_to_fill_expands_centerline_by_half_width() {
        let accuracy = GeometryAccuracy::default();

        let source = vec![line_contour(Point::new(0.0, 0.0), Point::new(10.0, 0.0))];
        let fill = stroke_to_fill(&source, StrokeStyle::new(2.0, LineCap::Butt), accuracy)
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
        let mut style = StrokeStyle::new(1.0, LineCap::Round);
        style.pattern = LinePattern::Phantom;
        let fill = stroke_to_fill(&[source], style, GeometryAccuracy::default())
            .unwrap()
            .unwrap();
        assert!(fill.iter().all(|contour| contour.uncertainty_mm >= 0.001));
    }

    #[test]
    fn patterned_strokes_flatten_curves_within_budget() {
        let ellipse = crate::geom::shapes::circle(4.0)
            .unwrap()
            .transformed(Affine2 {
                m00: 2.0,
                m01: 0.0,
                m02: 0.0,
                m10: 0.0,
                m11: 1.0,
                m12: 0.0,
            });
        assert!(ellipse.cmds.iter().any(|cmd| cmd.op == PathOp::EllipseTo));
        let mut style = StrokeStyle::new(0.2, LineCap::Round);
        style.pattern = LinePattern::Dashed;
        let accuracy = GeometryAccuracy::default();
        let dashes = stroke_to_fill(&[ellipse], style, accuracy)
            .unwrap()
            .unwrap();
        assert!(dashes.len() > 1);
        assert!(
            dashes
                .iter()
                .all(|dash| dash.uncertainty_mm <= accuracy.max_error_mm())
        );
    }

    fn line_contour(start: Point, end: Point) -> ContourBuf {
        ContourBuf::new(vec![PathCmd::move_to(start), PathCmd::line_to(end)])
    }
}
