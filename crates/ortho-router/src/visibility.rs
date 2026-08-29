//! Visibility graph construction for orthogonal routing.
//!
//! This module builds a grid-based visibility graph from obstacles and ports.
//! The graph is used by the A* pathfinder to find routes.
//!
//! ## Algorithm Overview
//!
//! 1. Collect unique X and Y coordinates from obstacle edges (with buffer) and port positions
//! 2. Create vertices at grid intersections that aren't blocked by obstacles
//! 3. Create edges between adjacent vertices, checking for obstacle blockage
//! 4. Apply visibility constraints for ports attached to obstacles

use crate::config::RouterConfig;
use crate::types::{ConnDirFlags, Direction, Obstacle, Point, Port, Rect, RouterInput};
use rstar::{RTree, AABB};
use std::collections::{BTreeSet, HashMap, HashSet};

#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};
#[cfg(target_arch = "wasm32")]
use web_time::{Duration, Instant};

// ============================================================================
// Build Timing Statistics
// ============================================================================

/// Timing statistics for visibility graph construction.
#[derive(Debug, Clone, Default)]
pub struct VisibilityBuildTimings {
    pub collect_coords: Duration,
    pub blocked_cells: Duration,
    pub create_vertices: Duration,
    pub build_rtrees: Duration,
    pub create_edges: Duration,
    pub total: Duration,
    /// Number of X grid lines
    pub x_coords_count: usize,
    /// Number of Y grid lines
    pub y_coords_count: usize,
    /// Number of blocked cells
    pub blocked_cells_count: usize,
}

impl VisibilityBuildTimings {
    /// Log timing breakdown at info level.
    pub fn log(&self, vertex_count: usize, edge_count: usize) {
        let grid_size = self.x_coords_count * self.y_coords_count;
        let density = if grid_size > 0 {
            vertex_count as f64 / grid_size as f64 * 100.0
        } else {
            0.0
        };
        log::info!(
            "[visibility] Built graph: {} vertices, {} edges in {:.2}ms (coords={:.2}ms, blocked={:.2}ms, vertices={:.2}ms, rtrees={:.2}ms, edges={:.2}ms)",
            vertex_count,
            edge_count,
            self.total.as_secs_f64() * 1000.0,
            self.collect_coords.as_secs_f64() * 1000.0,
            self.blocked_cells.as_secs_f64() * 1000.0,
            self.create_vertices.as_secs_f64() * 1000.0,
            self.build_rtrees.as_secs_f64() * 1000.0,
            self.create_edges.as_secs_f64() * 1000.0,
        );
        log::info!(
            "[visibility] Grid: {}x{} = {} cells, {} blocked ({:.1}% blocked), density={:.1}%",
            self.x_coords_count,
            self.y_coords_count,
            grid_size,
            self.blocked_cells_count,
            if grid_size > 0 {
                self.blocked_cells_count as f64 / grid_size as f64 * 100.0
            } else {
                0.0
            },
            density
        );
    }
}

// ============================================================================
// Spatial Index for Obstacles (R-tree based)
// ============================================================================

/// An obstacle entry in the R-tree spatial index.
/// Stores the obstacle index and its axis-aligned bounding box.
#[derive(Debug, Clone)]
struct ObstacleEntry {
    /// Index into the obstacles slice
    index: usize,
    /// Bounding box for R-tree queries
    aabb: AABB<[f64; 2]>,
}

impl rstar::RTreeObject for ObstacleEntry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.aabb
    }
}

/// R-tree spatial index for fast obstacle queries.
///
/// Uses an R-tree for efficient 2D range queries. When checking if an edge
/// intersects obstacles, we query the R-tree with the edge's bounding box
/// to find only potentially intersecting obstacles.
///
/// This reduces obstacle intersection checks from O(V × O) to O(V × log(O) + k)
/// where k is the number of actually intersecting obstacles.
struct ObstacleSpatialIndex {
    /// R-tree for spatial queries (stores obstacle index and bounds)
    rtree: RTree<ObstacleEntry>,
}

/// An exit path entry in the R-tree spatial index.
/// Exit paths are axis-aligned line segments from port position to obstacle edge + buffer.
#[derive(Debug, Clone)]
struct ExitPathEntry {
    /// The obstacle ID to exclude when an edge overlaps this exit path
    obstacle_id: String,
    /// Port X coordinate (for vertical exit paths)
    port_x: f64,
    /// Port Y coordinate (for horizontal exit paths)
    port_y: f64,
    /// Whether this is a vertical exit path (Up/Down) or horizontal (Left/Right)
    is_vertical: bool,
    /// Bounding box of the exit path segment
    aabb: AABB<[f64; 2]>,
}

impl rstar::RTreeObject for ExitPathEntry {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.aabb
    }
}

/// R-tree spatial index for exit paths.
struct ExitPathSpatialIndex {
    rtree: RTree<ExitPathEntry>,
}

impl ExitPathSpatialIndex {
    /// Build an R-tree of exit paths from ports attached to obstacles.
    fn new(input: &RouterInput, buffer: f64) -> Self {
        let mut entries = Vec::new();

        for port in &input.ports {
            let obstacle_id = match &port.obstacle_id {
                Some(id) => id,
                None => continue,
            };
            let obstacle = match input.get_obstacle(obstacle_id) {
                Some(o) => o,
                None => continue,
            };
            let exit_dir = match port.primary_direction() {
                Some(d) => d,
                None => continue,
            };

            let exit_point = VisibilityGraph::get_port_exit_point(port, obstacle, buffer);

            // Create bounding box for the exit path segment
            let (min_x, max_x, min_y, max_y) = match exit_dir {
                Direction::Up | Direction::Down => {
                    // Vertical exit path
                    let min_y = port.position.y.min(exit_point.y);
                    let max_y = port.position.y.max(exit_point.y);
                    // Use a thin AABB around the X coordinate
                    (port.position.x - 1e-6, port.position.x + 1e-6, min_y, max_y)
                }
                Direction::Left | Direction::Right => {
                    // Horizontal exit path
                    let min_x = port.position.x.min(exit_point.x);
                    let max_x = port.position.x.max(exit_point.x);
                    // Use a thin AABB around the Y coordinate
                    (min_x, max_x, port.position.y - 1e-6, port.position.y + 1e-6)
                }
            };

            entries.push(ExitPathEntry {
                obstacle_id: obstacle_id.clone(),
                port_x: port.position.x,
                port_y: port.position.y,
                is_vertical: matches!(exit_dir, Direction::Up | Direction::Down),
                aabb: AABB::from_corners([min_x, min_y], [max_x, max_y]),
            });
        }

        Self {
            rtree: RTree::bulk_load(entries),
        }
    }

    /// Find if an edge overlaps any exit path. Returns the obstacle ID to exclude.
    fn find_overlapping_exit_path(&self, from: &Point, to: &Point) -> Option<String> {
        let is_horizontal = (from.y - to.y).abs() < 1e-9;
        let is_vertical = (from.x - to.x).abs() < 1e-9;

        // Create query AABB for the edge
        let min_x = from.x.min(to.x);
        let max_x = from.x.max(to.x);
        let min_y = from.y.min(to.y);
        let max_y = from.y.max(to.y);
        let query_aabb = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        for entry in self.rtree.locate_in_envelope_intersecting(&query_aabb) {
            // Additional precise check based on exit path orientation
            if entry.is_vertical && is_vertical {
                // Both are vertical - check if X coordinates match
                if (from.x - entry.port_x).abs() < 1e-6 {
                    return Some(entry.obstacle_id.clone());
                }
            } else if !entry.is_vertical && is_horizontal {
                // Both are horizontal - check if Y coordinates match
                if (from.y - entry.port_y).abs() < 1e-6 {
                    return Some(entry.obstacle_id.clone());
                }
            }
        }

        None
    }
}

impl ObstacleSpatialIndex {
    /// Build an R-tree spatial index from obstacles.
    fn new(obstacles: &[Obstacle]) -> Self {
        let entries: Vec<ObstacleEntry> = obstacles
            .iter()
            .enumerate()
            .map(|(index, obs)| ObstacleEntry {
                index,
                aabb: AABB::from_corners(
                    [obs.bounds.min_x, obs.bounds.min_y],
                    [obs.bounds.max_x, obs.bounds.max_y],
                ),
            })
            .collect();

        Self {
            rtree: RTree::bulk_load(entries),
        }
    }

    /// Query obstacles that could intersect an edge from `from` to `to`.
    fn query_edge(
        &self,
        from: &Point,
        to: &Point,
        buffer: f64,
    ) -> impl Iterator<Item = usize> + '_ {
        let min_x = from.x.min(to.x) - buffer;
        let max_x = from.x.max(to.x) + buffer;
        let min_y = from.y.min(to.y) - buffer;
        let max_y = from.y.max(to.y) + buffer;
        let query_aabb = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        self.rtree
            .locate_in_envelope_intersecting(&query_aabb)
            .map(|entry| entry.index)
    }
}

/// A vertex in the visibility graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VertexId(pub usize);

/// A vertex in the visibility graph with position and metadata.
#[derive(Debug, Clone)]
pub struct Vertex {
    pub id: VertexId,
    pub position: Point,
    /// If this vertex corresponds to a port, the port ID.
    pub port_id: Option<String>,
    /// Grid indices (x_index, y_index) for quick lookup.
    pub grid_indices: (usize, usize),
}

/// An edge in the visibility graph.
#[derive(Debug, Clone)]
pub struct Edge {
    pub from: VertexId,
    pub to: VertexId,
    pub direction: Direction,
    pub distance: f64,
}

/// Context for computing edges lazily during pathfinding.
///
/// Instead of pre-computing all edges upfront (which is O(V) even when A* only explores
/// a small fraction of the graph), we store the information needed to compute edges
/// on-demand during pathfinding.
#[derive(Debug, Clone)]
pub struct LazyEdgeContext {
    /// R-tree for obstacle spatial queries
    obstacle_rtree: RTree<ObstacleEntry>,
    /// R-tree for exit path queries
    exit_path_rtree: RTree<ExitPathEntry>,
    /// Obstacles with their bounds
    obstacles: Vec<Obstacle>,
    /// Port ID to obstacle ID mapping
    port_obstacle_ids: HashMap<String, String>,
    /// Port visibility constraints
    port_visibility: HashMap<String, ConnDirFlags>,
    /// Buffer distance
    buffer: f64,
}

/// The visibility graph for orthogonal routing.
#[derive(Debug, Clone)]
pub struct VisibilityGraph {
    /// All vertices in the graph.
    pub vertices: Vec<Vertex>,
    /// Adjacency list: vertex_id -> list of outgoing edges.
    /// This may be empty if using lazy edge computation.
    pub adjacency: HashMap<VertexId, Vec<Edge>>,
    /// Map from port ID to vertex ID.
    pub port_to_vertex: HashMap<String, VertexId>,
    /// Sorted unique X coordinates.
    pub x_coords: Vec<f64>,
    /// Sorted unique Y coordinates.
    pub y_coords: Vec<f64>,
    /// Map from grid indices (x_idx, y_idx) to vertex ID.
    grid_to_vertex: HashMap<(usize, usize), VertexId>,
    /// Context for lazy edge computation (optional).
    lazy_context: Option<LazyEdgeContext>,
}

impl VisibilityGraph {
    /// Build a visibility graph from the router input.
    pub fn build(input: &RouterInput, config: &RouterConfig) -> Self {
        let (graph, timings) = Self::build_with_timings(input, config);
        let stats = graph.stats();
        timings.log(stats.vertex_count, stats.edge_count);
        graph
    }

    /// Build a visibility graph and return timing statistics.
    pub fn build_with_timings(
        input: &RouterInput,
        config: &RouterConfig,
    ) -> (Self, VisibilityBuildTimings) {
        let total_start = Instant::now();
        let mut timings = VisibilityBuildTimings::default();
        let buffer = config.shape_buffer_distance;

        // Step 1: Collect unique X and Y coordinates
        let t = Instant::now();
        let (x_coords, y_coords) = Self::collect_coordinates(input, config);
        timings.collect_coords = t.elapsed();

        // Step 2: Determine which grid cells are blocked by obstacles (including buffer zones)
        let t = Instant::now();
        let blocked_cells =
            Self::find_blocked_cells(&x_coords, &y_coords, &input.obstacles, buffer);
        timings.blocked_cells = t.elapsed();

        // Step 3: Create vertices at unblocked grid intersections
        let t = Instant::now();
        let (vertices, grid_to_vertex, port_to_vertex) =
            Self::create_vertices(&x_coords, &y_coords, &blocked_cells, input, buffer);
        timings.create_vertices = t.elapsed();

        // Step 4: Build R-tree spatial indices
        let t = Instant::now();
        let obstacle_rtree = ObstacleSpatialIndex::new(&input.obstacles);
        let exit_path_rtree = ExitPathSpatialIndex::new(input, buffer);
        timings.build_rtrees = t.elapsed();

        // Step 5: Create edges between adjacent vertices
        let t = Instant::now();
        let adjacency = Self::create_edges_with_rtrees(
            &vertices,
            &grid_to_vertex,
            &x_coords,
            &y_coords,
            &input.obstacles,
            buffer,
            input,
            &obstacle_rtree,
            &exit_path_rtree,
        );
        timings.create_edges = t.elapsed();

        timings.total = total_start.elapsed();

        // Record grid stats
        timings.x_coords_count = x_coords.len();
        timings.y_coords_count = y_coords.len();
        timings.blocked_cells_count = blocked_cells.len();

        // Log edge counts for port vertices
        for (port_id, &vertex_id) in &port_to_vertex {
            let edge_count = adjacency.get(&vertex_id).map(|e| e.len()).unwrap_or(0);
            if edge_count == 0 {
                // Debug: why does this port have no edges?
                if let Some(port) = input.get_port(port_id) {
                    let vertex = &vertices[vertex_id.0];
                    log::warn!(
                        "[visibility] Port '{}' at ({:.2},{:.2}) has NO edges! visibility={:?}, obstacle_id={:?}",
                        port_id,
                        vertex.position.x,
                        vertex.position.y,
                        port.visibility,
                        port.obstacle_id
                    );
                    // Check what's around this vertex
                    let (xi, yi) = vertex.grid_indices;
                    log::warn!(
                        "[visibility]   Grid indices: ({}, {}), x_coord={:.2}, y_coord={:.2}",
                        xi,
                        yi,
                        x_coords.get(xi).copied().unwrap_or(0.0),
                        y_coords.get(yi).copied().unwrap_or(0.0)
                    );
                    // Check adjacent positions
                    for (dir, (dx, dy)) in [
                        (Direction::Right, (1i32, 0i32)),
                        (Direction::Left, (-1i32, 0i32)),
                        (Direction::Down, (0i32, 1i32)),
                        (Direction::Up, (0i32, -1i32)),
                    ] {
                        let nx = xi as i32 + dx;
                        let ny = yi as i32 + dy;
                        let vis_allows = port.visibility.allows(dir);
                        if nx < 0
                            || ny < 0
                            || (nx as usize) >= x_coords.len()
                            || (ny as usize) >= y_coords.len()
                        {
                            log::warn!(
                                "[visibility]   {:?}: OUT OF BOUNDS (grid {}x{}), visibility allows={}",
                                dir,
                                x_coords.len(),
                                y_coords.len(),
                                vis_allows
                            );
                        } else {
                            let has_neighbor =
                                grid_to_vertex.contains_key(&(nx as usize, ny as usize));
                            let neighbor_x = x_coords.get(nx as usize).copied().unwrap_or(0.0);
                            let neighbor_y = y_coords.get(ny as usize).copied().unwrap_or(0.0);
                            log::warn!(
                                "[visibility]   {:?}: neighbor at ({:.2},{:.2}) exists={}, visibility allows={}",
                                dir,
                                neighbor_x,
                                neighbor_y,
                                has_neighbor,
                                vis_allows
                            );
                        }
                    }
                } else {
                    // Port not found in input - this shouldn't happen!
                    log::warn!(
                        "[visibility] Port '{}' vertex has NO edges - cannot route! (port not found in input, input has {} ports)",
                        port_id,
                        input.ports.len()
                    );
                    // Log first few port IDs for debugging
                    for (i, p) in input.ports.iter().take(5).enumerate() {
                        log::warn!("[visibility]   Sample port {}: '{}'", i, p.id);
                    }
                }
            } else {
                log::debug!(
                    "[visibility] Port '{}' vertex has {} edges",
                    port_id,
                    edge_count
                );
            }
        }

        let graph = Self {
            vertices,
            adjacency,
            port_to_vertex,
            x_coords,
            y_coords,
            grid_to_vertex,
            lazy_context: None,
        };

        (graph, timings)
    }

    /// Build a visibility graph with lazy edge computation.
    ///
    /// This is faster than `build()` for large graphs because edges are computed
    /// on-demand during pathfinding rather than all upfront. The tradeoff is that
    /// each edge lookup is slightly slower, but A* typically only explores a small
    /// fraction of the graph.
    pub fn build_lazy(input: &RouterInput, config: &RouterConfig) -> Self {
        let (graph, timings) = Self::build_lazy_with_timings(input, config);
        let stats = graph.stats();
        timings.log(stats.vertex_count, stats.edge_count);
        graph
    }

    /// Build a visibility graph with lazy edge computation and return timing statistics.
    pub fn build_lazy_with_timings(
        input: &RouterInput,
        config: &RouterConfig,
    ) -> (Self, VisibilityBuildTimings) {
        let total_start = Instant::now();
        let mut timings = VisibilityBuildTimings::default();
        let buffer = config.shape_buffer_distance;

        // Step 1: Collect unique X and Y coordinates
        let t = Instant::now();
        let (x_coords, y_coords) = Self::collect_coordinates(input, config);
        timings.collect_coords = t.elapsed();

        // Step 2: Determine which grid cells are blocked by obstacles (including buffer zones)
        let t = Instant::now();
        let blocked_cells =
            Self::find_blocked_cells(&x_coords, &y_coords, &input.obstacles, buffer);
        timings.blocked_cells = t.elapsed();

        // Step 3: Create vertices at unblocked grid intersections
        let t = Instant::now();
        let (vertices, grid_to_vertex, port_to_vertex) =
            Self::create_vertices(&x_coords, &y_coords, &blocked_cells, input, buffer);
        timings.create_vertices = t.elapsed();

        // Step 4: Build R-tree spatial indices and create lazy context
        let t = Instant::now();
        let obstacle_entries: Vec<ObstacleEntry> = input
            .obstacles
            .iter()
            .enumerate()
            .map(|(index, obs)| ObstacleEntry {
                index,
                aabb: AABB::from_corners(
                    [obs.bounds.min_x, obs.bounds.min_y],
                    [obs.bounds.max_x, obs.bounds.max_y],
                ),
            })
            .collect();
        let obstacle_rtree = RTree::bulk_load(obstacle_entries);

        let exit_path_rtree = ExitPathSpatialIndex::new(input, buffer);

        // Build port lookups for lazy context
        let port_obstacle_ids: HashMap<String, String> = input
            .ports
            .iter()
            .filter_map(|p| {
                p.obstacle_id
                    .as_ref()
                    .map(|oid| (p.id.clone(), oid.clone()))
            })
            .collect();
        let port_visibility: HashMap<String, ConnDirFlags> = input
            .ports
            .iter()
            .map(|p| (p.id.clone(), p.visibility))
            .collect();

        let lazy_context = LazyEdgeContext {
            obstacle_rtree,
            exit_path_rtree: exit_path_rtree.rtree,
            obstacles: input.obstacles.clone(),
            port_obstacle_ids,
            port_visibility,
            buffer,
        };
        timings.build_rtrees = t.elapsed();

        // Step 5: Skip eager edge computation - edges will be computed lazily
        timings.create_edges = Duration::ZERO;

        timings.total = total_start.elapsed();

        // Record grid stats
        timings.x_coords_count = x_coords.len();
        timings.y_coords_count = y_coords.len();
        timings.blocked_cells_count = blocked_cells.len();

        let graph = Self {
            vertices,
            adjacency: HashMap::new(), // Empty - edges computed lazily
            port_to_vertex,
            x_coords,
            y_coords,
            grid_to_vertex,
            lazy_context: Some(lazy_context),
        };

        (graph, timings)
    }

    /// Collect unique X and Y coordinates from obstacles and ports.
    fn collect_coordinates(input: &RouterInput, config: &RouterConfig) -> (Vec<f64>, Vec<f64>) {
        let buffer = config.shape_buffer_distance;
        let mut x_set: BTreeSet<OrderedFloat> = BTreeSet::new();
        let mut y_set: BTreeSet<OrderedFloat> = BTreeSet::new();

        // Add obstacle edge coordinates (with buffer)
        for obstacle in &input.obstacles {
            let bounds = &obstacle.bounds;
            x_set.insert(OrderedFloat(bounds.min_x - buffer));
            x_set.insert(OrderedFloat(bounds.max_x + buffer));
            y_set.insert(OrderedFloat(bounds.min_y - buffer));
            y_set.insert(OrderedFloat(bounds.max_y + buffer));
        }

        // Add port positions and routing offset coordinates.
        // We add offsets in ALL directions (not just visibility direction) so that
        // once a route exits a port, it has grid coordinates available to route
        // toward the destination. The visibility constraint only affects the first
        // edge from the port, not the rest of the route.
        let routing_offset = buffer.max(5.0); // At least 5 units offset

        for port in &input.ports {
            x_set.insert(OrderedFloat(port.position.x));
            y_set.insert(OrderedFloat(port.position.y));

            // Add offset coordinates in ALL directions for better routing options
            // The port's visibility constraint will still only allow leaving in
            // the allowed direction, but once on the grid, routes need options.
            x_set.insert(OrderedFloat(port.position.x - routing_offset));
            x_set.insert(OrderedFloat(port.position.x + routing_offset));
            y_set.insert(OrderedFloat(port.position.y - routing_offset));
            y_set.insert(OrderedFloat(port.position.y + routing_offset));

            // For attached ports, also add the exit point (obstacle edge + buffer)
            if let Some(obstacle_id) = &port.obstacle_id {
                if let Some(obstacle) = input.get_obstacle(obstacle_id) {
                    if port.primary_direction().is_some() {
                        let exit_point = Self::get_port_exit_point(port, obstacle, buffer);
                        x_set.insert(OrderedFloat(exit_point.x));
                        y_set.insert(OrderedFloat(exit_point.y));
                    }
                }
            }
        }

        // Add intermediate routing channels between port groups.
        // When ports are spread across a large area with no obstacles in between,
        // we need intermediate coordinates where routes can bend.
        Self::add_intermediate_channels(&mut x_set, &mut y_set, input, config);

        let x_coords: Vec<f64> = x_set.into_iter().map(|f| f.0).collect();
        let y_coords: Vec<f64> = y_set.into_iter().map(|f| f.0).collect();

        (x_coords, y_coords)
    }

    /// Add intermediate routing channels across the routing area.
    ///
    /// This creates a grid of routing channels that allows routes to spread out
    /// and avoid overlapping. More channels = more routing options but slower
    /// pathfinding.
    fn add_intermediate_channels(
        x_set: &mut BTreeSet<OrderedFloat>,
        y_set: &mut BTreeSet<OrderedFloat>,
        input: &RouterInput,
        config: &RouterConfig,
    ) {
        let spacing = config.grid_channel_spacing;
        if spacing <= 0.0 || input.ports.is_empty() {
            return;
        }

        // Find the bounding box of all ports and obstacles
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;

        for port in &input.ports {
            min_x = min_x.min(port.position.x);
            max_x = max_x.max(port.position.x);
            min_y = min_y.min(port.position.y);
            max_y = max_y.max(port.position.y);
        }

        for obstacle in &input.obstacles {
            min_x = min_x.min(obstacle.bounds.min_x);
            max_x = max_x.max(obstacle.bounds.max_x);
            min_y = min_y.min(obstacle.bounds.min_y);
            max_y = max_y.max(obstacle.bounds.max_y);
        }

        // Add some margin around the bounding box
        let margin = config.shape_buffer_distance * 2.0;
        min_x -= margin;
        max_x += margin;
        min_y -= margin;
        max_y += margin;

        // Add evenly-spaced X coordinates
        let x_range = max_x - min_x;
        let num_x_channels = (x_range / spacing).ceil() as usize;
        for i in 0..=num_x_channels {
            let x = min_x + (i as f64) * spacing;
            if x >= min_x && x <= max_x {
                x_set.insert(OrderedFloat(x));
            }
        }

        // Add evenly-spaced Y coordinates
        let y_range = max_y - min_y;
        let num_y_channels = (y_range / spacing).ceil() as usize;
        for i in 0..=num_y_channels {
            let y = min_y + (i as f64) * spacing;
            if y >= min_y && y <= max_y {
                y_set.insert(OrderedFloat(y));
            }
        }

        log::info!(
            "[visibility] Added intermediate channels: {} X lines, {} Y lines (spacing={:.1}, bbox={:.0}x{:.0})",
            num_x_channels + 1,
            num_y_channels + 1,
            spacing,
            x_range,
            y_range
        );
    }

    /// Calculate the exit point for a port attached to an obstacle.
    /// This is where the route will start after leaving the obstacle.
    fn get_port_exit_point(port: &Port, obstacle: &Obstacle, buffer: f64) -> Point {
        let dir = port.primary_direction().unwrap_or(Direction::Right);
        let bounds = &obstacle.bounds;

        match dir {
            Direction::Up => Point::new(port.position.x, bounds.min_y - buffer),
            Direction::Down => Point::new(port.position.x, bounds.max_y + buffer),
            Direction::Left => Point::new(bounds.min_x - buffer, port.position.y),
            Direction::Right => Point::new(bounds.max_x + buffer, port.position.y),
        }
    }

    /// Find grid cells that are blocked by obstacles (including buffer zones).
    ///
    /// A cell is blocked if it falls within `buffer` distance of any obstacle.
    /// This prevents routes from passing too close to obstacles.
    /// Exit corridors and port positions are exempted in `create_vertices`.
    fn find_blocked_cells(
        x_coords: &[f64],
        y_coords: &[f64],
        obstacles: &[Obstacle],
        buffer: f64,
    ) -> HashSet<(usize, usize)> {
        let mut blocked = HashSet::new();

        // For each obstacle, find the range of grid indices it blocks.
        // This is O(obstacles * affected_cells) instead of O(grid_cells * obstacles).
        for obstacle in obstacles {
            let bounds = &obstacle.bounds;
            let min_x = bounds.min_x - buffer;
            let max_x = bounds.max_x + buffer;
            let min_y = bounds.min_y - buffer;
            let max_y = bounds.max_y + buffer;

            // Binary search to find the range of X indices that fall within the buffered bounds
            let xi_start = x_coords.partition_point(|&x| x < min_x);
            let xi_end = x_coords.partition_point(|&x| x <= max_x);

            // Binary search for Y indices
            let yi_start = y_coords.partition_point(|&y| y < min_y);
            let yi_end = y_coords.partition_point(|&y| y <= max_y);

            // Mark all cells in this range as blocked
            for (xi, &x) in x_coords.iter().enumerate().take(xi_end).skip(xi_start) {
                for (yi, &y) in y_coords.iter().enumerate().take(yi_end).skip(yi_start) {
                    // Double-check the point is strictly inside (not on boundary)
                    if x > min_x && x < max_x && y > min_y && y < max_y {
                        blocked.insert((xi, yi));
                    }
                }
            }
        }

        blocked
    }

    /// Check if a point is strictly inside a rectangle (not on boundary).
    #[allow(dead_code)]
    fn point_strictly_inside(point: &Point, rect: &Rect) -> bool {
        point.x > rect.min_x && point.x < rect.max_x && point.y > rect.min_y && point.y < rect.max_y
    }

    /// Create vertices at unblocked grid intersections.
    #[allow(clippy::type_complexity)]
    fn create_vertices(
        x_coords: &[f64],
        y_coords: &[f64],
        blocked_cells: &HashSet<(usize, usize)>,
        input: &RouterInput,
        buffer: f64,
    ) -> (
        Vec<Vertex>,
        HashMap<(usize, usize), VertexId>,
        HashMap<String, VertexId>,
    ) {
        let mut vertices = Vec::new();
        let mut grid_to_vertex = HashMap::new();
        let mut port_to_vertex = HashMap::new();

        // Create a map of port positions for quick lookup
        let mut port_at_position: HashMap<(OrderedFloat, OrderedFloat), &Port> = HashMap::new();
        for port in &input.ports {
            port_at_position.insert(
                (OrderedFloat(port.position.x), OrderedFloat(port.position.y)),
                port,
            );
        }

        // Build a set of positions on exit corridors (from port through buffer zone).
        // These positions must have vertices even if they're blocked by buffer zones.
        // The corridor extends from the port position to the buffer boundary (obstacle edge + buffer).
        let mut exit_corridor_positions: HashSet<(OrderedFloat, OrderedFloat)> = HashSet::new();
        for port in &input.ports {
            if let (Some(obstacle_id), Some(dir)) = (&port.obstacle_id, port.primary_direction()) {
                if let Some(obstacle) = input.get_obstacle(obstacle_id) {
                    // Find the range from port to buffer boundary in the visibility direction
                    let bounds = &obstacle.bounds;
                    match dir {
                        Direction::Up => {
                            // Port exits upward (decreasing Y), corridor from port.y to bounds.min_y - buffer
                            let exit_y = bounds.min_y - buffer;
                            for &y in y_coords.iter() {
                                if y >= exit_y && y <= port.position.y {
                                    exit_corridor_positions
                                        .insert((OrderedFloat(port.position.x), OrderedFloat(y)));
                                }
                            }
                        }
                        Direction::Down => {
                            // Port exits downward (increasing Y), corridor from port.y to bounds.max_y + buffer
                            let exit_y = bounds.max_y + buffer;
                            for &y in y_coords.iter() {
                                if y >= port.position.y && y <= exit_y {
                                    exit_corridor_positions
                                        .insert((OrderedFloat(port.position.x), OrderedFloat(y)));
                                }
                            }
                        }
                        Direction::Left => {
                            // Port exits left (decreasing X), corridor from bounds.min_x - buffer to port.x
                            let exit_x = bounds.min_x - buffer;
                            for &x in x_coords.iter() {
                                if x >= exit_x && x <= port.position.x {
                                    exit_corridor_positions
                                        .insert((OrderedFloat(x), OrderedFloat(port.position.y)));
                                }
                            }
                        }
                        Direction::Right => {
                            // Port exits right (increasing X), corridor from port.x to bounds.max_x + buffer
                            let exit_x = bounds.max_x + buffer;
                            for &x in x_coords.iter() {
                                if x >= port.position.x && x <= exit_x {
                                    exit_corridor_positions
                                        .insert((OrderedFloat(x), OrderedFloat(port.position.y)));
                                }
                            }
                        }
                    }
                }
            }
        }

        log::debug!(
            "[visibility] Built {} exit corridor positions for {} ports",
            exit_corridor_positions.len(),
            input.ports.len()
        );

        for (xi, &x) in x_coords.iter().enumerate() {
            for (yi, &y) in y_coords.iter().enumerate() {
                // Check if this is a port position BEFORE checking if blocked
                let port_id = port_at_position
                    .get(&(OrderedFloat(x), OrderedFloat(y)))
                    .map(|p| p.id.clone());

                // Check if this is on an exit corridor
                let is_exit_corridor =
                    exit_corridor_positions.contains(&(OrderedFloat(x), OrderedFloat(y)));

                // Skip blocked cells UNLESS this is a port position or on an exit corridor
                // Ports and exit corridors must always have vertices
                if blocked_cells.contains(&(xi, yi)) && port_id.is_none() && !is_exit_corridor {
                    continue;
                }

                let id = VertexId(vertices.len());
                let position = Point::new(x, y);

                if let Some(ref pid) = port_id {
                    log::debug!(
                        "[visibility] Creating vertex for port '{}' at ({:.2},{:.2})",
                        pid,
                        x,
                        y
                    );
                    port_to_vertex.insert(pid.clone(), id);
                }

                vertices.push(Vertex {
                    id,
                    position,
                    port_id,
                    grid_indices: (xi, yi),
                });

                grid_to_vertex.insert((xi, yi), id);
            }
        }

        log::info!(
            "[visibility] Created {} port vertices out of {} ports",
            port_to_vertex.len(),
            port_at_position.len()
        );

        // Log any ports that didn't get vertices
        for ((x, y), port) in &port_at_position {
            if !port_to_vertex.contains_key(&port.id) {
                log::warn!(
                    "[visibility] Port '{}' at ({:.2},{:.2}) has no vertex! (possibly blocked)",
                    port.id,
                    x.0,
                    y.0
                );
            }
        }

        (vertices, grid_to_vertex, port_to_vertex)
    }

    /// Create edges between adjacent vertices using pre-built R-tree indices.
    #[allow(clippy::too_many_arguments)]
    fn create_edges_with_rtrees(
        vertices: &[Vertex],
        grid_to_vertex: &HashMap<(usize, usize), VertexId>,
        x_coords: &[f64],
        y_coords: &[f64],
        obstacles: &[Obstacle],
        buffer: f64,
        input: &RouterInput,
        spatial_index: &ObstacleSpatialIndex,
        exit_path_index: &ExitPathSpatialIndex,
    ) -> HashMap<VertexId, Vec<Edge>> {
        let mut adjacency: HashMap<VertexId, Vec<Edge>> = HashMap::new();

        // Initialize empty adjacency lists
        for vertex in vertices {
            adjacency.insert(vertex.id, Vec::new());
        }

        // For each vertex, try to connect to adjacent grid positions
        for vertex in vertices {
            let (xi, yi) = vertex.grid_indices;

            // Check visibility constraints for ports
            let visibility = Self::get_vertex_visibility(vertex, input);
            let is_port = vertex.port_id.is_some();

            // Try connecting in each direction
            let directions = [
                (Direction::Right, (1i32, 0i32)),
                (Direction::Left, (-1i32, 0i32)),
                (Direction::Down, (0i32, 1i32)),
                (Direction::Up, (0i32, -1i32)),
            ];

            for (dir, (dx, dy)) in directions {
                // For port vertices, only allow edges in the visibility direction.
                // For non-port vertices, allow all directions.
                if is_port && !visibility.allows(dir) {
                    continue;
                }

                let nx = xi as i32 + dx;
                let ny = yi as i32 + dy;

                if nx < 0 || ny < 0 {
                    continue;
                }

                let nx = nx as usize;
                let ny = ny as usize;

                if nx >= x_coords.len() || ny >= y_coords.len() {
                    continue;
                }

                if let Some(&neighbor_id) = grid_to_vertex.get(&(nx, ny)) {
                    let neighbor = &vertices[neighbor_id.0];

                    // Get obstacle IDs to exclude:
                    // - If this vertex is a port on an obstacle, exclude that obstacle
                    // - If the neighbor is a port on an obstacle, also exclude that obstacle
                    // - If this edge is on the exit path of a port-on-obstacle, exclude that obstacle
                    let exclude_obstacle_from_vertex = vertex
                        .port_id
                        .as_ref()
                        .and_then(|pid| input.get_port(pid).and_then(|p| p.obstacle_id.as_ref()));
                    let exclude_obstacle_from_neighbor = neighbor
                        .port_id
                        .as_ref()
                        .and_then(|pid| input.get_port(pid).and_then(|p| p.obstacle_id.as_ref()));
                    let exclude_obstacle_from_exit_path = exit_path_index
                        .find_overlapping_exit_path(&vertex.position, &neighbor.position);

                    // Check if edge is blocked by an obstacle (using spatial index)
                    let is_blocked = Self::edge_blocked_with_spatial_index(
                        &vertex.position,
                        &neighbor.position,
                        obstacles,
                        spatial_index,
                        buffer,
                        &[
                            exclude_obstacle_from_vertex,
                            exclude_obstacle_from_neighbor,
                            exclude_obstacle_from_exit_path.as_ref(),
                        ],
                    );

                    if is_blocked {
                        continue;
                    }

                    // Check visibility constraint for the neighbor too (if it's a port)
                    let neighbor_visibility = Self::get_vertex_visibility(neighbor, input);
                    if !neighbor_visibility.allows(dir.opposite()) {
                        continue;
                    }

                    let distance = vertex.position.manhattan_distance(&neighbor.position);

                    adjacency.get_mut(&vertex.id).unwrap().push(Edge {
                        from: vertex.id,
                        to: neighbor_id,
                        direction: dir,
                        distance,
                    });
                }
            }
        }

        adjacency
    }

    /// Check if an edge is blocked by an obstacle using R-tree spatial indexing.
    ///
    /// This is much faster than checking all obstacles because the R-tree
    /// query only returns obstacles whose bounding box intersects the edge's
    /// bounding box.
    fn edge_blocked_with_spatial_index(
        from: &Point,
        to: &Point,
        obstacles: &[Obstacle],
        spatial_index: &ObstacleSpatialIndex,
        buffer: f64,
        exclude_obstacle_ids: &[Option<&String>],
    ) -> bool {
        // Query R-tree for obstacles near this edge
        for idx in spatial_index.query_edge(from, to, buffer) {
            let obstacle = &obstacles[idx];

            // Skip excluded obstacles
            let should_skip = exclude_obstacle_ids
                .iter()
                .any(|opt_id| opt_id.is_some_and(|id| &obstacle.id == id));
            if should_skip {
                continue;
            }

            // Check precise intersection
            if Self::segment_intersects_buffered_rect(from, to, &obstacle.bounds, buffer) {
                return true;
            }
        }

        false
    }

    /// Get visibility constraints for a vertex.
    fn get_vertex_visibility(vertex: &Vertex, input: &RouterInput) -> ConnDirFlags {
        if let Some(ref port_id) = vertex.port_id {
            if let Some(port) = input.get_port(port_id) {
                return port.visibility;
            }
        }
        // Non-port vertices can connect in all directions
        ConnDirFlags::ALL
    }

    /// Check if a line segment intersects a rectangle (with buffer zone).
    ///
    /// For the perpendicular dimension (Y for horizontal edges, X for vertical edges),
    /// we use a small margin (1 unit) to prevent routes from being flush against
    /// obstacle edges, while still allowing routes to pass through corridors.
    ///
    /// For the parallel dimension, we block edges that:
    /// 1. Enter the actual obstacle bounds, OR
    /// 2. Start or end inside the buffer zone (not just pass through)
    ///
    /// This allows "corridor routing" - edges can pass through narrow gaps between
    /// obstacles even when buffer zones overlap, as long as the edge doesn't actually
    /// enter either obstacle's bounds.
    fn segment_intersects_buffered_rect(p1: &Point, p2: &Point, rect: &Rect, buffer: f64) -> bool {
        // For orthogonal segments, this is simpler
        let is_horizontal = (p1.y - p2.y).abs() < 1e-9;
        let is_vertical = (p1.x - p2.x).abs() < 1e-9;

        // Use full buffer for parallel dimension, small margin for perpendicular.
        // The small perpendicular margin (1 unit) prevents flush-against-edge routes
        // while still allowing corridor routing and ports near obstacles.
        let perp_margin = 1.0;

        if is_horizontal {
            let y = p1.y;
            let (min_x, max_x) = if p1.x < p2.x {
                (p1.x, p2.x)
            } else {
                (p2.x, p1.x)
            };

            // For horizontal edge: check if Y is inside obstacle bounds (with small margin)
            // This prevents routes at Y = rect.min_y or Y = rect.max_y (flush against edge)
            let y_blocked = y >= (rect.min_y - perp_margin) && y <= (rect.max_y + perp_margin);

            if !y_blocked {
                return false;
            }

            // For X range: block if edge enters actual bounds OR starts/ends in buffer zone
            // But allow edges that merely pass THROUGH the buffer zone (corridor routing)
            let buffered_min_x = rect.min_x - buffer;
            let buffered_max_x = rect.max_x + buffer;

            // Does edge enter actual obstacle bounds?
            let enters_actual = max_x > rect.min_x && min_x < rect.max_x;

            // Does edge START inside buffer zone (between buffer edge and actual edge)?
            let starts_in_buffer =
                min_x >= buffered_min_x && min_x < rect.min_x && max_x > rect.min_x;

            // Does edge END inside buffer zone?
            let ends_in_buffer =
                max_x <= buffered_max_x && max_x > rect.max_x && min_x < rect.max_x;

            enters_actual || starts_in_buffer || ends_in_buffer
        } else if is_vertical {
            let x = p1.x;
            let (min_y, max_y) = if p1.y < p2.y {
                (p1.y, p2.y)
            } else {
                (p2.y, p1.y)
            };

            // For vertical edge: check if X is inside obstacle bounds (with small margin)
            // This prevents routes at X = rect.min_x or X = rect.max_x (flush against edge)
            let x_blocked = x >= (rect.min_x - perp_margin) && x <= (rect.max_x + perp_margin);

            if !x_blocked {
                return false;
            }

            // For Y range: block if edge enters actual bounds OR starts/ends in buffer zone
            // But allow edges that merely pass THROUGH the buffer zone (corridor routing)
            let buffered_min_y = rect.min_y - buffer;
            let buffered_max_y = rect.max_y + buffer;

            // Does edge enter actual obstacle bounds?
            let enters_actual = max_y > rect.min_y && min_y < rect.max_y;

            // Does edge START inside buffer zone (between buffer edge and actual edge)?
            let starts_in_buffer =
                min_y >= buffered_min_y && min_y < rect.min_y && max_y > rect.min_y;

            // Does edge END inside buffer zone?
            let ends_in_buffer =
                max_y <= buffered_max_y && max_y > rect.max_y && min_y < rect.max_y;

            enters_actual || starts_in_buffer || ends_in_buffer
        } else {
            // Non-orthogonal segment (shouldn't happen in our case)
            false
        }
    }

    /// Get a vertex by its ID.
    pub fn get_vertex(&self, id: VertexId) -> Option<&Vertex> {
        self.vertices.get(id.0)
    }

    /// Get edges from a vertex.
    pub fn get_edges(&self, id: VertexId) -> &[Edge] {
        self.adjacency.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Check if this graph uses lazy edge computation.
    pub fn is_lazy(&self) -> bool {
        self.lazy_context.is_some()
    }

    /// Compute edges for a vertex on demand (for lazy graphs).
    ///
    /// This is called by the pathfinder when using a lazy graph. It computes
    /// edges to adjacent grid cells, checking obstacle blocking dynamically.
    pub fn compute_edges_lazy(&self, id: VertexId) -> Vec<Edge> {
        let ctx = match &self.lazy_context {
            Some(c) => c,
            None => return self.get_edges(id).to_vec(),
        };

        let vertex = match self.get_vertex(id) {
            Some(v) => v,
            None => return Vec::new(),
        };

        let (xi, yi) = vertex.grid_indices;
        let mut edges = Vec::with_capacity(4);

        // Check visibility constraints for ports
        let visibility = vertex
            .port_id
            .as_ref()
            .and_then(|pid| ctx.port_visibility.get(pid))
            .copied()
            .unwrap_or(ConnDirFlags::ALL);
        let is_port = vertex.port_id.is_some();

        // Get obstacle ID to exclude for this vertex (if it's a port on an obstacle)
        let exclude_obstacle_from_vertex = vertex
            .port_id
            .as_ref()
            .and_then(|pid| ctx.port_obstacle_ids.get(pid));

        // Try connecting in each direction
        let directions = [
            (Direction::Right, (1i32, 0i32)),
            (Direction::Left, (-1i32, 0i32)),
            (Direction::Down, (0i32, 1i32)),
            (Direction::Up, (0i32, -1i32)),
        ];

        for (dir, (dx, dy)) in directions {
            // For port vertices, only allow edges in the visibility direction
            if is_port && !visibility.allows(dir) {
                continue;
            }

            let nx = xi as i32 + dx;
            let ny = yi as i32 + dy;

            if nx < 0 || ny < 0 {
                continue;
            }

            let nx = nx as usize;
            let ny = ny as usize;

            if nx >= self.x_coords.len() || ny >= self.y_coords.len() {
                continue;
            }

            let neighbor_id = match self.grid_to_vertex.get(&(nx, ny)) {
                Some(&id) => id,
                None => continue,
            };

            let neighbor = match self.get_vertex(neighbor_id) {
                Some(v) => v,
                None => continue,
            };

            // Get obstacle ID to exclude for neighbor
            let exclude_obstacle_from_neighbor = neighbor
                .port_id
                .as_ref()
                .and_then(|pid| ctx.port_obstacle_ids.get(pid));

            // Check if edge is on an exit path
            let exclude_obstacle_from_exit_path =
                self.find_overlapping_exit_path_lazy(ctx, &vertex.position, &neighbor.position);

            // Check if edge is blocked by an obstacle
            let is_blocked = self.edge_blocked_lazy(
                ctx,
                &vertex.position,
                &neighbor.position,
                &[
                    exclude_obstacle_from_vertex,
                    exclude_obstacle_from_neighbor,
                    exclude_obstacle_from_exit_path.as_ref(),
                ],
            );

            if is_blocked {
                continue;
            }

            // Check visibility constraint for the neighbor too (if it's a port)
            let neighbor_visibility = neighbor
                .port_id
                .as_ref()
                .and_then(|pid| ctx.port_visibility.get(pid))
                .copied()
                .unwrap_or(ConnDirFlags::ALL);
            if !neighbor_visibility.allows(dir.opposite()) {
                continue;
            }

            let distance = vertex.position.manhattan_distance(&neighbor.position);

            edges.push(Edge {
                from: id,
                to: neighbor_id,
                direction: dir,
                distance,
            });
        }

        edges
    }

    /// Check if an edge overlaps any exit path (lazy version).
    fn find_overlapping_exit_path_lazy(
        &self,
        ctx: &LazyEdgeContext,
        from: &Point,
        to: &Point,
    ) -> Option<String> {
        let is_horizontal = (from.y - to.y).abs() < 1e-9;
        let is_vertical = (from.x - to.x).abs() < 1e-9;

        let min_x = from.x.min(to.x);
        let max_x = from.x.max(to.x);
        let min_y = from.y.min(to.y);
        let max_y = from.y.max(to.y);
        let query_aabb = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        for entry in ctx
            .exit_path_rtree
            .locate_in_envelope_intersecting(&query_aabb)
        {
            let matches = (entry.is_vertical
                && is_vertical
                && (from.x - entry.port_x).abs() < 1e-6)
                || (!entry.is_vertical && is_horizontal && (from.y - entry.port_y).abs() < 1e-6);
            if matches {
                return Some(entry.obstacle_id.clone());
            }
        }

        None
    }

    /// Check if an edge is blocked by an obstacle (lazy version).
    fn edge_blocked_lazy(
        &self,
        ctx: &LazyEdgeContext,
        from: &Point,
        to: &Point,
        exclude_obstacle_ids: &[Option<&String>],
    ) -> bool {
        let min_x = from.x.min(to.x) - ctx.buffer;
        let max_x = from.x.max(to.x) + ctx.buffer;
        let min_y = from.y.min(to.y) - ctx.buffer;
        let max_y = from.y.max(to.y) + ctx.buffer;
        let query_aabb = AABB::from_corners([min_x, min_y], [max_x, max_y]);

        for entry in ctx
            .obstacle_rtree
            .locate_in_envelope_intersecting(&query_aabb)
        {
            let obstacle = &ctx.obstacles[entry.index];

            // Skip excluded obstacles
            let should_skip = exclude_obstacle_ids
                .iter()
                .any(|opt_id| opt_id.is_some_and(|id| &obstacle.id == id));
            if should_skip {
                continue;
            }

            // Check precise intersection
            if Self::segment_intersects_buffered_rect(from, to, &obstacle.bounds, ctx.buffer) {
                return true;
            }
        }

        false
    }

    /// Get the vertex ID for a port.
    pub fn get_port_vertex(&self, port_id: &str) -> Option<VertexId> {
        self.port_to_vertex.get(port_id).copied()
    }

    /// Get the vertex at a specific grid position.
    pub fn get_vertex_at_grid(&self, x_idx: usize, y_idx: usize) -> Option<VertexId> {
        self.grid_to_vertex.get(&(x_idx, y_idx)).copied()
    }

    /// Find the nearest vertex to a point.
    pub fn nearest_vertex(&self, point: &Point) -> Option<VertexId> {
        self.vertices
            .iter()
            .min_by(|a, b| {
                let dist_a = a.position.manhattan_distance(point);
                let dist_b = b.position.manhattan_distance(point);
                dist_a.partial_cmp(&dist_b).unwrap()
            })
            .map(|v| v.id)
    }

    /// Get statistics about the graph.
    pub fn stats(&self) -> GraphStats {
        let edge_count: usize = self.adjacency.values().map(|edges| edges.len()).sum();
        GraphStats {
            vertex_count: self.vertices.len(),
            edge_count: edge_count / 2, // Each edge is stored twice (bidirectional)
            x_lines: self.x_coords.len(),
            y_lines: self.y_coords.len(),
        }
    }
}

/// Statistics about the visibility graph.
#[derive(Debug, Clone)]
pub struct GraphStats {
    pub vertex_count: usize,
    pub edge_count: usize,
    pub x_lines: usize,
    pub y_lines: usize,
}

/// Wrapper for f64 that implements Ord for use in BTreeSet.
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedFloat(f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl std::hash::Hash for OrderedFloat {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouterConfig;

    #[test]
    fn test_empty_graph() {
        let input = RouterInput::new();
        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        assert_eq!(graph.vertices.len(), 0);
    }

    #[test]
    fn test_single_obstacle() {
        let mut input = RouterInput::new();
        input.add_obstacle(Obstacle::from_xywh("obs1", 50.0, 50.0, 100.0, 100.0));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        // Should have vertices at the buffered corners
        let stats = graph.stats();
        assert!(stats.vertex_count > 0);
        assert_eq!(stats.x_lines, 2); // left-buffer and right+buffer
        assert_eq!(stats.y_lines, 2); // top-buffer and bottom+buffer
    }

    #[test]
    fn test_two_ports() {
        let mut input = RouterInput::new();
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(200.0, 50.0), ConnDirFlags::LEFT));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        // Should have vertices for both ports
        assert!(graph.get_port_vertex("p1").is_some());
        assert!(graph.get_port_vertex("p2").is_some());

        // Ports should be connected (direct line of sight)
        let p1_vertex = graph.get_port_vertex("p1").unwrap();
        let edges = graph.get_edges(p1_vertex);
        assert!(!edges.is_empty(), "Port p1 should have edges");
    }

    #[test]
    fn test_port_visibility() {
        let mut input = RouterInput::new();
        // Port that only allows RIGHT direction
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        // Port that only allows LEFT direction
        input.add_port(Port::new("p2", Point::new(100.0, 50.0), ConnDirFlags::LEFT));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        let p1_vertex = graph.get_port_vertex("p1").unwrap();
        let edges = graph.get_edges(p1_vertex);

        // p1 should only have edges going right
        for edge in edges {
            assert_eq!(edge.direction, Direction::Right);
        }
    }

    #[test]
    fn test_obstacle_blocking() {
        let mut input = RouterInput::new();
        input.add_obstacle(Obstacle::from_xywh("obs1", 50.0, 25.0, 100.0, 50.0));
        input.add_port(Port::new("p1", Point::new(0.0, 50.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(200.0, 50.0), ConnDirFlags::LEFT));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        // There should be no direct path between ports (obstacle in the way)
        let p1_vertex = graph.get_port_vertex("p1").unwrap();
        let p2_vertex = graph.get_port_vertex("p2").unwrap();

        // Check that p1 doesn't directly connect to p2
        let edges = graph.get_edges(p1_vertex);
        let direct_connection = edges.iter().any(|e| e.to == p2_vertex);
        assert!(
            !direct_connection,
            "Should not have direct connection through obstacle"
        );
    }

    #[test]
    fn test_port_visibility_right_only() {
        // A port with only RIGHT visibility should only have edges going right
        let mut input = RouterInput::new();
        input.add_port(Port::new(
            "p1",
            Point::new(20.0, 120.0),
            ConnDirFlags::RIGHT,
        ));
        input.add_port(Port::new(
            "p2",
            Point::new(100.0, 120.0),
            ConnDirFlags::LEFT,
        ));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        let p1_vertex = graph.get_port_vertex("p1").unwrap();
        let edges = graph.get_edges(p1_vertex);

        // Print edges for debugging
        let vertex = graph.get_vertex(p1_vertex).unwrap();
        println!(
            "Port p1 at ({}, {}) with RIGHT visibility has {} edges:",
            vertex.position.x,
            vertex.position.y,
            edges.len()
        );
        for edge in edges {
            let neighbor = graph.get_vertex(edge.to).unwrap();
            println!(
                "  -> ({}, {}) direction={:?}",
                neighbor.position.x, neighbor.position.y, edge.direction
            );
        }

        // All edges from p1 must be going RIGHT
        for edge in edges {
            assert_eq!(
                edge.direction,
                Direction::Right,
                "Port with RIGHT-only visibility should only have RIGHT edges, but found {:?}",
                edge.direction
            );
        }
    }

    #[test]
    fn test_grid_of_obstacles_port_visibility() {
        // Same setup as snapshot test grid_of_obstacles
        let mut input = RouterInput::new();

        // Create a 3x3 grid of obstacles
        for row in 0..3 {
            for col in 0..3 {
                let x = 50.0 + col as f64 * 80.0;
                let y = 50.0 + row as f64 * 70.0;
                input.add_obstacle(Obstacle::from_xywh(
                    format!("obs_{}_{}", row, col),
                    x,
                    y,
                    50.0,
                    40.0,
                ));
            }
        }

        // Add ports on the sides
        input.add_port(Port::new(
            "p_left",
            Point::new(20.0, 120.0),
            ConnDirFlags::RIGHT,
        ));
        input.add_port(Port::new(
            "p_right",
            Point::new(310.0, 120.0),
            ConnDirFlags::LEFT,
        ));

        let config = RouterConfig::default();
        let graph = VisibilityGraph::build(&input, &config);

        let p_left_vertex = graph.get_port_vertex("p_left").unwrap();
        let edges = graph.get_edges(p_left_vertex);

        // Print edges for debugging
        let vertex = graph.get_vertex(p_left_vertex).unwrap();
        println!(
            "Port p_left at ({}, {}) with RIGHT visibility has {} edges:",
            vertex.position.x,
            vertex.position.y,
            edges.len()
        );
        for edge in edges {
            let neighbor = graph.get_vertex(edge.to).unwrap();
            println!(
                "  -> ({}, {}) direction={:?}",
                neighbor.position.x, neighbor.position.y, edge.direction
            );
        }

        // All edges from p_left must be going RIGHT (it has RIGHT-only visibility)
        for edge in edges {
            assert_eq!(
                edge.direction,
                Direction::Right,
                "Port with RIGHT-only visibility should only have RIGHT edges, but found {:?}",
                edge.direction
            );
        }
    }
}
