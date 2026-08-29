//! A* pathfinding for orthogonal routing.
//!
//! This module implements A* search through the visibility graph to find
//! optimal orthogonal paths between ports.
//!
//! ## Cost Function
//!
//! The cost of a path is:
//! ```text
//! cost = distance + num_bends * segment_penalty
//! ```
//!
//! ## Heuristic
//!
//! The heuristic estimates the remaining cost as:
//! ```text
//! h = manhattan_distance + estimated_bends * segment_penalty
//! ```

use crate::config::RouterConfig;
use crate::segment::{BendPointRegistry, Segment, SegmentRegistry};
use crate::types::{Direction, ExistingRouteSegment, Point};
use crate::visibility::{Edge, VertexId, VisibilityGraph};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A node in the A* search.
#[derive(Debug, Clone)]
struct AStarNode {
    /// Current vertex ID.
    vertex_id: VertexId,
    /// Cost from start to this node.
    g_cost: f64,
    /// Estimated total cost (g + h).
    f_cost: f64,
    /// Direction we arrived from (None for start node).
    direction: Option<Direction>,
    /// Previous node for path reconstruction (used for debugging).
    #[allow(dead_code)]
    prev: Option<VertexId>,
    /// Timestamp for deterministic tie-breaking (lower = earlier = higher priority).
    /// This ensures consistent path selection when f_costs are equal.
    timestamp: u64,
}

impl PartialEq for AStarNode {
    fn eq(&self, other: &Self) -> bool {
        self.vertex_id == other.vertex_id
    }
}

impl Eq for AStarNode {}

impl PartialOrd for AStarNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for AStarNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap (lower f_cost = higher priority)
        // Use timestamp as tie-breaker for determinism (earlier nodes first)
        match other.f_cost.partial_cmp(&self.f_cost) {
            Some(Ordering::Equal) | None => {
                // Tie-breaker: lower timestamp = higher priority (reverse for min-heap)
                other.timestamp.cmp(&self.timestamp)
            }
            Some(ord) => ord,
        }
    }
}

/// Result of pathfinding.
#[derive(Debug)]
pub struct PathResult {
    /// The found path as a sequence of points.
    pub points: Vec<Point>,
    /// Total cost of the path.
    pub cost: f64,
    /// Number of bends in the path.
    pub bend_count: usize,
}

/// Context for net-aware pathfinding.
#[derive(Debug, Clone)]
pub struct NetAwareContext<'a> {
    /// Registry of existing route segments by net.
    pub registry: &'a SegmentRegistry,
    /// Registry of existing bend points by net.
    pub bend_registry: &'a BendPointRegistry,
    /// Net ID of the connector being routed.
    pub net_id: &'a str,
    /// Fixed segments that existed before this routing run.
    pub existing_segments: &'a [ExistingRouteSegment],
}

/// A* pathfinder for the visibility graph.
pub struct Pathfinder<'a> {
    graph: &'a VisibilityGraph,
    config: &'a RouterConfig,
}

impl<'a> Pathfinder<'a> {
    pub fn new(graph: &'a VisibilityGraph, config: &'a RouterConfig) -> Self {
        Self { graph, config }
    }

    /// Find a path between two ports.
    pub fn find_path(&self, source_port_id: &str, target_port_id: &str) -> Option<PathResult> {
        self.find_path_with_context(source_port_id, target_port_id, None)
    }

    /// Find a path between two ports with net-aware routing.
    ///
    /// When `net_context` is provided, the pathfinder will:
    /// - Penalize overlapping with segments from different nets
    /// - Give a large bonus (near-zero cost) for overlapping with segments from the same net
    pub fn find_path_with_context(
        &self,
        source_port_id: &str,
        target_port_id: &str,
        net_context: Option<NetAwareContext<'_>>,
    ) -> Option<PathResult> {
        let start_id = self.graph.get_port_vertex(source_port_id)?;
        let goal_id = self.graph.get_port_vertex(target_port_id)?;

        let start_vertex = self.graph.get_vertex(start_id)?;
        let goal_vertex = self.graph.get_vertex(goal_id)?;

        self.find_path_between_vertices(
            start_id,
            goal_id,
            &start_vertex.position,
            &goal_vertex.position,
            net_context,
        )
    }

    /// Find a path from source to target, preferring to follow existing same-net segments.
    ///
    /// This is used for net-aware routing where later routes should
    /// follow earlier routes from the same net. The cost of following an existing
    /// same-net segment is nearly zero, so A* will naturally find paths that
    /// overlap with the existing routes.
    ///
    /// # Arguments
    /// * `source_port_id` - Starting port ID
    /// * `target_port_id` - Goal port ID
    /// * `existing_segments` - SegmentRegistry containing existing routes for this net
    /// * `bend_registry` - BendPointRegistry for bend point tracking
    /// * `net_id` - Net ID for same-net bonus
    ///
    /// # Returns
    /// A PathResult that reaches the target, preferring to overlap with existing same-net segments.
    pub fn find_path_to_net(
        &self,
        source_port_id: &str,
        target_port_id: &str,
        existing_segments: &SegmentRegistry,
        bend_registry: &BendPointRegistry,
        net_id: &str,
    ) -> Option<PathResult> {
        let net_context = NetAwareContext {
            registry: existing_segments,
            bend_registry,
            net_id,
            existing_segments: &[],
        };
        self.find_path_with_context(source_port_id, target_port_id, Some(net_context))
    }

    /// Find a path between two vertices.
    fn find_path_between_vertices(
        &self,
        start_id: VertexId,
        goal_id: VertexId,
        start_pos: &Point,
        goal_pos: &Point,
        net_context: Option<NetAwareContext<'_>>,
    ) -> Option<PathResult> {
        // Priority queue (min-heap by f_cost)
        let mut open_set = BinaryHeap::new();

        // Best g_cost found for each (vertex, direction) pair
        // We track direction because the cost depends on whether we bend
        let mut g_scores: HashMap<(VertexId, Option<Direction>), f64> = HashMap::new();

        // Track how we reached each node for path reconstruction
        let mut came_from: HashMap<(VertexId, Option<Direction>), (VertexId, Option<Direction>)> =
            HashMap::new();

        // Cache for lazy-computed edges (only used when graph.is_lazy())
        let mut edge_cache: HashMap<VertexId, Vec<Edge>> = HashMap::new();

        // Timestamp counter for deterministic tie-breaking (matching libavoid's approach)
        let mut timestamp: u64 = 0;

        // Initialize with start node
        let h_start = self.heuristic(start_pos, goal_pos, None);
        open_set.push(AStarNode {
            vertex_id: start_id,
            g_cost: 0.0,
            f_cost: h_start,
            direction: None,
            prev: None,
            timestamp,
        });
        timestamp += 1;
        g_scores.insert((start_id, None), 0.0);

        // Limit iterations to prevent very slow searches
        const MAX_ITERATIONS: usize = 10_000;
        let mut iterations = 0;

        while let Some(current) = open_set.pop() {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                log::warn!(
                    "[pathfinder] Max iterations ({}) reached searching from {:?} to {:?}",
                    MAX_ITERATIONS,
                    start_pos,
                    goal_pos
                );
                return None;
            }

            // Check if we reached the goal
            if current.vertex_id == goal_id {
                return Some(self.reconstruct_path(
                    &came_from,
                    (current.vertex_id, current.direction),
                    current.g_cost,
                ));
            }

            let current_key = (current.vertex_id, current.direction);

            // Skip if we've already found a better path to this state
            if let Some(&best_g) = g_scores.get(&current_key) {
                if current.g_cost > best_g + 1e-9 {
                    continue;
                }
            }

            // Get current vertex position for net-aware cost calculation
            let current_pos = self
                .graph
                .get_vertex(current.vertex_id)
                .map(|v| v.position)
                .unwrap_or(*start_pos);

            // Explore neighbors (lazy with caching, or pre-computed edges)
            let edges: &[Edge] = if self.graph.is_lazy() {
                edge_cache
                    .entry(current.vertex_id)
                    .or_insert_with(|| self.graph.compute_edges_lazy(current.vertex_id))
            } else {
                self.graph.get_edges(current.vertex_id)
            };
            for edge in edges {
                let neighbor_id = edge.to;
                let neighbor_vertex = match self.graph.get_vertex(neighbor_id) {
                    Some(v) => v,
                    None => continue,
                };

                // Calculate the cost to reach this neighbor
                let edge_cost = self.edge_cost_with_context(
                    &current,
                    edge,
                    &current_pos,
                    &neighbor_vertex.position,
                    &net_context,
                );
                if !edge_cost.is_finite() {
                    continue;
                }
                let tentative_g = current.g_cost + edge_cost;

                let neighbor_key = (neighbor_id, Some(edge.direction));

                // Check if this is a better path
                let is_better = match g_scores.get(&neighbor_key) {
                    Some(&best_g) => tentative_g < best_g - 1e-9,
                    None => true,
                };

                if is_better {
                    g_scores.insert(neighbor_key, tentative_g);
                    came_from.insert(neighbor_key, current_key);

                    let h =
                        self.heuristic(&neighbor_vertex.position, goal_pos, Some(edge.direction));
                    let f = tentative_g + h;

                    open_set.push(AStarNode {
                        vertex_id: neighbor_id,
                        g_cost: tentative_g,
                        f_cost: f,
                        direction: Some(edge.direction),
                        prev: Some(current.vertex_id),
                        timestamp,
                    });
                    timestamp += 1;
                }
            }
        }

        // No path found
        None
    }

    /// Calculate the cost of traversing an edge with net-aware adjustments.
    fn edge_cost_with_context(
        &self,
        from_node: &AStarNode,
        edge: &Edge,
        from_pos: &Point,
        to_pos: &Point,
        net_context: &Option<NetAwareContext<'_>>,
    ) -> f64 {
        let mut cost = edge.distance;

        // Add bend penalty if direction changed
        let is_bend = if let Some(prev_dir) = from_node.direction {
            if prev_dir != edge.direction {
                // Check if it's a 180° turn (doubling back)
                if prev_dir.opposite() == edge.direction {
                    cost += 2.0 * self.config.segment_penalty;
                } else {
                    cost += self.config.segment_penalty;
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        // Add net-aware penalties/bonuses if context is provided
        if let Some(ctx) = net_context {
            // Check segment overlap with existing segments
            if let Some(segment) = Segment::from_points(from_pos, to_pos) {
                if path_segment_conflicts_with_existing_segments(
                    from_pos,
                    to_pos,
                    ctx.net_id,
                    ctx.existing_segments,
                ) {
                    log::trace!(
                        "[pathfinder] Existing-segment conflict for '{}': blocking edge ({:.1},{:.1}) -> ({:.1},{:.1})",
                        ctx.net_id,
                        from_pos.x,
                        from_pos.y,
                        to_pos.x,
                        to_pos.y
                    );
                    return f64::INFINITY;
                }

                // Check for same-net overlap first (bonus - use near-zero cost)
                // Only apply if same_net_coalescing is enabled
                let same_net_overlap = self.config.same_net_coalescing
                    && ctx.registry.overlaps_same_net(&segment, ctx.net_id);
                if same_net_overlap {
                    // Near-zero cost for following existing same-net segments
                    // This makes A* strongly prefer paths that overlap with the "trunk"
                    // Using 0.001 * distance maintains A* admissibility while giving huge preference
                    let original_cost = cost;
                    cost = 0.001 * edge.distance;
                    log::trace!(
                        "[pathfinder] Same-net overlap for '{}': cost {} -> {} (following trunk)",
                        ctx.net_id,
                        original_cost,
                        cost
                    );
                } else {
                    // Check for different-net overlap (penalty)
                    let different_net_overlap =
                        ctx.registry.overlaps_different_net(&segment, ctx.net_id);
                    if different_net_overlap {
                        // Heavy penalty for overlapping with different nets
                        log::trace!(
                            "[pathfinder] Different-net overlap for '{}': adding penalty {}",
                            ctx.net_id,
                            self.config.different_net_overlap_penalty
                        );
                        cost += self.config.different_net_overlap_penalty;
                    } else if self.config.grid_snap_size > 0.0 {
                        // Check for different-net proximity (lighter penalty)
                        let near_different_net = ctx.registry.near_different_net(
                            &segment,
                            ctx.net_id,
                            self.config.grid_snap_size,
                        );
                        if near_different_net {
                            log::trace!(
                                "[pathfinder] Different-net proximity for '{}': adding penalty {}",
                                ctx.net_id,
                                self.config.different_net_proximity_penalty
                            );
                            cost += self.config.different_net_proximity_penalty;
                        }
                    }
                }
            }

            // Check if we're bending at a vertex that's already a bend point for another net
            if is_bend
                && ctx
                    .bend_registry
                    .is_bend_point_for_different_net(from_pos, ctx.net_id)
            {
                // Heavy penalty for sharing a bend point with a different net
                log::trace!(
                    "[pathfinder] Bend point conflict for net '{}' at ({:.1}, {:.1}): adding penalty {}",
                    ctx.net_id,
                    from_pos.x,
                    from_pos.y,
                    self.config.different_net_bend_penalty
                );
                cost += self.config.different_net_bend_penalty;
            }
        }

        cost
    }

    /// Heuristic function: estimated cost from current position to goal.
    fn heuristic(&self, current: &Point, goal: &Point, current_dir: Option<Direction>) -> f64 {
        let manhattan = current.manhattan_distance(goal);
        let estimated_bends = self.estimate_bends(current, goal, current_dir);
        manhattan + estimated_bends * self.config.segment_penalty
    }

    /// Estimate the minimum number of bends needed to reach the goal.
    fn estimate_bends(&self, current: &Point, goal: &Point, current_dir: Option<Direction>) -> f64 {
        let dx = goal.x - current.x;
        let dy = goal.y - current.y;

        // If we're at the goal, no bends needed
        if dx.abs() < 1e-9 && dy.abs() < 1e-9 {
            return 0.0;
        }

        // Determine required final directions
        let need_horizontal = dx.abs() > 1e-9;
        let need_vertical = dy.abs() > 1e-9;

        match current_dir {
            None => {
                // Starting point: need at least 0 bends if aligned, 1 if L-shaped
                if need_horizontal && need_vertical {
                    1.0
                } else {
                    0.0
                }
            }
            Some(dir) => {
                let currently_horizontal = dir.is_horizontal();

                if need_horizontal && need_vertical {
                    // Need to go both directions - at least 1 bend
                    1.0
                } else if need_horizontal && currently_horizontal {
                    // Going horizontal, need horizontal - check if correct direction
                    let going_right = matches!(dir, Direction::Right);
                    if (going_right && dx > 0.0) || (!going_right && dx < 0.0) {
                        0.0
                    } else {
                        2.0 // Need to turn around
                    }
                } else if need_vertical && !currently_horizontal {
                    // Going vertical, need vertical - check if correct direction
                    let going_down = matches!(dir, Direction::Down);
                    if (going_down && dy > 0.0) || (!going_down && dy < 0.0) {
                        0.0
                    } else {
                        2.0 // Need to turn around
                    }
                } else {
                    // Need to change orientation
                    1.0
                }
            }
        }
    }

    /// Reconstruct the path from the came_from map.
    #[allow(clippy::type_complexity)]
    fn reconstruct_path(
        &self,
        came_from: &HashMap<(VertexId, Option<Direction>), (VertexId, Option<Direction>)>,
        end_key: (VertexId, Option<Direction>),
        total_cost: f64,
    ) -> PathResult {
        let mut path_keys = vec![end_key];
        let mut current = end_key;

        while let Some(&prev) = came_from.get(&current) {
            path_keys.push(prev);
            current = prev;
        }

        path_keys.reverse();

        // Debug: log path_keys (using println for test visibility)
        #[cfg(test)]
        {
            println!("[pathfinder] path_keys ({} entries):", path_keys.len());
            for (i, (vid, dir)) in path_keys.iter().enumerate() {
                if let Some(v) = self.graph.get_vertex(*vid) {
                    println!(
                        "  [{}] {:?} ({:.2}, {:.2}) dir={:?} port={:?}",
                        i, vid, v.position.x, v.position.y, dir, v.port_id
                    );
                }
            }
        }

        // Convert to points
        let points: Vec<Point> = path_keys
            .iter()
            .filter_map(|(vertex_id, _)| self.graph.get_vertex(*vertex_id).map(|v| v.position))
            .collect();

        // Simplify path (remove colinear points)
        let simplified = self.simplify_path(&points);

        // Count bends
        let bend_count = self.count_bends(&simplified);

        PathResult {
            points: simplified,
            cost: total_cost,
            bend_count,
        }
    }

    /// Remove colinear points from a path.
    fn simplify_path(&self, points: &[Point]) -> Vec<Point> {
        if points.len() <= 2 {
            return points.to_vec();
        }

        let mut result = vec![points[0]];

        for i in 1..points.len() - 1 {
            let prev = result.last().unwrap();
            let curr = &points[i];
            let next = &points[i + 1];

            // Check if curr is colinear with prev and next
            let is_horizontal_with_prev = (prev.y - curr.y).abs() < 1e-9;
            let is_horizontal_with_next = (curr.y - next.y).abs() < 1e-9;
            let is_vertical_with_prev = (prev.x - curr.x).abs() < 1e-9;
            let is_vertical_with_next = (curr.x - next.x).abs() < 1e-9;

            let colinear = (is_horizontal_with_prev && is_horizontal_with_next)
                || (is_vertical_with_prev && is_vertical_with_next);

            if !colinear {
                result.push(*curr);
            }
        }

        result.push(*points.last().unwrap());
        result
    }

    /// Count the number of bends in a path.
    fn count_bends(&self, points: &[Point]) -> usize {
        if points.len() < 3 {
            return 0;
        }

        let mut bends = 0;
        for i in 1..points.len() - 1 {
            let prev = &points[i - 1];
            let curr = &points[i];
            let next = &points[i + 1];

            let was_horizontal = (prev.y - curr.y).abs() < 1e-9;
            let is_horizontal = (curr.y - next.y).abs() < 1e-9;

            if was_horizontal != is_horizontal {
                bends += 1;
            }
        }
        bends
    }
}

pub(crate) fn path_conflicts_with_existing_segments(
    points: &[Point],
    net_id: &str,
    existing_segments: &[ExistingRouteSegment],
) -> bool {
    if existing_segments.is_empty() {
        return false;
    }

    points.windows(2).any(|points| {
        path_segment_conflicts_with_existing_segments(
            &points[0],
            &points[1],
            net_id,
            existing_segments,
        )
    })
}

fn path_segment_conflicts_with_existing_segments(
    start: &Point,
    end: &Point,
    net_id: &str,
    existing_segments: &[ExistingRouteSegment],
) -> bool {
    if existing_segments.is_empty() || Segment::from_points(start, end).is_none() {
        return false;
    }

    existing_segments
        .iter()
        .filter(|existing| existing.net_id != net_id)
        .any(|existing| fixed_segment_conflicts(start, end, existing.start, existing.end))
}

fn fixed_segment_conflicts(
    candidate_start: &Point,
    candidate_end: &Point,
    fixed_start: Point,
    fixed_end: Point,
) -> bool {
    let Some(candidate) = SegmentShape::from_points(*candidate_start, *candidate_end) else {
        return false;
    };
    let Some(fixed) = SegmentShape::from_points(fixed_start, fixed_end) else {
        return false;
    };

    if candidate.horizontal == fixed.horizontal {
        return candidate.same_line(&fixed) && candidate.ranges_touch_or_overlap(&fixed);
    }

    let (horizontal, vertical) = if candidate.horizontal {
        (candidate, fixed)
    } else {
        (fixed, candidate)
    };

    let x = vertical.fixed;
    let y = horizontal.fixed;
    if !horizontal.range_contains(x) || !vertical.range_contains(y) {
        return false;
    }

    let intersection = Point::new(x, y);
    candidate.endpoint_equals(&intersection) || fixed.endpoint_equals(&intersection)
}

#[derive(Debug, Clone, Copy)]
struct SegmentShape {
    start: Point,
    end: Point,
    fixed: f64,
    min_var: f64,
    max_var: f64,
    horizontal: bool,
}

impl SegmentShape {
    fn from_points(start: Point, end: Point) -> Option<Self> {
        const EPSILON: f64 = 1e-9;
        let dx = (start.x - end.x).abs();
        let dy = (start.y - end.y).abs();
        if dx <= EPSILON && dy <= EPSILON {
            return None;
        }
        if dy <= EPSILON {
            return Some(Self {
                start,
                end,
                fixed: start.y,
                min_var: start.x.min(end.x),
                max_var: start.x.max(end.x),
                horizontal: true,
            });
        }
        if dx <= EPSILON {
            return Some(Self {
                start,
                end,
                fixed: start.x,
                min_var: start.y.min(end.y),
                max_var: start.y.max(end.y),
                horizontal: false,
            });
        }
        None
    }

    fn same_line(&self, other: &Self) -> bool {
        (self.fixed - other.fixed).abs() <= 1e-9
    }

    fn ranges_touch_or_overlap(&self, other: &Self) -> bool {
        self.min_var.max(other.min_var) <= self.max_var.min(other.max_var) + 1e-9
    }

    fn range_contains(&self, value: f64) -> bool {
        value >= self.min_var - 1e-9 && value <= self.max_var + 1e-9
    }

    fn endpoint_equals(&self, point: &Point) -> bool {
        point_eq(&self.start, point) || point_eq(&self.end, point)
    }
}

fn point_eq(a: &Point, b: &Point) -> bool {
    (a.x - b.x).abs() <= 1e-9 && (a.y - b.y).abs() <= 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ConnDirFlags, Connector, Obstacle, Port, RouterInput};

    fn setup_simple_scenario() -> (RouterInput, RouterConfig) {
        let mut input = RouterInput::new();
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(200.0, 50.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        (input, RouterConfig::default())
    }

    #[test]
    fn test_simple_straight_path() {
        let (input, config) = setup_simple_scenario();
        let graph = VisibilityGraph::build(&input, &config);
        let pathfinder = Pathfinder::new(&graph, &config);

        let result = pathfinder.find_path("p1", "p2");
        assert!(result.is_some(), "Should find a path");

        let path = result.unwrap();
        assert!(path.points.len() >= 2, "Path should have at least 2 points");
        assert_eq!(path.bend_count, 0, "Straight path should have 0 bends");

        // Check start and end points
        assert!((path.points[0].x - 0.0).abs() < 1e-9);
        assert!((path.points[0].y - 50.0).abs() < 1e-9);
        assert!((path.points.last().unwrap().x - 200.0).abs() < 1e-9);
        assert!((path.points.last().unwrap().y - 50.0).abs() < 1e-9);
    }

    #[test]
    fn test_path_around_obstacle() {
        let mut input = RouterInput::new();
        input.add_obstacle(Obstacle::from_xywh("obs1", 80.0, 30.0, 40.0, 40.0));
        input.add_port(Port::new("p1", Point::new(50.0, 50.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(150.0, 50.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);
        let pathfinder = Pathfinder::new(&graph, &config);

        let result = pathfinder.find_path("p1", "p2");
        assert!(result.is_some(), "Should find a path around obstacle");

        let path = result.unwrap();
        assert!(
            path.bend_count >= 2,
            "Path around obstacle needs at least 2 bends"
        );
    }

    #[test]
    fn test_no_path() {
        let mut input = RouterInput::new();
        // Port that can only go right, but there's nothing to the right
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        // Port that can only go right, so can't receive from p1
        input.add_port(Port::new(
            "p2",
            Point::new(100.0, 50.0),
            ConnDirFlags::RIGHT,
        ));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);
        let pathfinder = Pathfinder::new(&graph, &config);

        let _result = pathfinder.find_path("p1", "p2");
        // This might find a path going around, or might not depending on graph structure
        // The key is it shouldn't panic
    }

    #[test]
    fn test_l_shaped_path() {
        let mut input = RouterInput::new();
        input.add_port(Port::new("p1", Point::new(0.0, 0.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(100.0, 100.0), ConnDirFlags::UP));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);
        let pathfinder = Pathfinder::new(&graph, &config);

        let result = pathfinder.find_path("p1", "p2");
        assert!(result.is_some(), "Should find L-shaped path");

        let path = result.unwrap();
        assert_eq!(path.bend_count, 1, "L-shaped path should have 1 bend");
    }
}
