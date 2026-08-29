//! Junction detection for same-net route bundling.
//!
//! When multiple routes of the same net share segments, they form junctions
//! at the points where they merge or split. These junctions are rendered as
//! dots in schematic diagrams to indicate electrical connection.
//!
//! A junction occurs when 3 or more wire segments meet at a point.
//! Two segments meeting is just a bend, not a junction.

use crate::types::{Point, RoutedPath};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// A junction point where same-net routes meet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Junction {
    /// Position of the junction.
    pub position: Point,
    /// Net ID this junction belongs to.
    pub net_id: String,
    /// IDs of connectors that pass through this junction.
    pub connector_ids: Vec<String>,
}

/// Cardinal direction of a segment at a point.
/// We use 4 directions (not 2) to distinguish between:
/// - T-junction: left + right + down = 3 arms = junction
/// - Shared bend: left + down = 2 arms = NOT a junction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum CardinalDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Information about segments meeting at a point.
#[derive(Debug, Default)]
struct PointInfo {
    /// Number of segments meeting at this point
    segment_count: usize,
    /// True if at least one path passes through (not just ends)
    has_pass_through: bool,
    /// IDs of connectors meeting at this point
    connector_ids: Vec<String>,
    /// Cardinal directions of segments at this point (to detect branching)
    /// A junction requires 3+ arms (cardinal directions)
    directions: std::collections::HashSet<CardinalDirection>,
}

/// Detect junctions in routed paths.
///
/// Junctions occur where 3 or more wire segments from the same net meet at a point
/// AND the paths diverge (go different directions). Paths that merely overlap
/// (all going the same direction) are NOT junctions.
///
/// This includes:
/// - T-junctions where one path's endpoint lies on another path's segment
/// - Cross junctions where two paths cross through a common point
///
/// # Arguments
/// * `paths` - The routed paths
/// * `net_ids` - Net ID for each path (same order as paths)
///
/// # Returns
/// A list of junctions where same-net routes share points.
pub fn detect_junctions(paths: &[RoutedPath], net_ids: &[String]) -> Vec<Junction> {
    if paths.is_empty() {
        return Vec::new();
    }

    let mut junctions = Vec::new();

    // Group paths by net ID (use BTreeMap for deterministic iteration order)
    let mut paths_by_net: BTreeMap<&str, Vec<(usize, &RoutedPath)>> = BTreeMap::new();
    for (idx, (path, net_id)) in paths.iter().zip(net_ids.iter()).enumerate() {
        paths_by_net
            .entry(net_id.as_str())
            .or_default()
            .push((idx, path));
    }

    // For each net with multiple paths, find junction points
    for (net_id, net_paths) in paths_by_net {
        if net_paths.len() < 2 {
            continue;
        }

        // Collect information about each point
        // Use BTreeMap for deterministic iteration order
        let mut point_info: BTreeMap<PointKey, PointInfo> = BTreeMap::new();

        // First pass: count connections from path vertices and track directions
        for (_, path) in &net_paths {
            for (i, point) in path.points.iter().enumerate() {
                let key = PointKey::from_point(point);
                let entry = point_info.entry(key).or_default();

                // Count how many segments connect to this point from THIS path
                let is_start = i == 0;
                let is_end = i == path.points.len() - 1;
                let segments_here = match (is_start, is_end) {
                    (true, true) => 0,   // Single point path (degenerate)
                    (true, false) => 1,  // Start: segment to next
                    (false, true) => 1,  // End: segment from previous
                    (false, false) => 2, // Middle: segment from prev + segment to next
                };
                entry.segment_count += segments_here;

                // Track cardinal directions at this point
                // We track the direction of each "arm" - where wires extend FROM this point
                if i > 0 {
                    // Arm extending toward the previous point
                    let prev = &path.points[i - 1];
                    entry.directions.insert(get_cardinal_direction(point, prev));
                }
                if i < path.points.len() - 1 {
                    // Arm extending toward the next point
                    let next = &path.points[i + 1];
                    entry.directions.insert(get_cardinal_direction(point, next));
                }

                // Track if this is a pass-through point (middle of path)
                if !is_start && !is_end {
                    entry.has_pass_through = true;
                }

                // Track which connector this is
                if !entry.connector_ids.contains(&path.connector_id) {
                    entry.connector_ids.push(path.connector_id.clone());
                }
            }
        }

        // Second pass: check if any path's vertex lies on another path's segment
        // This detects T-junctions and other junction types
        for (_, path_a) in &net_paths {
            for vertex in &path_a.points {
                for (_, path_b) in &net_paths {
                    if path_a.connector_id == path_b.connector_id {
                        continue; // Skip self
                    }

                    // Check if this vertex lies on any segment of path_b
                    if let Some((dir_in, dir_out)) = get_segment_directions_at_point(vertex, path_b)
                    {
                        let key = PointKey::from_point(vertex);
                        let entry = point_info.entry(key).or_default();

                        // This vertex creates a junction where path_b passes through
                        // path_b contributes 2 segments (in and out) at this point
                        // AND this is a pass-through point for path_b
                        if !entry.connector_ids.contains(&path_b.connector_id) {
                            entry.segment_count += 2; // The segment passes through this point
                            entry.has_pass_through = true; // Mark as pass-through
                            entry.directions.insert(dir_in); // Track incoming direction
                            entry.directions.insert(dir_out); // Track outgoing direction
                            entry.connector_ids.push(path_b.connector_id.clone());
                        }
                    }
                }
            }
        }

        // Third pass: check for segment-segment crossings (true cross junctions)
        // This detects cases where two segments cross but neither has a vertex at the crossing
        for (i, (_, path_a)) in net_paths.iter().enumerate() {
            for (_, path_b) in net_paths.iter().skip(i + 1) {
                // Check all segment pairs between path_a and path_b
                for seg_a in 0..path_a.points.len().saturating_sub(1) {
                    let a1 = &path_a.points[seg_a];
                    let a2 = &path_a.points[seg_a + 1];

                    for seg_b in 0..path_b.points.len().saturating_sub(1) {
                        let b1 = &path_b.points[seg_b];
                        let b2 = &path_b.points[seg_b + 1];

                        // Check if these orthogonal segments cross
                        if let Some(crossing) = orthogonal_segment_crossing(a1, a2, b1, b2) {
                            let key = PointKey::from_point(&crossing);
                            let entry = point_info.entry(key).or_default();

                            // Both segments pass through this point
                            entry.segment_count += 4; // 2 segments from each path
                            entry.has_pass_through = true;

                            // Add all 4 cardinal directions (cross junction)
                            entry
                                .directions
                                .insert(get_cardinal_direction(&crossing, a1));
                            entry
                                .directions
                                .insert(get_cardinal_direction(&crossing, a2));
                            entry
                                .directions
                                .insert(get_cardinal_direction(&crossing, b1));
                            entry
                                .directions
                                .insert(get_cardinal_direction(&crossing, b2));

                            if !entry.connector_ids.contains(&path_a.connector_id) {
                                entry.connector_ids.push(path_a.connector_id.clone());
                            }
                            if !entry.connector_ids.contains(&path_b.connector_id) {
                                entry.connector_ids.push(path_b.connector_id.clone());
                            }
                        }
                    }
                }
            }
        }

        // A junction exists where wires BRANCH - meaning 3+ "arms" (cardinal directions)
        // meet at a point. This distinguishes:
        //
        // - T-junction: left + right + down = 3 arms = JUNCTION
        // - Cross junction: left + right + up + down = 4 arms = JUNCTION
        // - Shared bend: left + down = 2 arms = NOT a junction (just a corner)
        // - Bundled straight: left + right = 2 arms = NOT a junction (overlapping paths)
        // - Net symbol: multiple paths ending = no pass-through = NOT a junction
        //
        // The key insight: a corner/bend has 2 arms. A junction has 3+.
        // Multiple paths sharing the same corner still only contribute 2 arms total.
        for (key, info) in point_info {
            if info.connector_ids.len() < 2 {
                continue; // Need at least 2 different paths
            }

            // A junction requires:
            // 1. At least 3 segments meeting
            // 2. At least one path passes through (not all paths just terminate here)
            // 3. At least 3 different cardinal directions (arms) - this is the key!
            //    A corner has 2 directions, a T-junction has 3, a cross has 4.
            if info.segment_count >= 3 && info.has_pass_through && info.directions.len() >= 3 {
                junctions.push(Junction {
                    position: key.to_point(),
                    net_id: net_id.to_string(),
                    connector_ids: info.connector_ids,
                });
            }
        }
    }

    junctions
}

/// Get the cardinal direction FROM p1 TO p2.
/// This tells us which direction we're going when moving from p1 to p2.
fn get_cardinal_direction(from: &Point, to: &Point) -> CardinalDirection {
    const EPSILON: f64 = 1e-6;
    let dx = to.x - from.x;
    let dy = to.y - from.y;

    if dy.abs() < EPSILON {
        // Horizontal segment
        if dx > 0.0 {
            CardinalDirection::Right
        } else {
            CardinalDirection::Left
        }
    } else {
        // Vertical segment
        if dy > 0.0 {
            CardinalDirection::Down // Y increases downward in router coords
        } else {
            CardinalDirection::Up
        }
    }
}

/// Get the arm directions if a point lies on any segment of a path (not at a vertex).
/// Returns (arm_to_p1, arm_to_p2) - the directions of the arms extending from the point.
/// Returns None if the point is not on any segment.
fn get_segment_directions_at_point(
    point: &Point,
    path: &RoutedPath,
) -> Option<(CardinalDirection, CardinalDirection)> {
    const EPSILON: f64 = 1e-6;

    for i in 0..path.points.len().saturating_sub(1) {
        let p1 = &path.points[i];
        let p2 = &path.points[i + 1];

        // Skip if point is at a vertex (we handle those separately)
        if (point.x - p1.x).abs() < EPSILON && (point.y - p1.y).abs() < EPSILON {
            continue;
        }
        if (point.x - p2.x).abs() < EPSILON && (point.y - p2.y).abs() < EPSILON {
            continue;
        }

        // Check if point lies on the segment p1-p2
        if point_on_segment(point, p1, p2) {
            // Arms extend from point toward p1 and p2
            let arm_to_p1 = get_cardinal_direction(point, p1);
            let arm_to_p2 = get_cardinal_direction(point, p2);
            return Some((arm_to_p1, arm_to_p2));
        }
    }
    None
}

/// Check if a point lies on a line segment (strictly between endpoints).
fn point_on_segment(point: &Point, p1: &Point, p2: &Point) -> bool {
    const EPSILON: f64 = 1e-6;

    let is_horizontal = (p1.y - p2.y).abs() < EPSILON;
    let is_vertical = (p1.x - p2.x).abs() < EPSILON;

    if is_horizontal {
        // Check if point is on this horizontal segment
        if (point.y - p1.y).abs() < EPSILON {
            let min_x = p1.x.min(p2.x);
            let max_x = p1.x.max(p2.x);
            return point.x > min_x + EPSILON && point.x < max_x - EPSILON;
        }
    } else if is_vertical {
        // Check if point is on this vertical segment
        if (point.x - p1.x).abs() < EPSILON {
            let min_y = p1.y.min(p2.y);
            let max_y = p1.y.max(p2.y);
            return point.y > min_y + EPSILON && point.y < max_y - EPSILON;
        }
    }

    false
}

/// Check if two orthogonal segments cross (one horizontal, one vertical).
/// Returns the crossing point if they intersect strictly inside both segments.
fn orthogonal_segment_crossing(a1: &Point, a2: &Point, b1: &Point, b2: &Point) -> Option<Point> {
    const EPSILON: f64 = 1e-6;

    let a_horizontal = (a1.y - a2.y).abs() < EPSILON;
    let a_vertical = (a1.x - a2.x).abs() < EPSILON;
    let b_horizontal = (b1.y - b2.y).abs() < EPSILON;
    let b_vertical = (b1.x - b2.x).abs() < EPSILON;

    // Need one horizontal and one vertical segment
    let (h1, h2, v1, v2) = if a_horizontal && b_vertical {
        (a1, a2, b1, b2)
    } else if a_vertical && b_horizontal {
        (b1, b2, a1, a2)
    } else {
        return None; // Both same orientation or diagonal
    };

    // Horizontal segment: y = h1.y, x in [min(h1.x, h2.x), max(h1.x, h2.x)]
    // Vertical segment: x = v1.x, y in [min(v1.y, v2.y), max(v1.y, v2.y)]
    let h_y = h1.y;
    let v_x = v1.x;

    let h_min_x = h1.x.min(h2.x);
    let h_max_x = h1.x.max(h2.x);
    let v_min_y = v1.y.min(v2.y);
    let v_max_y = v1.y.max(v2.y);

    // Check if they cross strictly inside both segments (not at endpoints)
    let x_inside = v_x > h_min_x + EPSILON && v_x < h_max_x - EPSILON;
    let y_inside = h_y > v_min_y + EPSILON && h_y < v_max_y - EPSILON;

    if x_inside && y_inside {
        Some(Point::new(v_x, h_y))
    } else {
        None
    }
}

/// Detect junctions from paths with connector-to-net mapping.
///
/// This is a convenience function that looks up net IDs from a mapping.
pub fn detect_junctions_with_mapping(
    paths: &[RoutedPath],
    connector_to_net: &HashMap<String, String>,
) -> Vec<Junction> {
    let net_ids: Vec<String> = paths
        .iter()
        .map(|p| {
            connector_to_net
                .get(&p.connector_id)
                .cloned()
                .unwrap_or_else(|| p.connector_id.clone())
        })
        .collect();

    detect_junctions(paths, &net_ids)
}

/// A hashable point key for deduplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct PointKey {
    // Store as integer microns to avoid float hashing issues
    x_microns: i64,
    y_microns: i64,
}

impl PointKey {
    fn from_point(p: &Point) -> Self {
        // Convert to microns (0.001 units) for reliable hashing
        Self {
            x_microns: (p.x * 1000.0).round() as i64,
            y_microns: (p.y * 1000.0).round() as i64,
        }
    }

    fn to_point(self) -> Point {
        Point::new(
            self.x_microns as f64 / 1000.0,
            self.y_microns as f64 / 1000.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_junctions_single_path() {
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(100.0, 0.0),
                Point::new(100.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert!(junctions.is_empty(), "Single path should have no junctions");
    }

    #[test]
    fn test_no_junctions_different_nets() {
        // Two paths that share a point but are different nets
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(50.0, 0.0)]),
            RoutedPath::new("c2", vec![Point::new(50.0, 0.0), Point::new(100.0, 0.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net2".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert!(
            junctions.is_empty(),
            "Different nets should not create junctions"
        );
    }

    #[test]
    fn test_t_junction() {
        // Two paths of same net forming a T-junction at (50, 0)
        // Path 1: horizontal line through (50, 0)
        // Path 2: ends at (50, 0) coming from below
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(100.0, 0.0)]),
            RoutedPath::new("c2", vec![Point::new(50.0, 50.0), Point::new(50.0, 0.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert_eq!(junctions.len(), 1, "Should detect T-junction");
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 0.0);
        assert_eq!(junctions[0].net_id, "net1");
        assert_eq!(junctions[0].connector_ids.len(), 2);
    }

    #[test]
    fn test_cross_junction() {
        // Two paths of same net crossing at (50, 50)
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(50.0, 0.0),
                    Point::new(50.0, 50.0),
                    Point::new(50.0, 100.0),
                ],
            ),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert_eq!(junctions.len(), 1, "Should detect cross junction");
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 50.0);
    }

    #[test]
    fn test_shared_endpoint_no_junction() {
        // Two paths share an endpoint (2 segments meet, not 3)
        // This is NOT a junction - it's just where one wire ends and another begins
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(50.0, 0.0)]),
            RoutedPath::new("c2", vec![Point::new(50.0, 0.0), Point::new(50.0, 50.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 0): c1 contributes 1 segment, c2 contributes 1 segment = 2 total
        // 2 segments is just a corner/bend, not a junction
        assert!(
            junctions.is_empty(),
            "Two segments meeting is not a junction"
        );
    }

    #[test]
    fn test_multiple_junctions() {
        // A bus-like structure with multiple T-junctions
        // Main horizontal line with two branches
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(100.0, 0.0)]),
            RoutedPath::new("c2", vec![Point::new(25.0, 50.0), Point::new(25.0, 0.0)]),
            RoutedPath::new("c3", vec![Point::new(75.0, 50.0), Point::new(75.0, 0.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert_eq!(junctions.len(), 2, "Should detect two T-junctions");
    }

    #[test]
    fn test_junction_with_bend() {
        // Path with a bend that also has another path joining
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(50.0, 0.0),
                    Point::new(50.0, 50.0),
                ],
            ),
            RoutedPath::new("c2", vec![Point::new(100.0, 0.0), Point::new(50.0, 0.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 0): c1 contributes 2 segments (incoming and outgoing), c2 contributes 1
        // Total = 3 segments from 2 connectors = junction!
        assert_eq!(junctions.len(), 1);
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 0.0);
    }

    #[test]
    fn test_multiple_endpoints_same_point_no_junction() {
        // Three paths all END at the same point (like at a net symbol port)
        // This should NOT be a junction - no path passes through
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(50.0, 50.0)]),
            RoutedPath::new("c2", vec![Point::new(100.0, 0.0), Point::new(50.0, 50.0)]),
            RoutedPath::new("c3", vec![Point::new(50.0, 100.0), Point::new(50.0, 50.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 50): c1 ends (1 segment), c2 ends (1 segment), c3 ends (1 segment)
        // Total = 3 segments, but NO pass-through, so NOT a junction
        assert!(
            junctions.is_empty(),
            "Multiple paths ending at same point (no pass-through) is NOT a junction"
        );
    }

    #[test]
    fn test_four_endpoints_same_point_no_junction() {
        // Four paths all END at the same point (like at a net symbol port)
        // Even with 4 segments, if none pass through, it's NOT a junction
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 50.0), Point::new(50.0, 50.0)]),
            RoutedPath::new("c2", vec![Point::new(100.0, 50.0), Point::new(50.0, 50.0)]),
            RoutedPath::new("c3", vec![Point::new(50.0, 0.0), Point::new(50.0, 50.0)]),
            RoutedPath::new("c4", vec![Point::new(50.0, 100.0), Point::new(50.0, 50.0)]),
        ];
        let net_ids = vec![
            "net1".to_string(),
            "net1".to_string(),
            "net1".to_string(),
            "net1".to_string(),
        ];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 50): all 4 paths end there, total 4 segments, but none pass through
        // This is NOT a junction - it's a shared endpoint (like a net symbol)
        assert!(
            junctions.is_empty(),
            "Four paths ending at same point (no pass-through) is NOT a junction"
        );
    }

    #[test]
    fn test_mixed_endpoint_and_passthrough() {
        // One path passes through, two paths end at the same point
        // This IS a junction (T-junction with extra branch)
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ), // passes through (50, 50)
            RoutedPath::new("c2", vec![Point::new(50.0, 0.0), Point::new(50.0, 50.0)]), // ends at (50, 50)
            RoutedPath::new("c3", vec![Point::new(50.0, 100.0), Point::new(50.0, 50.0)]), // ends at (50, 50)
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 50): c1 passes through (2 segments), c2 ends (1 segment), c3 ends (1 segment)
        // Total = 4 segments with pass-through = junction!
        assert_eq!(
            junctions.len(),
            1,
            "Should detect junction when one path passes through"
        );
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 50.0);
    }

    #[test]
    fn test_bundled_paths_same_direction_no_junction() {
        // Three paths all pass through the same point going the same direction (horizontal)
        // This is bundled/overlapping paths, NOT a junction
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ),
            RoutedPath::new(
                "c3",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 50): all 3 paths pass through horizontally
        // 6 segments total but only 1 direction = NOT a junction (bundled paths)
        assert!(
            junctions.is_empty(),
            "Bundled paths going same direction should NOT create a junction"
        );
    }

    #[test]
    fn test_bundled_horizontal_with_vertical_branch_is_junction() {
        // Two paths pass through horizontally, one path joins vertically
        // This IS a junction (the vertical path diverges)
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ), // horizontal through (50, 50)
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(0.0, 50.0),
                    Point::new(50.0, 50.0),
                    Point::new(100.0, 50.0),
                ],
            ), // horizontal through (50, 50)
            RoutedPath::new("c3", vec![Point::new(50.0, 0.0), Point::new(50.0, 50.0)]), // vertical ending at (50, 50)
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // At (50, 50): c1 and c2 pass through horizontally, c3 comes in vertically
        // 2 directions (horizontal + vertical) = junction
        assert_eq!(
            junctions.len(),
            1,
            "Bundled paths with diverging branch IS a junction"
        );
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 50.0);
    }

    #[test]
    fn test_segment_segment_crossing_no_vertex() {
        // Two paths cross but neither has a vertex at the crossing point
        // Path 1: vertical from (50, 0) to (50, 100)
        // Path 2: horizontal from (0, 50) to (100, 50)
        // They cross at (50, 50) but neither path has a vertex there
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(50.0, 0.0), Point::new(50.0, 100.0)]),
            RoutedPath::new("c2", vec![Point::new(0.0, 50.0), Point::new(100.0, 50.0)]),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        assert_eq!(
            junctions.len(),
            1,
            "Segment-segment crossing without vertex should be detected"
        );
        assert_eq!(junctions[0].position.x, 50.0);
        assert_eq!(junctions[0].position.y, 50.0);
    }

    #[test]
    fn test_segment_crossing_with_third_path() {
        // Scenario from USB_3PI.VBUS routing:
        // Path 1 (C_BULK): vertical at x=63.5 from y=-21.5 to y=-17.8, then horizontal to x=74.9
        // Path 2 (Current_Limit_Switch): horizontal at y=-20.3 from x=59.7 to x=74.9, then vertical up
        // These cross at (63.5, -20.3) without either having a vertex there
        let paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(63.5, -21.5),
                    Point::new(63.5, -17.8),
                    Point::new(74.9, -17.8),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(59.7, -20.3),
                    Point::new(74.9, -20.3),
                    Point::new(74.9, -14.0),
                ],
            ),
        ];
        let net_ids = vec!["net1".to_string(), "net1".to_string()];

        let junctions = detect_junctions(&paths, &net_ids);
        // Should detect crossing at (63.5, -20.3)
        let cross_junction = junctions
            .iter()
            .find(|j| (j.position.x - 63.5).abs() < 0.01 && (j.position.y - (-20.3)).abs() < 0.01);
        assert!(
            cross_junction.is_some(),
            "Should detect cross junction at (63.5, -20.3)"
        );
    }
}
