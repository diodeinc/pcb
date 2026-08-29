# ortho-router

A pure Rust orthogonal routing engine for schematic wire routing. Inspired by libavoid but simplified for rectangular obstacle routing with net-aware behavior.

## Overview

The router takes obstacles (component bounding boxes) and connectors (port-to-port connections with visibility constraints) and produces orthogonal wire routes that:

- Avoid obstacles with configurable buffer distance
- Minimize bends (segment penalty in A* cost)
- Separate different-net routes via nudging
- Align same-net routes for clean visual appearance
- Detect junction points where same-net wires meet

## Quick Start

### Running the Router

```bash
# Route a .zen file and view results
cargo run -p editor-sch -- ~/path/to/file.zen

# Debug routing without GUI
RUST_LOG=ortho_router=warn cargo run -p editor-sch -- --route-only ~/path/to/file.zen

# Dump router input as test code
cargo run -p editor-sch -- --dump-router-input ~/path/to/file.zen
```

### Log Levels

```bash
RUST_LOG=ortho_router=warn        # Warnings only
RUST_LOG=ortho_router=info        # Info + timing
RUST_LOG=ortho_router=debug       # Detailed debug
RUST_LOG=ortho_router::visibility=debug  # Just visibility graph
RUST_LOG=ortho_router::pathfinder=debug  # Just pathfinding
```

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                        RouterInput                            │
│  - obstacles: Vec<Obstacle>    (rectangles)                   │
│  - ports: Vec<Port>            (positions + visibility dirs)  │
│  - connectors: Vec<Connector>  (source→target + net_id)       │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│               Phase 1: Visibility Graph                       │
│  visibility.rs                                                │
│  - Extract unique X/Y coordinates from obstacles + ports      │
│  - Build grid graph with edges between adjacent cells         │
│  - Block edges that pass through obstacle buffer zones        │
│  - Mark port edges based on visibility directions             │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                 Phase 2: A* Pathfinding                       │
│  pathfinder.rs                                                │
│  - Standard A* with Manhattan + bend heuristic                │
│  - Cost = distance + num_bends × segment_penalty              │
│  - Net-aware: penalty for crossing different-net segments     │
│  - Registers routed segments for subsequent connectors        │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                   Phase 3: Nudging                            │
│  nudging_libavoid.rs + vpsc.rs                                │
│  - Extract bend points from all routes                        │
│  - Group overlapping segments by fixed coordinate             │
│  - Same-net: equality constraints (align)                     │
│  - Different-net: separation constraints (push apart)         │
│  - VPSC solver finds optimal positions                        │
│  - Center zigzag bends in available channel space             │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│              Phase 4: Post-Processing                         │
│  corner_separation.rs + junction detection                    │
│  - Separate corners shared by different nets                  │
│  - Detect junction points where same-net routes merge         │
│  - Simplify paths (remove collinear points)                   │
│  - Grid snap for clean alignment                              │
└──────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────┐
│                       RouterOutput                            │
│  - edges: Vec<RoutedEdge>      (point sequences + junctions)  │
│  - connected_node_ids: which nodes have routed connections    │
└──────────────────────────────────────────────────────────────┘
```

## Key Files

| File | Purpose |
|------|---------|
| `types.rs` | Core types: Port, Obstacle, Connector, RouterInput/Output |
| `visibility.rs` | Grid-based visibility graph construction |
| `pathfinder.rs` | A* pathfinding with bend cost and net awareness |
| `router.rs` | Main router coordinating all phases |
| `nudging_libavoid.rs` | VPSC-based route nudging and alignment |
| `vpsc.rs` | Variable Placement with Separation Constraints solver |
| `corner_separation.rs` | Handles different-net corner collisions |

## Key Algorithms

### Visibility Graph

The visibility graph is built from a grid of unique X/Y coordinates extracted from obstacle corners and port positions. Edges connect adjacent grid cells unless blocked by obstacles.

**Corridor handling**: When checking if an edge is blocked, we use actual obstacle bounds (not buffered) in the perpendicular dimension. This allows routes to pass through narrow corridors between obstacles.

### A* Pathfinding

Standard A* with domain-specific costs:

```
f(n) = g(n) + h(n)

g(n) = actual_distance + num_bends × segment_penalty
h(n) = manhattan_distance + estimated_bends × segment_penalty
```

**Net awareness**: The pathfinder tracks which segments are "owned" by which net. When routing:
- Different-net overlap: heavy penalty (avoid)
- Same-net overlap: small bonus (encourage bundling)

### Nudging (VPSC)

The nudging phase adjusts route positions using a constraint solver:

1. **Extract bend points**: Each interior vertex of a route that can move
2. **Classify by type**:
   - `Final`: First/last bend (weight: 0.001, resists movement)
   - `Zigzag`: S-bend or Z-bend (weight: 0.00001, prefers centering)
   - `Free`: Other middle bends (weight: 0.00001)
3. **Build constraints**:
   - Same-net overlapping segments → equality constraint (align)
   - Different-net overlapping segments → separation constraint (push apart)
4. **Solve**: VPSC minimizes weighted displacement from desired positions
5. **Apply**: Update route points, maintaining orthogonality

### Junction Detection

After nudging, the router detects where same-net routes share segments. These junction points are returned for rendering (typically as dots where wires merge).

## Configuration

Key parameters in `RouterConfig`:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `segment_penalty` | 1.0 | Cost per bend in A* |
| `shape_buffer_distance` | 7.0 | Gap between routes and obstacles |
| `ideal_nudging_distance` | 10.0 | Target separation between different-net routes |
| `different_net_overlap_penalty` | 10000.0 | A* penalty for crossing different nets |
| `same_net_overlap_bonus` | -5.0 | A* bonus for bundling with same net |

## Differences from libavoid

| Feature | libavoid | ortho-router |
|---------|----------|--------------|
| **Port bumping** | Required | Not needed (grid handles natively) |
| **Pin barriers** | Required for multi-pin sides | Not needed |
| **Hyperedges** | Native support | Decomposed to 2-port via MST |
| **Nudging** | Segment-centric | Bend-point-centric |
| **Same-net handling** | Same-connector only | Same-net (more aggressive alignment) |

## Testing

### Snapshot Tests

The router uses SVG snapshot tests for visual regression:

```bash
# Run all tests
cargo test -p ortho-router

# Update snapshots after intentional changes
cargo insta review
```

### Key Test Files

- `tests/snapshot_tests.rs` - Main snapshot tests
- `tests/fixtures/` - Test fixtures and helpers

### Debugging Failed Routes

Common issues and solutions:

1. **"Port has no edges"**: Port visibility direction doesn't have a valid neighbor. Check if there's a grid coordinate in the allowed direction.

2. **"Max iterations reached"**: A* hit 100K limit. Usually means disconnected graph or very complex routing.

3. **Overlapping different-net routes**: Nudging threshold may need adjustment, or routes are too constrained by obstacles.

## Integration

The router is integrated via `schematic/src/routing/integration.rs`:

```rust
use ortho_router::{RouterInput, Router};

// Build input from layout
let input = RouterInput::new()
    .with_obstacles(obstacles)
    .with_ports(ports)
    .with_connectors(connectors);

// Route
let router = Router::new(config);
let output = router.route(&input)?;

// Convert to rendered edges
let edges = output.edges;
```
