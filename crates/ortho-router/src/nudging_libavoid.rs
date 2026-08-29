//! Libavoid-compatible orthogonal route nudging.
//!
//! This module is a 1:1 port of libavoid's orthogonal nudging algorithm from
//! `orthogonal.cpp`. It adjusts route positions to:
//! - Separate overlapping segments from different connectors
//! - Center segments within available channel space
//! - Maintain alignment for same-connector segments
//!
//! ## Algorithm Overview
//!
//! The algorithm works in two passes:
//! 1. **Unifying pass**: Establishes segment ordering with lighter constraints
//! 2. **Nudging pass**: Applies full separation constraints based on ordering
//!
//! For each pass:
//! 1. Build segment list from all connector routes
//! 2. Group overlapping segments into "regions"
//! 3. Sort segments within each region
//! 4. Create VPSC variables and constraints
//! 5. Solve and apply positions
//!
//! ## Key Concepts
//!
//! - **Segment**: A horizontal or vertical portion of a route
//! - **Channel**: The valid movement range for a segment (minLimit, maxLimit)
//! - **Final segment**: First or last segment of a route (connected to port)
//! - **Zigzag (S-bend/Z-bend)**: Middle segment that can move freely
//!
//! ## References
//!
//! Based on libavoid's orthogonal.cpp from the Adaptagrams project.

use crate::config::RouterConfig;
use crate::types::{Obstacle, Point, RoutedPath};
use crate::vpsc::{Constraint, IncSolver, Variable};
use std::cmp::Ordering;

// =============================================================================
// Constants (from libavoid orthogonal.cpp)
// =============================================================================

/// Maximum channel bound (effectively infinite)
const CHANNEL_MAX: f64 = 1e9;

/// Variable ID for free (movable) segments
const FREE_SEGMENT_ID: usize = 0;

/// Variable ID for fixed segments
const FIXED_SEGMENT_ID: usize = 1;

/// Variable ID for left channel boundary
const CHANNEL_LEFT_ID: usize = 2;

/// Variable ID for right channel boundary
const CHANNEL_RIGHT_ID: usize = 3;

/// Weight for free segments - move easily
const FREE_WEIGHT: f64 = 0.00001;

/// Weight for zigzag segments - move easily, prefer centering
const ZIGZAG_WEIGHT: f64 = 0.00001;

/// Weight for final segments - resist movement somewhat
const STRONG_WEIGHT: f64 = 0.001;

/// Weight for single-connector segments bridging two shapes
const STRONGER_WEIGHT: f64 = 1.0;

/// Weight for fixed segments - don't move
const FIXED_WEIGHT: f64 = 100000.0;

/// Buffer distance for free connectors not in shapes
const FREE_CONN_BUFFER: f64 = 15.0;

// =============================================================================
// Debug Types for Visualization
// =============================================================================

/// Type classification for segment visualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Cannot move (port endpoints)
    Fixed,
    /// First/last segment, resists movement
    Final,
    /// S/Z-bend, prefers centering
    Zigzag,
    /// Other movable segments
    Free,
}

/// Debug information for a single segment.
#[derive(Debug, Clone)]
pub struct SegmentDebugInfo {
    /// Index of the path this segment belongs to
    pub path_idx: usize,
    /// Connector ID
    pub connector_id: String,
    /// Net ID
    pub net_id: String,
    /// Dimension (0 = X/vertical segments, 1 = Y/horizontal segments)
    pub dimension: usize,
    /// Position before VPSC solving
    pub position_before: f64,
    /// Position after VPSC solving
    pub position_after: f64,
    /// Minimum allowed position (channel limit)
    pub min_space_limit: f64,
    /// Maximum allowed position (channel limit)
    pub max_space_limit: f64,
    /// Segment type for visualization coloring
    pub segment_type: SegmentType,
    /// Range in the alternate dimension (min, max)
    pub alt_range: (f64, f64),
}

/// Debug information for a single nudging pass.
#[derive(Debug, Clone)]
pub struct NudgingPassDebugInfo {
    /// Name of this pass (e.g., "x_unify", "x_nudge")
    pub pass_name: String,
    /// Dimension being processed (0 = X, 1 = Y)
    pub dimension: usize,
    /// Segments processed in this pass
    pub segments: Vec<SegmentDebugInfo>,
    /// Paths after this pass completes
    pub paths_after: Vec<RoutedPath>,
}

/// Complete debug information for the nudging phase.
#[derive(Debug, Clone)]
pub struct NudgingDebugInfo {
    /// Paths before any nudging
    pub paths_before: Vec<RoutedPath>,
    /// Debug info for each pass (x_unify, x_nudge, y_unify, y_nudge)
    pub passes: Vec<NudgingPassDebugInfo>,
    /// Paths after same-net merging
    pub paths_after_merge: Vec<RoutedPath>,
}

// =============================================================================
// NudgingShiftSegment - Core segment structure
// =============================================================================

/// A segment that can be shifted during nudging.
///
/// This corresponds to libavoid's `NudgingShiftSegment` class.
#[derive(Debug, Clone)]
pub struct NudgingShiftSegment {
    /// Index of the path this segment belongs to
    pub path_idx: usize,

    /// Indexes of points that define this segment.
    /// For a simple segment, this is [low_idx, high_idx].
    /// Merged segments may have more indexes.
    pub indexes: Vec<usize>,

    /// The dimension this segment lies in (0 = X, 1 = Y)
    pub dimension: usize,

    /// Whether this segment is fixed (cannot be moved)
    pub fixed: bool,

    /// Whether this is a final segment (first or last in route)
    pub final_segment: bool,

    /// Whether this segment ends in a shape
    pub ends_in_shape: bool,

    /// Whether this is a single segment connector bridging two shapes
    pub single_connected_segment: bool,

    /// Whether this is an S-bend segment
    pub is_s_bend: bool,

    /// Whether this is a Z-bend segment
    pub is_z_bend: bool,

    /// Minimum allowed position (channel limit)
    pub min_space_limit: f64,

    /// Maximum allowed position (channel limit)
    pub max_space_limit: f64,

    /// Checkpoints on this segment
    pub checkpoints: Vec<Point>,

    /// Net ID for this segment's connector
    pub net_id: String,

    /// Connector ID
    pub connector_id: String,

    /// Solver variable (set during solving)
    pub variable_idx: Option<usize>,
}

impl NudgingShiftSegment {
    /// Create a new fixed segment (cannot be moved).
    pub fn new_fixed(
        path_idx: usize,
        low_idx: usize,
        high_idx: usize,
        dimension: usize,
        net_id: String,
        connector_id: String,
    ) -> Self {
        Self {
            path_idx,
            indexes: vec![low_idx, high_idx],
            dimension,
            fixed: true,
            final_segment: false,
            ends_in_shape: false,
            single_connected_segment: false,
            is_s_bend: false,
            is_z_bend: false,
            min_space_limit: -CHANNEL_MAX,
            max_space_limit: CHANNEL_MAX,
            checkpoints: Vec::new(),
            net_id,
            connector_id,
            variable_idx: None,
        }
    }

    /// Create a new movable segment with channel limits.
    #[allow(clippy::too_many_arguments)]
    pub fn new_movable(
        path_idx: usize,
        low_idx: usize,
        high_idx: usize,
        is_s_bend: bool,
        is_z_bend: bool,
        dimension: usize,
        min_limit: f64,
        max_limit: f64,
        net_id: String,
        connector_id: String,
    ) -> Self {
        Self {
            path_idx,
            indexes: vec![low_idx, high_idx],
            dimension,
            fixed: false,
            final_segment: false,
            ends_in_shape: false,
            single_connected_segment: false,
            is_s_bend,
            is_z_bend,
            min_space_limit: min_limit,
            max_space_limit: max_limit,
            checkpoints: Vec::new(),
            net_id,
            connector_id,
            variable_idx: None,
        }
    }

    /// Get the low point of this segment.
    pub fn low_point<'a>(&self, paths: &'a [RoutedPath]) -> &'a Point {
        &paths[self.path_idx].points[self.indexes[0]]
    }

    /// Get the high point of this segment.
    pub fn high_point<'a>(&self, paths: &'a [RoutedPath]) -> &'a Point {
        &paths[self.path_idx].points[*self.indexes.last().unwrap()]
    }

    /// Get the position of this segment in its dimension.
    pub fn position(&self, paths: &[RoutedPath]) -> f64 {
        let point = self.low_point(paths);
        if self.dimension == 0 {
            point.x
        } else {
            point.y
        }
    }

    /// Check if this is a zigzag segment (S-bend or Z-bend).
    pub fn is_zigzag(&self) -> bool {
        self.is_s_bend || self.is_z_bend
    }

    /// Check if this segment is immovable.
    pub fn is_immovable(&self) -> bool {
        !self.is_zigzag()
    }

    /// Get the ideal nudging distance from config.
    pub fn nudge_distance(&self, config: &RouterConfig) -> f64 {
        config.ideal_nudging_distance
    }

    /// Compute the weight for this segment's solver variable.
    pub fn compute_weight(&self, just_unifying: bool) -> f64 {
        if self.fixed {
            return FIXED_WEIGHT;
        }

        if self.final_segment {
            if self.single_connected_segment && !just_unifying {
                // Single segment bridging two shapes - prefer centering
                return STRONGER_WEIGHT;
            }
            return STRONG_WEIGHT;
        }

        if !self.checkpoints.is_empty() {
            return STRONG_WEIGHT;
        }

        if self.is_zigzag() {
            return ZIGZAG_WEIGHT;
        }

        FREE_WEIGHT
    }

    /// Compute the desired position for the solver variable.
    pub fn desired_position(&self, paths: &[RoutedPath]) -> f64 {
        let current_pos = self.position(paths);

        if self.is_zigzag()
            && self.min_space_limit > -CHANNEL_MAX
            && self.max_space_limit < CHANNEL_MAX
        {
            // For zigzag bends, prefer the middle of available space
            self.min_space_limit + (self.max_space_limit - self.min_space_limit) / 2.0
        } else {
            current_pos
        }
    }

    /// Check if this segment overlaps with another in the alternate dimension.
    ///
    /// Two segments overlap if:
    /// 1. They are in the same dimension
    /// 2. They are at similar positions (within nudging distance)
    /// 3. Their ranges in the alternate dimension intersect
    pub fn overlaps_with(&self, other: &NudgingShiftSegment, paths: &[RoutedPath]) -> bool {
        if self.dimension != other.dimension {
            return false;
        }

        // Check position similarity - segments must be close in their fixed dimension
        let self_pos = self.position(paths);
        let other_pos = other.position(paths);
        let nudge_dist = 30.0; // Use a reasonable threshold for overlap detection

        if (self_pos - other_pos).abs() > nudge_dist {
            return false;
        }

        let alt_dim = 1 - self.dimension;

        // Get the range of each segment in the alternate dimension
        let self_low = self.low_point(paths);
        let self_high = self.high_point(paths);
        let other_low = other.low_point(paths);
        let other_high = other.high_point(paths);

        let (self_min, self_max) = if alt_dim == 0 {
            (self_low.x.min(self_high.x), self_low.x.max(self_high.x))
        } else {
            (self_low.y.min(self_high.y), self_low.y.max(self_high.y))
        };

        let (other_min, other_max) = if alt_dim == 0 {
            (other_low.x.min(other_high.x), other_low.x.max(other_high.x))
        } else {
            (other_low.y.min(other_high.y), other_low.y.max(other_high.y))
        };

        // Check for overlap (touching at a point counts as overlap for nudging)
        self_min <= other_max && other_min <= self_max
    }

    /// Check if this segment should align with another (same connector final segments).
    pub fn should_align_with(
        &self,
        other: &NudgingShiftSegment,
        paths: &[RoutedPath],
        _dimension: usize,
    ) -> bool {
        // Only align segments from the same connector
        if self.connector_id != other.connector_id {
            return false;
        }

        // Both must be final segments
        if !self.final_segment || !other.final_segment {
            return false;
        }

        // Must overlap
        if !self.overlaps_with(other, paths) {
            return false;
        }

        // Check if aligning would create a single straight segment
        // (i.e., they share a common endpoint or are adjacent)
        true
    }

    /// Check if this segment can potentially align with another.
    pub fn can_align_with(
        &self,
        other: &NudgingShiftSegment,
        _paths: &[RoutedPath],
        _dimension: usize,
    ) -> bool {
        // Same connector segments that might drift together
        if self.connector_id != other.connector_id {
            return false;
        }

        // Only non-final segments can drift together
        if self.final_segment && other.final_segment {
            return false;
        }

        self.overlaps_with(other, _paths)
    }

    /// Merge another segment into this one.
    pub fn merge_with(&mut self, other: &NudgingShiftSegment) {
        // Add other's indexes
        for &idx in &other.indexes {
            if !self.indexes.contains(&idx) {
                self.indexes.push(idx);
            }
        }
        self.indexes.sort();

        // Take the intersection of space limits
        self.min_space_limit = self.min_space_limit.max(other.min_space_limit);
        self.max_space_limit = self.max_space_limit.min(other.max_space_limit);

        // Merge checkpoints
        self.checkpoints.extend(other.checkpoints.iter().cloned());
    }

    /// Update route positions from solver result.
    pub fn update_positions(&self, paths: &mut [RoutedPath], new_position: f64) {
        if self.fixed {
            return;
        }

        // Clamp to limits
        let new_pos = new_position
            .max(self.min_space_limit)
            .min(self.max_space_limit);

        let path_len = paths[self.path_idx].points.len();

        // If ANY point in this segment is a port endpoint, don't update anything
        // to preserve orthogonality (can't move one endpoint without the other)
        for &idx in &self.indexes {
            if idx == 0 || idx == path_len - 1 {
                return; // Contains port endpoint, skip entire segment
            }
        }

        // Update all points in this segment
        // Round to 2 decimal places to avoid floating point precision artifacts
        let rounded_pos = (new_pos * 100.0).round() / 100.0;

        for &idx in &self.indexes {
            let point = &mut paths[self.path_idx].points[idx];
            if self.dimension == 0 {
                point.x = rounded_pos;
            } else {
                point.y = rounded_pos;
            }
        }
    }

    /// Classify the segment type for visualization.
    pub fn classify_type(&self) -> SegmentType {
        if self.fixed {
            SegmentType::Fixed
        } else if self.final_segment {
            SegmentType::Final
        } else if self.is_zigzag() {
            SegmentType::Zigzag
        } else {
            SegmentType::Free
        }
    }

    /// Get the range in the alternate dimension.
    pub fn get_alt_range(&self, paths: &[RoutedPath]) -> (f64, f64) {
        let low = self.low_point(paths);
        let high = self.high_point(paths);
        let alt_dim = 1 - self.dimension;

        if alt_dim == 0 {
            (low.x.min(high.x), low.x.max(high.x))
        } else {
            (low.y.min(high.y), low.y.max(high.y))
        }
    }

    /// Convert to debug info for visualization.
    pub fn to_debug_info(&self, paths: &[RoutedPath]) -> SegmentDebugInfo {
        SegmentDebugInfo {
            path_idx: self.path_idx,
            connector_id: self.connector_id.clone(),
            net_id: self.net_id.clone(),
            dimension: self.dimension,
            position_before: self.position(paths),
            position_after: self.position(paths), // Will be updated after solve
            min_space_limit: self.min_space_limit,
            max_space_limit: self.max_space_limit,
            segment_type: self.classify_type(),
            alt_range: self.get_alt_range(paths),
        }
    }
}

// =============================================================================
// Segment Building
// =============================================================================

/// Build the list of nudging segments from all routes.
///
/// This corresponds to libavoid's `buildOrthogonalNudgingSegments`.
pub fn build_nudging_segments(
    paths: &[RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
    dimension: usize,
    nudge_final_segments: bool,
) -> Vec<NudgingShiftSegment> {
    let mut segments = Vec::new();
    let alt_dim = 1 - dimension;
    let buffer = config.shape_buffer_distance;

    // Cache obstacle bounds for final segment limiting (with buffer applied)
    let obstacle_bounds: Vec<_> = obstacles
        .iter()
        .map(|obs| {
            (
                obs.bounds.min_x,
                obs.bounds.max_x,
                obs.bounds.min_y,
                obs.bounds.max_y,
            )
        })
        .collect();

    for (path_idx, path) in paths.iter().enumerate() {
        let net_id = net_ids
            .get(path_idx)
            .cloned()
            .unwrap_or_else(|| path.connector_id.clone());
        let connector_id = path.connector_id.clone();

        if path.points.len() < 2 {
            continue;
        }

        // Iterate through segments (pairs of adjacent points)
        for i in 1..path.points.len() {
            let p1 = &path.points[i - 1];
            let p2 = &path.points[i];

            // Check if this segment is in the dimension we're processing
            let is_in_dimension = if dimension == 0 {
                // Processing X dimension - looking for vertical segments (same X)
                (p1.x - p2.x).abs() < 1e-9
            } else {
                // Processing Y dimension - looking for horizontal segments (same Y)
                (p1.y - p2.y).abs() < 1e-9
            };

            if !is_in_dimension {
                continue;
            }

            // Skip zero-length segments
            let alt_coord_1 = if alt_dim == 0 { p1.x } else { p1.y };
            let alt_coord_2 = if alt_dim == 0 { p2.x } else { p2.y };
            if (alt_coord_1 - alt_coord_2).abs() < 1e-9 {
                continue;
            }

            // Determine low and high indexes (sorted by alt dimension)
            let (low_idx, high_idx) = if alt_coord_1 < alt_coord_2 {
                (i - 1, i)
            } else {
                (i, i - 1)
            };

            let this_pos = if dimension == 0 { p1.x } else { p1.y };

            // Check if this is a final segment (first or last)
            let is_final = i == 1 || i == path.points.len() - 1;

            if is_final {
                if nudge_final_segments {
                    // Compute limits for final segments
                    let mut min_limit = -CHANNEL_MAX;
                    let mut max_limit = CHANNEL_MAX;

                    // Check if segment is within any obstacle
                    let mut ends_in_shape = false;
                    for &(obs_min_x, obs_max_x, obs_min_y, obs_max_y) in &obstacle_bounds {
                        let (obs_min, obs_max) = if dimension == 0 {
                            (obs_min_x, obs_max_x)
                        } else {
                            (obs_min_y, obs_max_y)
                        };

                        // Check if either endpoint is in this obstacle
                        let p1_in = p1.x >= obs_min_x
                            && p1.x <= obs_max_x
                            && p1.y >= obs_min_y
                            && p1.y <= obs_max_y;
                        let p2_in = p2.x >= obs_min_x
                            && p2.x <= obs_max_x
                            && p2.y >= obs_min_y
                            && p2.y <= obs_max_y;

                        if p1_in || p2_in {
                            min_limit = min_limit.max(obs_min);
                            max_limit = max_limit.min(obs_max);
                            ends_in_shape = true;
                        }
                    }

                    if !ends_in_shape {
                        // Limit movement for free connectors
                        min_limit = min_limit.max(this_pos - FREE_CONN_BUFFER);
                        max_limit = max_limit.min(this_pos + FREE_CONN_BUFFER);
                    }

                    if (min_limit - max_limit).abs() < 1e-9 {
                        // Fixed - no room to move
                        segments.push(NudgingShiftSegment::new_fixed(
                            path_idx,
                            low_idx,
                            high_idx,
                            dimension,
                            net_id.clone(),
                            connector_id.clone(),
                        ));
                    } else {
                        // Movable final segment
                        let mut seg = NudgingShiftSegment::new_movable(
                            path_idx,
                            low_idx,
                            high_idx,
                            false,
                            false,
                            dimension,
                            min_limit,
                            max_limit,
                            net_id.clone(),
                            connector_id.clone(),
                        );
                        seg.final_segment = true;
                        seg.ends_in_shape = ends_in_shape;

                        // Check for single-segment connector
                        if path.points.len() == 2 && ends_in_shape {
                            seg.single_connected_segment = true;
                        }

                        segments.push(seg);
                    }
                } else {
                    // Final segments can't be moved
                    segments.push(NudgingShiftSegment::new_fixed(
                        path_idx,
                        low_idx,
                        high_idx,
                        dimension,
                        net_id.clone(),
                        connector_id.clone(),
                    ));
                }
                continue;
            }

            // Middle segment - compute limits from adjacent segments and obstacles
            let mut min_limit = -CHANNEL_MAX;
            let mut max_limit = CHANNEL_MAX;
            let mut is_s_bend = false;
            let mut is_z_bend = false;

            // Get positions of adjacent points (for S-bend/Z-bend detection)
            if i >= 2 && i + 1 < path.points.len() {
                let prev_point = &path.points[i - 2];
                let next_point = &path.points[i + 1];

                let prev_pos = if dimension == 0 {
                    prev_point.x
                } else {
                    prev_point.y
                };
                let next_pos = if dimension == 0 {
                    next_point.x
                } else {
                    next_point.y
                };

                // Detect S-bend or Z-bend
                if (prev_pos < this_pos && next_pos > this_pos)
                    || (prev_pos > this_pos && next_pos < this_pos)
                {
                    if prev_pos < this_pos && next_pos > this_pos {
                        min_limit = min_limit.max(prev_pos);
                        max_limit = max_limit.min(next_pos);
                        is_z_bend = true;
                    } else {
                        min_limit = min_limit.max(next_pos);
                        max_limit = max_limit.min(prev_pos);
                        is_s_bend = true;
                    }
                }
            }

            // Further restrict limits based on obstacles
            // Get the alt-dimension range of this segment
            let (seg_alt_min, seg_alt_max) = if alt_dim == 0 {
                (p1.x.min(p2.x), p1.x.max(p2.x))
            } else {
                (p1.y.min(p2.y), p1.y.max(p2.y))
            };

            // Check each obstacle
            for &(obs_min_x, obs_max_x, obs_min_y, obs_max_y) in &obstacle_bounds {
                let (obs_alt_min, obs_alt_max) = if alt_dim == 0 {
                    (obs_min_x, obs_max_x)
                } else {
                    (obs_min_y, obs_max_y)
                };

                // Check if segment's alt-dimension range overlaps with obstacle's BUFFERED alt-dimension range
                // We use buffered bounds here because segments in the buffer zone should also be constrained
                // Use inclusive bounds (touching at boundary counts as overlap)
                let buffered_alt_min = obs_alt_min - buffer;
                let buffered_alt_max = obs_alt_max + buffer;
                if seg_alt_min <= buffered_alt_max && seg_alt_max >= buffered_alt_min {
                    // Segment passes through this obstacle's alt-dimension range
                    // Get the obstacle bounds in the nudge dimension
                    let (obs_dim_min, obs_dim_max) = if dimension == 0 {
                        (obs_min_x, obs_max_x)
                    } else {
                        (obs_min_y, obs_max_y)
                    };

                    // Restrict limits to avoid this obstacle (with buffer zone)
                    // The buffered bounds define where segments can safely be placed
                    let buffered_min = obs_dim_min - buffer;
                    let buffered_max = obs_dim_max + buffer;

                    let old_min = min_limit;
                    let old_max = max_limit;
                    if this_pos < buffered_min {
                        // Segment is to the left/above obstacle's buffer - can't go past buffered_min
                        max_limit = max_limit.min(buffered_min);
                    } else if this_pos > buffered_max {
                        // Segment is to the right/below obstacle's buffer - can't go before buffered_max
                        min_limit = min_limit.max(buffered_max);
                    } else {
                        // Segment is INSIDE obstacle's buffer zone - this can happen if a previous
                        // nudging pass pushed it inside. Constrain it to move to the nearest edge.
                        let dist_to_min = (this_pos - buffered_min).abs();
                        let dist_to_max = (this_pos - buffered_max).abs();
                        if dist_to_max <= dist_to_min {
                            // Closer to max edge (right/bottom), push it outside that edge
                            min_limit = min_limit.max(buffered_max);
                            log::debug!(
                                "[nudging] Segment inside obstacle buffer at pos={:.2}, pushing toward max edge {:.2}",
                                this_pos,
                                buffered_max
                            );
                        } else {
                            // Closer to min edge (left/top), push it outside that edge
                            max_limit = max_limit.min(buffered_min);
                            log::debug!(
                                "[nudging] Segment inside obstacle buffer at pos={:.2}, pushing toward min edge {:.2}",
                                this_pos,
                                buffered_min
                            );
                        }
                    }

                    if min_limit != old_min || max_limit != old_max {
                        log::debug!(
                            "[nudging] Middle segment in path {} (connector={}) at pos={:.2}: \
                             obstacle restricts limits from ({:.2},{:.2}) to ({:.2},{:.2})",
                            path_idx,
                            connector_id,
                            this_pos,
                            old_min,
                            old_max,
                            min_limit,
                            max_limit
                        );
                    }
                }
            }

            segments.push(NudgingShiftSegment::new_movable(
                path_idx,
                low_idx,
                high_idx,
                is_s_bend,
                is_z_bend,
                dimension,
                min_limit,
                max_limit,
                net_id.clone(),
                connector_id.clone(),
            ));
        }
    }

    segments
}

// =============================================================================
// Segment Sorting (linesort)
// =============================================================================

/// Compare two segments for sorting.
///
/// This implements libavoid's segment ordering logic.
fn compare_segments(
    a: &NudgingShiftSegment,
    b: &NudgingShiftSegment,
    paths: &[RoutedPath],
    _dimension: usize,
) -> Ordering {
    let pos_a = a.position(paths);
    let pos_b = b.position(paths);

    // Primary sort by position
    if (pos_a - pos_b).abs() > 1e-9 {
        return pos_a.partial_cmp(&pos_b).unwrap_or(Ordering::Equal);
    }

    // Same position - use secondary criteria
    // Prefer fixed segments first
    if a.fixed != b.fixed {
        return if a.fixed {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }

    // Prefer final segments
    if a.final_segment != b.final_segment {
        return if a.final_segment {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }

    // Prefer non-zigzag (c-bends) over zigzag
    if a.is_zigzag() != b.is_zigzag() {
        return if a.is_zigzag() {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }

    // Fall back to path index for stability
    a.path_idx.cmp(&b.path_idx)
}

/// Sort and merge segments for a region.
///
/// This corresponds to libavoid's `linesort` function.
pub fn linesort(
    mut segments: Vec<NudgingShiftSegment>,
    paths: &[RoutedPath],
    dimension: usize,
    nudge_final_segments: bool,
) -> Vec<NudgingShiftSegment> {
    if segments.is_empty() {
        return segments;
    }

    // Merge segments of the same connector if nudging final segments
    // Only merge if segments are ADJACENT (share a common point index)
    if nudge_final_segments {
        let mut i = 0;
        while i < segments.len() {
            let mut j = i + 1;
            while j < segments.len() {
                // Check if same connector and overlapping
                if segments[i].connector_id == segments[j].connector_id
                    && segments[i].overlaps_with(&segments[j], paths)
                {
                    // Only merge if they share a common index (are truly adjacent)
                    let shares_index = segments[i]
                        .indexes
                        .iter()
                        .any(|idx| segments[j].indexes.contains(idx));
                    if shares_index {
                        let other = segments.remove(j);
                        segments[i].merge_with(&other);
                    } else {
                        j += 1;
                    }
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }

    // Sort segments
    segments.sort_by(|a, b| compare_segments(a, b, paths, dimension));

    segments
}

// =============================================================================
// Main Nudging Algorithm
// =============================================================================

/// Nudge orthogonal routes to separate overlapping segments.
///
/// This is the main entry point, corresponding to libavoid's `nudgeOrthogonalRoutes`.
pub fn nudge_orthogonal_routes(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
) {
    if paths.is_empty() {
        return;
    }

    // Process each dimension separately
    for dimension in 0..2 {
        nudge_dimension(paths, net_ids, obstacles, config, dimension);
    }
}

/// Nudge routes in a single dimension.
fn nudge_dimension(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
    dimension: usize,
) {
    let nudge_final_segments = true; // Could be configurable

    // Two-pass algorithm
    // Pass 1: Unifying - establish ordering with lighter constraints
    nudge_pass(
        paths,
        net_ids,
        obstacles,
        config,
        dimension,
        nudge_final_segments,
        true, // just_unifying
        None, // no debug capture
    );

    // Pass 2: Nudging - apply full separation constraints
    nudge_pass(
        paths,
        net_ids,
        obstacles,
        config,
        dimension,
        nudge_final_segments,
        false, // not unifying
        None,  // no debug capture
    );
}

/// Execute a single nudging pass.
///
/// If `pass_name` is provided, captures and returns debug info for this pass.
#[allow(clippy::too_many_arguments)]
fn nudge_pass(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
    dimension: usize,
    nudge_final_segments: bool,
    just_unifying: bool,
    pass_name: Option<String>,
) -> Option<NudgingPassDebugInfo> {
    // Build segments for this dimension
    let mut all_segments = build_nudging_segments(
        paths,
        net_ids,
        obstacles,
        config,
        dimension,
        nudge_final_segments,
    );

    // Capture segment info BEFORE solving (for debug)
    let mut segment_debug_infos: Option<Vec<SegmentDebugInfo>> = if pass_name.is_some() {
        Some(
            all_segments
                .iter()
                .map(|seg| seg.to_debug_info(paths))
                .collect(),
        )
    } else {
        None
    };

    if all_segments.is_empty() {
        return pass_name.map(|name| NudgingPassDebugInfo {
            pass_name: name,
            dimension,
            segments: Vec::new(),
            paths_after: paths.to_vec(),
        });
    }

    // Process segments in overlapping regions
    while !all_segments.is_empty() {
        // Take the first segment
        let current = all_segments.remove(0);

        // Find all segments that overlap with current
        let mut region = vec![current];
        let mut i = 0;
        while i < all_segments.len() {
            let overlaps_any = region
                .iter()
                .any(|seg| seg.overlaps_with(&all_segments[i], paths));
            if overlaps_any {
                region.push(all_segments.remove(i));
            } else {
                i += 1;
            }
        }

        // Sort the region
        region = linesort(region, paths, dimension, nudge_final_segments);

        // Solve constraints for this region
        solve_region(paths, &mut region, config, dimension, just_unifying);
    }

    // Update position_after in debug info from the modified paths
    if let Some(ref mut debug_infos) = segment_debug_infos {
        for debug_seg in debug_infos.iter_mut() {
            if debug_seg.path_idx < paths.len() {
                let path = &paths[debug_seg.path_idx];
                for i in 0..path.points.len().saturating_sub(1) {
                    let p1 = &path.points[i];
                    let p2 = &path.points[i + 1];

                    let is_in_dim = if debug_seg.dimension == 0 {
                        (p1.x - p2.x).abs() < 1e-9
                    } else {
                        (p1.y - p2.y).abs() < 1e-9
                    };

                    if is_in_dim {
                        let (alt_min, alt_max) = if debug_seg.dimension == 0 {
                            (p1.y.min(p2.y), p1.y.max(p2.y))
                        } else {
                            (p1.x.min(p2.x), p1.x.max(p2.x))
                        };

                        let overlap =
                            alt_min <= debug_seg.alt_range.1 && alt_max >= debug_seg.alt_range.0;
                        if overlap {
                            debug_seg.position_after =
                                if debug_seg.dimension == 0 { p1.x } else { p1.y };
                            break;
                        }
                    }
                }
            }
        }
    }

    pass_name.map(|name| NudgingPassDebugInfo {
        pass_name: name,
        dimension,
        segments: segment_debug_infos.unwrap_or_default(),
        paths_after: paths.to_vec(),
    })
}

/// Solve constraints for a region of overlapping segments.
fn solve_region(
    paths: &mut [RoutedPath],
    segments: &mut [NudgingShiftSegment],
    config: &RouterConfig,
    _dimension: usize,
    just_unifying: bool,
) {
    if segments.is_empty() {
        return;
    }

    let base_sep_dist = config.ideal_nudging_distance;
    let mut sep_dist = base_sep_dist;

    // Try solving with decreasing separation distance if unsatisfiable
    for iteration in 0..10 {
        let result = try_solve_region(paths, segments, sep_dist, just_unifying);

        if result {
            // Successfully solved
            return;
        }

        // Reduce separation distance and retry
        sep_dist *= 0.5;
        if sep_dist < 1.0 {
            break;
        }

        log::debug!(
            "[nudging] Iteration {}: reducing separation to {:.2}",
            iteration,
            sep_dist
        );
    }

    // Fall back to zero separation if nothing works
    let _ = try_solve_region(paths, segments, 0.0, just_unifying);
}

/// Try to solve constraints with given separation distance.
/// Returns true if successful.
fn try_solve_region(
    paths: &mut [RoutedPath],
    segments: &mut [NudgingShiftSegment],
    sep_dist: f64,
    just_unifying: bool,
) -> bool {
    if segments.is_empty() {
        return true;
    }

    // Create solver variables
    let mut variables: Vec<Variable> = Vec::new();
    let mut constraints: Vec<Constraint> = Vec::new();

    // Debug logging for region analysis
    log::debug!("[nudging] Region with {} segments", segments.len());
    for seg in segments.iter() {
        let seg_type = seg.classify_type();
        log::debug!(
            "[nudging]   {} seg net={} pos={:.2} limits=({:.2},{:.2}) type={:?}",
            if seg.dimension == 0 { "X" } else { "Y" },
            seg.net_id,
            seg.position(paths),
            seg.min_space_limit,
            seg.max_space_limit,
            seg_type
        );
    }

    for seg in segments.iter_mut() {
        let weight = seg.compute_weight(just_unifying);

        // Use the segment's default desired position
        // Global anchor alignment was removed because it caused problems with large nets
        // (e.g., V3V3 in dm0001) where unrelated segments would be incorrectly aligned
        let desired_pos = seg.desired_position(paths);

        let var_id = if seg.fixed {
            FIXED_SEGMENT_ID
        } else {
            FREE_SEGMENT_ID
        };

        // Record the actual variable index before pushing
        let seg_var_idx = variables.len();
        variables.push(Variable::new(var_id, desired_pos, weight));
        seg.variable_idx = Some(seg_var_idx);

        // Add channel boundary constraints
        if !seg.fixed {
            if seg.min_space_limit > -CHANNEL_MAX {
                // Left boundary
                let boundary_idx = variables.len();
                variables.push(Variable::new(
                    CHANNEL_LEFT_ID,
                    seg.min_space_limit,
                    FIXED_WEIGHT,
                ));
                let constraint_id = constraints.len();
                constraints.push(Constraint::new(
                    constraint_id,
                    boundary_idx,
                    seg_var_idx,
                    0.0,
                ));
            }
            if seg.max_space_limit < CHANNEL_MAX {
                // Right boundary
                let boundary_idx = variables.len();
                variables.push(Variable::new(
                    CHANNEL_RIGHT_ID,
                    seg.max_space_limit,
                    FIXED_WEIGHT,
                ));
                let constraint_id = constraints.len();
                constraints.push(Constraint::new(
                    constraint_id,
                    seg_var_idx,
                    boundary_idx,
                    0.0,
                ));
            }
        }
    }

    // Add separation constraints between segments
    for i in 1..segments.len() {
        let prev_var_idx = segments[i - 1].variable_idx.unwrap();
        let curr_var_idx = segments[i].variable_idx.unwrap();

        // Determine separation
        let mut this_sep = sep_dist;
        let mut equality = false;

        // Check if segments should align (same connector)
        if segments[i].should_align_with(&segments[i - 1], paths, 0) {
            this_sep = 0.0;
            equality = true;
        } else if segments[i].can_align_with(&segments[i - 1], paths, 0) {
            this_sep = 0.0;
        } else if segments[i].net_id == segments[i - 1].net_id {
            // Same net - force alignment to unite same-net routes
            // But only if their channel limits overlap - otherwise alignment is impossible
            let limits_overlap = segments[i].min_space_limit <= segments[i - 1].max_space_limit
                && segments[i - 1].min_space_limit <= segments[i].max_space_limit;
            if limits_overlap {
                this_sep = 0.0;
                equality = true;
            }
        }

        if equality {
            // Equality constraint: both at same position
            // Implemented as two separation constraints with 0 gap
            let c1_id = constraints.len();
            constraints.push(Constraint::new(c1_id, prev_var_idx, curr_var_idx, 0.0));
            let c2_id = constraints.len();
            constraints.push(Constraint::new(c2_id, curr_var_idx, prev_var_idx, 0.0));
        } else if this_sep > 0.0 {
            // Separation constraint
            let c_id = constraints.len();
            constraints.push(Constraint::new(c_id, prev_var_idx, curr_var_idx, this_sep));
        }
    }

    // Solve
    let mut solver = IncSolver::with_problem(variables, constraints);
    solver.solve();

    // Apply results
    for seg in segments.iter() {
        if let Some(var_idx) = seg.variable_idx {
            let new_pos = solver.get_position(var_idx);
            let current_pos = seg.position(paths);
            let clamped_pos = new_pos.max(seg.min_space_limit).min(seg.max_space_limit);

            // Log if VPSC returned a position outside the limits
            if new_pos < seg.min_space_limit - 1e-6 || new_pos > seg.max_space_limit + 1e-6 {
                log::warn!(
                    "[nudging] VPSC violated limit for segment in path {} (connector={}): \
                     solver_pos={:.2}, min_limit={:.2}, max_limit={:.2}, clamped_pos={:.2}, \
                     current_pos={:.2}",
                    seg.path_idx,
                    seg.connector_id,
                    new_pos,
                    seg.min_space_limit,
                    seg.max_space_limit,
                    clamped_pos,
                    current_pos
                );
            }

            seg.update_positions(paths, new_pos);
        }
    }

    true // VPSC solver always produces a result
}

// =============================================================================
// Same-Net Path Merging
// =============================================================================

/// Threshold for considering same-net segments "close enough" to merge.
/// This is larger than the nudging overlap threshold to catch near-misses.
const SAME_NET_MERGE_THRESHOLD: f64 = 30.0;

/// Merge same-net paths that have parallel segments close to each other.
///
/// This handles cases where pathfinding found slightly different paths for
/// same-net routes, creating a "box" artifact instead of a clean tree.
fn merge_same_net_paths(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
) {
    if paths.len() < 2 {
        return;
    }

    // For each dimension (0=X/vertical segments, 1=Y/horizontal segments)
    for dimension in 0..2 {
        merge_same_net_paths_in_dimension(paths, net_ids, dimension, obstacles, config);
    }
}

/// Merge same-net paths in a single dimension.
fn merge_same_net_paths_in_dimension(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    dimension: usize,
    obstacles: &[Obstacle],
    config: &RouterConfig,
) {
    // Collect segments with their net info
    let mut segments: Vec<SegmentInfo> = Vec::new();

    for (path_idx, path) in paths.iter().enumerate() {
        let net_id = net_ids
            .get(path_idx)
            .cloned()
            .unwrap_or_else(|| path.connector_id.clone());

        for i in 0..path.points.len().saturating_sub(1) {
            let p1 = &path.points[i];
            let p2 = &path.points[i + 1];

            // Check if segment is in this dimension
            let is_in_dimension = if dimension == 0 {
                (p1.x - p2.x).abs() < 1e-9 // Vertical segment (same X)
            } else {
                (p1.y - p2.y).abs() < 1e-9 // Horizontal segment (same Y)
            };

            if !is_in_dimension {
                continue;
            }

            let position = if dimension == 0 { p1.x } else { p1.y };
            let (alt_min, alt_max) = if dimension == 0 {
                (p1.y.min(p2.y), p1.y.max(p2.y))
            } else {
                (p1.x.min(p2.x), p1.x.max(p2.x))
            };

            let is_final = i == 0 || i == path.points.len() - 2;
            // Middle segments can be merge targets (they have room to absorb others)
            let can_be_target = !is_final && path.points.len() > 3;

            segments.push(SegmentInfo {
                path_idx,
                seg_start_idx: i,
                net_id: net_id.clone(),
                position,
                alt_min,
                alt_max,
                is_final,
                can_be_target,
            });
        }
    }

    // Find merge opportunities: same-net segments that are close but not overlapping
    // We want to merge a final segment TO a middle segment target
    let mut merges: Vec<(usize, usize, f64)> = Vec::new(); // (source_idx, target_idx, target_pos)

    for (i, seg_i) in segments.iter().enumerate() {
        if !seg_i.is_final {
            continue; // Only consider merging final segments
        }

        for (j, seg_j) in segments.iter().enumerate() {
            if i == j || seg_i.path_idx == seg_j.path_idx {
                continue;
            }
            if seg_i.net_id != seg_j.net_id {
                continue;
            }
            if !seg_j.can_be_target {
                continue;
            }

            // Check if positions are close (but not already overlapping)
            let pos_diff = (seg_i.position - seg_j.position).abs();
            if !(1e-9..=SAME_NET_MERGE_THRESHOLD).contains(&pos_diff) {
                continue;
            }

            // Check if alternate dimension ranges overlap
            if seg_i.alt_min > seg_j.alt_max || seg_j.alt_min > seg_i.alt_max {
                continue;
            }

            // Found a merge candidate!
            merges.push((i, j, seg_j.position));
        }
    }

    // Apply merges (restructure paths)
    // Sort by path_idx descending so we process from end to not invalidate indices
    merges.sort_by(|a, b| segments[b.0].path_idx.cmp(&segments[a.0].path_idx));

    for (source_idx, _target_idx, target_pos) in merges {
        let seg = &segments[source_idx];
        apply_segment_merge(
            paths,
            seg.path_idx,
            seg.seg_start_idx,
            dimension,
            target_pos,
            obstacles,
            config,
        );
    }
}

/// Segment info for merging
#[derive(Debug)]
struct SegmentInfo {
    path_idx: usize,
    seg_start_idx: usize,
    net_id: String,
    position: f64,
    alt_min: f64,
    alt_max: f64,
    is_final: bool,
    #[allow(dead_code)]
    can_be_target: bool,
}

/// Apply a merge by restructuring a path to align a segment with a target position.
///
/// For a horizontal segment at Y=current that we want to move to Y=target:
/// - Original: port(x1, y_port) → (x2, y_port) → next_point
/// - New: port(x1, y_port) → (x1, y_target) → (x2, y_target) → next_point
///
/// This adds one point and modifies one point to create the detour.
///
/// **Important**: This function checks if the resulting detour segment would be
/// flush with an obstacle edge. If so, the merge is skipped to maintain proper
/// clearance from obstacles.
fn apply_segment_merge(
    paths: &mut [RoutedPath],
    path_idx: usize,
    seg_start_idx: usize,
    dimension: usize,
    target_pos: f64,
    obstacles: &[Obstacle],
    config: &RouterConfig,
) {
    let path = &mut paths[path_idx];

    if seg_start_idx + 1 >= path.points.len() {
        return;
    }

    let p1 = path.points[seg_start_idx];
    let p2 = path.points[seg_start_idx + 1];

    // Current position and target
    let current_pos = if dimension == 0 { p1.x } else { p1.y };

    // Don't merge if already at target
    if (current_pos - target_pos).abs() < 1e-9 {
        return;
    }

    // Only handle first segment (connected to port) for now
    if seg_start_idx != 0 {
        return;
    }

    // The detour creates a new segment from p1 to the detour point.
    // Check if this segment would be flush with any obstacle edge.
    let buffer = config.shape_buffer_distance;

    if dimension == 1 {
        // Horizontal segment, moving Y position
        // Original: p1(x1, y1) → p2(x2, y1) → ...
        // Detour: p1(x1, y1) → (x1, target) → (x2, target) → ...
        // New segment is VERTICAL at X=p1.x from Y=p1.y to Y=target

        let detour_point = Point::new(p1.x, target_pos);
        let new_p2 = Point::new(p2.x, target_pos);

        // Check if detour point already exists (path was already modified)
        if path.points.len() > 1 {
            let next = &path.points[seg_start_idx + 1];
            if (next.x - detour_point.x).abs() < 1e-9 && (next.y - detour_point.y).abs() < 1e-9 {
                return; // Already merged
            }
        }

        // Check if the new vertical segment at X=p1.x would be flush with any obstacle
        let seg_x = p1.x;
        let seg_y_min = p1.y.min(target_pos);
        let seg_y_max = p1.y.max(target_pos);

        for obs in obstacles {
            // Check if this X coordinate is on the left or right edge of the obstacle
            let on_left_edge = (seg_x - obs.bounds.min_x).abs() < buffer;
            let on_right_edge = (seg_x - obs.bounds.max_x).abs() < buffer;

            if on_left_edge || on_right_edge {
                // Check if the Y range overlaps with the obstacle
                if seg_y_min < obs.bounds.max_y && seg_y_max > obs.bounds.min_y {
                    // The new vertical segment would be flush with this obstacle - skip merge
                    log::debug!(
                        "[merge] Skipping merge: vertical segment at X={:.2} would be flush with obstacle '{}' edge",
                        seg_x,
                        obs.id
                    );
                    return;
                }
            }
        }

        // Build new path segment by segment to avoid index issues
        let mut new_points = Vec::with_capacity(path.points.len() + 1);
        new_points.push(p1); // Port stays same
        new_points.push(detour_point); // Vertical jog
        new_points.push(new_p2); // Horizontal at target level

        // Add remaining points from original path (skip old p2)
        for i in (seg_start_idx + 2)..path.points.len() {
            new_points.push(path.points[i]);
        }

        path.points = new_points;
    } else {
        // Vertical segment, moving X position
        // New segment is HORIZONTAL at Y=p1.y from X=p1.x to X=target
        let detour_point = Point::new(target_pos, p1.y);
        let new_p2 = Point::new(target_pos, p2.y);

        // Check if detour point already exists
        if path.points.len() > 1 {
            let next = &path.points[seg_start_idx + 1];
            if (next.x - detour_point.x).abs() < 1e-9 && (next.y - detour_point.y).abs() < 1e-9 {
                return;
            }
        }

        // Check if the new horizontal segment at Y=p1.y would be flush with any obstacle
        let seg_y = p1.y;
        let seg_x_min = p1.x.min(target_pos);
        let seg_x_max = p1.x.max(target_pos);

        for obs in obstacles {
            // Check if this Y coordinate is on the top or bottom edge of the obstacle
            let on_top_edge = (seg_y - obs.bounds.min_y).abs() < buffer;
            let on_bottom_edge = (seg_y - obs.bounds.max_y).abs() < buffer;

            if on_top_edge || on_bottom_edge {
                // Check if the X range overlaps with the obstacle
                if seg_x_min < obs.bounds.max_x && seg_x_max > obs.bounds.min_x {
                    // The new horizontal segment would be flush with this obstacle - skip merge
                    log::debug!(
                        "[merge] Skipping merge: horizontal segment at Y={:.2} would be flush with obstacle '{}' edge",
                        seg_y,
                        obs.id
                    );
                    return;
                }
            }
        }

        let mut new_points = Vec::with_capacity(path.points.len() + 1);
        new_points.push(p1);
        new_points.push(detour_point);
        new_points.push(new_p2);

        for i in (seg_start_idx + 2)..path.points.len() {
            new_points.push(path.points[i]);
        }

        path.points = new_points;
    }
}

// =============================================================================
// Debug Capture Functions
// =============================================================================

/// Nudge routes in a single dimension with debug capture.
fn nudge_dimension_debug(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
    dimension: usize,
    debug_info: &mut NudgingDebugInfo,
) {
    let nudge_final_segments = true;
    let dim_name = if dimension == 0 { "x" } else { "y" };

    // Pass 1: Unifying - uses the same nudge_pass() with debug capture
    if let Some(pass_debug) = nudge_pass(
        paths,
        net_ids,
        obstacles,
        config,
        dimension,
        nudge_final_segments,
        true, // just_unifying
        Some(format!("{}_unify", dim_name)),
    ) {
        debug_info.passes.push(pass_debug);
    }

    // Pass 2: Nudging - uses the same nudge_pass() with debug capture
    if let Some(pass_debug) = nudge_pass(
        paths,
        net_ids,
        obstacles,
        config,
        dimension,
        nudge_final_segments,
        false, // not unifying
        Some(format!("{}_nudge", dim_name)),
    ) {
        debug_info.passes.push(pass_debug);
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Main entry point for nudging routes.
///
/// This replaces the old nudging implementation with a libavoid-compatible one.
///
/// When `capture_debug` is true, returns debug information about each nudging pass.
pub fn nudge_routes(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
    capture_debug: bool,
) -> Option<NudgingDebugInfo> {
    if !capture_debug {
        // Fast path - no debug capture
        nudge_orthogonal_routes(paths, net_ids, obstacles, config);
        merge_same_net_paths(paths, net_ids, obstacles, config);
        return None;
    }

    // Debug path - capture state at each step
    let paths_before = paths.to_vec();
    let mut debug_info = NudgingDebugInfo {
        paths_before,
        passes: Vec::new(),
        paths_after_merge: Vec::new(),
    };

    // Process each dimension with debug capture
    for dimension in 0..2 {
        nudge_dimension_debug(
            paths,
            net_ids,
            obstacles,
            config,
            dimension,
            &mut debug_info,
        );
    }

    // Same-net merging
    merge_same_net_paths(paths, net_ids, obstacles, config);
    debug_info.paths_after_merge = paths.to_vec();

    Some(debug_info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_creation() {
        let seg = NudgingShiftSegment::new_fixed(0, 0, 1, 0, "net1".into(), "c1".into());
        assert!(seg.fixed);
        assert_eq!(seg.indexes, vec![0, 1]);
    }

    #[test]
    fn test_segment_overlap() {
        let paths = vec![
            RoutedPath::new("c1", vec![Point::new(0.0, 0.0), Point::new(0.0, 100.0)]),
            RoutedPath::new("c2", vec![Point::new(0.0, 50.0), Point::new(0.0, 150.0)]),
        ];

        let seg1 = NudgingShiftSegment::new_fixed(0, 0, 1, 0, "net1".into(), "c1".into());
        let seg2 = NudgingShiftSegment::new_fixed(1, 0, 1, 0, "net2".into(), "c2".into());

        // Vertical segments at X=0, Y ranges [0,100] and [50,150] should overlap
        assert!(seg1.overlaps_with(&seg2, &paths));
    }

    #[test]
    fn test_build_segments() {
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(50.0, 0.0),
                Point::new(50.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".into()];
        let obstacles = vec![];
        let config = RouterConfig::default();

        // Build segments for Y dimension (horizontal segments)
        let segments = build_nudging_segments(&paths, &net_ids, &obstacles, &config, 1, true);

        // Should have one horizontal segment (Y=0)
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].dimension, 1);
    }
}
