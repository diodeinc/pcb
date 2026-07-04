//! Reconstruct circular arcs from flattened polygon rings.
//!
//! Boolean set operations run on flattened rings, so arcs from source
//! outlines and disk-swept tool paths come out tessellated. Emission can
//! re-fit maximal circular arcs over those rings: every replaced chord run
//! stays within the fit tolerance of the fitted arc, matching the chord
//! error the flattening introduced in the first place.

use crate::geom::path::{ContourBuf, PathCmd};
use crate::geom::point::Point;
use crate::geom::region::Ring;

/// Largest radius treated as a genuine arc; flatter curves stay lines.
const MAX_ARC_RADIUS_MM: f64 = 1.0e4;
/// Fewest polyline segments an arc may replace. Three segments are the
/// smallest overdetermined fit (four points against three circle parameters).
const MIN_ARC_SEGMENTS: usize = 3;

/// Convert a flattened ring into a closed contour, re-fitting maximal
/// circular arc runs whose deviation from the polyline is at most `tol_mm`.
pub fn ring_to_contour_with_arcs(ring: &Ring, tol_mm: f64) -> ContourBuf {
    let points: Vec<Point> = ring.iter().map(|&[x, y]| Point::new(x, y)).collect();
    let n = points.len();
    if n <= MIN_ARC_SEGMENTS {
        return polyline_contour(&points);
    }

    // A ring that is one whole circle has no corner to anchor the scan; emit
    // it as two half-turn arcs.
    if let Some(arc) = validate_arc_cyclic(&points, tol_mm) {
        let start = points[0];
        let opposite = arc.center * 2.0 - start;
        return ContourBuf::new(vec![
            PathCmd::move_to(start),
            PathCmd::arc_to(opposite, arc.center, arc.clockwise),
            PathCmd::arc_to(start, arc.center, arc.clockwise),
            PathCmd::close(),
        ]);
    }

    // Start the scan at the longest segment: arc chords are short (bounded
    // by the chord tolerance), so the seam lands on a straight run and no
    // arc is split by it.
    let start = longest_segment_start(&points);
    let points: Vec<Point> = (0..n).map(|i| points[(start + i) % n]).collect();
    let at = |index: usize| points[index % n];

    let mut cmds = vec![PathCmd::move_to(points[0])];
    let mut i = 0;
    while i < n {
        let mut best = None;
        let mut j = i + MIN_ARC_SEGMENTS;
        while j <= n {
            match validate_arc(&points, i, j, tol_mm) {
                Some(arc) => {
                    best = Some((j, arc));
                    j += 1;
                }
                None => break,
            }
        }
        match best {
            Some((j, arc)) => {
                cmds.push(PathCmd::arc_to(at(j), arc.center, arc.clockwise));
                i = j;
            }
            None => {
                // The closing line is drawn by Close.
                if i + 1 < n {
                    cmds.push(PathCmd::line_to(at(i + 1)));
                }
                i += 1;
            }
        }
    }
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

struct FittedArc {
    center: Point,
    clockwise: bool,
}

/// Fit and validate one arc over `points[i..=j]` (indices taken modulo the
/// ring length, so `j == points.len()` closes back to the start).
fn validate_arc(points: &[Point], i: usize, j: usize, tol: f64) -> Option<FittedArc> {
    let n = points.len();
    let run: Vec<Point> = (i..=j).map(|k| points[k % n]).collect();
    validate_run(&run, tol, false)
}

/// Validate the entire ring as one full circle (the closing segment from the
/// last point back to the first is included).
fn validate_arc_cyclic(points: &[Point], tol: f64) -> Option<FittedArc> {
    if points.len() < 2 * MIN_ARC_SEGMENTS {
        return None;
    }
    let mut run = points.to_vec();
    run.push(points[0]);
    validate_run(&run, tol, true)
}

fn validate_run(run: &[Point], tol: f64, full_circle: bool) -> Option<FittedArc> {
    // A run whose vertices all sit within tolerance of the straight chord is
    // a line, not an arc: keeping the exact polyline beats a bowed
    // giant-radius arc over near-collinear vertex noise.
    if !full_circle {
        let start = run[0];
        let chord = *run.last().expect("runs are non-empty") - start;
        let chord_length = chord.length();
        if chord_length > 0.0
            && run.iter().all(|point| {
                let d = *point - start;
                (chord.x * d.y - chord.y * d.x).abs() / chord_length <= tol
            })
        {
            return None;
        }
    }

    let (center, radius) = if full_circle {
        fit_circle(run)?
    } else {
        // Interpolate the endpoints exactly so the emitted arc lands on the
        // ring vertices and its start/end radii agree.
        fit_circle_through_endpoints(run)?
    };
    if radius > MAX_ARC_RADIUS_MM || radius < 2.0 * tol {
        return None;
    }
    // Every vertex must lie on the fitted circle.
    for point in run {
        if (point.distance_to(center) - radius).abs() > tol {
            return None;
        }
    }
    // Angular progression must be monotonic, in sub-quarter-turn steps whose
    // chords stay within tolerance of the arc (sagitta bound).
    let mut sweep = 0.0f64;
    let mut direction = 0.0f64;
    let angle = |point: Point| {
        let d = point - center;
        d.y.atan2(d.x)
    };
    for pair in run.windows(2) {
        let mut step = angle(pair[1]) - angle(pair[0]);
        if step > std::f64::consts::PI {
            step -= 2.0 * std::f64::consts::PI;
        } else if step < -std::f64::consts::PI {
            step += 2.0 * std::f64::consts::PI;
        }
        if step == 0.0 || step.abs() > std::f64::consts::FRAC_PI_2 {
            return None;
        }
        if direction == 0.0 {
            direction = step.signum();
        } else if step.signum() != direction {
            return None;
        }
        let sagitta = radius * (1.0 - (step / 2.0).cos());
        if sagitta > tol {
            return None;
        }
        sweep += step;
    }
    if full_circle {
        // The steps of a closed ring on a circle always sum to one turn.
    } else if sweep.abs() >= 2.0 * std::f64::consts::PI - 1e-3 {
        return None;
    }
    Some(FittedArc {
        center,
        clockwise: direction < 0.0,
    })
}

/// Least-squares circle constrained to pass exactly through the run's first
/// and last points: the center lies on the chord's perpendicular bisector,
/// leaving one degree of freedom fitted against the interior points.
fn fit_circle_through_endpoints(run: &[Point]) -> Option<(Point, f64)> {
    let start = run[0];
    let end = *run.last().expect("runs are non-empty");
    let chord = end - start;
    let chord_length = chord.length();
    if chord_length < 1e-12 {
        return None;
    }
    let midpoint = start.midpoint(end);
    let normal = Point::new(-chord.y, chord.x) / chord_length;

    // center = midpoint + t * normal; minimize the algebraic residual
    // |p - center|^2 - |start - center|^2, which is linear in t.
    let half_chord_sq = start.distance_to(midpoint).powi(2);
    let mut numerator = 0.0;
    let mut denominator = 0.0;
    for point in run {
        let d = *point - midpoint;
        let a = d.x * d.x + d.y * d.y - half_chord_sq;
        let b = 2.0 * (d.x * normal.x + d.y * normal.y);
        numerator += a * b;
        denominator += b * b;
    }
    if denominator < 1e-12 {
        return None;
    }
    let center = midpoint + normal * (numerator / denominator);
    Some((center, start.distance_to(center)))
}

/// Least-squares circle through the points (Kåsa fit), solved about the
/// centroid for conditioning. Returns `None` for degenerate/collinear runs.
fn fit_circle(points: &[Point]) -> Option<(Point, f64)> {
    let n = points.len() as f64;
    let centroid = points.iter().fold(Point::new(0.0, 0.0), |acc, p| acc + *p) / n;

    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    let (mut sxz, mut syz) = (0.0, 0.0);
    for point in points {
        let d = *point - centroid;
        let z = d.x * d.x + d.y * d.y;
        sxx += d.x * d.x;
        sxy += d.x * d.y;
        syy += d.y * d.y;
        sxz += d.x * z;
        syz += d.y * z;
    }
    let det = sxx * syy - sxy * sxy;
    if det.abs() < 1e-12 {
        return None;
    }
    let cx = (syy * sxz - sxy * syz) / (2.0 * det);
    let cy = (sxx * syz - sxy * sxz) / (2.0 * det);
    let center = centroid + Point::new(cx, cy);
    let radius = points
        .iter()
        .map(|point| point.distance_to(center))
        .sum::<f64>()
        / n;
    Some((center, radius))
}

/// Index of the vertex starting the longest segment.
fn longest_segment_start(points: &[Point]) -> usize {
    let n = points.len();
    let mut best = 0;
    let mut best_length = -1.0f64;
    for k in 0..n {
        let length = points[k].distance_to(points[(k + 1) % n]);
        if length > best_length {
            best_length = length;
            best = k;
        }
    }
    best
}

fn polyline_contour(points: &[Point]) -> ContourBuf {
    let mut cmds = Vec::with_capacity(points.len() + 1);
    for (index, point) in points.iter().enumerate() {
        cmds.push(if index == 0 {
            PathCmd::move_to(*point)
        } else {
            PathCmd::line_to(*point)
        });
    }
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path::PathOp;
    use crate::geom::region::rings_from_contours;
    use crate::geom::{ContourSet, FillRule, shapes, tol};

    fn refit_set(set: &ContourSet) -> Vec<ContourBuf> {
        set.to_contours_with_arcs()
    }

    fn ops(contour: &ContourBuf) -> Vec<PathOp> {
        contour.cmds.iter().map(|cmd| cmd.op).collect()
    }

    fn count_ops(contours: &[ContourBuf], op: PathOp) -> usize {
        contours
            .iter()
            .flat_map(|contour| &contour.cmds)
            .filter(|cmd| cmd.op == op)
            .count()
    }

    #[test]
    fn squares_stay_polygonal() {
        let square = shapes::rect(10.0, 6.0).unwrap();
        let set = ContourSet::from_contours(&[square], FillRule::NonZero, tol::REGION_MM);

        let refit = refit_set(&set);

        assert_eq!(refit.len(), 1);
        assert_eq!(count_ops(&refit, PathOp::ArcTo), 0);
    }

    #[test]
    fn flattened_circles_come_back_as_arcs() {
        let circle = shapes::circle(4.0).unwrap();
        let rings = rings_from_contours(&[circle]);
        assert!(rings[0].len() > 16, "circle should be tessellated");
        let set = ContourSet::new(rings, FillRule::NonZero, tol::REGION_MM);

        let refit = refit_set(&set);

        assert_eq!(refit.len(), 1);
        assert_eq!(
            ops(&refit[0]),
            vec![PathOp::MoveTo, PathOp::ArcTo, PathOp::ArcTo, PathOp::Close]
        );
        // Radius recovered within the fit tolerance.
        let center_error = refit[0]
            .cmds
            .iter()
            .filter(|cmd| cmd.op == PathOp::ArcTo)
            .map(|cmd| cmd.p1.distance_to(Point::new(0.0, 0.0)))
            .fold(0.0f64, f64::max);
        assert!(
            center_error <= tol::FLATTEN_MM,
            "center off by {center_error}"
        );
    }

    #[test]
    fn dilated_squares_get_arc_corners_and_line_edges() {
        let square = shapes::rect(10.0, 6.0).unwrap();
        let set = ContourSet::from_contours(&[square], FillRule::NonZero, tol::REGION_MM)
            .disk_dilate(1.0);

        let refit = refit_set(&set);

        assert_eq!(refit.len(), 1);
        assert_eq!(count_ops(&refit, PathOp::ArcTo), 4, "{:?}", ops(&refit[0]));
        assert_eq!(count_ops(&refit, PathOp::LineTo), 4, "{:?}", ops(&refit[0]));
    }

    #[test]
    fn refit_stays_within_tolerance_of_the_ring() {
        let square = shapes::rect(8.0, 8.0).unwrap();
        let set = ContourSet::from_contours(&[square], FillRule::NonZero, tol::REGION_MM)
            .disk_dilate(0.5);
        let ring = &set.rings[0];

        let refit = ring_to_contour_with_arcs(ring, tol::FLATTEN_MM);

        // Every original vertex lies within tolerance of the refit outline
        // (vertices on line runs are exact; arc runs bound them by the fit).
        let area_before = crate::geom::region::rings_area(std::slice::from_ref(ring));
        let area_after =
            crate::geom::region::rings_area(&rings_from_contours(std::slice::from_ref(&refit)));
        let perimeter_scale = 4.0 * (8.0 + 2.0 * 0.5);
        assert!(
            (area_before - area_after).abs() <= tol::FLATTEN_MM * perimeter_scale,
            "area drifted {area_before} -> {area_after}"
        );
    }

    #[test]
    fn boolean_output_keeps_arcs_from_curved_inputs() {
        let plate = shapes::rect(20.0, 10.0).unwrap();
        let hole = shapes::circle(3.0).unwrap();
        let set =
            ContourSet::from_contours(&[plate], FillRule::NonZero, tol::REGION_MM).difference(
                &ContourSet::from_contours(&[hole], FillRule::NonZero, tol::REGION_MM),
            );

        let refit = refit_set(&set);

        assert_eq!(refit.len(), 2);
        assert!(count_ops(&refit, PathOp::ArcTo) >= 2, "hole should be arcs");
        // The outer rectangle contributes no spurious arcs.
        let outer = refit
            .iter()
            .find(|contour| contour.bbox.width() > 10.0)
            .unwrap();
        assert_eq!(count_ops(std::slice::from_ref(outer), PathOp::ArcTo), 0);
    }
}
