//! Origin-centered primitive shape constructors.
//!
//! These are the shared pad/aperture primitives used by every frontend
//! (IPC-2581 standard primitives, Gerber standard apertures) and by aperture
//! flattening. All shapes are built in local coordinates centered on the
//! origin; apply an [`Affine2`](crate::geom::Affine2) placement with
//! [`ContourBuf::transformed`].
//!
//! Transformations convert arcs only when required by the placement.
//!
//! Constructors return `None` for degenerate dimensions.

use crate::geom::path::{ContourBuf, PathCmd};
use crate::geom::point::Point;
use std::f64::consts::PI;

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
/// transform; circles retain exact arcs.
pub fn ellipse(width: f64, height: f64) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    if width == height {
        return circle(width);
    }
    let rx = width / 2.0;
    let ry = height / 2.0;
    let k = KAPPA;
    Some(
        ContourBuf::new(vec![
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
        ])
        .with_uncertainty(0.0003 * rx.max(ry))
        .with_ellipse_source(crate::geom::Affine2 {
            m00: rx,
            m11: ry,
            ..crate::geom::Affine2::IDENTITY
        }),
    )
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
pub fn rounded_rect(width: f64, height: f64, radius: f64, corners: Corners) -> Option<ContourBuf> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let hw = width / 2.0;
    let hh = height / 2.0;
    let r = radius.min(hw).min(hh).max(0.0);
    if r == 0.0 || !corners.iter().any(|corner| *corner) {
        return rect(width, height);
    }

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
        cmds.push(PathCmd::arc_to(
            Point::new(hw, -hh + r),
            Point::new(hw - r, -hh + r),
            false,
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        hw,
        hh - if upper_right { r } else { 0.0 },
    )));
    if upper_right {
        cmds.push(PathCmd::arc_to(
            Point::new(hw - r, hh),
            Point::new(hw - r, hh - r),
            false,
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw + if upper_left { r } else { 0.0 },
        hh,
    )));
    if upper_left {
        cmds.push(PathCmd::arc_to(
            Point::new(-hw, hh - r),
            Point::new(-hw + r, hh - r),
            false,
        ));
    }

    cmds.push(PathCmd::line_to(Point::new(
        -hw,
        -hh + if lower_left { r } else { 0.0 },
    )));
    if lower_left {
        cmds.push(PathCmd::arc_to(
            Point::new(-hw + r, -hh),
            Point::new(-hw + r, -hh + r),
            false,
        ));
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
pub fn obround(width: f64, height: f64) -> Option<ContourBuf> {
    rounded_rect(width, height, width.min(height) / 2.0, ALL_CORNERS)
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

/// A regular hexagon with circular corner fillets.
///
/// `radius` is the sharp hexagon's circumradius. `corner_radius` is measured
/// from each fillet center to its tangent points on the adjacent sides.
pub fn rounded_hexagon(
    radius: f64,
    corner_radius: f64,
    rotation_degrees: f64,
) -> Option<ContourBuf> {
    const SQRT_3: f64 = 1.732_050_807_568_877_2;

    if !radius.is_finite()
        || !corner_radius.is_finite()
        || !rotation_degrees.is_finite()
        || radius <= 0.0
        || corner_radius <= 0.0
        || corner_radius >= radius * SQRT_3 / 2.0
    {
        return None;
    }

    let tangent_distance = corner_radius / SQRT_3;
    let center_inset = 2.0 * corner_radius / SQRT_3;
    let rotation = rotation_degrees.to_radians();
    let vertices = (0..6)
        .map(|index| {
            let angle = rotation + index as f64 * PI / 3.0;
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect::<Vec<_>>();

    let corner = |index: usize| {
        let previous = vertices[(index + 5) % 6];
        let vertex = vertices[index];
        let next = vertices[(index + 1) % 6];
        let incoming = vertex + (previous - vertex) * (tangent_distance / radius);
        let outgoing = vertex + (next - vertex) * (tangent_distance / radius);
        let center = vertex * ((radius - center_inset) / radius);
        (incoming, outgoing, center)
    };

    let (first_incoming, _, _) = corner(0);
    let mut commands = Vec::with_capacity(14);
    commands.push(PathCmd::move_to(first_incoming));
    for index in 0..6 {
        let (incoming, outgoing, center) = corner(index);
        if index > 0 {
            commands.push(PathCmd::line_to(incoming));
        }
        commands.push(PathCmd::arc_to(outgoing, center, false));
    }
    commands.push(PathCmd::close());
    Some(ContourBuf::new(commands))
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
    use crate::geom::GeometryAccuracy;

    use super::*;
    use crate::geom::affine::Affine2;
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
        let rounded = rounded_rect(4.0, 2.0, 0.0, ALL_CORNERS).unwrap();
        let plain = rect(4.0, 2.0).unwrap();

        assert_eq!(rounded.cmds, plain.cmds);
    }

    #[test]
    fn placement_transform_moves_shape() {
        let accuracy = GeometryAccuracy::default();

        let contour = circle(2.0).unwrap();
        let transform = Affine2::placement(Point::new(10.0, 5.0), 0.0, Mirror::NONE, 1.0);

        let placed = contour.transformed(transform, accuracy).unwrap();

        assert!((placed.bbox.min.x - 9.0).abs() <= 1e-9);
        assert!((placed.bbox.max.y - 6.0).abs() <= 1e-9);
    }
}
