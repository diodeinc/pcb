//! Segment tracking for net-aware routing.
//!
//! This module provides data structures to track which route segments belong
//! to which nets. This enables:
//! - Same-net segments to overlap/bundle together
//! - Different-net segments to be kept separate with penalties

use crate::types::Point;
use ordered_float::OrderedFloat;
use std::collections::{HashMap, HashSet};

/// A coordinate quantized to grid precision for reliable hashing and comparison.
///
/// Internally stores coordinates as i64 with 0.01 precision (multiply by 100).
/// This avoids floating point comparison issues where values like 38.1 and
/// 38.099999999999994 would be treated as different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GridCoord {
    /// x coordinate * 100, rounded to nearest integer
    x: i64,
    /// y coordinate * 100, rounded to nearest integer
    y: i64,
}

impl GridCoord {
    /// Precision multiplier: 100 = 0.01 resolution
    const PRECISION: f64 = 100.0;

    /// Create a GridCoord from floating point coordinates.
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x: (x * Self::PRECISION).round() as i64,
            y: (y * Self::PRECISION).round() as i64,
        }
    }

    /// Create a GridCoord from a Point.
    pub fn from_point(p: &Point) -> Self {
        Self::new(p.x, p.y)
    }

    /// Get the x coordinate as f64.
    #[allow(dead_code)]
    pub fn x(&self) -> f64 {
        self.x as f64 / Self::PRECISION
    }

    /// Get the y coordinate as f64.
    #[allow(dead_code)]
    pub fn y(&self) -> f64 {
        self.y as f64 / Self::PRECISION
    }
}

/// An orthogonal segment (horizontal or vertical line).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Segment {
    /// Fixed coordinate (y for horizontal segments, x for vertical).
    pub fixed_coord: OrderedFloat<f64>,
    /// Minimum value on the varying axis.
    pub min_var: OrderedFloat<f64>,
    /// Maximum value on the varying axis.
    pub max_var: OrderedFloat<f64>,
    /// Whether this segment is horizontal (true) or vertical (false).
    pub is_horizontal: bool,
}

impl Segment {
    /// Create a segment from two points.
    ///
    /// Returns None if points are the same or not orthogonal.
    pub fn from_points(p1: &Point, p2: &Point) -> Option<Self> {
        let dx = (p1.x - p2.x).abs();
        let dy = (p1.y - p2.y).abs();

        const EPSILON: f64 = 1e-9;

        if dy < EPSILON && dx > EPSILON {
            // Horizontal segment
            let min_x = p1.x.min(p2.x);
            let max_x = p1.x.max(p2.x);
            Some(Segment {
                fixed_coord: OrderedFloat(p1.y),
                min_var: OrderedFloat(min_x),
                max_var: OrderedFloat(max_x),
                is_horizontal: true,
            })
        } else if dx < EPSILON && dy > EPSILON {
            // Vertical segment
            let min_y = p1.y.min(p2.y);
            let max_y = p1.y.max(p2.y);
            Some(Segment {
                fixed_coord: OrderedFloat(p1.x),
                min_var: OrderedFloat(min_y),
                max_var: OrderedFloat(max_y),
                is_horizontal: false,
            })
        } else {
            // Same point or diagonal (not orthogonal)
            None
        }
    }

    /// Check if this segment overlaps with another segment.
    ///
    /// Two segments overlap if:
    /// 1. They are parallel (both horizontal or both vertical)
    /// 2. They are on the same line (same fixed coordinate)
    /// 3. Their variable ranges have a non-zero overlap (not just touching at a point)
    ///
    /// Segments that merely touch at a single point (T-junction) are NOT considered
    /// overlapping - only actual parallel segment overlap matters.
    ///
    /// Note: Perpendicular crossings (horizontal crossing vertical) are allowed in
    /// schematic routing - they just represent wire crossings. Only parallel overlaps
    /// are illegal.
    pub fn overlaps(&self, other: &Segment) -> bool {
        // Must be same orientation
        if self.is_horizontal != other.is_horizontal {
            return false;
        }

        // Must be on the same line
        if self.fixed_coord != other.fixed_coord {
            return false;
        }

        // Check if ranges have non-zero overlap
        // Two ranges [a, b] and [c, d] have non-zero overlap if max(a, c) < min(b, d)
        // Using strict inequality so touching at a single point doesn't count as overlap
        let overlap_start = self.min_var.max(other.min_var);
        let overlap_end = self.max_var.min(other.max_var);

        overlap_start < overlap_end
    }

    /// Check if this segment contains a point.
    pub fn contains_point(&self, point: &Point) -> bool {
        const EPSILON: f64 = 1e-9;

        if self.is_horizontal {
            (point.y - self.fixed_coord.0).abs() < EPSILON
                && point.x >= self.min_var.0 - EPSILON
                && point.x <= self.max_var.0 + EPSILON
        } else {
            (point.x - self.fixed_coord.0).abs() < EPSILON
                && point.y >= self.min_var.0 - EPSILON
                && point.y <= self.max_var.0 + EPSILON
        }
    }

    /// Get the length of this segment.
    pub fn length(&self) -> f64 {
        self.max_var.0 - self.min_var.0
    }

    /// Get the start point of this segment.
    pub fn start_point(&self) -> Point {
        if self.is_horizontal {
            Point::new(self.min_var.0, self.fixed_coord.0)
        } else {
            Point::new(self.fixed_coord.0, self.min_var.0)
        }
    }

    /// Get the end point of this segment.
    pub fn end_point(&self) -> Point {
        if self.is_horizontal {
            Point::new(self.max_var.0, self.fixed_coord.0)
        } else {
            Point::new(self.fixed_coord.0, self.max_var.0)
        }
    }

    /// Check if this segment is within a given distance of another parallel segment.
    ///
    /// Two segments are "near" if:
    /// 1. They are parallel (both horizontal or both vertical)
    /// 2. Their fixed coordinates are within `distance` of each other
    /// 3. Their variable ranges overlap (so they're actually adjacent, not just parallel elsewhere)
    ///
    /// This does NOT include segments that actually overlap (use `overlaps()` for that).
    pub fn is_near(&self, other: &Segment, distance: f64) -> bool {
        // Must be same orientation
        if self.is_horizontal != other.is_horizontal {
            return false;
        }

        // Check if fixed coordinates are within distance (but not equal - that's overlap territory)
        let fixed_diff = (self.fixed_coord.0 - other.fixed_coord.0).abs();
        if fixed_diff < 1e-9 || fixed_diff > distance {
            return false;
        }

        // Check if variable ranges overlap (segments are actually adjacent)
        let overlap_start = self.min_var.max(other.min_var);
        let overlap_end = self.max_var.min(other.max_var);

        // Need some overlap in the variable dimension
        overlap_start < overlap_end
    }
}

/// Registry tracking which segments belong to which nets.
///
/// This is used during routing to:
/// - Penalize routes that would overlap with different-net segments
/// - Encourage routes that share segments with same-net routes
///
/// Uses 1D spatial indexes (by fixed coordinate) for fast overlap and proximity queries.
/// This reduces segment lookups from O(all_segments) to O(segments_on_same_or_nearby_line).
#[derive(Debug, Clone, Default)]
pub struct SegmentRegistry {
    /// All registered segments, grouped by net ID.
    segments_by_net: HashMap<String, Vec<Segment>>,

    /// Index of horizontal segments by their Y coordinate (quantized).
    /// Maps quantized Y -> list of (net_id, segment_index within that net's Vec).
    horiz_by_y: HashMap<i64, Vec<(String, usize)>>,

    /// Index of vertical segments by their X coordinate (quantized).
    /// Maps quantized X -> list of (net_id, segment_index within that net's Vec).
    vert_by_x: HashMap<i64, Vec<(String, usize)>>,
}

impl SegmentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Quantize a coordinate for index lookup.
    /// Uses same precision as GridCoord for consistency.
    #[inline]
    fn quantize(coord: f64) -> i64 {
        (coord * GridCoord::PRECISION).round() as i64
    }

    /// Register all segments from a routed path.
    pub fn register_path(&mut self, points: &[Point], net_id: &str) {
        if points.len() < 2 {
            return;
        }

        for i in 1..points.len() {
            if let Some(segment) = Segment::from_points(&points[i - 1], &points[i]) {
                self.register_segment(segment, net_id);
            }
        }
    }

    /// Register a single orthogonal segment.
    pub fn register_segment(&mut self, segment: Segment, net_id: &str) {
        let segs = self.segments_by_net.entry(net_id.to_string()).or_default();
        segs.push(segment);
        let idx = segs.len() - 1;

        let fixed_key = Self::quantize(segs[idx].fixed_coord.0);
        if segs[idx].is_horizontal {
            self.horiz_by_y
                .entry(fixed_key)
                .or_default()
                .push((net_id.to_string(), idx));
        } else {
            self.vert_by_x
                .entry(fixed_key)
                .or_default()
                .push((net_id.to_string(), idx));
        }
    }

    /// Check if a segment overlaps with any segment from a different net.
    ///
    /// Uses the 1D spatial index for fast lookup - only checks segments on the same line.
    pub fn overlaps_different_net(&self, segment: &Segment, net_id: &str) -> bool {
        let fixed_key = Self::quantize(segment.fixed_coord.0);

        let candidates = if segment.is_horizontal {
            self.horiz_by_y.get(&fixed_key)
        } else {
            self.vert_by_x.get(&fixed_key)
        };

        if let Some(cands) = candidates {
            for (other_net, idx) in cands {
                if other_net == net_id {
                    continue;
                }
                if let Some(existing) = self
                    .segments_by_net
                    .get(other_net)
                    .and_then(|v| v.get(*idx))
                {
                    if segment.overlaps(existing) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check if a segment is within `distance` of any segment from a different net.
    ///
    /// This is used to penalize routes that are too close to other nets,
    /// even if they don't actually overlap.
    ///
    /// Note: For performance, this falls back to scanning all segments since
    /// the 1D index is optimized for exact-line lookups (overlaps), not range queries.
    /// The number of segments is typically small enough that linear scan is acceptable.
    pub fn near_different_net(&self, segment: &Segment, net_id: &str, distance: f64) -> bool {
        if distance <= 0.0 {
            return false;
        }

        // Linear scan is acceptable here because:
        // 1. near_different_net is only called when overlap check returns false
        // 2. The number of segments is typically small (hundreds, not millions)
        // 3. Range iteration over quantized keys would require iterating thousands of keys
        for (other_net, segments) in &self.segments_by_net {
            if other_net == net_id {
                continue;
            }
            for existing in segments {
                // Quick filter: must be same orientation
                if segment.is_horizontal != existing.is_horizontal {
                    continue;
                }
                if segment.is_near(existing, distance) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if a segment overlaps with any segment from the same net.
    ///
    /// Uses the 1D spatial index for fast lookup.
    pub fn overlaps_same_net(&self, segment: &Segment, net_id: &str) -> bool {
        let fixed_key = Self::quantize(segment.fixed_coord.0);

        let candidates = if segment.is_horizontal {
            self.horiz_by_y.get(&fixed_key)
        } else {
            self.vert_by_x.get(&fixed_key)
        };

        if let Some(cands) = candidates {
            for (cand_net, idx) in cands {
                if cand_net != net_id {
                    continue;
                }
                if let Some(existing) = self.segments_by_net.get(cand_net).and_then(|v| v.get(*idx))
                {
                    if segment.overlaps(existing) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Get all segments for a specific net.
    pub fn get_net_segments(&self, net_id: &str) -> Option<&Vec<Segment>> {
        self.segments_by_net.get(net_id)
    }

    /// Find a segment from the given net that contains the specified point.
    ///
    /// This is used for net-aware routing where a new route can join
    /// an existing segment from the same net.
    pub fn find_segment_at_point(&self, point: &Point, net_id: &str) -> Option<&Segment> {
        if let Some(segments) = self.segments_by_net.get(net_id) {
            for segment in segments {
                if segment.contains_point(point) {
                    return Some(segment);
                }
            }
        }
        None
    }

    /// Check if a point lies on any segment from the given net.
    pub fn point_on_net(&self, point: &Point, net_id: &str) -> bool {
        self.find_segment_at_point(point, net_id).is_some()
    }

    /// Get all registered net IDs.
    pub fn net_ids(&self) -> impl Iterator<Item = &String> {
        self.segments_by_net.keys()
    }

    /// Get total number of segments across all nets.
    pub fn total_segment_count(&self) -> usize {
        self.segments_by_net.values().map(|v| v.len()).sum()
    }

    /// Clear all segments.
    pub fn clear(&mut self) {
        self.segments_by_net.clear();
        self.horiz_by_y.clear();
        self.vert_by_x.clear();
    }
}

/// Registry tracking which bend points (corners) belong to which nets.
///
/// This prevents different-net routes from sharing corner points, which
/// would visually appear as a junction (connection).
#[derive(Debug, Clone, Default)]
pub struct BendPointRegistry {
    /// All registered bend points, grouped by net ID.
    /// Uses GridCoord for reliable coordinate comparison (avoids floating point issues).
    bend_points_by_net: HashMap<String, HashSet<GridCoord>>,

    /// Index for fast "is this point a bend for any net?" lookups.
    /// Maps coord -> set of net IDs that have a bend at this coord.
    coord_to_nets: HashMap<GridCoord, HashSet<String>>,
}

impl BendPointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register all bend points from a routed path.
    ///
    /// Bend points are intermediate vertices (not start/end ports).
    /// For a path with N points, points 1 through N-2 are bend points.
    pub fn register_path(&mut self, points: &[Point], net_id: &str) {
        if points.len() <= 2 {
            return; // No bend points in a straight line
        }

        // Points 1 to len-2 are bend points (index 0 is start, len-1 is end)
        for point in points.iter().take(points.len() - 1).skip(1) {
            let coord = GridCoord::from_point(point);
            self.bend_points_by_net
                .entry(net_id.to_string())
                .or_default()
                .insert(coord);
            // Also add to the reverse index
            self.coord_to_nets
                .entry(coord)
                .or_default()
                .insert(net_id.to_string());
        }
    }

    /// Check if a point is a bend point for a different net.
    ///
    /// Uses the coord_to_nets index for O(1) lookup instead of iterating all nets.
    pub fn is_bend_point_for_different_net(&self, point: &Point, net_id: &str) -> bool {
        let coord = GridCoord::from_point(point);

        // Use the reverse index for fast lookup
        if let Some(nets_at_coord) = self.coord_to_nets.get(&coord) {
            // Check if any net other than ours has a bend here
            for other_net in nets_at_coord {
                if other_net != net_id {
                    return true;
                }
            }
        }
        false
    }

    /// Check if a point is a bend point for the same net.
    pub fn is_bend_point_for_same_net(&self, point: &Point, net_id: &str) -> bool {
        let coord = GridCoord::from_point(point);
        if let Some(points) = self.bend_points_by_net.get(net_id) {
            return points.contains(&coord);
        }
        false
    }

    /// Get the total number of bend points across all nets.
    pub fn total_bend_point_count(&self) -> usize {
        self.bend_points_by_net.values().map(|v| v.len()).sum()
    }

    /// Clear all bend points.
    pub fn clear(&mut self) {
        self.bend_points_by_net.clear();
        self.coord_to_nets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_from_horizontal_points() {
        let p1 = Point::new(0.0, 10.0);
        let p2 = Point::new(100.0, 10.0);

        let seg = Segment::from_points(&p1, &p2).unwrap();
        assert!(seg.is_horizontal);
        assert_eq!(seg.fixed_coord.0, 10.0);
        assert_eq!(seg.min_var.0, 0.0);
        assert_eq!(seg.max_var.0, 100.0);
    }

    #[test]
    fn test_segment_from_vertical_points() {
        let p1 = Point::new(50.0, 0.0);
        let p2 = Point::new(50.0, 200.0);

        let seg = Segment::from_points(&p1, &p2).unwrap();
        assert!(!seg.is_horizontal);
        assert_eq!(seg.fixed_coord.0, 50.0);
        assert_eq!(seg.min_var.0, 0.0);
        assert_eq!(seg.max_var.0, 200.0);
    }

    #[test]
    fn test_segment_from_same_point() {
        let p = Point::new(10.0, 10.0);
        assert!(Segment::from_points(&p, &p).is_none());
    }

    #[test]
    fn test_horizontal_segments_overlap() {
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(40.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };
        assert!(seg1.overlaps(&seg2));
        assert!(seg2.overlaps(&seg1));
    }

    #[test]
    fn test_horizontal_segments_no_overlap() {
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(60.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };
        assert!(!seg1.overlaps(&seg2));
    }

    #[test]
    fn test_parallel_segments_different_lines() {
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(20.0), // Different Y
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        assert!(!seg1.overlaps(&seg2));
    }

    #[test]
    fn test_perpendicular_segments_no_overlap() {
        let horizontal = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let vertical = Segment {
            fixed_coord: OrderedFloat(25.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(20.0),
            is_horizontal: false,
        };
        // Perpendicular segments don't "overlap" in our definition
        // (they cross at a point, but don't share a line segment)
        assert!(!horizontal.overlaps(&vertical));
    }

    #[test]
    fn test_registry_same_net_overlap() {
        let mut registry = SegmentRegistry::new();

        // Path 1 for net "VCC"
        let path1 = vec![Point::new(0.0, 10.0), Point::new(100.0, 10.0)];
        registry.register_path(&path1, "VCC");

        // Segment that overlaps with path1
        let seg = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(50.0),
            max_var: OrderedFloat(150.0),
            is_horizontal: true,
        };

        assert!(registry.overlaps_same_net(&seg, "VCC"));
        assert!(!registry.overlaps_different_net(&seg, "VCC"));
    }

    #[test]
    fn test_registry_different_net_overlap() {
        let mut registry = SegmentRegistry::new();

        // Path for net "VCC"
        let path1 = vec![Point::new(0.0, 10.0), Point::new(100.0, 10.0)];
        registry.register_path(&path1, "VCC");

        // Segment from a different net that overlaps
        let seg = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(50.0),
            max_var: OrderedFloat(150.0),
            is_horizontal: true,
        };

        assert!(registry.overlaps_different_net(&seg, "GND"));
        assert!(!registry.overlaps_same_net(&seg, "GND"));
    }

    #[test]
    fn test_segment_is_near() {
        // Two horizontal segments at Y=10 and Y=20, both spanning X=[0, 100]
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(20.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };

        // Distance of 10, within threshold of 15
        assert!(seg1.is_near(&seg2, 15.0));
        assert!(seg2.is_near(&seg1, 15.0));

        // Distance of 10, outside threshold of 5
        assert!(!seg1.is_near(&seg2, 5.0));
    }

    #[test]
    fn test_segment_is_near_no_range_overlap() {
        // Two horizontal segments at Y=10 and Y=20, but non-overlapping X ranges
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(20.0),
            min_var: OrderedFloat(60.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };

        // Even though Y coordinates are within 15, X ranges don't overlap
        assert!(!seg1.is_near(&seg2, 15.0));
    }

    #[test]
    fn test_segment_is_near_same_line() {
        // Two segments on the same line should NOT be "near" (they'd overlap instead)
        let seg1 = Segment {
            fixed_coord: OrderedFloat(10.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(50.0),
            is_horizontal: true,
        };
        let seg2 = Segment {
            fixed_coord: OrderedFloat(10.0), // Same Y
            min_var: OrderedFloat(60.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };

        // Same line - not "near", would use overlap check instead
        assert!(!seg1.is_near(&seg2, 15.0));
    }

    #[test]
    fn test_registry_near_different_net() {
        let mut registry = SegmentRegistry::new();

        // Path for net "VCC" at Y=10
        let path1 = vec![Point::new(0.0, 10.0), Point::new(100.0, 10.0)];
        registry.register_path(&path1, "VCC");

        // Segment at Y=20 (10 units away)
        let seg = Segment {
            fixed_coord: OrderedFloat(20.0),
            min_var: OrderedFloat(0.0),
            max_var: OrderedFloat(100.0),
            is_horizontal: true,
        };

        // Should be near when checking from different net
        assert!(registry.near_different_net(&seg, "GND", 15.0));
        // Should not be near when checking from same net
        assert!(!registry.near_different_net(&seg, "VCC", 15.0));
        // Should not be near if distance threshold is too small
        assert!(!registry.near_different_net(&seg, "GND", 5.0));
    }
}
