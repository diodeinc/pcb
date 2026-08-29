//! Main router implementation.
//!
//! This module contains the `OrthoRouter` struct which orchestrates
//! the orthogonal routing process:
//! 1. Build visibility graph from obstacles
//! 2. Find paths using A* search
//! 3. Nudge routes to improve layout

use crate::config::RouterConfig;
use crate::improve_crossings::improve_crossings;
use crate::legalization::{legalize_paths, snap_paths_to_grid};
use crate::nudging_libavoid::{nudge_routes, NudgingDebugInfo};
use crate::pathfinder::{path_conflicts_with_existing_segments, NetAwareContext, Pathfinder};
use crate::segment::{BendPointRegistry, SegmentRegistry};
use crate::types::{
    ExistingRouteSegment, Point, RoutedPath, RouterInput, RouterOutput, RoutingTiming,
};
use crate::visibility::VisibilityGraph;
use std::collections::{BTreeMap, BTreeSet};

/// Intermediate routing state captured at each phase.
///
/// This is returned by [`OrthoRouter::route_with_steps`] for debugging and
/// visualization. Contains the visibility graph, paths after each phase,
/// and timing information.
#[derive(Debug, Clone)]
pub struct RoutingSteps {
    /// The visibility graph built in phase 1.
    pub graph: VisibilityGraph,
    /// Paths after A* pathfinding (before improve_crossings).
    pub paths_after_pathfinding: Vec<RoutedPath>,
    /// Paths after improve_crossings (before nudging).
    pub paths_after_improve_crossings: Vec<RoutedPath>,
    /// Final paths after nudging.
    pub paths_final: Vec<RoutedPath>,
    /// Timing breakdown for each phase.
    pub timing: RoutingTiming,
    /// Debug info for nudging passes (optional, only when capture_steps=true).
    pub nudging_debug: Option<NudgingDebugInfo>,
}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

// Corner separation is disabled - see NUDGING.md for details
// use crate::corner_separation::separate_corners_v2;

/// The main orthogonal router.
///
/// # Example
///
/// ```
/// use ortho_router::{OrthoRouter, RouterConfig, RouterInput, Obstacle, Port, Connector, Point, ConnDirFlags, Rect};
///
/// let mut input = RouterInput::new();
/// input.add_obstacle(Obstacle::new("obs1", Rect::new(50.0, 50.0, 100.0, 100.0)));
/// input.add_port(Port::new("p1", Point::new(0.0, 75.0), ConnDirFlags::RIGHT));
/// input.add_port(Port::new("p2", Point::new(150.0, 75.0), ConnDirFlags::LEFT));
/// input.add_connector(Connector::new("c1", "p1", "p2"));
///
/// let router = OrthoRouter::new(RouterConfig::default());
/// let output = router.route(&input);
/// ```
pub struct OrthoRouter {
    config: RouterConfig,
}

impl OrthoRouter {
    /// Create a new router with the given configuration.
    pub fn new(config: RouterConfig) -> Self {
        Self { config }
    }

    /// Create a new router with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RouterConfig::default())
    }

    /// Get a reference to the router configuration.
    pub fn config(&self) -> &RouterConfig {
        &self.config
    }

    /// Route all connectors in the input, avoiding obstacles.
    ///
    /// This is the main entry point for routing. It performs:
    /// 1. Visibility graph construction
    /// 2. A* pathfinding for each connector (net-aware)
    /// 3. Route nudging/centering
    ///
    /// Connectors are processed grouped by net_id. Same-net connectors can
    /// share segments, while different-net connectors are penalized for
    /// overlapping.
    pub fn route(&self, input: &RouterInput) -> RouterOutput {
        self.route_internal(input, false).0
    }

    /// Route all connectors and capture intermediate state at each phase.
    ///
    /// This is useful for debugging and visualization. It returns the same
    /// result as [`route`], plus the visibility graph, paths after each
    /// phase, and timing information.
    ///
    /// Note: This has additional overhead from cloning paths at each phase
    /// boundary. Use [`route`] for production code.
    pub fn route_with_steps(&self, input: &RouterInput) -> (RouterOutput, RoutingSteps) {
        let (output, steps) = self.route_internal(input, true);
        (
            output,
            steps.expect("steps should be Some when capture_steps=true"),
        )
    }

    /// Internal routing implementation with optional step capture.
    ///
    /// When `capture_steps` is false, this is the fast path with no cloning.
    /// When `capture_steps` is true, paths are cloned after each phase.
    fn route_internal(
        &self,
        input: &RouterInput,
        capture_steps: bool,
    ) -> (RouterOutput, Option<RoutingSteps>) {
        let total_start = Instant::now();

        log::info!(
            "[ortho-router] Starting routing: {} obstacles, {} ports, {} connectors",
            input.obstacles.len(),
            input.ports.len(),
            input.connectors.len()
        );

        // Phase 1: Build visibility graph
        let phase1_start = Instant::now();
        let graph = if self.config.use_lazy_edges {
            VisibilityGraph::build_lazy(input, &self.config)
        } else {
            VisibilityGraph::build(input, &self.config)
        };
        let phase1_time = phase1_start.elapsed();
        let stats = graph.stats();
        log::info!(
            "[ortho-router] Visibility graph: {} vertices, {} edges (took {:.2}ms, lazy={})",
            stats.vertex_count,
            stats.edge_count,
            phase1_time.as_secs_f64() * 1000.0,
            graph.is_lazy()
        );

        // Phase 2: A* search for each connector (net-aware)
        let phase2_start = Instant::now();
        let pathfinder = Pathfinder::new(&graph, &self.config);
        let mut segment_registry = SegmentRegistry::new();
        let mut bend_registry = BendPointRegistry::new();
        let mut paths = Vec::new();
        seed_existing_segments(&mut segment_registry, &input.existing_segments);

        // Group connectors by net for optimal routing order
        // Route connectors from the same net consecutively so they can share paths
        // BTreeMap ensures deterministic iteration order
        let connectors_by_net = group_connectors_by_net(&input.connectors);

        let total_connectors: usize = connectors_by_net.values().map(|v| v.len()).sum();

        for (net_id, connector_ids) in &connectors_by_net {
            log::debug!(
                "[ortho-router] Routing net '{}' with {} connectors",
                net_id,
                connector_ids.len()
            );

            for connector_id in connector_ids {
                let connector = match input.connectors.iter().find(|c| &c.id == connector_id) {
                    Some(c) => c,
                    None => continue,
                };

                log::debug!(
                    "[ortho-router] Routing connector '{}' (net '{}'): {} -> {}",
                    connector.id,
                    net_id,
                    connector.source_port_id,
                    connector.target_port_id
                );

                // Create net-aware context for pathfinding
                let net_context = NetAwareContext {
                    registry: &segment_registry,
                    bend_registry: &bend_registry,
                    net_id,
                    existing_segments: &input.existing_segments,
                };

                match pathfinder.find_path_with_context(
                    &connector.source_port_id,
                    &connector.target_port_id,
                    Some(net_context),
                ) {
                    Some(result) => {
                        log::debug!(
                            "[ortho-router] Found path for '{}': {} points, {} bends, cost {:.2}",
                            connector.id,
                            result.points.len(),
                            result.bend_count,
                            result.cost
                        );
                        // Log the actual path points for debugging route quality
                        let points_str: Vec<String> = result
                            .points
                            .iter()
                            .map(|p| format!("({:.2},{:.2})", p.x, p.y))
                            .collect();
                        log::debug!(
                            "[ortho-router] Path for '{}' (net '{}'): cost={:.2}, bends={}, path={}",
                            connector.id,
                            net_id,
                            result.cost,
                            result.bend_count,
                            points_str.join(" -> ")
                        );

                        // Register this path's segments and bend points for subsequent routing
                        segment_registry.register_path(&result.points, net_id);
                        bend_registry.register_path(&result.points, net_id);

                        paths.push(RoutedPath::with_net(
                            connector.id.clone(),
                            result.points,
                            net_id.clone(),
                        ));
                    }
                    None => {
                        log::warn!(
                            "[ortho-router] No path found for connector '{}'",
                            connector.id
                        );
                    }
                }
            }
        }
        let phase2_time = phase2_start.elapsed();
        log::info!(
            "[ortho-router] Pathfinding complete: {}/{} connectors routed (took {:.2}ms)",
            paths.len(),
            total_connectors,
            phase2_time.as_secs_f64() * 1000.0
        );

        // Capture paths after pathfinding (before improve_crossings)
        let paths_after_pathfinding = if capture_steps {
            Some(paths.clone())
        } else {
            None
        };

        // Phase 3: Improve crossings - reroute overlapping different-net connectors
        let phase3_start = Instant::now();
        if paths.len() > 1 {
            // Build connector list in same order as paths
            let connector_list: Vec<_> = paths
                .iter()
                .filter_map(|p| input.connectors.iter().find(|c| c.id == p.connector_id))
                .cloned()
                .collect();

            let net_ids: Vec<String> = paths.iter().map(|p| p.net_id.clone()).collect();

            improve_crossings(
                &mut paths,
                &connector_list,
                &net_ids,
                &graph,
                &self.config,
                &input.existing_segments,
            );
        }
        let phase3_time = phase3_start.elapsed();
        log::info!(
            "[ortho-router] Improve crossings complete (took {:.2}ms)",
            phase3_time.as_secs_f64() * 1000.0
        );

        // Capture paths after improve_crossings (before nudging)
        let paths_after_improve_crossings = if capture_steps {
            Some(paths.clone())
        } else {
            None
        };

        // Phase 4: Nudge routes to separate overlapping different-net segments
        let phase4_start = Instant::now();
        let paths_before_nudging = if input.existing_segments.is_empty() {
            None
        } else {
            Some(paths.clone())
        };
        let nudging_debug = if !paths.is_empty() {
            let net_ids: Vec<String> = paths.iter().map(|p| p.net_id.clone()).collect();
            nudge_routes(
                &mut paths,
                &net_ids,
                &input.obstacles,
                &self.config,
                capture_steps,
            )
        } else {
            None
        };
        let phase4_time = phase4_start.elapsed();
        log::info!(
            "[ortho-router] Nudging complete (took {:.2}ms)",
            phase4_time.as_secs_f64() * 1000.0
        );

        // Phase 5: Grid snapping - snap all path points to the routing grid
        let phase5_start = Instant::now();
        snap_paths_to_grid(&mut paths, &self.config);
        let phase5_time = phase5_start.elapsed();
        log::info!(
            "[ortho-router] Grid snapping complete (took {:.2}ms)",
            phase5_time.as_secs_f64() * 1000.0
        );

        let canonicalized_paths = canonicalize_same_net_paths(&mut paths);
        if canonicalized_paths > 0 {
            log::debug!(
                "[ortho-router] Canonicalized {} same-net paths",
                canonicalized_paths
            );
        }

        // Phase 6: Legalization - remove illegal overlaps between different nets
        let phase6_start = Instant::now();
        let legalization_result = legalize_paths(&mut paths);
        let phase6_time = phase6_start.elapsed();
        if !legalization_result.removed_path_ids.is_empty() {
            log::info!(
                "[ortho-router] Legalization removed {} paths (took {:.2}ms)",
                legalization_result.removed_path_ids.len(),
                phase6_time.as_secs_f64() * 1000.0
            );
        }
        let removed_existing_conflicts = resolve_paths_conflicting_with_existing_segments(
            &mut paths,
            paths_before_nudging.as_deref(),
            &input.existing_segments,
        );
        if removed_existing_conflicts > 0 {
            log::info!(
                "[ortho-router] Existing-segment validation removed {} paths",
                removed_existing_conflicts
            );
        }

        // Check for non-orthogonal paths (indicates a bug)
        for path in &paths {
            if !path.is_orthogonal() {
                log::warn!(
                    "[ortho-router] Non-orthogonal path '{}': {:?}",
                    path.connector_id,
                    path.points.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()
                );
            }
        }

        // Phase 7: Junction detection - find where same-net paths meet
        let phase7_start = Instant::now();
        let net_ids: Vec<String> = paths.iter().map(|p| p.net_id.clone()).collect();
        let junctions = crate::junction::detect_junctions(&paths, &net_ids);

        // Populate junction_points on each path
        for junction in &junctions {
            for path in &mut paths {
                if junction.connector_ids.contains(&path.connector_id) {
                    // Check if this junction point is on this path's segments
                    if is_point_on_path(&junction.position, path) {
                        path.junction_points.push(junction.position);
                    }
                }
            }
        }

        let phase7_time = phase7_start.elapsed();
        if !junctions.is_empty() {
            log::info!(
                "[ortho-router] Junction detection found {} junctions (took {:.2}ms)",
                junctions.len(),
                phase7_time.as_secs_f64() * 1000.0
            );
        }

        let total_time = total_start.elapsed();
        log::info!(
            "[ortho-router] Routing complete: {}/{} connectors routed in {:.2}ms",
            paths.len(),
            input.connectors.len(),
            total_time.as_secs_f64() * 1000.0
        );
        log::info!(
            "[ortho-router] Timing breakdown: visibility={:.2}ms, pathfinding={:.2}ms, crossings={:.2}ms, nudging={:.2}ms, snap={:.2}ms, legalize={:.2}ms",
            phase1_time.as_secs_f64() * 1000.0,
            phase2_time.as_secs_f64() * 1000.0,
            phase3_time.as_secs_f64() * 1000.0,
            phase4_time.as_secs_f64() * 1000.0,
            phase5_time.as_secs_f64() * 1000.0,
            phase6_time.as_secs_f64() * 1000.0
        );

        // Build result
        if capture_steps {
            let timing = RoutingTiming {
                visibility_graph_ms: phase1_time.as_secs_f64() * 1000.0,
                pathfinding_ms: phase2_time.as_secs_f64() * 1000.0,
                improve_crossings_ms: phase3_time.as_secs_f64() * 1000.0,
                nudging_ms: phase4_time.as_secs_f64() * 1000.0,
                grid_snap_ms: phase5_time.as_secs_f64() * 1000.0,
                legalization_ms: phase6_time.as_secs_f64() * 1000.0,
                total_ms: total_time.as_secs_f64() * 1000.0,
            };
            let steps = RoutingSteps {
                graph,
                paths_after_pathfinding: paths_after_pathfinding.unwrap(),
                paths_after_improve_crossings: paths_after_improve_crossings.unwrap(),
                paths_final: paths.clone(),
                timing,
                nudging_debug,
            };
            (RouterOutput { paths, junctions }, Some(steps))
        } else {
            (RouterOutput { paths, junctions }, None)
        }
    }
}

impl Default for OrthoRouter {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Group connectors by their effective net ID.
///
/// Returns a BTreeMap from net_id to list of connector IDs.
/// BTreeMap ensures deterministic iteration order (sorted by net_id).
/// Connectors without a net_id are treated as their own net (using connector ID).
fn group_connectors_by_net(
    connectors: &[crate::types::Connector],
) -> BTreeMap<String, Vec<String>> {
    let mut by_net: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for connector in connectors {
        let net_id = connector.effective_net_id().to_string();
        by_net.entry(net_id).or_default().push(connector.id.clone());
    }

    by_net
}

pub(crate) fn seed_existing_segments(
    registry: &mut SegmentRegistry,
    existing_segments: &[ExistingRouteSegment],
) {
    for existing in existing_segments {
        if let Some(segment) = crate::segment::Segment::from_points(&existing.start, &existing.end)
        {
            registry.register_segment(segment, &existing.net_id);
        }
    }
}

fn resolve_paths_conflicting_with_existing_segments(
    paths: &mut Vec<RoutedPath>,
    fallback_paths: Option<&[RoutedPath]>,
    existing_segments: &[ExistingRouteSegment],
) -> usize {
    if existing_segments.is_empty() {
        return 0;
    }

    let fallback_by_connector: BTreeMap<_, _> = fallback_paths
        .unwrap_or(&[])
        .iter()
        .map(|path| (path.connector_id.as_str(), path))
        .collect();

    let mut removed = 0;
    let mut resolved = Vec::with_capacity(paths.len());
    for path in paths.drain(..) {
        let conflicts =
            path_conflicts_with_existing_segments(&path.points, &path.net_id, existing_segments);
        if conflicts {
            if let Some(fallback) = fallback_by_connector.get(path.connector_id.as_str()) {
                let fallback_conflicts = path_conflicts_with_existing_segments(
                    &fallback.points,
                    &fallback.net_id,
                    existing_segments,
                );
                if !fallback_conflicts {
                    log::info!(
                        "[ortho-router] Restoring pre-nudge path '{}' (net '{}') to avoid fixed-segment conflict",
                        fallback.connector_id,
                        fallback.net_id
                    );
                    resolved.push((*fallback).clone());
                    continue;
                }
            }
            log::warn!(
                "[ortho-router] Removing path '{}' (net '{}') - conflicts with an existing different-net segment",
                path.connector_id,
                path.net_id
            );
            removed += 1;
            continue;
        }
        resolved.push(path);
    }
    *paths = resolved;
    removed
}

/// Re-express same-net routes on a canonical topology graph.
///
/// Binary routing can create visually redundant boxes when one path crosses an
/// existing same-net trunk and then joins it later. This pass preserves each
/// connector's fixed endpoints, splits same-net geometry at intersections, and
/// chooses a deterministic graph path between those endpoints.
fn canonicalize_same_net_paths(paths: &mut [RoutedPath]) -> usize {
    let original_paths = paths.to_vec();
    let mut paths_by_net: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (idx, path) in original_paths.iter().enumerate() {
        paths_by_net
            .entry(path.net_id.clone())
            .or_default()
            .push(idx);
    }

    let mut changed = 0;
    for path_indices in paths_by_net.values() {
        if path_indices.len() < 2 {
            continue;
        }

        let graph = SameNetGraph::build(&original_paths, path_indices);
        for &path_idx in path_indices {
            let path = &original_paths[path_idx];
            if path.points.len() < 2 {
                continue;
            }

            let start = PointKey::from_point(&path.points[0]);
            let target = PointKey::from_point(path.points.last().expect("path has target"));
            let Some(points) = graph.best_path(path_idx, start, target) else {
                continue;
            };
            let points = simplify_polyline(&points);
            if points.len() >= 2 && point_keys(&points) != point_keys(&path.points) {
                paths[path_idx].points = points;
                paths[path_idx].junction_points.clear();
                changed += 1;
            }
        }
    }

    changed
}

#[derive(Debug, Clone)]
struct SameNetGraph {
    adjacency: BTreeMap<PointKey, Vec<GraphEdge>>,
}

#[derive(Debug, Clone)]
struct GraphEdge {
    to: PointKey,
    length_units: i64,
    owners: BTreeSet<usize>,
}

impl SameNetGraph {
    fn build(paths: &[RoutedPath], path_indices: &[usize]) -> Self {
        let mut segment_points: BTreeMap<SegmentId, BTreeSet<PointKey>> = BTreeMap::new();
        let mut segments = Vec::new();

        for &path_idx in path_indices {
            let path = &paths[path_idx];
            for segment_idx in 0..path.points.len().saturating_sub(1) {
                let start = path.points[segment_idx];
                let end = path.points[segment_idx + 1];
                if points_equal(&start, &end) {
                    continue;
                }

                let id = SegmentId {
                    path_idx,
                    segment_idx,
                };
                segment_points
                    .entry(id)
                    .or_default()
                    .extend([PointKey::from_point(&start), PointKey::from_point(&end)]);
                segments.push((id, start, end));
            }
        }

        for i in 0..segments.len() {
            for j in (i + 1)..segments.len() {
                let (id_a, a1, a2) = segments[i];
                let (id_b, b1, b2) = segments[j];
                if id_a.path_idx == id_b.path_idx {
                    continue;
                }

                for point in segment_intersections(a1, a2, b1, b2) {
                    let key = PointKey::from_point(&point);
                    segment_points.entry(id_a).or_default().insert(key);
                    segment_points.entry(id_b).or_default().insert(key);
                }
            }
        }

        let mut edge_owners: BTreeMap<(PointKey, PointKey), BTreeSet<usize>> = BTreeMap::new();
        for (id, start, end) in segments {
            let mut points: Vec<_> = segment_points
                .remove(&id)
                .unwrap_or_default()
                .into_iter()
                .collect();
            sort_points_along_segment(&mut points, start, end);

            for pair in points.windows(2) {
                let from = pair[0];
                let to = pair[1];
                if from == to {
                    continue;
                }
                let key = if from <= to { (from, to) } else { (to, from) };
                edge_owners.entry(key).or_default().insert(id.path_idx);
            }
        }

        let mut adjacency: BTreeMap<PointKey, Vec<GraphEdge>> = BTreeMap::new();
        for ((from, to), owners) in edge_owners {
            let length_units = length_units(from, to);
            adjacency.entry(from).or_default().push(GraphEdge {
                to,
                length_units,
                owners: owners.clone(),
            });
            adjacency.entry(to).or_default().push(GraphEdge {
                to: from,
                length_units,
                owners,
            });
        }

        for edges in adjacency.values_mut() {
            edges.sort_by_key(|edge| edge.to);
        }

        Self { adjacency }
    }

    fn best_path(
        &self,
        current_path_idx: usize,
        start: PointKey,
        target: PointKey,
    ) -> Option<Vec<Point>> {
        let start_state = SearchState {
            node: start,
            previous: None,
        };
        let mut best: BTreeMap<SearchState, RouteCost> = BTreeMap::new();
        let mut parents: BTreeMap<SearchState, SearchState> = BTreeMap::new();
        let mut frontier: BTreeSet<(RouteCost, SearchState)> = BTreeSet::new();

        best.insert(start_state, RouteCost::default());
        frontier.insert((RouteCost::default(), start_state));

        while let Some((cost, state)) = frontier.pop_first() {
            if best.get(&state).is_none_or(|known| *known != cost) {
                continue;
            }

            let Some(edges) = self.adjacency.get(&state.node) else {
                continue;
            };
            for edge in edges {
                let next_state = SearchState {
                    node: edge.to,
                    previous: Some(state.node),
                };
                let next_cost = cost.extend(state, edge, current_path_idx);

                let should_update = best.get(&next_state).is_none_or(|known| next_cost < *known);
                if should_update {
                    if let Some(old_cost) = best.insert(next_state, next_cost) {
                        frontier.remove(&(old_cost, next_state));
                    }
                    parents.insert(next_state, state);
                    frontier.insert((next_cost, next_state));
                }
            }
        }

        let mut candidates: Vec<_> = best
            .iter()
            .filter(|(state, _)| state.node == target)
            .filter_map(|(state, cost)| {
                let points = reconstruct_path(*state, start_state, &parents)?;
                Some((*cost, point_keys(&points), points))
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        candidates.into_iter().next().map(|(_, _, points)| points)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentId {
    path_idx: usize,
    segment_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SearchState {
    node: PointKey,
    previous: Option<PointKey>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct RouteCost {
    length_units: i64,
    bends: usize,
    private_length_units: i64,
}

impl RouteCost {
    fn extend(self, state: SearchState, edge: &GraphEdge, current_path_idx: usize) -> Self {
        let bends = self.bends
            + usize::from(
                state
                    .previous
                    .is_some_and(|previous| is_bend(previous, state.node, edge.to)),
            );
        let private_length_units = self.private_length_units
            + if edge.owners.iter().any(|owner| *owner != current_path_idx) {
                0
            } else {
                edge.length_units
            };

        Self {
            length_units: self.length_units + edge.length_units,
            bends,
            private_length_units,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PointKey {
    x: i64,
    y: i64,
}

impl PointKey {
    const PRECISION: f64 = 1000.0;

    fn from_point(point: &Point) -> Self {
        Self {
            x: (point.x * Self::PRECISION).round() as i64,
            y: (point.y * Self::PRECISION).round() as i64,
        }
    }

    fn to_point(self) -> Point {
        Point::new(
            self.x as f64 / Self::PRECISION,
            self.y as f64 / Self::PRECISION,
        )
    }
}

fn reconstruct_path(
    mut state: SearchState,
    start_state: SearchState,
    parents: &BTreeMap<SearchState, SearchState>,
) -> Option<Vec<Point>> {
    let mut keys = vec![state.node];
    while state != start_state {
        state = *parents.get(&state)?;
        keys.push(state.node);
    }
    keys.reverse();
    Some(keys.into_iter().map(PointKey::to_point).collect())
}

fn segment_intersections(a1: Point, a2: Point, b1: Point, b2: Point) -> Vec<Point> {
    let a_horizontal = (a1.y - a2.y).abs() < EPSILON;
    let a_vertical = (a1.x - a2.x).abs() < EPSILON;
    let b_horizontal = (b1.y - b2.y).abs() < EPSILON;
    let b_vertical = (b1.x - b2.x).abs() < EPSILON;

    if a_horizontal && b_vertical {
        return perpendicular_intersection(a1, a2, b1, b2)
            .into_iter()
            .collect();
    }
    if a_vertical && b_horizontal {
        return perpendicular_intersection(b1, b2, a1, a2)
            .into_iter()
            .collect();
    }
    if a_horizontal && b_horizontal && (a1.y - b1.y).abs() < EPSILON {
        return overlapping_segment_points(a1.x, a2.x, b1.x, b2.x)
            .into_iter()
            .map(|x| Point::new(x, a1.y))
            .collect();
    }
    if a_vertical && b_vertical && (a1.x - b1.x).abs() < EPSILON {
        return overlapping_segment_points(a1.y, a2.y, b1.y, b2.y)
            .into_iter()
            .map(|y| Point::new(a1.x, y))
            .collect();
    }

    Vec::new()
}

fn perpendicular_intersection(
    horizontal_start: Point,
    horizontal_end: Point,
    vertical_start: Point,
    vertical_end: Point,
) -> Option<Point> {
    let x = vertical_start.x;
    let y = horizontal_start.y;
    let horizontal_min_x = horizontal_start.x.min(horizontal_end.x);
    let horizontal_max_x = horizontal_start.x.max(horizontal_end.x);
    let vertical_min_y = vertical_start.y.min(vertical_end.y);
    let vertical_max_y = vertical_start.y.max(vertical_end.y);

    (x >= horizontal_min_x - EPSILON
        && x <= horizontal_max_x + EPSILON
        && y >= vertical_min_y - EPSILON
        && y <= vertical_max_y + EPSILON)
        .then_some(Point::new(x, y))
}

fn overlapping_segment_points(a1: f64, a2: f64, b1: f64, b2: f64) -> Vec<f64> {
    let overlap_min = a1.min(a2).max(b1.min(b2));
    let overlap_max = a1.max(a2).min(b1.max(b2));
    if overlap_min > overlap_max + EPSILON {
        return Vec::new();
    }
    if (overlap_min - overlap_max).abs() < EPSILON {
        vec![overlap_min]
    } else {
        vec![overlap_min, overlap_max]
    }
}

fn sort_points_along_segment(points: &mut [PointKey], start: Point, end: Point) {
    if (start.x - end.x).abs() < EPSILON {
        points.sort_by_key(|point| point.y);
    } else {
        points.sort_by_key(|point| point.x);
    }
}

fn simplify_polyline(points: &[Point]) -> Vec<Point> {
    let mut simplified: Vec<Point> = Vec::new();
    for &point in points {
        if simplified
            .last()
            .is_none_or(|last| !points_equal(last, &point))
        {
            simplified.push(point);
        }
    }

    let mut index = 1;
    while index + 1 < simplified.len() {
        if are_collinear(
            simplified[index - 1],
            simplified[index],
            simplified[index + 1],
        ) {
            simplified.remove(index);
        } else {
            index += 1;
        }
    }

    simplified
}

fn point_keys(points: &[Point]) -> Vec<PointKey> {
    points.iter().map(PointKey::from_point).collect()
}

fn length_units(from: PointKey, to: PointKey) -> i64 {
    (from.x - to.x).abs() + (from.y - to.y).abs()
}

fn is_bend(previous: PointKey, current: PointKey, next: PointKey) -> bool {
    !are_collinear(previous.to_point(), current.to_point(), next.to_point())
}

fn are_collinear(a: Point, b: Point, c: Point) -> bool {
    ((a.x - b.x).abs() < EPSILON && (b.x - c.x).abs() < EPSILON)
        || ((a.y - b.y).abs() < EPSILON && (b.y - c.y).abs() < EPSILON)
}

fn points_equal(a: &Point, b: &Point) -> bool {
    (a.x - b.x).abs() < EPSILON && (a.y - b.y).abs() < EPSILON
}

const EPSILON: f64 = 1e-6;

/// Check if a point lies on any segment of a path (including vertices).
fn is_point_on_path(point: &crate::types::Point, path: &RoutedPath) -> bool {
    // Check if point is at any vertex
    for vertex in &path.points {
        if (point.x - vertex.x).abs() < EPSILON && (point.y - vertex.y).abs() < EPSILON {
            return true;
        }
    }

    // Check if point lies on any segment
    for i in 0..path.points.len().saturating_sub(1) {
        let p1 = &path.points[i];
        let p2 = &path.points[i + 1];

        let is_horizontal = (p1.y - p2.y).abs() < EPSILON;
        let is_vertical = (p1.x - p2.x).abs() < EPSILON;

        if is_horizontal {
            if (point.y - p1.y).abs() < EPSILON {
                let min_x = p1.x.min(p2.x);
                let max_x = p1.x.max(p2.x);
                if point.x >= min_x - EPSILON && point.x <= max_x + EPSILON {
                    return true;
                }
            }
        } else if is_vertical && (point.x - p1.x).abs() < EPSILON {
            let min_y = p1.y.min(p2.y);
            let max_y = p1.y.max(p2.y);
            if point.y >= min_y - EPSILON && point.y <= max_y + EPSILON {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ConnDirFlags, Connector, ExistingRouteSegment, Obstacle, Point, Port, Rect,
    };

    #[test]
    fn test_router_creation() {
        let router = OrthoRouter::with_defaults();
        assert_eq!(router.config().segment_penalty, 1.0);
    }

    #[test]
    fn test_empty_input() {
        let router = OrthoRouter::with_defaults();
        let input = RouterInput::new();
        let output = router.route(&input);
        assert!(output.paths.is_empty());
    }

    #[test]
    fn test_simple_routing() {
        let mut input = RouterInput::new();

        // Add a simple obstacle
        input.add_obstacle(Obstacle::new("obs1", Rect::new(50.0, 50.0, 100.0, 100.0)));

        // Add two ports
        input.add_port(Port::new("p1", Point::new(0.0, 75.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(150.0, 75.0), ConnDirFlags::LEFT));

        // Add a connector
        input.add_connector(Connector::new("c1", "p1", "p2"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        // Should find a path
        assert_eq!(output.paths.len(), 1, "Should route one connector");

        let path = &output.paths[0];
        assert_eq!(path.connector_id, "c1");
        assert!(path.points.len() >= 2, "Path should have at least 2 points");
        assert!(path.is_orthogonal(), "Path should be orthogonal");
    }

    #[test]
    fn test_straight_path() {
        let mut input = RouterInput::new();

        // Two ports with direct line of sight
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(100.0, 50.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        assert_eq!(output.paths.len(), 1);
        let path = &output.paths[0];

        // Should be a straight line with 2 points
        assert_eq!(
            path.points.len(),
            2,
            "Straight path should have exactly 2 points"
        );
        assert_eq!(path.bend_count(), 0, "Straight path should have 0 bends");
    }

    #[test]
    fn existing_segment_allows_perpendicular_interior_crossing() {
        let mut input = RouterInput::new();
        input.add_existing_segment(ExistingRouteSegment::new(
            "fixed-b",
            Point::new(50.0, -50.0),
            Point::new(50.0, 50.0),
            "B",
        ));
        input.add_port(Port::new("p1", Point::new(0.0, 0.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(100.0, 0.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::with_net("c1", "p1", "p2", "A"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        assert_eq!(output.paths.len(), 1);
        assert!(output.paths[0].points.len() >= 2);
        assert!(
            output.paths[0]
                .points
                .windows(2)
                .any(|pair| segment_crosses_vertical_interior(pair[0], pair[1], 50.0, -50.0, 50.0)),
            "path should be allowed to cross fixed segment interiors: {:?}",
            output.paths[0].points
        );
    }

    #[test]
    fn existing_segment_blocks_different_net_parallel_overlap() {
        let mut input = RouterInput::new();
        input.add_existing_segment(ExistingRouteSegment::new(
            "fixed-b",
            Point::new(25.0, 0.0),
            Point::new(75.0, 0.0),
            "B",
        ));
        input.add_port(Port::new("p1", Point::new(0.0, 0.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(100.0, 0.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::with_net("c1", "p1", "p2", "A"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        assert_eq!(output.paths.len(), 1);
        assert!(
            !path_contains_segment_overlap(
                &output.paths[0].points,
                Point::new(25.0, 0.0),
                Point::new(75.0, 0.0)
            ),
            "path should not overlap fixed different-net segment: {:?}",
            output.paths[0].points
        );
    }

    #[test]
    fn existing_segment_blocks_different_net_endpoint_touch() {
        let mut input = RouterInput::new();
        input.add_existing_segment(ExistingRouteSegment::new(
            "fixed-b",
            Point::new(50.0, 0.0),
            Point::new(50.0, 50.0),
            "B",
        ));
        input.add_port(Port::new("p1", Point::new(0.0, 0.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(50.0, 0.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::with_net("c1", "p1", "p2", "A"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        assert!(output.paths.is_empty());
    }

    #[test]
    fn test_grid_of_obstacles_port_directionality() {
        // Same setup as snapshot test grid_of_obstacles
        // Use grid-aligned coordinates (multiples of 12.7)
        let mut input = RouterInput::new();

        // Create a 3x3 grid of obstacles (using grid-aligned coords)
        // Grid: 12.7 * n
        for row in 0..3 {
            for col in 0..3 {
                let x = 50.8 + col as f64 * 76.2; // 4*12.7 + col*6*12.7
                let y = 50.8 + row as f64 * 63.5; // 4*12.7 + row*5*12.7
                input.add_obstacle(Obstacle::from_xywh(
                    format!("obs_{}_{}", row, col),
                    x,
                    y,
                    50.8, // 4*12.7
                    38.1, // 3*12.7
                ));
            }
        }

        // Add ports on the sides (grid-aligned: 12.7 * n)
        // p_left at x=25.4 (2*12.7), y=114.3 (9*12.7)
        // p_right at x=304.8 (24*12.7), y=114.3 (9*12.7)
        input.add_port(Port::new(
            "p_left",
            Point::new(25.4, 114.3),
            ConnDirFlags::RIGHT,
        ));
        input.add_port(Port::new(
            "p_right",
            Point::new(304.8, 114.3),
            ConnDirFlags::LEFT,
        ));
        input.add_connector(Connector::new("c1", "p_left", "p_right"));

        let router = OrthoRouter::with_defaults();
        let output = router.route(&input);

        assert_eq!(output.paths.len(), 1);
        let path = &output.paths[0];

        // Print path for debugging
        println!("\nPath points:");
        for (i, p) in path.points.iter().enumerate() {
            println!("  {}: ({}, {})", i, p.x, p.y);
        }

        // First point should be the left port at (25.4, 114.3)
        assert!(
            (path.points[0].x - 25.4).abs() < 0.1,
            "First point X should be 25.4, got {}",
            path.points[0].x
        );
        assert!(
            (path.points[0].y - 114.3).abs() < 0.1,
            "First point Y should be 114.3, got {}",
            path.points[0].y
        );

        // Check for duplicates - there shouldn't be any
        if path.points.len() > 1 {
            let p0 = &path.points[0];
            let p1 = &path.points[1];
            assert!(
                (p0.x - p1.x).abs() > 0.001 || (p0.y - p1.y).abs() > 0.001,
                "Path has duplicate points at start: ({}, {}) and ({}, {})",
                p0.x,
                p0.y,
                p1.x,
                p1.y
            );
        }

        // Second point should be to the RIGHT (same Y, larger X) because p_left has RIGHT visibility
        if path.points.len() > 1 {
            let first_edge_x_delta = path.points[1].x - path.points[0].x;
            let first_edge_y_delta = path.points[1].y - path.points[0].y;

            // The port has RIGHT-only visibility, so:
            // - First edge should move in the +X direction
            // - First edge should NOT move in Y direction (up or down)
            assert!(
                first_edge_x_delta > 0.0 && first_edge_y_delta.abs() < 0.1,
                "Port with RIGHT visibility should have first edge going RIGHT. Delta: ({}, {})",
                first_edge_x_delta,
                first_edge_y_delta
            );
        }

        // Last point should be the right port at (304.8, 114.3)
        let last = path.points.last().unwrap();
        assert!(
            (last.x - 304.8).abs() < 0.1,
            "Last point X should be 304.8, got {}",
            last.x
        );
        assert!(
            (last.y - 114.3).abs() < 0.1,
            "Last point Y should be 114.3, got {}",
            last.y
        );
    }
    fn path_contains_segment_overlap(points: &[Point], start: Point, end: Point) -> bool {
        points.windows(2).any(|pair| {
            let route = crate::segment::Segment::from_points(&pair[0], &pair[1]);
            let fixed = crate::segment::Segment::from_points(&start, &end);
            matches!((route, fixed), (Some(route), Some(fixed)) if route.overlaps(&fixed))
        })
    }

    fn segment_crosses_vertical_interior(
        start: Point,
        end: Point,
        x: f64,
        min_y: f64,
        max_y: f64,
    ) -> bool {
        let horizontal = (start.y - end.y).abs() < 1e-6;
        if !horizontal {
            return false;
        }
        let crosses_x = x > start.x.min(end.x) + 1e-6 && x < start.x.max(end.x) - 1e-6;
        let crosses_y = start.y > min_y + 1e-6 && start.y < max_y - 1e-6;
        crosses_x && crosses_y
    }
}
