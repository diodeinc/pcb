//! Widths on unsnapped segment bisectors. All classification boundaries are
//! quadratic roots; polylines are made only after measurement, for display.

use super::{ContourSet, PreparedRegion, ring_edges, ring_winding};
use crate::geom::{BBox, Point, accuracy::numerical_error, dist};

type Polynomial = [f64; 3];

fn dot(a: Point, b: Point) -> f64 {
    a.x * b.x + a.y * b.y
}

fn perpendicular(p: Point) -> Point {
    Point::new(-p.y, p.x)
}

fn value(p: Polynomial, t: f64) -> f64 {
    p[2].mul_add(t, p[1]).mul_add(t, p[0])
}

fn roots([c, b, a]: Polynomial) -> Vec<f64> {
    if a == 0.0 {
        return if b == 0.0 { vec![] } else { vec![-c / b] };
    }
    let discriminant = b.mul_add(b, -4.0 * a * c);
    if discriminant < 0.0 {
        return vec![];
    }
    let q = -0.5 * (b + discriminant.sqrt().copysign(b));
    if q == 0.0 {
        vec![-b / (2.0 * a)]
    } else {
        vec![q / a, c / q]
    }
}

/// The same polynomials supply both cell cuts and open-cell membership.
struct SegmentClearance {
    projection: Polynomial,
    length: f64,
    endpoints: [Polynomial; 2],
    line: Vec<Polynomial>,
}

impl SegmentClearance {
    fn contains(&self, t: f64) -> bool {
        let projection = value(self.projection, t);
        if projection <= 0.0 {
            value(self.endpoints[0], t) >= 0.0
        } else if projection >= self.length {
            value(self.endpoints[1], t) >= 0.0
        } else {
            self.line.iter().map(|&p| value(p, t)).product::<f64>() >= 0.0
        }
    }
}

/// A line or parabola, with its active point/interior-segment contacts.
/// Degenerate contact segments denote endpoints, not short supporting lines.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WidthAxis {
    center: [Point; 3],
    radius: Polynomial,
    squared_radius: bool,
    contacts: [(Point, Point); 2],
    range: (f64, f64),
}

impl WidthAxis {
    pub fn between(first: (Point, Point), second: (Point, Point), bounds: BBox) -> Vec<Self> {
        let features = |(a, b)| {
            if a == b {
                vec![(a, a)]
            } else {
                vec![(a, a), (b, b), (a, b)]
            }
        };
        let mut axes = Vec::new();
        for first in features(first) {
            for second in features(second) {
                let (mut first, mut second) = (first, second);
                // Put the point first in point/line pairs.
                if first.0 != first.1 && second.0 == second.1 {
                    std::mem::swap(&mut first, &mut second);
                }
                let contacts = [first, second];
                if first.0 == first.1 && second.0 == second.1 {
                    let across = second.0 - first.0;
                    let diameter = across.length();
                    if diameter > 0.0 {
                        let half = diameter / 2.0;
                        axes.push(Self {
                            center: [
                                first.0.midpoint(second.0),
                                perpendicular(across) / diameter,
                                Point::ZERO,
                            ],
                            radius: [half * half, 0.0, 1.0],
                            squared_radius: true,
                            contacts,
                            range: (-half, half),
                        });
                    }
                } else if first.0 == first.1 {
                    let along = (second.1 - second.0) / second.0.distance_to(second.1);
                    let foot = second.0 + along * dot(first.0 - second.0, along);
                    let height = first.0.distance_to(foot);
                    if height > numerical_error(bounds) {
                        let normal = (first.0 - foot) / height;
                        axes.push(Self {
                            center: [foot.midpoint(first.0), along, normal / (2.0 * height)],
                            radius: [height / 2.0, 0.0, 1.0 / (2.0 * height)],
                            squared_radius: false,
                            contacts,
                            range: (-height, height),
                        });
                    }
                } else {
                    let edges = contacts.map(|(a, b)| b - a);
                    let normals = contacts.map(|(a, b)| perpendicular(b - a) / a.distance_to(b));
                    // Each edge subtracts two coordinates. Propagate their
                    // numerical error through the unnormalized dot product;
                    // normalizing first would hide cancellation in short edges.
                    let edge_error = 2.0
                        * numerical_error(
                            BBox::spanning(first.0, first.1)
                                .union(BBox::spanning(second.0, second.1)),
                        );
                    let dot_error = edge_error * (edges[0].length() + edges[1].length())
                        + edge_error * edge_error;
                    for sign in [-1.0, 1.0] {
                        // Signed-distance equality fixes the contact angle on
                        // this branch. Right-angle line pairs never qualify.
                        if sign * dot(edges[0], edges[1]) >= -dot_error {
                            continue;
                        }
                        let normal = normals[0] - normals[1] * sign;
                        let norm2 = dot(normal, normal);
                        if norm2 == 0.0 {
                            continue;
                        }
                        let middle = bounds.center();
                        let offset = sign * dot(normals[1], first.0 - second.0);
                        let origin =
                            middle + normal * ((offset - dot(normal, middle - first.0)) / norm2);
                        let direction = perpendicular(normal) / norm2.sqrt();
                        let extent = bounds.min.distance_to(bounds.max) / 2.0;
                        axes.push(Self {
                            center: [origin, direction, Point::ZERO],
                            radius: [
                                dot(normals[0], origin - first.0),
                                dot(normals[0], direction),
                                0.0,
                            ],
                            squared_radius: false,
                            contacts,
                            range: (-extent, extent),
                        });
                    }
                }
            }
        }
        axes
    }

    fn at(self, t: f64) -> Point {
        self.center[0] + (self.center[1] + self.center[2] * t) * t
    }

    fn radius_at(self, t: f64) -> f64 {
        let r = value(self.radius, t);
        if self.squared_radius {
            r.max(0.0).sqrt()
        } else {
            r.abs()
        }
    }

    fn projection(self, origin: Point, direction: Point) -> Polynomial {
        [
            dot(self.center[0] - origin, direction),
            dot(self.center[1], direction),
            dot(self.center[2], direction),
        ]
    }

    fn bounds(self) -> BBox {
        let (start, end) = self.endpoints();
        let mut bounds = BBox::spanning(start, end);
        for (linear, quadratic) in [
            (self.center[1].x, self.center[2].x),
            (self.center[1].y, self.center[2].y),
        ] {
            if quadratic != 0.0 {
                bounds.include_point(
                    self.at((-linear / (2.0 * quadratic)).clamp(self.range.0, self.range.1)),
                );
            }
        }
        bounds
    }

    /// Partition by every predicate root, keeping valid singleton intersections
    /// as well as intervals. Identically zero polynomials need no subdivision.
    fn clip(self, mut cuts: Vec<f64>, valid: impl Fn(f64, bool) -> bool) -> Vec<Self> {
        cuts.retain(|t| t.is_finite() && *t > self.range.0 && *t < self.range.1);
        cuts.extend([self.range.0, self.range.1]);
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let mut kept: Vec<Self> = Vec::new();
        let mut add = |start, end| {
            if let Some(last) = kept.last_mut()
                && last.range.1 == start
            {
                last.range.1 = end;
            } else {
                kept.push(Self {
                    range: (start, end),
                    ..self
                });
            }
        };
        for (index, &start) in cuts.iter().enumerate() {
            if valid(start, true) {
                add(start, start);
            }
            if let Some(&end) = cuts.get(index + 1)
                && valid(start.midpoint(end), false)
            {
                add(start, end);
            }
        }
        kept
    }

    pub fn clipped_radius(self, minimum: f64, maximum: f64) -> Vec<Self> {
        let mut cuts = Vec::new();
        for limit in [minimum, maximum] {
            for sign in [-1.0, 1.0] {
                let mut polynomial = self.radius;
                polynomial[0] -= if self.squared_radius {
                    limit * limit
                } else {
                    sign * limit
                };
                cuts.extend(roots(polynomial));
            }
        }
        self.clip(cuts, |t, _| {
            (minimum..=maximum).contains(&self.radius_at(t))
        })
    }

    /// Squared distance to a point minus the squared common radius. Using
    /// a point contact cancels the fourth-degree terms of a parabola exactly.
    fn point_clearance(self, point: Point) -> Polynomial {
        if self.contacts[0].0 == self.contacts[0].1 {
            let contact = self.contacts[0].0;
            let delta = contact - point;
            [
                dot(delta, (self.center[0] - contact) * 2.0 + delta),
                2.0 * dot(delta, self.center[1]),
                2.0 * dot(delta, self.center[2]),
            ]
        } else {
            let delta = self.center[0] - point;
            let [r0, r1, _] = self.radius;
            [
                dot(delta, delta) - r0 * r0,
                2.0 * (dot(delta, self.center[1]) - r0 * r1),
                dot(self.center[1], self.center[1]) - r1 * r1,
            ]
        }
    }

    pub fn in_region(self, region: &ContourSet, boundary: &PreparedRegion) -> Vec<Self> {
        let bounds = self.bounds();
        if !bounds.intersects(region.bbox) {
            return Vec::new();
        }
        let error = numerical_error(bounds.union(region.bbox));
        let maximum_radius = self
            .radius_at(self.range.0)
            .max(self.radius_at(self.range.1));
        let nearby = boundary
            .segments_meeting(bounds.expand(maximum_radius + error))
            .collect::<Vec<_>>();
        let mut constraints = Vec::new();
        for (a, b) in self.contacts {
            if a != b {
                let length = a.distance_to(b);
                let projection = self.projection(a, (b - a) / length);
                constraints.push(projection);
                constraints.push([length - projection[0], -projection[1], -projection[2]]);
            } else {
                // Endpoint normal cone, including adjacent source edges. A
                // shadowed endpoint is not a nearest contact even when the
                // distance difference rounds to zero at a corner transition.
                for &(start, end) in &nearby {
                    let direction = if a == start {
                        end - start
                    } else if a == end {
                        start - end
                    } else {
                        continue;
                    };
                    if direction.length() > 0.0 {
                        constraints.push(self.projection(a, -direction / direction.length()));
                    }
                }
            }
        }
        let mut cuts = constraints
            .iter()
            .flat_map(|&polynomial| roots(polynomial))
            .collect::<Vec<_>>();
        cuts.extend(roots(self.radius));
        for (a, b) in region.rings.iter().flat_map(ring_edges) {
            cuts.extend(roots(self.projection(a, perpendicular(b - a))));
        }
        let mut clearance = Vec::new();
        for &(a, b) in &nearby {
            // These equalities hold by construction, including endpoint
            // contacts after their incident normal-cone constraints above.
            if self.contacts.iter().any(|&(p, q)| {
                (p == a && q == b) || (p == b && q == a) || (p == q && (p == a || p == b))
            }) {
                continue;
            }
            let endpoints = [self.point_clearance(a), self.point_clearance(b)];
            cuts.extend(endpoints.into_iter().flat_map(roots));
            let length = a.distance_to(b);
            let direction = if length == 0.0 {
                Point::ZERO
            } else {
                (b - a) / length
            };
            let projection = self.projection(a, direction);
            cuts.extend(roots(projection));
            cuts.extend(roots([
                projection[0] - length,
                projection[1],
                projection[2],
            ]));
            let distance = self.projection(a, perpendicular(direction));
            let line = if self.squared_radius {
                vec![[
                    distance[0] * distance[0] - self.radius[0],
                    2.0 * distance[0] * distance[1] - self.radius[1],
                    distance[1] * distance[1] - self.radius[2],
                ]]
            } else {
                // d²-r² = (d-r)(d+r); both factors are quadratic.
                [-1.0, 1.0]
                    .map(|sign| std::array::from_fn(|i| distance[i] + sign * self.radius[i]))
                    .to_vec()
            };
            cuts.extend(line.iter().copied().flat_map(roots));
            clearance.push(SegmentClearance {
                projection,
                length,
                endpoints,
                line,
            });
        }
        self.clip(cuts, |t, isolated| {
            let center = self.at(t);
            let radius = self.radius_at(t);
            let inside = region
                .rings
                .iter()
                .map(|ring| ring_winding(ring, center))
                .sum::<i32>()
                != 0
                || region.rings.iter().flat_map(ring_edges).any(|(a, b)| {
                    dot(center - a, perpendicular(b - a)) == 0.0
                        && dot(center - a, center - b) <= 0.0
                });
            if isolated {
                let vectors = self
                    .contacts
                    .map(|(a, b)| dist::point_segment(center, a, b).1 - center);
                radius > error
                    && inside
                    && constraints.iter().all(|&p| value(p, t) >= -error)
                    && dot(vectors[0], vectors[1])
                        < -error * (vectors[0].length() + vectors[1].length())
                    && nearby
                        .iter()
                        .all(|&(a, b)| dist::point_segment(center, a, b).0 >= radius - error)
            } else {
                // Tolerant point checks must never certify an open interval:
                // its membership is constant only for these exact root predicates.
                radius > 0.0
                    && inside
                    && constraints.iter().all(|&p| value(p, t) >= 0.0)
                    && clearance.iter().all(|wall| wall.contains(t))
            }
        })
    }

    pub fn endpoints(self) -> (Point, Point) {
        (self.at(self.range.0), self.at(self.range.1))
    }

    pub fn minimum(self) -> (Point, f64, Point, Point) {
        let mut candidates = vec![self.range.0, self.range.1];
        if self.radius[2] != 0.0 {
            candidates
                .push((-self.radius[1] / (2.0 * self.radius[2])).clamp(self.range.0, self.range.1));
        }
        if !self.squared_radius {
            candidates.extend(
                roots(self.radius)
                    .into_iter()
                    .filter(|t| (self.range.0..=self.range.1).contains(t)),
            );
        }
        let t = candidates
            .into_iter()
            .min_by(|&a, &b| self.radius_at(a).total_cmp(&self.radius_at(b)))
            .unwrap();
        let center = self.at(t);
        let contacts = self
            .contacts
            .map(|(a, b)| dist::point_segment(center, a, b).1);
        (center, self.radius_at(t), contacts[0], contacts[1])
    }

    pub fn walls(self) -> [(Point, Point); 2] {
        let (start, end) = self.endpoints();
        self.contacts.map(|(a, b)| {
            (
                dist::point_segment(start, a, b).1,
                dist::point_segment(end, a, b).1,
            )
        })
    }

    pub fn polyline(self, tolerance_mm: f64) -> Vec<Point> {
        let mut points = vec![self.at(self.range.0)];
        let mut pending = vec![self.range];
        while let Some((start, end)) = pending.pop() {
            let middle = start.midpoint(end);
            // Exact maximum deviation of a quadratic from its chord.
            let deviation = self.center[2].length() * (end - start).powi(2) / 4.0;
            if deviation > tolerance_mm && middle != start && middle != end {
                pending.push((middle, end));
                pending.push((start, middle));
            } else {
                points.push(self.at(end));
            }
        }
        points
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Resolution;

    fn region() -> ContourSet {
        ContourSet::rectangle(
            BBox::new(Point::new(-2.0, -2.0), Point::new(2.0, 2.0)),
            Resolution::default(),
        )
    }

    #[test]
    fn off_grid_parallel_walls_have_equal_radii() {
        let first = (Point::new(-3.0, 0.00037), Point::new(3.0, 0.00037));
        let second = (Point::new(-3.0, 0.00837), Point::new(3.0, 0.00837));
        let region = region();
        let boundary = PreparedRegion::from_segments(vec![first, second], 0.0);
        let axes = WidthAxis::between(first, second, region.bbox)
            .into_iter()
            .flat_map(|a| a.in_region(&region, &boundary))
            .collect::<Vec<_>>();
        assert!(!axes.is_empty());
        for axis in axes {
            let (center, radius, first, second) = axis.minimum();
            assert!((radius - 0.004).abs() < 1e-12);
            assert!((center.y - 0.00437).abs() < 1e-12);
            assert!((center.distance_to(first) - radius).abs() < 1e-12);
            assert!((center.distance_to(second) - radius).abs() < 1e-12);
        }
    }

    #[test]
    fn oblique_bisectors_remain_equidistant_throughout_their_spans() {
        let transform = |p: Point| {
            Point::new(
                123.4567 + 0.6 * p.x - 0.8 * p.y,
                -98.7654 + 0.8 * p.x + 0.6 * p.y,
            )
        };
        let point = Point::new(0.0, 0.7);
        for (first, second) in [
            (
                (Point::new(-2.0, -0.3), Point::new(2.0, -0.1)),
                (Point::new(-2.0, 0.4), Point::new(2.0, 0.2)),
            ),
            (
                (point, point),
                (Point::new(-2.0, -0.1), Point::new(2.0, 0.2)),
            ),
            (
                (point, point),
                (Point::new(0.4, -0.3), Point::new(0.4, -0.3)),
            ),
        ] {
            let walls = [first, second].map(|(a, b)| (transform(a), transform(b)));
            let region = ContourSet::rectangle(
                BBox::from_point(transform(Point::ZERO)).expand(1.5),
                Resolution::default(),
            );
            let boundary = PreparedRegion::from_segments(walls.to_vec(), 0.0);
            let axes = WidthAxis::between(walls[0], walls[1], region.bbox)
                .into_iter()
                .flat_map(|a| a.in_region(&region, &boundary))
                .collect::<Vec<_>>();
            assert!(!axes.is_empty());
            for axis in axes {
                for i in 0..=7 {
                    let t = axis.range.0 + (axis.range.1 - axis.range.0) * i as f64 / 7.0;
                    let center = axis.at(t);
                    let radius = axis.radius_at(t);
                    for (a, b) in walls {
                        assert!((dist::point_segment(center, a, b).0 - radius).abs() < 1e-10);
                    }
                }
            }
        }
    }

    #[test]
    fn a_shadowed_step_endpoint_cannot_establish_width() {
        // On the point/line parabola y=(h²-x²)/(2h), obtuse contacts require
        // y>0, but the adjoining vertical wall makes the point valid only
        // for y<=0. No vertex or span satisfies both conditions.
        let q = Point::ZERO;
        let wall = (Point::new(-1.0, 0.05), Point::new(1.0, 0.05));
        let boundary = PreparedRegion::from_segments(
            vec![wall, (q, Point::new(0.0, 0.05)), (q, Point::new(1.0, 0.0))],
            0.0,
        );
        let region = ContourSet::rectangle(
            BBox::new(Point::new(-0.2, -0.1), Point::new(0.2, 0.1)),
            Resolution::default(),
        );
        let axes = WidthAxis::between((q, q), wall, region.bbox);
        assert!(
            axes.into_iter()
                .flat_map(|a| a.in_region(&region, &boundary))
                .next()
                .is_none()
        );
    }

    #[test]
    fn third_wall_clips_the_interior_not_just_endpoints() {
        let first = (Point::new(-3.0, -1.0), Point::new(3.0, -1.0));
        let second = (Point::new(-3.0, 1.0), Point::new(3.0, 1.0));
        let boundary = PreparedRegion::from_segments(
            vec![first, second, (Point::new(0.0, 0.5), Point::new(0.0, 0.6))],
            0.0,
        );
        let region = region();
        let axes = WidthAxis::between(first, second, region.bbox)
            .into_iter()
            .flat_map(|a| a.in_region(&region, &boundary))
            .collect::<Vec<_>>();
        assert_eq!(axes.len(), 2);
        for axis in axes {
            let (start, end) = axis.endpoints();
            let nearer = start.x.abs().min(end.x.abs());
            assert!((nearer - 0.75_f64.sqrt()).abs() < 1e-12);
            assert!(start.x * end.x > 0.0);
        }
    }

    #[test]
    fn a_nearly_coincident_wall_cannot_validate_a_root_free_span() {
        let point = (Point::new(0.0, 1.0), Point::new(0.0, 1.0));
        let wall = (Point::new(-3.0, 0.0), Point::new(3.0, 0.0));
        let region = region();
        for displacement in [-1e-14, 0.0, 1e-8] {
            let third = (
                Point::new(-3.0, 1.0 + displacement),
                Point::new(3.0, 1.0 + displacement),
            );
            let boundary = PreparedRegion::from_segments(vec![point, wall, third], 0.0);
            let axes = WidthAxis::between(point, wall, region.bbox)
                .into_iter()
                .flat_map(|a| a.in_region(&region, &boundary))
                .collect::<Vec<_>>();
            if displacement < 0.0 {
                assert!(axes.is_empty());
            } else {
                assert_eq!(axes.len(), 1);
                let (start, end) = axes[0].endpoints();
                assert!((start.x + displacement.sqrt()).abs() < 1e-10);
                assert!((end.x - displacement.sqrt()).abs() < 1e-10);
            }
            for axis in axes {
                // Checking only the minimum misses the original defect:
                // at t=.5 its reported radius .625 exceeded clearance .375.
                for fraction in [0.0, 0.25, 0.75, 1.0] {
                    let t = axis.range.0 + fraction * (axis.range.1 - axis.range.0);
                    assert!(
                        dist::point_segment(axis.at(t), third.0, third.1).0
                            >= axis.radius_at(t) - 1e-12
                    );
                }
            }
        }
    }

    #[test]
    fn parabola_measurement_is_independent_of_display_flattening() {
        let point = (Point::new(0.0, 1.0), Point::new(0.0, 1.0));
        let wall = (Point::new(-3.0, 0.0), Point::new(3.0, 0.0));
        let boundary = PreparedRegion::from_segments(vec![point, wall], 0.0);
        let region = region();
        let axes = WidthAxis::between(point, wall, region.bbox)
            .into_iter()
            .flat_map(|a| a.in_region(&region, &boundary))
            .collect::<Vec<_>>();
        assert_eq!(axes.len(), 1);
        let axis = axes[0];
        assert!((axis.minimum().1 - 0.5).abs() < 1e-12);
        assert!(axis.polyline(0.001).len() > axis.polyline(0.1).len());
        let clipped = axis.clipped_radius(0.0, 0.625);
        assert_eq!(clipped.len(), 1);
        let (start, end) = clipped[0].endpoints();
        assert!((start.x + 0.5).abs() < 1e-12 && (end.x - 0.5).abs() < 1e-12);
        assert!((start.y - 0.625).abs() < 1e-12);
        // A genuinely nearest, barely obtuse pair is not discarded by a
        // significance-sized angular margin: t=.999 lies strictly inside.
        let center = axis.at(0.999);
        let region = ContourSet::rectangle(
            BBox::from_point(center).expand(1e-5),
            Resolution::default().with_tolerance(1e-7),
        );
        assert!(!axis.in_region(&region, &boundary).is_empty());
    }
}
