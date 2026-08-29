//! Legalization pass for orthogonal routes.
//!
//! This module provides post-processing steps to ensure routes are valid:
//! 1. Grid snapping - snap all path points to the routing grid
//! 2. Conflict detection - remove edges that have illegal overlaps with other nets
//!
//! An "illegal" overlap occurs when two edges from different nets:
//! - Share a bend point (corner), which would visually appear as a junction
//! - Have overlapping segments (same line, overlapping range)

use crate::config::RouterConfig;
use crate::types::RoutedPath;
use std::collections::{HashMap, HashSet};

/// Snap a value to the nearest grid point.
#[inline]
fn snap_to_grid(value: f64, grid_size: f64) -> f64 {
    if grid_size <= 0.0 {
        return value;
    }
    (value / grid_size).round() * grid_size
}

/// Snap all points in a path to the grid.
/// In real schematics, ports are already grid-aligned.
fn snap_path_to_grid(path: &mut RoutedPath, grid_size: f64) {
    if grid_size <= 0.0 {
        return;
    }
    for point in &mut path.points {
        point.x = snap_to_grid(point.x, grid_size);
        point.y = snap_to_grid(point.y, grid_size);
    }
}

/// Apply grid snapping to all paths.
pub fn snap_paths_to_grid(paths: &mut [RoutedPath], config: &RouterConfig) {
    let grid_size = config.grid_snap_size;
    if grid_size <= 0.0 {
        return;
    }

    for path in paths.iter_mut() {
        snap_path_to_grid(path, grid_size);
    }
}

/// A segment represented by its two endpoints, normalized so p1 < p2.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Segment {
    /// Start point (smaller coordinate)
    p1: (i64, i64),
    /// End point (larger coordinate)
    p2: (i64, i64),
    /// True if horizontal (same Y), false if vertical (same X)
    is_horizontal: bool,
}

impl Segment {
    fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        // Quantize to avoid floating point issues
        let qx1 = (x1 * 100.0).round() as i64;
        let qy1 = (y1 * 100.0).round() as i64;
        let qx2 = (x2 * 100.0).round() as i64;
        let qy2 = (y2 * 100.0).round() as i64;

        let is_horizontal = qy1 == qy2;

        // Normalize so p1 < p2
        let (p1, p2) = if is_horizontal {
            if qx1 <= qx2 {
                ((qx1, qy1), (qx2, qy2))
            } else {
                ((qx2, qy2), (qx1, qy1))
            }
        } else if qy1 <= qy2 {
            ((qx1, qy1), (qx2, qy2))
        } else {
            ((qx2, qy2), (qx1, qy1))
        };

        Segment {
            p1,
            p2,
            is_horizontal,
        }
    }

    /// Check if this segment overlaps with another segment of the same orientation.
    fn overlaps_with(&self, other: &Segment) -> bool {
        if self.is_horizontal != other.is_horizontal {
            return false;
        }

        if self.is_horizontal {
            // Both horizontal - must be on same Y
            if self.p1.1 != other.p1.1 {
                return false;
            }
            // Check X ranges overlap
            self.p1.0 < other.p2.0 && self.p2.0 > other.p1.0
        } else {
            // Both vertical - must be on same X
            if self.p1.0 != other.p1.0 {
                return false;
            }
            // Check Y ranges overlap
            self.p1.1 < other.p2.1 && self.p2.1 > other.p1.1
        }
    }
}

/// A bend point (corner) in a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BendPoint {
    x: i64,
    y: i64,
}

impl BendPoint {
    fn new(x: f64, y: f64) -> Self {
        BendPoint {
            x: (x * 100.0).round() as i64,
            y: (y * 100.0).round() as i64,
        }
    }
}

/// Extract bend points from a path.
/// A bend point is any point that is neither the start nor end,
/// where the direction changes.
fn extract_bend_points(path: &RoutedPath) -> Vec<BendPoint> {
    let mut bends = Vec::new();

    if path.points.len() < 3 {
        return bends;
    }

    for i in 1..path.points.len() - 1 {
        let prev = &path.points[i - 1];
        let curr = &path.points[i];
        let next = &path.points[i + 1];

        // Check if direction changes at this point
        let dy1 = curr.y - prev.y;
        let dy2 = next.y - curr.y;

        let was_horizontal = dy1.abs() < 1e-6;
        let is_horizontal = dy2.abs() < 1e-6;

        if was_horizontal != is_horizontal {
            bends.push(BendPoint::new(curr.x, curr.y));
        }
    }

    bends
}

/// Extract segments from a path.
fn extract_segments(path: &RoutedPath) -> Vec<Segment> {
    let mut segments = Vec::new();

    for i in 1..path.points.len() {
        let p1 = &path.points[i - 1];
        let p2 = &path.points[i];

        // Skip zero-length segments
        if (p1.x - p2.x).abs() < 1e-6 && (p1.y - p2.y).abs() < 1e-6 {
            continue;
        }

        segments.push(Segment::new(p1.x, p1.y, p2.x, p2.y));
    }

    segments
}

/// Result of legalization pass.
#[derive(Debug, Default)]
pub struct LegalizationResult {
    /// Number of paths removed due to shared bend points.
    pub removed_shared_bends: usize,
    /// Number of paths removed due to overlapping segments.
    pub removed_overlapping_segments: usize,
    /// IDs of removed paths.
    pub removed_path_ids: Vec<String>,
}

/// Legalize paths by removing those with illegal conflicts.
///
/// This function:
/// 1. Detects paths from different nets that share bend points
/// 2. Detects paths from different nets that have overlapping segments
/// 3. Removes the conflicting paths (keeps the first one found per net)
///
/// Returns information about what was removed.
pub fn legalize_paths(paths: &mut Vec<RoutedPath>) -> LegalizationResult {
    let mut result = LegalizationResult::default();

    if paths.len() < 2 {
        return result;
    }

    // Build maps of bend points and segments to net IDs
    // For each bend point, track which nets use it
    let mut bend_to_nets: HashMap<BendPoint, Vec<(usize, String)>> = HashMap::new();
    // For each segment, track which nets use it
    let mut segment_to_nets: HashMap<Segment, Vec<(usize, String)>> = HashMap::new();

    for (idx, path) in paths.iter().enumerate() {
        let net_id = &path.net_id;

        // Register bend points
        for bend in extract_bend_points(path) {
            bend_to_nets
                .entry(bend)
                .or_default()
                .push((idx, net_id.clone()));
        }

        // Register segments
        for segment in extract_segments(path) {
            segment_to_nets
                .entry(segment)
                .or_default()
                .push((idx, net_id.clone()));
        }
    }

    // Find paths to remove
    let mut paths_to_remove: HashSet<usize> = HashSet::new();

    // Check for shared bend points between different nets
    // Sort bend points for deterministic iteration order
    let mut sorted_bends: Vec<_> = bend_to_nets.iter().collect();
    sorted_bends.sort_by_key(|(b, _)| (b.x, b.y));

    for (bend, users) in sorted_bends {
        if users.len() < 2 {
            continue;
        }

        // Group by net
        let mut nets_seen: HashSet<&str> = HashSet::new();
        let mut conflict_indices: Vec<usize> = Vec::new();

        for (idx, net_id) in users {
            if nets_seen.contains(net_id.as_str()) {
                continue; // Same net, OK to share
            }

            if !nets_seen.is_empty() {
                // Different net using same bend point - conflict!
                conflict_indices.push(*idx);
            }
            nets_seen.insert(net_id);
        }

        // Remove all conflicting paths (those from different nets sharing this bend point)
        // conflict_indices contains all paths except the first unique net's first path
        for &idx in &conflict_indices {
            if paths_to_remove.insert(idx) {
                log::warn!(
                    "[legalization] Removing path '{}' (net '{}') - shares bend point ({}, {}) with different net",
                    paths[idx].connector_id,
                    paths[idx].net_id,
                    bend.x as f64 / 100.0,
                    bend.y as f64 / 100.0
                );
                result.removed_shared_bends += 1;
            }
        }
    }

    // Check for overlapping segments between different nets
    // This is O(n²) but typically n is small
    // Sort segments for deterministic iteration order
    let mut segment_list: Vec<_> = segment_to_nets.iter().collect();
    segment_list.sort_by_key(|(s, _)| (s.is_horizontal, s.p1.0, s.p1.1, s.p2.0, s.p2.1));
    for i in 0..segment_list.len() {
        for j in (i + 1)..segment_list.len() {
            let (seg_i, users_i) = segment_list[i];
            let (seg_j, users_j) = segment_list[j];

            if !seg_i.overlaps_with(seg_j) {
                continue;
            }

            // Check if any users are from different nets
            for (idx_i, net_i) in users_i {
                for (idx_j, net_j) in users_j {
                    if net_i == net_j {
                        continue; // Same net, OK
                    }

                    // Different nets with overlapping segments!
                    // Remove the path with the HIGHER index for deterministic behavior
                    let (remove_idx, keep_idx) = if idx_i > idx_j {
                        (*idx_i, *idx_j)
                    } else {
                        (*idx_j, *idx_i)
                    };

                    if paths_to_remove.insert(remove_idx) {
                        log::warn!(
                            "[legalization] Removing path '{}' (net '{}') - overlapping segment with path '{}' (net '{}')",
                            paths[remove_idx].connector_id,
                            paths[remove_idx].net_id,
                            paths[keep_idx].connector_id,
                            paths[keep_idx].net_id
                        );
                        result.removed_overlapping_segments += 1;
                    }
                }
            }
        }
    }

    // Collect IDs of removed paths
    result.removed_path_ids = paths_to_remove
        .iter()
        .map(|&idx| paths[idx].connector_id.clone())
        .collect();

    // Remove paths in reverse order to preserve indices
    let mut indices_to_remove: Vec<_> = paths_to_remove.into_iter().collect();
    indices_to_remove.sort_unstable();
    indices_to_remove.reverse();

    for idx in indices_to_remove {
        paths.remove(idx);
    }

    if result.removed_shared_bends > 0 || result.removed_overlapping_segments > 0 {
        log::info!(
            "[legalization] Removed {} paths ({} shared bends, {} overlapping segments)",
            result.removed_path_ids.len(),
            result.removed_shared_bends,
            result.removed_overlapping_segments
        );
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Point;

    #[test]
    fn test_snap_to_grid() {
        assert_eq!(snap_to_grid(10.3, 1.0), 10.0);
        assert_eq!(snap_to_grid(10.7, 1.0), 11.0);
        assert_eq!(snap_to_grid(12.6, 12.7), 12.7);
        assert_eq!(snap_to_grid(19.0, 12.7), 12.7);
        assert_eq!(snap_to_grid(25.0, 12.7), 25.4);
    }

    #[test]
    fn test_segment_overlap() {
        // Horizontal segments on same line, overlapping
        let s1 = Segment::new(0.0, 10.0, 20.0, 10.0);
        let s2 = Segment::new(10.0, 10.0, 30.0, 10.0);
        assert!(s1.overlaps_with(&s2));

        // Horizontal segments on same line, not overlapping
        let s3 = Segment::new(0.0, 10.0, 10.0, 10.0);
        let s4 = Segment::new(20.0, 10.0, 30.0, 10.0);
        assert!(!s3.overlaps_with(&s4));

        // Horizontal segments on different lines
        let s5 = Segment::new(0.0, 10.0, 20.0, 10.0);
        let s6 = Segment::new(0.0, 20.0, 20.0, 20.0);
        assert!(!s5.overlaps_with(&s6));

        // Vertical segments, overlapping
        let s7 = Segment::new(10.0, 0.0, 10.0, 20.0);
        let s8 = Segment::new(10.0, 10.0, 10.0, 30.0);
        assert!(s7.overlaps_with(&s8));
    }

    #[test]
    fn test_extract_bend_points() {
        let path = RoutedPath::with_net(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 10.0),
                Point::new(20.0, 10.0),
            ],
            "net1",
        );

        let bends = extract_bend_points(&path);
        assert_eq!(bends.len(), 2);
        assert_eq!(bends[0], BendPoint::new(10.0, 0.0));
        assert_eq!(bends[1], BendPoint::new(10.0, 10.0));
    }

    #[test]
    fn test_legalize_shared_bend() {
        let mut paths = vec![
            RoutedPath::with_net(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(10.0, 0.0),
                    Point::new(10.0, 10.0),
                ],
                "net1",
            ),
            RoutedPath::with_net(
                "c2",
                vec![
                    Point::new(5.0, 5.0),
                    Point::new(10.0, 5.0),
                    Point::new(10.0, 0.0), // Same bend point as c1
                    Point::new(15.0, 0.0),
                ],
                "net2",
            ),
        ];

        let result = legalize_paths(&mut paths);
        assert_eq!(
            result.removed_shared_bends, 1,
            "expected 1 shared bend removal"
        );
        assert_eq!(paths.len(), 1, "expected 1 path remaining");
        assert_eq!(paths[0].connector_id, "c1");
    }

    #[test]
    fn test_legalize_same_net_ok() {
        let mut paths = vec![
            RoutedPath::with_net(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(10.0, 0.0),
                    Point::new(10.0, 10.0),
                ],
                "net1",
            ),
            RoutedPath::with_net(
                "c2",
                vec![
                    Point::new(5.0, 5.0),
                    Point::new(10.0, 5.0),
                    Point::new(10.0, 0.0), // Same bend point, but same net
                    Point::new(15.0, 0.0),
                ],
                "net1", // Same net!
            ),
        ];

        let result = legalize_paths(&mut paths);
        assert_eq!(result.removed_shared_bends, 0);
        assert_eq!(paths.len(), 2); // Both kept
    }
}
