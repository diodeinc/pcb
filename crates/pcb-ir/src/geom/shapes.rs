//! Origin-centered primitive shape constructors.
//!
//! These are the shared pad/aperture primitives used by every frontend
//! (IPC-2581 standard primitives, Gerber standard apertures) and by aperture
//! flattening. All shapes are built in local coordinates centered on the
//! origin; apply an [`Affine2`](crate::geom::Affine2) placement with
//! [`transform_cmds`](crate::geom::path::transform_cmds).
//!
//! Builders that can represent corners either as circular arcs or as cubic
//! Beziers take an `arcs` flag: arcs are exact and preferred, but only
//! transform correctly under similarity transforms
//! ([`Affine2::preserves_circles`](crate::geom::Affine2::preserves_circles)); pass
//! `arcs = false` when the placement may skew or scale non-uniformly.
//!
//! Constructors return `None` for degenerate dimensions.

use crate::geom::path::{ContourBuf, PathCmd};
use crate::geom::point::Point;

/// Cubic Bezier circle approximation constant.
const KAPPA: f64 = 0.552_284_749_830_793_6;

/// A circle of the given diameter, as four quarter arcs.
pub fn circle(diameter: f64) -> Option<ContourBuf> {
    if diameter <= 0.0 {
        return None;
    }
    let r = diameter / 2.0;
    let center = Point::ZERO;
    Some(ContourBuf::new(vec![
        PathCmd::move_to(Point::new(r, 0.0)),
        PathCmd::arc_to(Point::new(0.0, r), center, false),
        PathCmd::arc_to(Point::new(-r, 0.0), center, false),
        PathCmd::arc_to(Point::new(0.0, -r), center, false),
        PathCmd::arc_to(Point::new(r, 0.0), center, false),
        PathCmd::close(),
    ]))
}

/// An axis-aligned ellipse as four cubic segments. Safe under any affine
/// transform; also the cubic fallback for circles.
pub fn ellipse(width: f64, height: f64) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let rx = width / 2.0;
    let ry = height / 2.0;
    let k = KAPPA;
    Some(ContourBuf::new(vec![
        PathCmd::move_to(Point::new(rx, 0.0)),
        PathCmd::cubic_to(
            Point::new(rx, k * ry),
            Point::new(k * rx, ry),
            Point::new(0.0, ry),
        ),
        PathCmd::cubic_to(
            Point::new(-k * rx, ry),
            Point::new(-rx, k * ry),
            Point::new(-rx, 0.0),
        ),
        PathCmd::cubic_to(
            Point::new(-rx, -k * ry),
            Point::new(-k * rx, -ry),
            Point::new(0.0, -ry),
        ),
        PathCmd::cubic_to(
            Point::new(k * rx, -ry),
            Point::new(rx, -k * ry),
            Point::new(rx, 0.0),
        ),
        PathCmd::close(),
    ]))
}

/// An axis-aligned centered rectangle.
pub fn rect(width: f64, height: f64) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let hw = width / 2.0;
    let hh = height / 2.0;
    closed_polygon(vec![
        Point::new(-hw, -hh),
        Point::new(hw, -hh),
        Point::new(hw, hh),
        Point::new(-hw, hh),
    ])
}

/// Corner selection for [`rounded_rect`] and [`chamfered_rect`], in IPC-2581
/// order: `[upper_right, lower_right, lower_left, upper_left]`.
pub type Corners = [bool; 4];

pub const ALL_CORNERS: Corners = [true; 4];

/// A centered rectangle with the selected corners rounded to `radius`.
pub fn rounded_rect(
    width: f64,
    height: f64,
    radius: f64,
    corners: Corners,
    arcs: bool,
) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let hw = width / 2.0;
    let hh = height / 2.0;
    let r = radius.min(hw).min(hh).max(0.0);
    if r == 0.0 || !corners.iter().any(|corner| *corner) {
        return rect(width, height);
    }

    let k = KAPPA;
    let [upper_right, lower_right, lower_left, upper_left] = corners;
    let mut cmds = Vec::new();

    cmds.push(PathCmd::move_to(Point::new(
        -hw + if lower_left { r } else { 0.0 },
        -hh,
    )));

    cmds.push(PathCmd::line_to(Point::new(
        hw - if lower_right { r } else { 0.0 },
        -hh,
    )));
    if lower_right {
        if arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(hw, -hh + r),
                Point::new(hw - r, -hh + r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(hw - r + k * r, -hh),
                Point::new(hw, -hh + r - k * r),
                Point::new(hw, -hh + r),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        hw,
        hh - if upper_right { r } else { 0.0 },
    )));
    if upper_right {
        if arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(hw - r, hh),
                Point::new(hw - r, hh - r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(hw, hh - r + k * r),
                Point::new(hw - r + k * r, hh),
                Point::new(hw - r, hh),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw + if upper_left { r } else { 0.0 },
        hh,
    )));
    if upper_left {
        if arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(-hw, hh - r),
                Point::new(-hw + r, hh - r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(-hw + r - k * r, hh),
                Point::new(-hw, hh - r + k * r),
                Point::new(-hw, hh - r),
            ));
        }
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw,
        -hh + if lower_left { r } else { 0.0 },
    )));
    if lower_left {
        if arcs {
            cmds.push(PathCmd::arc_to(
                Point::new(-hw + r, -hh),
                Point::new(-hw + r, -hh + r),
                false,
            ));
        } else {
            cmds.push(PathCmd::cubic_to(
                Point::new(-hw, -hh + r - k * r),
                Point::new(-hw + r - k * r, -hh),
                Point::new(-hw + r, -hh),
            ));
        }
    }
    cmds.push(PathCmd::close());

    Some(ContourBuf::new(cmds))
}

/// A centered rectangle with the selected corners cut at 45° by `chamfer`.
pub fn chamfered_rect(
    width: f64,
    height: f64,
    chamfer: f64,
    corners: Corners,
) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let hw = width / 2.0;
    let hh = height / 2.0;
    let c = chamfer.min(hw).min(hh).max(0.0);
    if c == 0.0 || !corners.iter().any(|corner| *corner) {
        return rect(width, height);
    }

    let [upper_right, lower_right, lower_left, upper_left] = corners;
    let mut points = Vec::with_capacity(8);

    points.push(Point::new(-hw + if lower_left { c } else { 0.0 }, -hh));

    points.push(Point::new(hw - if lower_right { c } else { 0.0 }, -hh));
    if lower_right {
        points.push(Point::new(hw, -hh + c));
    }

    points.push(Point::new(hw, hh - if upper_right { c } else { 0.0 }));
    if upper_right {
        points.push(Point::new(hw - c, hh));
    }

    points.push(Point::new(-hw + if upper_left { c } else { 0.0 }, hh));
    if upper_left {
        points.push(Point::new(-hw, hh - c));
    }

    points.push(Point::new(-hw, -hh + if lower_left { c } else { 0.0 }));

    closed_polygon(points)
}

/// A stadium/obround: a rectangle with full-radius caps on the short axis.
pub fn obround(width: f64, height: f64, arcs: bool) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    if (width - height).abs() < f64::EPSILON {
        return if arcs {
            circle(width)
        } else {
            ellipse(width, height)
        };
    }

    let k = KAPPA;
    let cmds = if width > height {
        let r = height / 2.0;
        let a = (width - height) / 2.0;
        if arcs {
            vec![
                PathCmd::move_to(Point::new(-a, -r)),
                PathCmd::line_to(Point::new(a, -r)),
                PathCmd::arc_to(Point::new(a, r), Point::new(a, 0.0), false),
                PathCmd::line_to(Point::new(-a, r)),
                PathCmd::arc_to(Point::new(-a, -r), Point::new(-a, 0.0), false),
                PathCmd::close(),
            ]
        } else {
            vec![
                PathCmd::move_to(Point::new(a, -r)),
                PathCmd::line_to(Point::new(-a, -r)),
                PathCmd::cubic_to(
                    Point::new(-a - k * r, -r),
                    Point::new(-a - r, -k * r),
                    Point::new(-a - r, 0.0),
                ),
                PathCmd::cubic_to(
                    Point::new(-a - r, k * r),
                    Point::new(-a - k * r, r),
                    Point::new(-a, r),
                ),
                PathCmd::line_to(Point::new(a, r)),
                PathCmd::cubic_to(
                    Point::new(a + k * r, r),
                    Point::new(a + r, k * r),
                    Point::new(a + r, 0.0),
                ),
                PathCmd::cubic_to(
                    Point::new(a + r, -k * r),
                    Point::new(a + k * r, -r),
                    Point::new(a, -r),
                ),
                PathCmd::close(),
            ]
        }
    } else {
        let r = width / 2.0;
        let a = (height - width) / 2.0;
        if arcs {
            vec![
                PathCmd::move_to(Point::new(r, -a)),
                PathCmd::line_to(Point::new(r, a)),
                PathCmd::arc_to(Point::new(-r, a), Point::new(0.0, a), false),
                PathCmd::line_to(Point::new(-r, -a)),
                PathCmd::arc_to(Point::new(r, -a), Point::new(0.0, -a), false),
                PathCmd::close(),
            ]
        } else {
            vec![
                PathCmd::move_to(Point::new(r, -a)),
                PathCmd::line_to(Point::new(r, a)),
                PathCmd::cubic_to(
                    Point::new(r, a + k * r),
                    Point::new(k * r, a + r),
                    Point::new(0.0, a + r),
                ),
                PathCmd::cubic_to(
                    Point::new(-k * r, a + r),
                    Point::new(-r, a + k * r),
                    Point::new(-r, a),
                ),
                PathCmd::line_to(Point::new(-r, -a)),
                PathCmd::cubic_to(
                    Point::new(-r, -a - k * r),
                    Point::new(-k * r, -a - r),
                    Point::new(0.0, -a - r),
                ),
                PathCmd::cubic_to(
                    Point::new(k * r, -a - r),
                    Point::new(r, -a - k * r),
                    Point::new(r, -a),
                ),
                PathCmd::close(),
            ]
        }
    };

    Some(ContourBuf::new(cmds))
}

/// A regular polygon inscribed in a circle of `outer_diameter`, with the
/// first vertex at `rotation_degrees` from the positive X axis (the Gerber
/// `P` aperture convention).
pub fn regular_polygon(
    outer_diameter: f64,
    vertices: u32,
    rotation_degrees: f64,
) -> Option<ContourBuf> {
    if outer_diameter <= 0.0 || vertices < 3 {
        return None;
    }
    let radius = outer_diameter / 2.0;
    let base = rotation_degrees.to_radians();
    let points = (0..vertices)
        .map(|index| {
            let angle = base + index as f64 * std::f64::consts::TAU / vertices as f64;
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect();
    closed_polygon(points)
}

/// A closed polygon through the given points.
pub fn closed_polygon(points: Vec<Point>) -> Option<ContourBuf> {
    if points.len() < 3 {
        return None;
    }
    let mut cmds = Vec::with_capacity(points.len() + 1);
    let mut iter = points.into_iter();
    cmds.push(PathCmd::move_to(iter.next().expect("checked length")));
    cmds.extend(iter.map(PathCmd::line_to));
    cmds.push(PathCmd::close());
    Some(ContourBuf::new(cmds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::affine::Affine2;
    use crate::geom::path::transform_cmds;
    use crate::geom::point::Mirror;

    #[test]
    fn circle_bbox_is_tight() {
        let contour = circle(3.0).unwrap();

        assert!((contour.bbox.min.x + 1.5).abs() <= 1e-9);
        assert!((contour.bbox.max.y - 1.5).abs() <= 1e-9);
    }

    #[test]
    fn degenerate_shapes_are_rejected() {
        assert!(circle(0.0).is_none());
        assert!(rect(1.0, 0.0).is_none());
        assert!(regular_polygon(1.0, 2, 0.0).is_none());
    }

    #[test]
    fn rounded_rect_with_zero_radius_is_a_rect() {
        let rounded = rounded_rect(4.0, 2.0, 0.0, ALL_CORNERS, true).unwrap();
        let plain = rect(4.0, 2.0).unwrap();

        assert_eq!(rounded.cmds, plain.cmds);
    }

    #[test]
    fn obround_arcs_and_cubics_agree_on_bounds() {
        let arcs = obround(5.0, 2.0, true).unwrap();
        let cubics = obround(5.0, 2.0, false).unwrap();

        for (a, c) in [
            (arcs.bbox.min.x, cubics.bbox.min.x),
            (arcs.bbox.min.y, cubics.bbox.min.y),
            (arcs.bbox.max.x, cubics.bbox.max.x),
            (arcs.bbox.max.y, cubics.bbox.max.y),
        ] {
            assert!((a - c).abs() <= 1e-9, "expected {a} to be close to {c}");
        }
    }

    #[test]
    fn placement_transform_moves_shape() {
        let contour = circle(2.0).unwrap();
        let transform = Affine2::placement(Point::new(10.0, 5.0), 0.0, Mirror::NONE, 1.0);

        let placed = transform_cmds(contour.cmds, transform);

        assert!((placed.bbox.min.x - 9.0).abs() <= 1e-9);
        assert!((placed.bbox.max.y - 6.0).abs() <= 1e-9);
    }
}
