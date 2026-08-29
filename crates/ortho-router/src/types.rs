//! Core data types for orthogonal routing.
//!
//! These types represent the input and output of the routing algorithm.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A 2D point with f64 coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Manhattan distance to another point.
    pub fn manhattan_distance(&self, other: &Point) -> f64 {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }

    /// Euclidean distance to another point.
    pub fn euclidean_distance(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({:.2}, {:.2})", self.x, self.y)
    }
}

/// An axis-aligned rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Minimum X coordinate (left edge).
    pub min_x: f64,
    /// Minimum Y coordinate (top edge in Y-down coordinate system).
    pub min_y: f64,
    /// Maximum X coordinate (right edge).
    pub max_x: f64,
    /// Maximum Y coordinate (bottom edge in Y-down coordinate system).
    pub max_y: f64,
}

impl Rect {
    /// Create a rectangle from min/max coordinates.
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    /// Create a rectangle from position and size.
    pub fn from_xywh(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x + width,
            max_y: y + height,
        }
    }

    pub fn width(&self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(&self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn center(&self) -> Point {
        Point::new(
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }

    /// Check if a point is inside or on the boundary of this rectangle.
    pub fn contains(&self, point: &Point) -> bool {
        point.x >= self.min_x
            && point.x <= self.max_x
            && point.y >= self.min_y
            && point.y <= self.max_y
    }

    /// Check if this rectangle intersects another rectangle.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min_x < other.max_x
            && self.max_x > other.min_x
            && self.min_y < other.max_y
            && self.max_y > other.min_y
    }

    /// Expand rectangle by a buffer distance on all sides.
    pub fn expand(&self, buffer: f64) -> Rect {
        Rect {
            min_x: self.min_x - buffer,
            min_y: self.min_y - buffer,
            max_x: self.max_x + buffer,
            max_y: self.max_y + buffer,
        }
    }
}

/// Direction flags for connector endpoint visibility.
///
/// These control which directions a connector can leave/enter a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConnDirFlags(u8);

impl ConnDirFlags {
    pub const NONE: Self = Self(0);
    pub const UP: Self = Self(1);
    pub const DOWN: Self = Self(2);
    pub const LEFT: Self = Self(4);
    pub const RIGHT: Self = Self(8);
    pub const ALL: Self = Self(15);

    /// Check if a specific direction is allowed.
    pub fn allows(&self, dir: Direction) -> bool {
        match dir {
            Direction::Up => self.0 & 1 != 0,
            Direction::Down => self.0 & 2 != 0,
            Direction::Left => self.0 & 4 != 0,
            Direction::Right => self.0 & 8 != 0,
        }
    }

    /// Combine two direction flags.
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl From<Direction> for ConnDirFlags {
    fn from(dir: Direction) -> Self {
        match dir {
            Direction::Up => Self::UP,
            Direction::Down => Self::DOWN,
            Direction::Left => Self::LEFT,
            Direction::Right => Self::RIGHT,
        }
    }
}

/// Cardinal direction for orthogonal routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    /// Get the opposite direction.
    pub fn opposite(&self) -> Self {
        match self {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
            Direction::Left => Direction::Right,
            Direction::Right => Direction::Left,
        }
    }

    /// Check if this direction is horizontal.
    pub fn is_horizontal(&self) -> bool {
        matches!(self, Direction::Left | Direction::Right)
    }

    /// Check if this direction is vertical.
    pub fn is_vertical(&self) -> bool {
        matches!(self, Direction::Up | Direction::Down)
    }
}

/// An obstacle that connectors must route around.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Obstacle {
    /// Unique identifier for this obstacle.
    pub id: String,
    /// Bounding rectangle of the obstacle.
    pub bounds: Rect,
}

impl Obstacle {
    pub fn new(id: impl Into<String>, bounds: Rect) -> Self {
        Self {
            id: id.into(),
            bounds,
        }
    }

    pub fn from_xywh(id: impl Into<String>, x: f64, y: f64, width: f64, height: f64) -> Self {
        Self::new(id, Rect::from_xywh(x, y, width, height))
    }
}

/// A port (connection endpoint) on an obstacle or in free space.
///
/// When a port is attached to an obstacle (via `obstacle_id`), the router will:
/// 1. Draw a straight line from the port position to the obstacle edge
///    (following the visibility direction)
/// 2. Leave at least `shape_buffer_distance` from the obstacle edge
/// 3. Then route normally from that point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Port {
    /// Unique identifier for this port.
    pub id: String,
    /// Position of the port.
    pub position: Point,
    /// Allowed visibility directions for routing.
    /// For ports attached to obstacles, this should be a single direction
    /// pointing away from the obstacle.
    pub visibility: ConnDirFlags,
    /// Optional ID of the obstacle this port is attached to.
    /// When set, the router will route from the obstacle edge, not the port position.
    pub obstacle_id: Option<String>,
}

impl Port {
    pub fn new(id: impl Into<String>, position: Point, visibility: ConnDirFlags) -> Self {
        Self {
            id: id.into(),
            position,
            visibility,
            obstacle_id: None,
        }
    }

    /// Create a port attached to an obstacle.
    ///
    /// The visibility direction should point away from the obstacle (outward).
    pub fn on_obstacle(
        id: impl Into<String>,
        position: Point,
        visibility: ConnDirFlags,
        obstacle_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            position,
            visibility,
            obstacle_id: Some(obstacle_id.into()),
        }
    }

    /// Create a port with visibility in all directions (free-floating port).
    pub fn with_all_visibility(id: impl Into<String>, position: Point) -> Self {
        Self::new(id, position, ConnDirFlags::ALL)
    }

    /// Check if this port is attached to an obstacle.
    pub fn is_attached(&self) -> bool {
        self.obstacle_id.is_some()
    }

    /// Get the primary direction this port faces.
    /// Returns None if multiple or no directions are set.
    pub fn primary_direction(&self) -> Option<Direction> {
        let dirs = [
            (Direction::Up, ConnDirFlags::UP),
            (Direction::Down, ConnDirFlags::DOWN),
            (Direction::Left, ConnDirFlags::LEFT),
            (Direction::Right, ConnDirFlags::RIGHT),
        ];

        let mut found: Option<Direction> = None;
        for (dir, _flag) in dirs {
            if self.visibility.allows(dir) {
                if found.is_some() {
                    return None; // Multiple directions
                }
                found = Some(dir);
            }
        }
        found
    }
}

/// A connector to be routed between two ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connector {
    /// Unique identifier for this connector.
    pub id: String,
    /// Source port ID.
    pub source_port_id: String,
    /// Target port ID.
    pub target_port_id: String,
    /// Net ID this connector belongs to.
    ///
    /// Connectors with the same net_id can share/overlap segments (they're part
    /// of the same logical connection). Connectors with different net_ids should
    /// NOT overlap (they're separate connections).
    pub net_id: Option<String>,
}

impl Connector {
    pub fn new(
        id: impl Into<String>,
        source_port_id: impl Into<String>,
        target_port_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source_port_id: source_port_id.into(),
            target_port_id: target_port_id.into(),
            net_id: None,
        }
    }

    /// Create a connector with a net ID.
    ///
    /// Connectors with the same net_id can share segments, while connectors
    /// with different net_ids will be kept separate.
    pub fn with_net(
        id: impl Into<String>,
        source_port_id: impl Into<String>,
        target_port_id: impl Into<String>,
        net_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source_port_id: source_port_id.into(),
            target_port_id: target_port_id.into(),
            net_id: Some(net_id.into()),
        }
    }

    /// Get the effective net ID for this connector.
    ///
    /// If no net_id is set, returns the connector's own ID (each connector
    /// becomes its own "net").
    pub fn effective_net_id(&self) -> &str {
        self.net_id.as_deref().unwrap_or(&self.id)
    }
}

/// A fixed, already-existing orthogonal segment in the routed document.
///
/// Existing segments are not obstacles: new routes may cross them
/// perpendicularly, but may not overlap or electrically touch a different-net
/// segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExistingRouteSegment {
    /// Unique identifier for this fixed segment.
    pub id: String,
    /// Segment start point.
    pub start: Point,
    /// Segment end point.
    pub end: Point,
    /// Net ID this fixed segment belongs to.
    pub net_id: String,
}

impl ExistingRouteSegment {
    pub fn new(id: impl Into<String>, start: Point, end: Point, net_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            start,
            end,
            net_id: net_id.into(),
        }
    }
}

/// A routed path as a sequence of points.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutedPath {
    /// The connector ID this path belongs to.
    pub connector_id: String,
    /// Sequence of points forming the orthogonal path.
    pub points: Vec<Point>,
    /// The net ID this path belongs to (for nudging and overlap detection).
    pub net_id: String,
    /// Junction points where this path intersects with other paths on the same net.
    /// These are points where 3+ segments meet and at least one passes through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub junction_points: Vec<Point>,
}

impl RoutedPath {
    pub fn new(connector_id: impl Into<String>, points: Vec<Point>) -> Self {
        let connector_id = connector_id.into();
        Self {
            net_id: connector_id.clone(), // Default net_id to connector_id
            connector_id,
            points,
            junction_points: Vec::new(),
        }
    }

    /// Create a routed path with an explicit net ID.
    pub fn with_net(
        connector_id: impl Into<String>,
        points: Vec<Point>,
        net_id: impl Into<String>,
    ) -> Self {
        Self {
            connector_id: connector_id.into(),
            points,
            net_id: net_id.into(),
            junction_points: Vec::new(),
        }
    }

    /// Check if this path is valid (has at least 2 points and is orthogonal).
    pub fn is_valid(&self) -> bool {
        if self.points.len() < 2 {
            return false;
        }
        self.is_orthogonal()
    }

    /// Check if all segments are orthogonal (horizontal or vertical).
    pub fn is_orthogonal(&self) -> bool {
        const TOLERANCE: f64 = 1e-6;
        for i in 1..self.points.len() {
            let prev = &self.points[i - 1];
            let curr = &self.points[i];
            let dx = (prev.x - curr.x).abs();
            let dy = (prev.y - curr.y).abs();
            let is_horizontal = dy < TOLERANCE;
            let is_vertical = dx < TOLERANCE;
            if !is_horizontal && !is_vertical {
                return false;
            }
        }
        true
    }

    /// Calculate total path length.
    pub fn length(&self) -> f64 {
        let mut total = 0.0;
        for i in 1..self.points.len() {
            total += self.points[i - 1].manhattan_distance(&self.points[i]);
        }
        total
    }

    /// Count the number of bends in the path.
    pub fn bend_count(&self) -> usize {
        if self.points.len() < 3 {
            return 0;
        }
        let mut bends = 0;
        for i in 2..self.points.len() {
            let p1 = &self.points[i - 2];
            let p2 = &self.points[i - 1];
            let p3 = &self.points[i];

            // Check if direction changed (horizontal vs vertical)
            let dy1 = p2.y - p1.y;
            let dy2 = p3.y - p2.y;

            let was_horizontal = dy1.abs() < 1e-6;
            let is_horizontal = dy2.abs() < 1e-6;

            if was_horizontal != is_horizontal {
                bends += 1;
            }
        }
        bends
    }
}

/// Input specification for the router.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouterInput {
    /// Obstacles to route around.
    pub obstacles: Vec<Obstacle>,
    /// Connection ports.
    pub ports: Vec<Port>,
    /// Connectors to route.
    pub connectors: Vec<Connector>,
    /// Fixed document segments already present before this routing run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub existing_segments: Vec<ExistingRouteSegment>,
}

impl RouterInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_obstacle(&mut self, obstacle: Obstacle) -> &mut Self {
        self.obstacles.push(obstacle);
        self
    }

    pub fn add_port(&mut self, port: Port) -> &mut Self {
        self.ports.push(port);
        self
    }

    pub fn add_connector(&mut self, connector: Connector) -> &mut Self {
        self.connectors.push(connector);
        self
    }

    pub fn add_existing_segment(&mut self, segment: ExistingRouteSegment) -> &mut Self {
        self.existing_segments.push(segment);
        self
    }

    /// Find a port by ID.
    pub fn get_port(&self, id: &str) -> Option<&Port> {
        self.ports.iter().find(|p| p.id == id)
    }

    /// Find an obstacle by ID.
    pub fn get_obstacle(&self, id: &str) -> Option<&Obstacle> {
        self.obstacles.iter().find(|o| o.id == id)
    }

    /// Get the obstacle that a port is attached to.
    pub fn get_port_obstacle(&self, port: &Port) -> Option<&Obstacle> {
        port.obstacle_id
            .as_ref()
            .and_then(|id| self.get_obstacle(id))
    }
}

/// Output from the router.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RouterOutput {
    /// Routed paths for each connector.
    pub paths: Vec<RoutedPath>,
    /// Junction points where same-net routes meet.
    /// Junctions occur where 3+ segments meet and at least one path passes through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub junctions: Vec<crate::junction::Junction>,
}

impl RouterOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Find a path by connector ID.
    pub fn get_path(&self, connector_id: &str) -> Option<&RoutedPath> {
        self.paths.iter().find(|p| p.connector_id == connector_id)
    }
}

/// Timing breakdown for routing phases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingTiming {
    /// Time to build visibility graph (ms).
    pub visibility_graph_ms: f64,
    /// Time for A* pathfinding (ms).
    pub pathfinding_ms: f64,
    /// Time to improve crossings (ms).
    pub improve_crossings_ms: f64,
    /// Time for route nudging (ms).
    pub nudging_ms: f64,
    /// Time for grid snapping (ms).
    pub grid_snap_ms: f64,
    /// Time for legalization (ms).
    pub legalization_ms: f64,
    /// Total routing time (ms).
    pub total_ms: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_manhattan_distance() {
        let p1 = Point::new(0.0, 0.0);
        let p2 = Point::new(3.0, 4.0);
        assert_eq!(p1.manhattan_distance(&p2), 7.0);
    }

    #[test]
    fn test_rect_contains() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(rect.contains(&Point::new(5.0, 5.0)));
        assert!(rect.contains(&Point::new(0.0, 0.0)));
        assert!(!rect.contains(&Point::new(-1.0, 5.0)));
    }

    #[test]
    fn test_rect_intersects() {
        let r1 = Rect::new(0.0, 0.0, 10.0, 10.0);
        let r2 = Rect::new(5.0, 5.0, 15.0, 15.0);
        let r3 = Rect::new(20.0, 20.0, 30.0, 30.0);
        assert!(r1.intersects(&r2));
        assert!(!r1.intersects(&r3));
    }

    #[test]
    fn test_conn_dir_flags() {
        let flags = ConnDirFlags::UP.union(ConnDirFlags::RIGHT);
        assert!(flags.allows(Direction::Up));
        assert!(flags.allows(Direction::Right));
        assert!(!flags.allows(Direction::Down));
        assert!(!flags.allows(Direction::Left));
    }

    #[test]
    fn test_routed_path_orthogonal() {
        let path = RoutedPath::new(
            "test",
            vec![
                Point::new(0.0, 0.0),
                Point::new(10.0, 0.0),
                Point::new(10.0, 10.0),
            ],
        );
        assert!(path.is_orthogonal());
        assert_eq!(path.bend_count(), 1);
    }

    #[test]
    fn test_routed_path_non_orthogonal() {
        let path = RoutedPath::new("test", vec![Point::new(0.0, 0.0), Point::new(10.0, 10.0)]);
        assert!(!path.is_orthogonal());
    }
}
