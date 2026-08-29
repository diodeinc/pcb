// Allow uninlined format args in log statements throughout the crate
#![allow(clippy::uninlined_format_args)]

//! Orthogonal Router - Simple orthogonal routing for schematic diagrams.
//!
//! This crate provides an orthogonal (Manhattan) routing engine optimized for
//! schematic diagrams. It routes connections between ports while avoiding
//! rectangular obstacles, producing paths with only horizontal and vertical
//! segments.
//!
//! # Overview
//!
//! The routing process consists of three main phases:
//!
//! 1. **Visibility Graph Construction**: Build a graph of visibility edges
//!    between obstacle corners and connector endpoints using a sweep line algorithm.
//!
//! 2. **A* Pathfinding**: Find optimal paths through the visibility graph using
//!    A* search with Manhattan distance heuristic and bend penalties.
//!
//! 3. **Route Nudging**: Adjust parallel route segments to maintain minimum
//!    spacing and improve visual clarity.
//!
//! # Example
//!
//! ```
//! use ortho_router::{OrthoRouter, RouterConfig, RouterInput, Obstacle, Port, Connector, Point, ConnDirFlags, Rect};
//!
//! // Create input
//! let mut input = RouterInput::new();
//!
//! // Add an obstacle
//! input.add_obstacle(Obstacle::new("obs1", Rect::new(50.0, 50.0, 100.0, 100.0)));
//!
//! // Add ports with visibility directions
//! input.add_port(Port::new("p1", Point::new(0.0, 75.0), ConnDirFlags::RIGHT));
//! input.add_port(Port::new("p2", Point::new(150.0, 75.0), ConnDirFlags::LEFT));
//!
//! // Add a connector between ports
//! input.add_connector(Connector::new("c1", "p1", "p2"));
//!
//! // Route
//! let router = OrthoRouter::with_defaults();
//! let output = router.route(&input);
//!
//! // Access results
//! for path in &output.paths {
//!     println!("Path {}: {} points", path.connector_id, path.points.len());
//! }
//! ```
//!
//! # Configuration
//!
//! The router can be configured with various parameters:
//!
//! ```
//! use ortho_router::{OrthoRouter, RouterConfig};
//!
//! let config = RouterConfig::default()
//!     .with_segment_penalty(1.0)      // Penalty per bend
//!     .with_shape_buffer_distance(7.0) // Gap around obstacles
//!     .with_ideal_nudging_distance(12.7); // Spacing between routes
//!
//! let router = OrthoRouter::new(config);
//! ```

pub mod config;
pub mod debug;
pub mod improve_crossings;
pub mod junction;
pub mod legalization;
pub mod nudging;
pub mod nudging_libavoid;
pub mod pathfinder;
pub mod render;
pub mod router;
pub mod segment;
pub mod types;
pub mod visibility;
pub mod vpsc;

// Re-export main types for convenience
pub use config::RouterConfig;
pub use junction::{detect_junctions, detect_junctions_with_mapping, Junction};
pub use nudging_libavoid::{NudgingDebugInfo, NudgingPassDebugInfo, SegmentDebugInfo, SegmentType};
pub use pathfinder::{NetAwareContext, PathResult, Pathfinder};
pub use render::{RenderConfig, SegmentTypeColors, SvgRenderer};
pub use router::{OrthoRouter, RoutingSteps};
pub use segment::{Segment, SegmentRegistry};
pub use types::{
    ConnDirFlags, Connector, Direction, ExistingRouteSegment, Obstacle, Point, Port, Rect,
    RoutedPath, RouterInput, RouterOutput, RoutingTiming,
};
pub use visibility::{Edge, GraphStats, Vertex, VertexId, VisibilityGraph};
