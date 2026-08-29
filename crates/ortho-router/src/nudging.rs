//! Route nudging using VPSC solver - Vertex-Centric Approach.
//!
//! This module adjusts route positions to separate overlapping segments from
//! different nets while keeping same-net segments together.
//!
//! ## Key Insight: Vertex-Centric vs Segment-Centric
//!
//! Previous implementation thought about segments independently, but segments
//! share vertices. Moving a segment's endpoint affects adjacent segments.
//!
//! This implementation thinks about **bend points** (intermediate vertices):
//! - Port endpoints (first/last points) are FIXED
//! - Bend points (middle points) can be nudged in ONE dimension
//! - Each bend point affects exactly two segments (before and after)
//!
//! ## Algorithm
//!
//! 1. Extract bend points from all paths
//! 2. For each bend point, determine:
//!    - Which dimension it can move (X or Y)
//!    - Channel limits from obstacles and adjacent fixed points
//!    - Segment type (final, zigzag, free)
//! 3. Find overlapping segments that need separation
//! 4. Create VPSC constraints for different-net overlaps
//! 5. Solve and apply new positions to bend points

use crate::config::RouterConfig;
use crate::types::{Obstacle, RoutedPath};
use crate::vpsc::IncSolver;
use std::collections::HashMap;

// =============================================================================
// Weight Constants (from libavoid)
// =============================================================================

/// Weight for zigzag segments - move easily, prefer centering
const ZIGZAG_WEIGHT: f64 = 0.00001;
/// Weight for free segments - move easily
const FREE_WEIGHT: f64 = 0.00001;
/// Weight for final segments (first/last bend point) - resist movement
const FINAL_WEIGHT: f64 = 0.001;
/// Weight for single-bend connectors - prefer centering
#[allow(dead_code)]
const SINGLE_CONNECTED_WEIGHT: f64 = 1.0;
/// Weight for anchored segments (cannot move, acts as anchor for alignment)
/// Very high weight ensures other segments align to this position
const ANCHOR_WEIGHT: f64 = 100000.0;

// =============================================================================
// Data Structures
// =============================================================================

/// Which dimension a bend point can move in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    /// Can move in X direction (horizontal nudging)
    X,
    /// Can move in Y direction (vertical nudging)
    Y,
}

/// Type of bend pattern for classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BendType {
    /// First or last bend point in path - resists movement
    Final,
    /// S-bend or Z-bend - moves easily, prefers centering
    Zigzag,
    /// Other middle bend points - moves easily
    Free,
    /// Anchored bend point - cannot move, acts as anchor for same-net alignment
    Anchor,
}

/// Information about a segment adjacent to a bend point.
#[derive(Debug, Clone)]
struct SegmentInfo {
    /// Whether this segment is horizontal
    is_horizontal: bool,
    /// Start of segment in variable dimension (perpendicular to orientation)
    var_start: f64,
    /// End of segment in variable dimension
    var_end: f64,
    /// Fixed coordinate of this segment
    fixed_coord: f64,
}

impl SegmentInfo {
    /// Check if this segment overlaps with another.
    ///
    /// Two parallel segments overlap if:
    /// 1. Same orientation (both horizontal or both vertical)
    /// 2. Same or very close fixed coordinate (same Y for horizontal, same X for vertical)
    /// 3. Variable ranges intersect (strict overlap, not just touching at a point)
    fn overlaps(&self, other: &SegmentInfo, tolerance: f64) -> bool {
        if self.is_horizontal != other.is_horizontal {
            return false;
        }
        // Check if the fixed coordinates are close enough
        // If segments are already separated by more than tolerance, they don't overlap
        if (self.fixed_coord - other.fixed_coord).abs() > tolerance {
            return false;
        }
        // Check variable range overlap (strict - segments must actually overlap, not just touch)
        let overlap_start = self.var_start.max(other.var_start);
        let overlap_end = self.var_end.min(other.var_end);
        overlap_start < overlap_end
    }
}

/// A bend point (intermediate vertex) that can be nudged.
#[derive(Debug, Clone)]
struct NudgeBendPoint {
    /// Index of the path this bend point belongs to
    path_idx: usize,
    /// Index of the point in path.points (always 1 to N-1)
    point_idx: usize,
    /// Net ID this bend point belongs to
    net_id: String,
    /// Connector ID for same-connector alignment
    #[allow(dead_code)]
    connector_id: String,

    /// Which dimension this bend point can move in
    nudge_dimension: Dimension,
    /// Current position in the nudge dimension
    current_position: f64,

    /// Minimum allowed position (from obstacles and adjacent points)
    min_limit: f64,
    /// Maximum allowed position (from obstacles and adjacent points)
    max_limit: f64,

    /// Segment from point_idx-1 to point_idx
    segment_before: SegmentInfo,
    /// Segment from point_idx to point_idx+1
    segment_after: SegmentInfo,

    /// Classification for weight assignment
    bend_type: BendType,
}

impl NudgeBendPoint {
    /// Compute the desired position for this bend point.
    /// For zigzag bends, prefer the midpoint of available space (centering).
    fn desired_position(&self) -> f64 {
        // Only center zigzag bends when:
        // 1. The bend type is Zigzag
        // 2. The channel has valid limits (min < max)
        // 3. The channel width is reasonable (not unconstrained)
        let channel_width = self.max_limit - self.min_limit;
        if self.bend_type == BendType::Zigzag
            && self.min_limit < self.max_limit
            && channel_width < MAX_CENTERING_CHANNEL_WIDTH
        {
            (self.min_limit + self.max_limit) / 2.0
        } else {
            self.current_position
        }
    }

    /// Check if this bend point has meaningful channel limits for centering.
    fn has_meaningful_limits(&self) -> bool {
        let channel_width = self.max_limit - self.min_limit;
        self.min_limit < self.max_limit && channel_width < MAX_CENTERING_CHANNEL_WIDTH
    }

    /// Compute the weight for this bend point.
    fn weight(&self) -> f64 {
        match self.bend_type {
            BendType::Final => FINAL_WEIGHT,
            BendType::Zigzag => ZIGZAG_WEIGHT,
            BendType::Free => FREE_WEIGHT,
            BendType::Anchor => ANCHOR_WEIGHT,
        }
    }

    /// Check if this bend point's segments overlap with another's.
    /// Returns true if any segment from self overlaps with any segment from other.
    fn has_overlapping_segments(&self, other: &NudgeBendPoint, tolerance: f64) -> bool {
        // Check all four combinations
        self.segment_before
            .overlaps(&other.segment_before, tolerance)
            || self
                .segment_before
                .overlaps(&other.segment_after, tolerance)
            || self
                .segment_after
                .overlaps(&other.segment_before, tolerance)
            || self.segment_after.overlaps(&other.segment_after, tolerance)
    }
}

// =============================================================================
// Main Entry Point
// =============================================================================

/// Nudge routes to separate overlapping segments from different nets.
///
/// This is the main entry point for route nudging. It:
/// 1. Extracts bend points from all paths
/// 2. Finds overlapping segments that need separation
/// 3. Uses VPSC solver to compute optimal positions
/// 4. Applies adjustments to bend points
pub fn nudge_routes(
    paths: &mut [RoutedPath],
    net_ids: &[String],
    obstacles: &[Obstacle],
    config: &RouterConfig,
) {
    if std::env::var("DEBUG_NUDGING").is_ok() {
        eprintln!("\n[nudging] nudge_routes called with {} paths", paths.len());
        for (i, path) in paths.iter().enumerate() {
            let points: Vec<String> = path
                .points
                .iter()
                .map(|p| format!("({:.2},{:.2})", p.x, p.y))
                .collect();
            eprintln!(
                "[nudging] INPUT path[{}] {}: {} ({} points)",
                i,
                path.connector_id,
                points.join(" -> "),
                path.points.len()
            );
        }
    }

    if paths.is_empty() {
        return;
    }

    let separation = config.ideal_nudging_distance;
    let obstacle_buffer = config.shape_buffer_distance;

    // Build connector_id mapping
    let connector_ids: Vec<String> = paths.iter().map(|p| p.connector_id.clone()).collect();

    // Extract bend points
    let bend_points =
        extract_bend_points(paths, net_ids, &connector_ids, obstacles, obstacle_buffer);
    if bend_points.is_empty() {
        return;
    }

    // Process X-dimension bend points
    let x_points: Vec<_> = bend_points
        .iter()
        .enumerate()
        .filter(|(_, bp)| bp.nudge_dimension == Dimension::X)
        .collect();
    let x_adjustments = compute_adjustments(&x_points, separation);

    // Process Y-dimension bend points
    let y_points: Vec<_> = bend_points
        .iter()
        .enumerate()
        .filter(|(_, bp)| bp.nudge_dimension == Dimension::Y)
        .collect();
    let y_adjustments = compute_adjustments(&y_points, separation);

    // Log bend points and adjustments for debugging same-net alignment
    if std::env::var("DEBUG_NUDGING").is_ok() {
        eprintln!(
            "\n[nudging] === DEBUG: {} paths, {} bend points ===",
            paths.len(),
            bend_points.len()
        );
        for (i, bp) in bend_points.iter().enumerate() {
            let path = &paths[bp.path_idx];
            eprintln!(
                "[nudging] BP[{}]: path={} ({}) point={} {:?} pos={:.1} limits=[{:.1},{:.1}] type={:?}",
                i,
                bp.path_idx,
                path.connector_id,
                bp.point_idx,
                bp.nudge_dimension,
                bp.current_position,
                bp.min_limit,
                bp.max_limit,
                bp.bend_type
            );
        }
        eprintln!("[nudging] X adjustments: {x_adjustments:?}");
        eprintln!("[nudging] Y adjustments: {y_adjustments:?}");
    }

    // Debug: print paths before adjustments
    if std::env::var("DEBUG_NUDGING").is_ok() {
        eprintln!("\n[nudging] === PATHS BEFORE APPLY ===");
        for (i, path) in paths.iter().enumerate() {
            let points: Vec<String> = path
                .points
                .iter()
                .map(|p| format!("({:.2},{:.2})", p.x, p.y))
                .collect();
            eprintln!(
                "[nudging] path[{}] {}: {}",
                i,
                path.connector_id,
                points.join(" -> ")
            );
        }
    }

    // Apply adjustments
    apply_adjustments(paths, &bend_points, &x_adjustments, &y_adjustments);

    // Debug: print paths after adjustments
    if std::env::var("DEBUG_NUDGING").is_ok() {
        eprintln!("\n[nudging] === PATHS AFTER APPLY ===");
        for (i, path) in paths.iter().enumerate() {
            let points: Vec<String> = path
                .points
                .iter()
                .map(|p| format!("({:.2},{:.2})", p.x, p.y))
                .collect();
            let orth = path.is_orthogonal();
            eprintln!(
                "[nudging] path[{}] {}: {} [orth={}]",
                i,
                path.connector_id,
                points.join(" -> "),
                orth
            );
        }
    }
}

// =============================================================================
// Bend Point Extraction
// =============================================================================

/// Extract all nudgeable bend points from paths.
fn extract_bend_points(
    paths: &[RoutedPath],
    net_ids: &[String],
    connector_ids: &[String],
    obstacles: &[Obstacle],
    obstacle_buffer: f64,
) -> Vec<NudgeBendPoint> {
    let mut bend_points = Vec::new();

    for (path_idx, path) in paths.iter().enumerate() {
        let net_id = net_ids
            .get(path_idx)
            .cloned()
            .unwrap_or_else(|| path.connector_id.clone());
        let connector_id = connector_ids
            .get(path_idx)
            .cloned()
            .unwrap_or_else(|| path.connector_id.clone());

        let num_points = path.points.len();

        // Need at least 3 points to have a bend point
        if num_points < 3 {
            continue;
        }

        // Bend points are indices 1 to N-2 (not endpoints)
        for i in 1..num_points - 1 {
            let prev = &path.points[i - 1];
            let curr = &path.points[i];
            let next = &path.points[i + 1];

            // Determine segment orientations
            let seg_before_horizontal = is_horizontal(prev, curr);
            let seg_after_horizontal = is_horizontal(curr, next);

            // Determine nudge dimension based on segment orientations
            // H-V bend: can move in X (affects vertical segment after)
            // V-H bend: can move in Y (affects horizontal segment after)
            let is_next_endpoint = i + 1 == num_points - 1;

            let nudge_dimension = match (seg_before_horizontal, seg_after_horizontal) {
                (true, false) => Dimension::X, // H-V: moving X affects vertical segment after
                (false, true) => Dimension::Y, // V-H: moving Y affects horizontal segment after
                _ => continue,                 // Skip non-orthogonal or same-direction segments
            };

            let current_position = match nudge_dimension {
                Dimension::X => curr.x,
                Dimension::Y => curr.y,
            };

            // Compute segment info
            let segment_before = compute_segment_info(prev, curr);
            let segment_after = compute_segment_info(curr, next);

            // Compute channel limits
            let (mut min_limit, mut max_limit) =
                compute_channel_limits(path, i, nudge_dimension, obstacles, obstacle_buffer);

            // If next point is an endpoint, this bend point is "anchored" - it cannot
            // move because that would require moving the fixed endpoint to maintain
            // orthogonality. However, it still participates in same-net alignment
            // as an anchor that pulls other segments toward it.
            let is_anchored = is_next_endpoint;
            if is_anchored {
                // Anchor to the endpoint's coordinate - this bend cannot move
                let anchor_coord = match nudge_dimension {
                    Dimension::X => next.x,
                    Dimension::Y => next.y,
                };
                min_limit = anchor_coord;
                max_limit = anchor_coord;
            }

            // Determine bend type - anchored bends have very high weight to act as alignment anchors
            let bend_type = if is_anchored {
                BendType::Anchor
            } else {
                classify_bend_type(path, i, num_points)
            };

            bend_points.push(NudgeBendPoint {
                path_idx,
                point_idx: i,
                net_id: net_id.clone(),
                connector_id: connector_id.clone(),
                nudge_dimension,
                current_position,
                min_limit,
                max_limit,
                segment_before,
                segment_after,
                bend_type,
            });
        }
    }

    bend_points
}

/// Check if a segment is horizontal.
fn is_horizontal(p1: &crate::types::Point, p2: &crate::types::Point) -> bool {
    (p1.y - p2.y).abs() < 1e-9
}

/// Compute segment info for overlap detection.
fn compute_segment_info(p1: &crate::types::Point, p2: &crate::types::Point) -> SegmentInfo {
    let is_horizontal = is_horizontal(p1, p2);
    if is_horizontal {
        SegmentInfo {
            is_horizontal: true,
            var_start: p1.x.min(p2.x),
            var_end: p1.x.max(p2.x),
            fixed_coord: p1.y,
        }
    } else {
        SegmentInfo {
            is_horizontal: false,
            var_start: p1.y.min(p2.y),
            var_end: p1.y.max(p2.y),
            fixed_coord: p1.x,
        }
    }
}

/// Maximum channel width for centering to be applied.
/// If the channel is wider than this, we consider it "unconstrained" and don't center.
const MAX_CENTERING_CHANNEL_WIDTH: f64 = 500.0;

/// Compute channel limits for a bend point.
///
/// Limits are computed from:
/// 1. Path endpoints (can't move past start/end)
/// 2. Adjacent points in the path (can't create zero-length or reversed segments)
/// 3. Obstacles (can't move through obstacles, maintaining buffer distance)
fn compute_channel_limits(
    path: &RoutedPath,
    point_idx: usize,
    nudge_dimension: Dimension,
    obstacles: &[Obstacle],
    obstacle_buffer: f64,
) -> (f64, f64) {
    let curr = &path.points[point_idx];
    let current_coord = match nudge_dimension {
        Dimension::X => curr.x,
        Dimension::Y => curr.y,
    };

    // Default: wide open limits
    let mut min_limit = current_coord - 10000.0;
    let mut max_limit = current_coord + 10000.0;

    // Helper to apply a constraint from a point
    let apply_constraint = |coord: f64, current: f64, min: &mut f64, max: &mut f64| {
        if coord < current {
            *min = min.max(coord);
        } else if coord > current {
            *max = max.min(coord);
        }
        // If coord == current, no constraint needed
    };

    // Constraint from path endpoints
    let start_point = &path.points[0];
    let end_point = path.points.last().unwrap();

    match nudge_dimension {
        Dimension::X => {
            apply_constraint(start_point.x, current_coord, &mut min_limit, &mut max_limit);
            apply_constraint(end_point.x, current_coord, &mut min_limit, &mut max_limit);
        }
        Dimension::Y => {
            apply_constraint(start_point.y, current_coord, &mut min_limit, &mut max_limit);
            apply_constraint(end_point.y, current_coord, &mut min_limit, &mut max_limit);
        }
    }

    // Constraint from adjacent points in the path
    // When we nudge point[i], we also update point[i+1]. This affects:
    // - Segment from point[i-1] to point[i] (changes length)
    // - Segment from point[i+1] to point[i+2] (if exists, changes length)
    //
    // We can't reverse a segment's direction, so:
    // - If point[i-1] is at coord C, we can't move past C
    // - If point[i+2] exists and is at coord C, we can't move past C
    let prev_point = &path.points[point_idx - 1];
    let next_next_point = path.points.get(point_idx + 2);

    match nudge_dimension {
        Dimension::X => {
            apply_constraint(prev_point.x, current_coord, &mut min_limit, &mut max_limit);
            if let Some(nnp) = next_next_point {
                apply_constraint(nnp.x, current_coord, &mut min_limit, &mut max_limit);
            }
        }
        Dimension::Y => {
            apply_constraint(prev_point.y, current_coord, &mut min_limit, &mut max_limit);
            if let Some(nnp) = next_next_point {
                apply_constraint(nnp.y, current_coord, &mut min_limit, &mut max_limit);
            }
        }
    }

    // Constraint from obstacles (with buffer to maintain spacing)
    let prev = &path.points[point_idx - 1];
    let next = &path.points[point_idx + 1];

    for obs in obstacles {
        // Use buffered obstacle bounds to maintain spacing
        let obs_min_x = obs.bounds.min_x - obstacle_buffer;
        let obs_max_x = obs.bounds.max_x + obstacle_buffer;
        let obs_min_y = obs.bounds.min_y - obstacle_buffer;
        let obs_max_y = obs.bounds.max_y + obstacle_buffer;

        match nudge_dimension {
            Dimension::X => {
                // Moving in X direction
                // Check if the vertical segment (before or after) would intersect obstacle
                let seg_y_min = curr.y.min(prev.y).min(next.y);
                let seg_y_max = curr.y.max(prev.y).max(next.y);

                // Use non-strict inequality to include edge cases
                if seg_y_max >= obs_min_y && seg_y_min <= obs_max_y {
                    // Y ranges overlap, obstacle constrains X movement
                    if obs_max_x <= current_coord {
                        min_limit = min_limit.max(obs_max_x);
                    } else if obs_min_x >= current_coord {
                        max_limit = max_limit.min(obs_min_x);
                    }
                }
            }
            Dimension::Y => {
                // Moving in Y direction
                let seg_x_min = curr.x.min(prev.x).min(next.x);
                let seg_x_max = curr.x.max(prev.x).max(next.x);

                // Use non-strict inequality to include edge cases
                if seg_x_max >= obs_min_x && seg_x_min <= obs_max_x {
                    // X ranges overlap, obstacle constrains Y movement
                    if obs_max_y <= current_coord {
                        min_limit = min_limit.max(obs_max_y);
                    } else if obs_min_y >= current_coord {
                        max_limit = max_limit.min(obs_min_y);
                    }
                }
            }
        }
    }

    (min_limit, max_limit)
}

/// Classify the bend type for weight assignment.
fn classify_bend_type(path: &RoutedPath, point_idx: usize, num_points: usize) -> BendType {
    // First or last bend point is "final" - resists movement
    if point_idx == 1 || point_idx == num_points - 2 {
        return BendType::Final;
    }

    // Check for zigzag pattern (S-bend or Z-bend)
    // A zigzag occurs when the bend point is between two segments that form a detour
    if num_points >= 4 && point_idx >= 2 && point_idx + 2 < num_points {
        let p0 = &path.points[point_idx - 2];
        let p1 = &path.points[point_idx - 1];
        let p2 = &path.points[point_idx];
        let p3 = &path.points[point_idx + 1];

        // Check if this is part of a zigzag pattern
        let seg_0_1_horizontal = is_horizontal(p0, p1);
        let seg_1_2_horizontal = is_horizontal(p1, p2);
        let seg_2_3_horizontal = is_horizontal(p2, p3);

        // Zigzag: alternating H-V-H or V-H-V
        if seg_0_1_horizontal != seg_1_2_horizontal && seg_1_2_horizontal != seg_2_3_horizontal {
            return BendType::Zigzag;
        }
    }

    BendType::Free
}

// =============================================================================
// Constraint Generation and Solving
// =============================================================================

/// Compute adjustments for bend points in one dimension.
fn compute_adjustments(
    dimension_points: &[(usize, &NudgeBendPoint)],
    separation: f64,
) -> HashMap<usize, f64> {
    if dimension_points.is_empty() {
        return HashMap::new();
    }

    // Find groups of overlapping bend points
    let groups = find_overlapping_groups(dimension_points, separation);

    let mut adjustments = HashMap::new();

    for group in groups {
        if group.len() < 2 {
            // Single bend point - check if it needs centering
            let group_idx = group[0];
            let (bp_idx, bp) = &dimension_points[group_idx];

            if bp.bend_type == BendType::Zigzag && bp.has_meaningful_limits() {
                // Center zigzag bends in their channel
                let desired = bp.desired_position();
                if (desired - bp.current_position).abs() > 1e-6 {
                    adjustments.insert(*bp_idx, desired);
                }
            }
            continue;
        }

        // Create VPSC problem for this group
        let mut solver = IncSolver::new();
        let mut var_indices: Vec<(usize, usize)> = Vec::new(); // (group_idx, var_idx)

        // Add variables for each bend point
        for &group_idx in &group {
            let (_, bp) = &dimension_points[group_idx];
            let desired = bp.desired_position();
            let weight = bp.weight();
            let var_idx = solver.add_variable(desired, weight);
            var_indices.push((group_idx, var_idx));
        }

        // Sort by current position for constraint generation
        var_indices.sort_by(|a, b| {
            let bp_a = &dimension_points[a.0].1;
            let bp_b = &dimension_points[b.0].1;
            bp_a.current_position
                .partial_cmp(&bp_b.current_position)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Add constraints between overlapping segments
        let mut has_any_constraint = false;
        for i in 0..var_indices.len() {
            for j in (i + 1)..var_indices.len() {
                let (group_i, var_i) = var_indices[i];
                let (group_j, var_j) = var_indices[j];
                let (_bp_idx_i, bp_i) = &dimension_points[group_i];
                let (_bp_idx_j, bp_j) = &dimension_points[group_j];

                if !bp_i.has_overlapping_segments(bp_j, separation) {
                    continue;
                }

                if bp_i.net_id == bp_j.net_id {
                    // Same net: align segments using equality constraint
                    // This makes same-net routes merge cleanly without kinks
                    solver.add_equality_constraint(var_i, var_j, 0.0);
                    has_any_constraint = true;
                } else {
                    // Different nets: separate segments
                    solver.add_constraint(var_i, var_j, separation);
                    has_any_constraint = true;
                }
            }
        }

        // Skip if no constraints were added (nothing to solve)
        if !has_any_constraint {
            continue;
        }

        // Solve
        solver.solve();

        // Extract results
        for (group_idx, var_idx) in var_indices {
            let (bp_idx, bp) = &dimension_points[group_idx];
            let new_pos = solver.get_position(var_idx);
            let clamped = new_pos.clamp(bp.min_limit, bp.max_limit);
            if (clamped - bp.current_position).abs() > 1e-6 {
                adjustments.insert(*bp_idx, clamped);
            }
        }
    }

    adjustments
}

/// Find groups of overlapping bend points.
fn find_overlapping_groups(
    dimension_points: &[(usize, &NudgeBendPoint)],
    separation: f64,
) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut assigned = vec![false; dimension_points.len()];

    for i in 0..dimension_points.len() {
        if assigned[i] {
            continue;
        }

        let mut group = vec![i];
        assigned[i] = true;

        // Find all bend points that overlap with any in the group
        let mut changed = true;
        while changed {
            changed = false;
            for j in 0..dimension_points.len() {
                if assigned[j] {
                    continue;
                }
                // Check if j overlaps with any in the group
                for &g in &group {
                    let (_, bp_g) = &dimension_points[g];
                    let (_, bp_j) = &dimension_points[j];
                    if bp_j.has_overlapping_segments(bp_g, separation) {
                        group.push(j);
                        assigned[j] = true;
                        changed = true;
                        break;
                    }
                }
            }
        }

        groups.push(group);
    }

    groups
}

// =============================================================================
// Adjustment Application
// =============================================================================

/// Apply computed adjustments to bend points.
///
/// This is the key function that maintains path integrity. When we move a
/// bend point, we must also update the adjacent point to maintain the
/// orthogonal segment between them.
///
/// For an H-V bend (horizontal before, vertical after):
/// - Moving in X affects the vertical segment after
/// - We must update BOTH the bend point AND the next point to keep segment vertical
///
/// For a V-H bend (vertical before, horizontal after):
/// - Moving in Y affects the horizontal segment after
/// - We must update BOTH the bend point AND the next point to keep segment horizontal
fn apply_adjustments(
    paths: &mut [RoutedPath],
    bend_points: &[NudgeBendPoint],
    x_adjustments: &HashMap<usize, f64>,
    y_adjustments: &HashMap<usize, f64>,
) {
    for (bp_idx, bp) in bend_points.iter().enumerate() {
        let new_pos = match bp.nudge_dimension {
            Dimension::X => x_adjustments.get(&bp_idx).copied(),
            Dimension::Y => y_adjustments.get(&bp_idx).copied(),
        };

        if let Some(pos) = new_pos {
            let path = &mut paths[bp.path_idx];
            let num_points = path.points.len();

            match bp.nudge_dimension {
                Dimension::X => {
                    // H-V bend: moving X affects the vertical segment AFTER the bend
                    // Update both the bend point and the next point to maintain verticality
                    path.points[bp.point_idx].x = pos;

                    // Also update the next point (other end of vertical segment)
                    // unless it's a fixed endpoint
                    if bp.point_idx + 1 < num_points - 1 {
                        path.points[bp.point_idx + 1].x = pos;
                    }
                }
                Dimension::Y => {
                    // V-H bend: moving Y affects the horizontal segment AFTER the bend
                    // Update both the bend point and the next point to maintain horizontality
                    path.points[bp.point_idx].y = pos;

                    // Also update the next point (other end of horizontal segment)
                    // unless it's a fixed endpoint
                    if bp.point_idx + 1 < num_points - 1 {
                        path.points[bp.point_idx + 1].y = pos;
                    }
                }
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Point;

    #[test]
    fn test_extract_bend_points_simple() {
        // Simple L-shaped path: (0,0) -> (50,0) -> (50,100)
        // This path has only 3 points. The bend at index 1 is an H-V bend.
        // Since next point (index 2) is an endpoint, this bend is "anchored" -
        // it participates in same-net alignment but cannot actually move.
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(50.0, 0.0),
                Point::new(50.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let connector_ids = vec!["c1".to_string()];

        let bend_points = extract_bend_points(&paths, &net_ids, &connector_ids, &[], 7.0);

        // One anchored bend point at index 1
        assert_eq!(bend_points.len(), 1);
        assert_eq!(bend_points[0].point_idx, 1);
        assert_eq!(bend_points[0].nudge_dimension, Dimension::X);
        // Anchored: limits are equal to endpoint's X coordinate (50.0)
        assert_eq!(bend_points[0].min_limit, 50.0);
        assert_eq!(bend_points[0].max_limit, 50.0);
        assert_eq!(bend_points[0].bend_type, BendType::Anchor);
    }

    #[test]
    fn test_extract_bend_points_z_shape() {
        // Z-shaped path: (0,0) -> (50,0) -> (50,50) -> (100,50)
        // Point 1 (H-V): next point is 2 (not endpoint) → CAN nudge X
        // Point 2 (V-H): next point is 3 (endpoint) → anchored (participates but can't move)
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(50.0, 0.0),
                Point::new(50.0, 50.0),
                Point::new(100.0, 50.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let connector_ids = vec!["c1".to_string()];

        let bend_points = extract_bend_points(&paths, &net_ids, &connector_ids, &[], 7.0);

        // 2 bend points: point 1 can move, point 2 is anchored
        assert_eq!(bend_points.len(), 2);

        // First bend point at index 1: H-V transition, can move X
        assert_eq!(bend_points[0].point_idx, 1);
        assert_eq!(bend_points[0].nudge_dimension, Dimension::X);

        // Second bend point at index 2: V-H transition, anchored at Y=50
        assert_eq!(bend_points[1].point_idx, 2);
        assert_eq!(bend_points[1].nudge_dimension, Dimension::Y);
        assert_eq!(bend_points[1].min_limit, 50.0);
        assert_eq!(bend_points[1].max_limit, 50.0);
    }

    #[test]
    fn test_extract_bend_points_5_point_path() {
        // 5-point S-shaped path: (0,0) -> (50,0) -> (50,50) -> (100,50) -> (100,100)
        // Point 1 (H-V): next is 2 (not endpoint) → CAN nudge X
        // Point 2 (V-H): next is 3 (not endpoint) → CAN nudge Y
        // Point 3 (H-V): next is 4 (endpoint) → anchored at X=100
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(50.0, 0.0),
                Point::new(50.0, 50.0),
                Point::new(100.0, 50.0),
                Point::new(100.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let connector_ids = vec!["c1".to_string()];

        let bend_points = extract_bend_points(&paths, &net_ids, &connector_ids, &[], 7.0);

        // 3 bend points: points 1 and 2 can move, point 3 is anchored
        assert_eq!(bend_points.len(), 3);

        // First bend point at index 1: H-V transition, can move X
        assert_eq!(bend_points[0].point_idx, 1);
        assert_eq!(bend_points[0].nudge_dimension, Dimension::X);

        // Second bend point at index 2: V-H transition, can move Y
        assert_eq!(bend_points[1].point_idx, 2);
        assert_eq!(bend_points[1].nudge_dimension, Dimension::Y);

        // Third bend point at index 3: H-V transition, anchored at X=100
        assert_eq!(bend_points[2].point_idx, 3);
        assert_eq!(bend_points[2].nudge_dimension, Dimension::X);
        assert_eq!(bend_points[2].min_limit, 100.0);
        assert_eq!(bend_points[2].max_limit, 100.0);
    }

    #[test]
    fn test_nudge_preserves_orthogonality() {
        // Test that nudging preserves orthogonality
        let mut paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(50.0, 0.0),
                Point::new(50.0, 100.0),
                Point::new(100.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let config = RouterConfig::default();

        assert!(
            paths[0].is_orthogonal(),
            "Path should be orthogonal before nudging"
        );

        nudge_routes(&mut paths, &net_ids, &[], &config);

        assert!(
            paths[0].is_orthogonal(),
            "Path should remain orthogonal after nudging. Points: {:?}",
            paths[0].points
        );
    }

    #[test]
    fn test_nudge_separates_different_nets() {
        // Two paths with overlapping segments from different nets
        let mut paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(50.0, 0.0),
                    Point::new(50.0, 100.0),
                    Point::new(100.0, 100.0),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(0.0, 10.0),
                    Point::new(50.0, 10.0),
                    Point::new(50.0, 90.0),
                    Point::new(100.0, 90.0),
                ],
            ),
        ];
        let net_ids = vec!["net1".to_string(), "net2".to_string()];
        let config = RouterConfig::default();

        let initial_x1 = paths[0].points[1].x;
        let initial_x2 = paths[1].points[1].x;

        println!("Before nudging:");
        println!("  Path 0: {:?}", paths[0].points);
        println!("  Path 1: {:?}", paths[1].points);

        nudge_routes(&mut paths, &net_ids, &[], &config);

        println!("After nudging:");
        println!("  Path 0: {:?}", paths[0].points);
        println!("  Path 1: {:?}", paths[1].points);

        // Both paths should remain orthogonal
        assert!(
            paths[0].is_orthogonal(),
            "Path 0 should be orthogonal: {:?}",
            paths[0].points
        );
        assert!(
            paths[1].is_orthogonal(),
            "Path 1 should be orthogonal: {:?}",
            paths[1].points
        );

        // The vertical segments should be separated
        let x1 = paths[0].points[1].x;
        let x2 = paths[1].points[1].x;
        let separation = (x1 - x2).abs();

        assert!(
            separation >= config.ideal_nudging_distance - 0.1
                || (x1 - initial_x1).abs() > 0.1
                || (x2 - initial_x2).abs() > 0.1,
            "Segments should be separated. X1={}, X2={}, separation={}",
            x1,
            x2,
            separation
        );
    }

    #[test]
    fn test_segment_overlap_detection() {
        let tolerance = 10.0; // Typical separation distance

        // seg1 and seg2 at same fixed_coord (50.0), overlapping var range
        let seg1 = SegmentInfo {
            is_horizontal: true,
            var_start: 0.0,
            var_end: 100.0,
            fixed_coord: 50.0,
        };
        let seg2 = SegmentInfo {
            is_horizontal: true,
            var_start: 50.0,
            var_end: 150.0,
            fixed_coord: 50.0,
        };
        // seg3 at same fixed_coord but non-overlapping var range
        let seg3 = SegmentInfo {
            is_horizontal: true,
            var_start: 110.0,
            var_end: 200.0,
            fixed_coord: 50.0,
        };
        // seg4 at different fixed_coord (far away) - should NOT overlap
        let seg4 = SegmentInfo {
            is_horizontal: true,
            var_start: 50.0,
            var_end: 150.0,
            fixed_coord: 100.0, // 50 units away, more than tolerance
        };

        assert!(
            seg1.overlaps(&seg2, tolerance),
            "Same-line overlapping segments should be detected"
        );
        assert!(
            !seg1.overlaps(&seg3, tolerance),
            "Non-overlapping var ranges should not overlap"
        );
        assert!(
            !seg1.overlaps(&seg4, tolerance),
            "Segments on different lines should not overlap"
        );
    }

    #[test]
    fn test_channel_limits_prevent_endpoint_collision() {
        // Path where bend point could theoretically collapse onto endpoint.
        // Path: (0,0) -> (50,0) -> (50,50) -> (100,50) -> (100,100)
        // Point 1 at (50,0) is H-V bend, can nudge X
        // Point 2 at (50,50) is V-H bend, can nudge Y
        // Point 3 at (100,50) is H-V bend, anchored (next is endpoint)
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),     // Fixed endpoint (index 0)
                Point::new(50.0, 0.0),    // Bend point (index 1) - can nudge X
                Point::new(50.0, 50.0),   // Bend point (index 2) - can nudge Y
                Point::new(100.0, 50.0),  // Bend point (index 3) - anchored
                Point::new(100.0, 100.0), // Fixed endpoint (index 4)
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let connector_ids = vec!["c1".to_string()];

        let bend_points = extract_bend_points(&paths, &net_ids, &connector_ids, &[], 7.0);

        // Should have 3 bend points: indices 1 and 2 can move, index 3 is anchored
        assert_eq!(bend_points.len(), 3);
        let bp = &bend_points[0];

        // First bend point should be at index 1, nudging X
        assert_eq!(bp.point_idx, 1);
        assert_eq!(bp.nudge_dimension, Dimension::X);

        // min_limit should prevent moving past the start endpoint (x=0)
        assert!(
            bp.min_limit >= 0.0,
            "min_limit {} should not allow moving past endpoint at x=0",
            bp.min_limit
        );
    }

    #[test]
    fn test_same_net_alignment() {
        // Two paths from the same net with overlapping vertical segments
        // They should be aligned (moved to same X position)
        let mut paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(50.0, 0.0),
                    Point::new(50.0, 100.0),
                    Point::new(100.0, 100.0),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(0.0, 20.0),
                    Point::new(55.0, 20.0), // Slightly different X than path 1
                    Point::new(55.0, 80.0),
                    Point::new(100.0, 80.0),
                ],
            ),
        ];
        // Same net for both connectors
        let net_ids = vec!["net1".to_string(), "net1".to_string()];
        let config = RouterConfig::default();

        println!("Before nudging:");
        println!("  Path 0 X at bend: {}", paths[0].points[1].x);
        println!("  Path 1 X at bend: {}", paths[1].points[1].x);

        nudge_routes(&mut paths, &net_ids, &[], &config);

        println!("After nudging:");
        println!("  Path 0 X at bend: {}", paths[0].points[1].x);
        println!("  Path 1 X at bend: {}", paths[1].points[1].x);

        // Both paths should remain orthogonal
        assert!(paths[0].is_orthogonal(), "Path 0 should be orthogonal");
        assert!(paths[1].is_orthogonal(), "Path 1 should be orthogonal");

        // Same-net segments should be aligned (same X position)
        let x1 = paths[0].points[1].x;
        let x2 = paths[1].points[1].x;
        let diff = (x1 - x2).abs();

        assert!(
            diff < 1.0,
            "Same-net segments should be aligned. X1={}, X2={}, diff={}",
            x1,
            x2,
            diff
        );
    }

    #[test]
    fn test_zigzag_centering() {
        // A 6-point path with a zigzag in the middle that should be centered
        // Path: (0,0) -> (20,0) -> (20,50) -> (80,50) -> (80,100) -> (100,100)
        //
        // Point 2 at (20,50) is a V-H bend, can nudge Y
        // The channel for Y is constrained by:
        // - prev_point at (20,0): Y >= 0
        // - next_next_point at (80,100): Y <= 100
        // So channel is [0, 100], center at 50
        //
        // Since it's already at 50, no centering needed... let's offset it
        let mut paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(20.0, 0.0),
                Point::new(20.0, 30.0), // Off-center at Y=30 (should move to ~50)
                Point::new(80.0, 30.0),
                Point::new(80.0, 100.0),
                Point::new(100.0, 100.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let config = RouterConfig::default();

        let initial_y = paths[0].points[2].y;
        assert_eq!(initial_y, 30.0, "Initial Y should be 30");

        nudge_routes(&mut paths, &net_ids, &[], &config);

        // Path should remain orthogonal
        assert!(
            paths[0].is_orthogonal(),
            "Path should be orthogonal after nudging"
        );

        // The zigzag segment should be centered
        // Channel is [0, 100], center is 50
        let new_y = paths[0].points[2].y;
        assert!(
            (new_y - 50.0).abs() < 1.0,
            "Zigzag should be centered around Y=50, got Y={}",
            new_y
        );

        // Both points 2 and 3 should be updated together
        assert!(
            (paths[0].points[2].y - paths[0].points[3].y).abs() < 0.001,
            "Points 2 and 3 should have same Y after nudging"
        );
    }

    #[test]
    fn test_zigzag_centering_with_obstacles() {
        use crate::types::{Obstacle, Rect};

        // A path with a zigzag that should be centered between obstacles
        // Path goes around an obstacle, creating a detour
        let mut paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 50.0),
                Point::new(40.0, 50.0),
                Point::new(40.0, 20.0), // Zigzag - should center between 0 and obstacle
                Point::new(160.0, 20.0),
                Point::new(160.0, 50.0),
                Point::new(200.0, 50.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];

        // Add an obstacle that constrains the zigzag
        let obstacles = vec![Obstacle::new("obs1", Rect::new(50.0, 30.0, 150.0, 70.0))];

        let config = RouterConfig::default();

        println!("Before nudging: Y = {}", paths[0].points[2].y);
        nudge_routes(&mut paths, &net_ids, &obstacles, &config);
        println!("After nudging: Y = {}", paths[0].points[2].y);

        // Path should remain orthogonal
        assert!(
            paths[0].is_orthogonal(),
            "Path should be orthogonal after nudging"
        );

        // The zigzag Y should be between 0 and the obstacle (30)
        // Center would be around 15
        let new_y = paths[0].points[2].y;
        assert!(
            new_y < 30.0,
            "Zigzag Y={} should be below obstacle at Y=30",
            new_y
        );
    }

    #[test]
    fn test_channel_limits_from_adjacent_points() {
        // Test that channel limits are correctly computed from adjacent points
        // Path: (0,0) -> (0,50) -> (30,50) -> (30,0) -> (60,0)
        // Point 1 at (0,50) is V-H, nudges Y
        // Point 2 at (30,50) is H-V, nudges X
        // Point 3 at (30,0) is V-H, anchored (next is endpoint)
        let paths = vec![RoutedPath::new(
            "c1",
            vec![
                Point::new(0.0, 0.0),
                Point::new(0.0, 50.0),
                Point::new(30.0, 50.0),
                Point::new(30.0, 0.0),
                Point::new(60.0, 0.0),
            ],
        )];
        let net_ids = vec!["net1".to_string()];
        let connector_ids = vec!["c1".to_string()];

        let bend_points = extract_bend_points(&paths, &net_ids, &connector_ids, &[], 7.0);

        // Should have 3 bend points: 1 and 2 can move, 3 is anchored
        assert_eq!(bend_points.len(), 3);

        // Point 1 nudges Y, should be constrained by:
        // - prev_point (0,0): Y >= 0
        // - next_next_point (30,0): Y >= 0 (both below current Y=50)
        // So min_limit should be 0
        let bp1 = &bend_points[0];
        assert_eq!(bp1.nudge_dimension, Dimension::Y);
        assert!(
            bp1.min_limit >= 0.0,
            "min_limit {} should be >= 0 from adjacent point",
            bp1.min_limit
        );

        // Point 2 nudges X, should be constrained by:
        // - prev_point (0,50): X >= 0
        // - next_next_point (60,0): X <= 60
        // So limits should be [0, 60]
        let bp2 = &bend_points[1];
        assert_eq!(bp2.nudge_dimension, Dimension::X);
        assert!(
            bp2.min_limit >= 0.0,
            "min_limit {} should be >= 0",
            bp2.min_limit
        );
        assert!(
            bp2.max_limit <= 60.0,
            "max_limit {} should be <= 60",
            bp2.max_limit
        );

        // Point 3 is anchored at Y=0 (endpoint's Y)
        let bp3 = &bend_points[2];
        assert_eq!(bp3.nudge_dimension, Dimension::Y);
        assert_eq!(bp3.min_limit, 0.0);
        assert_eq!(bp3.max_limit, 0.0);
    }

    // =========================================================================
    // Different-Net Separation Tests
    // =========================================================================

    /// Helper to check if two paths have overlapping segments (not just crossing)
    fn paths_have_overlapping_segment(path1: &RoutedPath, path2: &RoutedPath) -> bool {
        for i in 0..path1.points.len().saturating_sub(1) {
            let seg1_start = &path1.points[i];
            let seg1_end = &path1.points[i + 1];

            for j in 0..path2.points.len().saturating_sub(1) {
                let seg2_start = &path2.points[j];
                let seg2_end = &path2.points[j + 1];

                // Check if segments are collinear and overlapping
                let seg1_horiz = (seg1_start.y - seg1_end.y).abs() < 1e-6;
                let seg2_horiz = (seg2_start.y - seg2_end.y).abs() < 1e-6;

                if seg1_horiz && seg2_horiz {
                    // Both horizontal, check if same Y and overlapping X
                    if (seg1_start.y - seg2_start.y).abs() < 1e-6 {
                        let x1_min = seg1_start.x.min(seg1_end.x);
                        let x1_max = seg1_start.x.max(seg1_end.x);
                        let x2_min = seg2_start.x.min(seg2_end.x);
                        let x2_max = seg2_start.x.max(seg2_end.x);
                        if x1_max > x2_min && x2_max > x1_min {
                            return true;
                        }
                    }
                } else if !seg1_horiz && !seg2_horiz {
                    // Both vertical, check if same X and overlapping Y
                    if (seg1_start.x - seg2_start.x).abs() < 1e-6 {
                        let y1_min = seg1_start.y.min(seg1_end.y);
                        let y1_max = seg1_start.y.max(seg1_end.y);
                        let y2_min = seg2_start.y.min(seg2_end.y);
                        let y2_max = seg2_start.y.max(seg2_end.y);
                        if y1_max > y2_min && y2_max > y1_min {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    #[test]
    fn test_different_nets_overlapping_vertical_segments() {
        // Two paths with overlapping vertical segments at X=50
        // Path 1: (0,0) -> (50,0) -> (50,100) -> (100,100)
        // Path 2: (0,20) -> (50,20) -> (50,80) -> (100,80)
        // The vertical segments from (50,0)-(50,100) and (50,20)-(50,80) overlap
        let mut paths = vec![
            RoutedPath::new(
                "c1",
                vec![
                    Point::new(0.0, 0.0),
                    Point::new(50.0, 0.0),
                    Point::new(50.0, 100.0),
                    Point::new(100.0, 100.0),
                ],
            ),
            RoutedPath::new(
                "c2",
                vec![
                    Point::new(0.0, 20.0),
                    Point::new(50.0, 20.0),
                    Point::new(50.0, 80.0),
                    Point::new(100.0, 80.0),
                ],
            ),
        ];
        let net_ids = vec!["net1".to_string(), "net2".to_string()];
        let config = RouterConfig::default();

        println!("Before nudging:");
        println!("  Path 0 vertical at X={}", paths[0].points[1].x);
        println!("  Path 1 vertical at X={}", paths[1].points[1].x);
        assert!(
            paths_have_overlapping_segment(&paths[0], &paths[1]),
            "Should have overlapping segments before nudging"
        );

        nudge_routes(&mut paths, &net_ids, &[], &config);

        println!("After nudging:");
        println!("  Path 0 vertical at X={}", paths[0].points[1].x);
        println!("  Path 1 vertical at X={}", paths[1].points[1].x);

        // Paths should be orthogonal
        assert!(paths[0].is_orthogonal(), "Path 0 should be orthogonal");
        assert!(paths[1].is_orthogonal(), "Path 1 should be orthogonal");

        // Segments should no longer overlap
        assert!(
            !paths_have_overlapping_segment(&paths[0], &paths[1]),
            "Different-net paths should NOT have overlapping segments after nudging. \
             Path 0 X={}, Path 1 X={}",
            paths[0].points[1].x,
            paths[1].points[1].x
        );

        // Vertical segments should be separated by at least the nudging distance
        let x1 = paths[0].points[1].x;
        let x2 = paths[1].points[1].x;
        let separation = (x1 - x2).abs();
        assert!(
            separation >= config.ideal_nudging_distance - 0.1,
            "Vertical segments should be separated by at least {}. Got {}",
            config.ideal_nudging_distance,
            separation
        );
    }
}
