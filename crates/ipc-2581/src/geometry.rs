use crate::{PolyStep, Polygon};
use kurbo::{Arc as KurboArc, Point, Rect, Shape};

/// Helper to create kurbo Arc from IPC-2581 curve data
pub fn create_arc(
    start_x: f64,
    start_y: f64,
    end_x: f64,
    end_y: f64,
    center_x: f64,
    center_y: f64,
    clockwise: bool,
) -> KurboArc {
    let start_angle = (start_y - center_y).atan2(start_x - center_x);
    let end_angle = (end_y - center_y).atan2(end_x - center_x);
    let radius = ((start_x - center_x).powi(2) + (start_y - center_y).powi(2)).sqrt();

    // Compute sweep angle respecting IPC-2581 semantics:
    // - clockwise flag determines direction
    // - sweep magnitude can be >180° (long arcs are valid)
    let two_pi = 2.0 * std::f64::consts::PI;
    let delta = end_angle - start_angle;

    // Helper to normalize angle to [0, 2π)
    let mod_2pi = |mut a: f64| -> f64 {
        a %= two_pi;
        if a < 0.0 {
            a += two_pi;
        }
        a
    };

    let mut sweep_angle = if clockwise {
        // CW sweep is negative in (-2π, 0]
        let d_ccw = mod_2pi(delta);
        if d_ccw == 0.0 {
            0.0
        } else {
            d_ccw - two_pi
        }
    } else {
        // CCW sweep is positive in [0, 2π)
        mod_2pi(delta)
    };

    // Snap very small angles to zero to avoid spurious tiny arcs
    let eps = 1e-12;
    if sweep_angle.abs() < eps {
        sweep_angle = 0.0;
    }

    KurboArc::new(
        Point::new(center_x, center_y),
        kurbo::Vec2::new(radius, radius),
        start_angle,
        sweep_angle,
        0.0,
    )
}

/// Calculate bounding box for a single polygon, accounting for arc geometry.
/// Returns (min_x, min_y, max_x, max_y)
pub fn polygon_bounding_box(
    polygon: &Polygon,
    x_offset: f64,
    y_offset: f64,
) -> (f64, f64, f64, f64) {
    let mut bounds = Rect::new(
        polygon.begin.x + x_offset,
        polygon.begin.y + y_offset,
        polygon.begin.x + x_offset,
        polygon.begin.y + y_offset,
    );

    let mut current_x = polygon.begin.x + x_offset;
    let mut current_y = polygon.begin.y + y_offset;

    for poly_step in &polygon.steps {
        match poly_step {
            PolyStep::Segment(s) => {
                current_x = s.x + x_offset;
                current_y = s.y + y_offset;
                bounds = bounds.union_pt(Point::new(current_x, current_y));
            }
            PolyStep::Curve(c) => {
                let arc = create_arc(
                    current_x,
                    current_y,
                    c.x + x_offset,
                    c.y + y_offset,
                    c.center_x + x_offset,
                    c.center_y + y_offset,
                    c.clockwise,
                );
                bounds = bounds.union(arc.bounding_box());
                current_x = c.x + x_offset;
                current_y = c.y + y_offset;
            }
        }
    }

    (bounds.x0, bounds.y0, bounds.x1, bounds.y1)
}

/// Calculate accurate bounding box for a board outline including the main polygon,
/// cutouts, and slots. Properly handles arc geometry.
/// Returns (width, height) in the same units as the polygon coordinates.
pub fn calculate_board_outline_dimensions(
    outline: &Polygon,
    cutouts: &[Polygon],
    slots: &[(Polygon, f64, f64)],
) -> (f64, f64) {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = polygon_bounding_box(outline, 0.0, 0.0);

    for cutout in cutouts {
        let (x0, y0, x1, y1) = polygon_bounding_box(cutout, 0.0, 0.0);
        min_x = min_x.min(x0);
        min_y = min_y.min(y0);
        max_x = max_x.max(x1);
        max_y = max_y.max(y1);
    }

    for (slot_outline, x_offset, y_offset) in slots {
        let (x0, y0, x1, y1) = polygon_bounding_box(slot_outline, *x_offset, *y_offset);
        min_x = min_x.min(x0);
        min_y = min_y.min(y0);
        max_x = max_x.max(x1);
        max_y = max_y.max(y1);
    }

    (max_x - min_x, max_y - min_y)
}
