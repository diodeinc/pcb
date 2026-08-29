# ortho-router

Pure Rust orthogonal routing engine for schematic wire routing. See parent [projects/editor/AGENTS.md](../../AGENTS.md) for app-level context.

## Quick Reference

```bash
# Run tests
cargo test -p ortho-router

# Run specific test
cargo test -p ortho-router two_ports_horizontal

# Update snapshots
cargo insta review

# Debug routing on a .zen file (no GUI)
RUST_LOG=ortho_router=warn cargo run -p editor-schematic-viewer -- --route-only path/to/file.zen

# Dump router input as test code
cargo run -p editor-schematic-viewer -- --dump-router-input path/to/file.zen
```

## Architecture

```
RouterInput (obstacles, ports, connectors, existing_segments)
    │
    ▼
┌─────────────────────────────────────┐
│  1. Visibility Graph (visibility.rs) │  Extract grid from obstacle/port coords
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│  2. A* Pathfinding (pathfinder.rs)   │  Route each connector, net-aware costs
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│  3. Improve Crossings                │  Reduce same-net crossings
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│  4. Nudging (nudging_libavoid.rs)    │  VPSC solver: align same-net, separate different-net
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│  5. Junction Detection (junction.rs) │  Find where same-net routes merge
└─────────────────────────────────────┘
    │
    ▼
RouterOutput (paths, junctions)
```

## Key Files

| File | Purpose |
|------|---------|
| `types.rs` | Core types: `Port`, `Obstacle`, `Connector`, `ExistingRouteSegment`, `RouterInput/Output` |
| `router.rs` | Main `OrthoRouter` coordinating all phases |
| `visibility.rs` | Grid-based visibility graph construction |
| `pathfinder.rs` | A* with bend cost + net awareness |
| `nudging_libavoid.rs` | VPSC-based route nudging (main nudging impl) |
| `vpsc.rs` | Variable Placement with Separation Constraints solver |
| `improve_crossings.rs` | Reduces unnecessary same-net crossings |
| `junction.rs` | Detects merge points for same-net routes |
| `render.rs` | SVG rendering for tests/debugging |
| `debug.rs` | Step-by-step SVG output pipeline |
| `config.rs` | `RouterConfig` with tunable parameters |

## Key Types

```rust
// Input
RouterInput { obstacles, ports, connectors, existing_segments }
Port { id, position, visibility_direction, obstacle_id? }
Connector { id, source_port_id, target_port_id, net_id? }
ExistingRouteSegment { id, start, end, net_id }

// Output
RouterOutput { paths, junctions }
RoutedPath { connector_id, points: Vec<Point>, net_id? }
```

Existing route segments are fixed document geometry. New routes can cross a
different-net segment at both interiors, but cannot overlap it or touch one of
its endpoints. Same-net segments remain available for coalescing.

## Configuration

Key parameters in `RouterConfig`:

| Parameter | Default | Effect |
|-----------|---------|--------|
| `segment_penalty` | 1.0 | Cost per bend in A* (higher = fewer bends) |
| `shape_buffer_distance` | 7.0 | Gap between routes and obstacles |
| `ideal_nudging_distance` | 10.0 | Target separation between different-net routes |
| `different_net_overlap_penalty` | 10000.0 | A* penalty for crossing different nets |
| `same_net_overlap_bonus` | -5.0 | A* bonus for bundling with same net |

## Testing

### Snapshot Tests

Tests render routing scenarios to SVG and compare against saved snapshots:

```bash
cargo test -p ortho-router           # Run all
cargo test -p ortho-router junction  # Run tests matching "junction"
cargo insta review                   # Review/accept changed snapshots
```

Test structure in `tests/snapshot_tests.rs`:
```rust
#[test]
fn my_routing_scenario() {
    let mut fixture = TestFixture::new();
    fixture.add_obstacle("obs1", 0.0, 0.0, 50.0, 50.0);
    fixture.add_port("p1", 0.0, 25.0, ConnDirFlags::RIGHT);
    fixture.add_port("p2", 100.0, 25.0, ConnDirFlags::LEFT);
    fixture.add_connector("c1", "p1", "p2");

    let input = fixture.build();
    let svg = render_scenario(&input);
    assert_svg_snapshot!("my_routing_scenario", &svg);
}
```

### Debug SVG Pipeline

For complex issues, generate step-by-step SVGs:

1. Save RouterInput JSON (via GUI debug panel, or programmatically)
2. Place in `tests/debug_inputs/`
3. Run `cargo test -p ortho-router debug_all`
4. View `tests/debug_outputs/<name>/`:
   - `01_input.svg` - Obstacles and ports
   - `02_visibility_graph.svg` - Grid edges
   - `03_pathfinding.svg` - A* paths
   - `04_improve_crossings.svg` - After crossing fix
   - `05a-e_nudge_*.svg` - Each nudging pass
   - `06_final.svg` - Final result
   - `timing.json` - Performance breakdown

## Common Issues

| Error | Cause | Fix |
|-------|-------|-----|
| "Port has no edges" | Port visibility direction blocked | Check obstacle placement, ensure grid coord in allowed direction |
| "Max iterations reached" | A* hit 100K limit | Disconnected graph, or reduce problem complexity |
| Routes overlap after nudging | Insufficient space | Increase `ideal_nudging_distance` or space obstacles |
| Same-net routes not aligned | Different routing order | Check nudging equality constraints in debug SVGs |

## Log Levels

```bash
RUST_LOG=ortho_router=warn                      # Warnings only
RUST_LOG=ortho_router=info                      # + timing
RUST_LOG=ortho_router=debug                     # + detailed
RUST_LOG=ortho_router::visibility=debug         # Visibility graph only
RUST_LOG=ortho_router::pathfinder=debug         # A* only
RUST_LOG=ortho_router::nudging_libavoid=debug   # Nudging only
```

---

## Maintaining This File

**Update this AGENTS.md** when you:
- Add new public types or APIs
- Change the routing pipeline phases
- Add new test patterns or debugging tools
- Discover non-obvious failure modes
- Change config parameter semantics

Most code changes don't need doc updates—only update for things that help future debugging or onboarding.
