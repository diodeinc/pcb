//! Router configuration parameters.
//!
//! These match the configuration used with libavoid in the schematic router.

/// Configuration for the orthogonal router.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Penalty for each bend/segment beyond the first.
    /// Higher values prefer straighter paths.
    /// Default: 1.0 (libavoid C++ default is 10.0)
    pub segment_penalty: f64,

    /// Buffer distance around obstacles.
    /// Routes will stay at least this far from obstacle edges.
    /// Default: 7.0
    pub shape_buffer_distance: f64,

    /// Ideal distance between parallel route segments.
    /// Used during nudging to separate overlapping routes.
    /// Default: 12.7 (libavoid C++ default is 4.0)
    pub ideal_nudging_distance: f64,

    /// Penalty for leaving a port in a non-preferred direction.
    /// Higher values enforce visibility direction constraints more strictly.
    /// Default: 100.0
    pub port_direction_penalty: f64,

    /// Whether to nudge final segments connected to shapes.
    /// When true, the first/last segments can be moved for better layout.
    /// Default: true
    pub nudge_segments_connected_to_shapes: bool,

    /// Whether to nudge colinear segments that touch at endpoints.
    /// Default: true
    pub nudge_touching_colinear_segments: bool,

    // --- Net-aware routing parameters ---
    /// Penalty for routing over segments owned by different nets.
    /// Higher values make the router try harder to avoid overlapping
    /// with other nets' routes.
    /// Set to f64::INFINITY to make it a hard constraint (may fail to route).
    /// Default: 10000.0
    pub different_net_overlap_penalty: f64,

    /// Penalty for bending at a vertex that is already a bend point
    /// for a different net. This prevents different-net routes from
    /// sharing corner points, which would visually appear as a junction.
    /// Set to f64::INFINITY to make it a hard constraint (may fail to route).
    /// Default: 10000.0
    pub different_net_bend_penalty: f64,

    /// Penalty for routing within the grid snap distance of a different net's segment.
    /// This encourages routes to maintain separation even when they don't directly overlap.
    /// Uses `grid_snap_size` as the proximity threshold.
    /// Default: 100.0 (lighter than overlap penalty, but still discouraging)
    pub different_net_proximity_penalty: f64,

    /// Whether to give a bonus for following existing same-net segments.
    /// When true, A* will prefer paths that overlap with already-routed
    /// segments from the same net, creating tree-like structures.
    /// When false, each path is routed independently.
    /// Default: true
    pub same_net_coalescing: bool,

    // --- Grid generation parameters ---
    /// Spacing between intermediate grid lines for routing channels.
    /// Smaller values create more routing options but increase pathfinding time.
    /// Set to 0.0 to disable intermediate grid lines.
    /// Default: 25.0
    pub grid_channel_spacing: f64,

    // --- Post-processing parameters ---
    /// Grid size for snapping final edge points.
    /// Set to 0.0 to disable grid snapping.
    /// Default: 12.7 (50 mil = 1.27mm in 10x router coords)
    pub grid_snap_size: f64,

    // --- Performance tuning ---
    /// Use lazy edge computation for the visibility graph.
    /// When true, edges are computed on-demand during pathfinding rather than
    /// all upfront. This is faster for large graphs where A* only explores a
    /// small fraction of the graph.
    /// Default: true
    pub use_lazy_edges: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            segment_penalty: 1.0,
            shape_buffer_distance: 7.0,
            ideal_nudging_distance: 12.7,
            port_direction_penalty: 100.0,
            nudge_segments_connected_to_shapes: true,
            nudge_touching_colinear_segments: true,
            different_net_overlap_penalty: 10000.0,
            different_net_bend_penalty: 10000.0,
            different_net_proximity_penalty: 100.0,
            same_net_coalescing: false,
            grid_channel_spacing: 50.0,
            grid_snap_size: 12.7,
            use_lazy_edges: true,
        }
    }
}

impl RouterConfig {
    /// Create a new configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder method to set segment penalty.
    pub fn with_segment_penalty(mut self, penalty: f64) -> Self {
        self.segment_penalty = penalty;
        self
    }

    /// Builder method to set shape buffer distance.
    pub fn with_shape_buffer_distance(mut self, distance: f64) -> Self {
        self.shape_buffer_distance = distance;
        self
    }

    /// Builder method to set ideal nudging distance.
    pub fn with_ideal_nudging_distance(mut self, distance: f64) -> Self {
        self.ideal_nudging_distance = distance;
        self
    }

    /// Builder method to set port direction penalty.
    pub fn with_port_direction_penalty(mut self, penalty: f64) -> Self {
        self.port_direction_penalty = penalty;
        self
    }

    /// Builder method to set different-net overlap penalty.
    pub fn with_different_net_overlap_penalty(mut self, penalty: f64) -> Self {
        self.different_net_overlap_penalty = penalty;
        self
    }

    /// Builder method to enable/disable same-net coalescing.
    pub fn with_same_net_coalescing(mut self, enabled: bool) -> Self {
        self.same_net_coalescing = enabled;
        self
    }
}
