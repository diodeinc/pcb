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
pub const KAPPA: f32 = 0.552_284_8;

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

/// Add an arc segment to a path
///
/// Adds an arc from the current position to `end_point`, centered at `center`.
/// The arc direction is determined by `clockwise`.
///
/// This is used for polygon curves and other arc segments in IPC-2581 geometries.
pub fn add_arc_segment(
    path: &mut Path,
    start: (f64, f64),
    end: (f64, f64),
    center: (f64, f64),
    clockwise: bool,
) {
    let (start_x, start_y) = start;
    let (end_x, end_y) = end;
    let (center_x, center_y) = center;

    // Calculate arc parameters
    let sx = (start_x - center_x) as f32;
    let sy = (start_y - center_y) as f32;
    let ex = (end_x - center_x) as f32;
    let ey = (end_y - center_y) as f32;

    let start_radius = (sx * sx + sy * sy).sqrt();
    let end_radius = (ex * ex + ey * ey).sqrt();

    // Use average radius (handles minor floating point errors)
    let radius = (start_radius + end_radius) / 2.0;

    // Calculate start and end angles (in degrees)
    let start_angle = sy.atan2(sx).to_degrees();
    let end_angle = ey.atan2(ex).to_degrees();

    // Calculate sweep angle (accounting for direction)
    let mut sweep_angle = end_angle - start_angle;

    // Normalize sweep angle based on direction
    if clockwise {
        // For clockwise, we want negative sweep
        if sweep_angle > 0.0 {
            sweep_angle -= 360.0;
        }
    } else {
        // For counter-clockwise, we want positive sweep
        if sweep_angle < 0.0 {
            sweep_angle += 360.0;
        }
    }

    // Create oval (bounding box for the arc)
    let oval = skia_safe::Rect::from_xywh(
        center_x as f32 - radius,
        center_y as f32 - radius,
        radius * 2.0,
        radius * 2.0,
    );

    // Add arc to path
    path.arc_to(oval, start_angle, sweep_angle, false);
}

/// Create an oval/stadium shape (line segment with semicircular caps)
///
/// Per IPC-2581 spec: "rectangle with complete radius (180° arc) at each end"
/// This is different from an ellipse - it has flat sides parallel to the longer axis.
///
/// For a 1.0mm × 2.1mm vertical oval:
/// - radius = 0.5mm (width / 2)
/// - segment = 1.1mm (height - width)
/// - Shape: vertical line with 0.5mm radius semicircular caps at top/bottom
pub fn add_oval_as_stadium(path: &mut Path, center: (f32, f32), width: f64, height: f64) {
    let (cx, cy) = center;

    // Determine orientation and calculate geometry
    let (radius, half_segment, is_vertical) = if height > width {
        (width / 2.0, (height - width) / 2.0, true)
    } else {
        (height / 2.0, (width - height) / 2.0, false)
    };

    let r = radius as f32;
    let hs = half_segment as f32;
    let k = r * KAPPA; // Kappa for semicircle (same as full circle)

    if is_vertical {
        // Vertical stadium: line segment along y-axis with cubic bezier caps
        path.move_to((cx + r, cy - hs));
        path.line_to((cx + r, cy + hs));

        // Top semicircular cap (right to left via cubic beziers)
        // Start at (cx+r, cy+hs), go CCW to (cx-r, cy+hs)
        let cap_cy = cy + hs; // Cap center Y coordinate
        path.cubic_to((cx + r, cap_cy + k), (cx + k, cap_cy + r), (cx, cap_cy + r));
        path.cubic_to((cx - k, cap_cy + r), (cx - r, cap_cy + k), (cx - r, cap_cy));

        path.line_to((cx - r, cy - hs));

        // Bottom semicircular cap (left to right via cubic beziers)
        // Start at (cx-r, cy-hs), go CCW to (cx+r, cy-hs)
        let cap_cy = cy - hs; // Cap center Y coordinate
        path.cubic_to((cx - r, cap_cy - k), (cx - k, cap_cy - r), (cx, cap_cy - r));
        path.cubic_to((cx + k, cap_cy - r), (cx + r, cap_cy - k), (cx + r, cap_cy));
    } else {
        // Horizontal stadium: line segment along x-axis with cubic bezier caps
        path.move_to((cx - hs, cy - r));

        // Left semicircular cap (bottom to top via cubic beziers)
        // Start at (cx-hs, cy-r), go CCW to (cx-hs, cy+r)
        let cap_cx = cx - hs; // Cap center X coordinate
        path.cubic_to((cap_cx - k, cy - r), (cap_cx - r, cy - k), (cap_cx - r, cy));
        path.cubic_to((cap_cx - r, cy + k), (cap_cx - k, cy + r), (cap_cx, cy + r));

        path.line_to((cx + hs, cy + r));

        // Right semicircular cap (top to bottom via cubic beziers)
        // Start at (cx+hs, cy+r), go CCW to (cx+hs, cy-r)
        let cap_cx = cx + hs; // Cap center X coordinate
        path.cubic_to((cap_cx + k, cy + r), (cap_cx + r, cy + k), (cap_cx + r, cy));
        path.cubic_to((cap_cx + r, cy - k), (cap_cx + k, cy - r), (cap_cx, cy - r));
    }

    path.close();
}
