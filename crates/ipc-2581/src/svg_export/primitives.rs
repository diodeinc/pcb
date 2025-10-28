/// Geometric Primitives using Cubic Bezier Approximations
///
/// All primitives use cubic Bezier curves for maximum quality through
/// boolean operations. The kappa constant (0.5522847498) provides
/// mathematically optimal circle approximation with 0.027% error.

use skia_safe::Path;

/// Kappa constant for optimal cubic Bezier circle approximation
///
/// Derived from: k = 4 × (√2 - 1) / 3
/// Error: 0.027% of radius (~0.27 microns for 1mm circle)
/// Source: PostScript (1982), used in PDF, SVG
pub const KAPPA: f32 = 0.5522847498;

/// Create a circle as 4 cubic Bezier curves
///
/// This is the industry-standard method for representing circles in vector graphics.
/// Much better quality through boolean operations than Skia's conic circles.
pub fn add_circle_as_cubics(path: &mut Path, center: (f32, f32), radius: f32) {
    let (cx, cy) = center;
    let r = radius;
    let k = r * KAPPA;

    path.move_to((cx + r, cy));

    // Quadrant 1: right → top (0° → 90°)
    path.cubic_to((cx + r, cy - k), (cx + k, cy - r), (cx, cy - r));

    // Quadrant 2: top → left (90° → 180°)
    path.cubic_to((cx - k, cy - r), (cx - r, cy - k), (cx - r, cy));

    // Quadrant 3: left → bottom (180° → 270°)
    path.cubic_to((cx - r, cy + k), (cx - k, cy + r), (cx, cy + r));

    // Quadrant 4: bottom → right (270° → 360°)
    path.cubic_to((cx + k, cy + r), (cx + r, cy + k), (cx + r, cy));

    path.close();
}

/// Create an ellipse as 4 cubic Bezier curves (generalized circle)
pub fn add_ellipse_as_cubics(path: &mut Path, center: (f32, f32), rx: f32, ry: f32) {
    let (cx, cy) = center;
    let kx = rx * KAPPA;
    let ky = ry * KAPPA;

    path.move_to((cx + rx, cy));
    path.cubic_to((cx + rx, cy - ky), (cx + kx, cy - ry), (cx, cy - ry));
    path.cubic_to((cx - kx, cy - ry), (cx - rx, cy - ky), (cx - rx, cy));
    path.cubic_to((cx - rx, cy + ky), (cx - kx, cy + ry), (cx, cy + ry));
    path.cubic_to((cx + kx, cy + ry), (cx + rx, cy + ky), (cx + rx, cy));
    path.close();
}

/// Add a 90° corner arc using cubic bezier
///
/// For rounded rectangles. Draws a 90° clockwise arc around the outside of the rectangle.
/// The path cursor should be at the start of the arc when this is called.
pub fn add_corner_arc_for_rounded_rect(
    path: &mut Path,
    corner: RectCorner,
    arc_center: (f32, f32),
    radius: f32,
) {
    let k = radius * KAPPA;
    let (cx, cy) = arc_center;

    match corner {
        RectCorner::UpperRight => {
            // From top edge (south of arc center) to right edge (east of arc center)
            // Current position: (cx, cy - r)
            // End position: (cx + r, cy)
            path.cubic_to((cx + k, cy - radius), (cx + radius, cy - k), (cx + radius, cy));
        }
        RectCorner::LowerRight => {
            // From right edge (east of arc center) to bottom edge (south of arc center)
            // Current position: (cx + r, cy)
            // End position: (cx, cy + r)
            path.cubic_to((cx + radius, cy + k), (cx + k, cy + radius), (cx, cy + radius));
        }
        RectCorner::LowerLeft => {
            // From bottom edge (south of arc center) to left edge (west of arc center)
            // Current position: (cx, cy + r)
            // End position: (cx - r, cy)
            path.cubic_to((cx - k, cy + radius), (cx - radius, cy + k), (cx - radius, cy));
        }
        RectCorner::UpperLeft => {
            // From left edge (west of arc center) to top edge (north of arc center)
            // Current position: (cx - r, cy)
            // End position: (cx, cy - r)
            path.cubic_to((cx - radius, cy - k), (cx - k, cy - radius), (cx, cy - radius));
        }
    }
}

/// Which corner of a rectangle
#[derive(Debug, Clone, Copy)]
pub enum RectCorner {
    UpperRight,
    LowerRight,
    LowerLeft,
    UpperLeft,
}
