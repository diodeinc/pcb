//! Snapshot tests for ortho-router.
//!
//! These tests create routing scenarios, render them to SVG, and compare
//! against saved snapshots. This provides visual regression testing for
//! the routing algorithm.
//!
//! SVG files are written to `tests/snapshots/svg/` for easy viewing.

use ortho_router::{
    detect_junctions, ConnDirFlags, Connector, Obstacle, OrthoRouter, Point, Port, RenderConfig,
    RouterConfig, RouterInput, SvgRenderer, VisibilityGraph,
};
use std::fs;
use std::path::Path;

/// Test fixture for creating common test scenarios.
struct TestFixture {
    input: RouterInput,
}

impl TestFixture {
    fn new() -> Self {
        Self {
            input: RouterInput::new(),
        }
    }

    fn add_obstacle(&mut self, id: &str, x: f64, y: f64, width: f64, height: f64) -> &mut Self {
        self.input
            .add_obstacle(Obstacle::from_xywh(id, x, y, width, height));
        self
    }

    fn add_port(&mut self, id: &str, x: f64, y: f64, visibility: ConnDirFlags) -> &mut Self {
        self.input
            .add_port(Port::new(id, Point::new(x, y), visibility));
        self
    }

    fn add_port_on_obstacle(
        &mut self,
        id: &str,
        x: f64,
        y: f64,
        visibility: ConnDirFlags,
        obstacle_id: &str,
    ) -> &mut Self {
        self.input.add_port(Port::on_obstacle(
            id,
            Point::new(x, y),
            visibility,
            obstacle_id,
        ));
        self
    }

    fn add_connector(&mut self, id: &str, source: &str, target: &str) -> &mut Self {
        self.input.add_connector(Connector::new(id, source, target));
        self
    }

    fn add_connector_with_net(
        &mut self,
        id: &str,
        source: &str,
        target: &str,
        net_id: &str,
    ) -> &mut Self {
        self.input
            .add_connector(Connector::with_net(id, source, target, net_id));
        self
    }

    fn build(self) -> RouterInput {
        self.input
    }
}

/// Options for rendering test scenarios.
#[derive(Default)]
struct RenderOptions {
    color_by_net: bool,
    show_visibility_graph: bool,
}

/// Render a test scenario to SVG for snapshot comparison.
/// Automatically detects and renders junctions for same-net routes.
fn render_scenario(input: &RouterInput) -> String {
    render_scenario_with_options(input, RenderOptions::default())
}

/// Render a test scenario with net coloring enabled.
/// Automatically detects and renders junctions for same-net routes.
fn render_scenario_with_net_colors(input: &RouterInput) -> String {
    render_scenario_with_options(
        input,
        RenderOptions {
            color_by_net: true,
            ..Default::default()
        },
    )
}

/// Render a test scenario with the visibility graph visible.
/// Automatically detects and renders junctions for same-net routes.
fn render_scenario_with_graph(input: &RouterInput) -> String {
    render_scenario_with_options(
        input,
        RenderOptions {
            show_visibility_graph: true,
            ..Default::default()
        },
    )
}

/// Render a test scenario with configurable options.
/// Always detects and renders junctions for same-net routes.
fn render_scenario_with_options(input: &RouterInput, options: RenderOptions) -> String {
    let router_config = RouterConfig::default();
    let router = OrthoRouter::new(router_config.clone());
    let output = router.route(input);

    // Build net_ids for junction detection
    // Check connectors for net ID
    let net_ids: Vec<String> = output
        .paths
        .iter()
        .map(|p| {
            // First check connectors
            if let Some(c) = input.connectors.iter().find(|c| c.id == p.connector_id) {
                return c.effective_net_id().to_string();
            }
            // Fall back to connector_id
            p.connector_id.clone()
        })
        .collect();

    // Detect junctions
    let junctions = detect_junctions(&output.paths, &net_ids);

    // Build visibility graph if needed
    let graph = if options.show_visibility_graph {
        Some(VisibilityGraph::build(input, &router_config))
    } else {
        None
    };

    let renderer = SvgRenderer::new(RenderConfig {
        padding: 30.0,
        color_by_net: options.color_by_net,
        show_visibility_graph: options.show_visibility_graph,
        ..Default::default()
    });

    renderer.render_full(input, &output, graph.as_ref(), &junctions)
}

/// Write SVG to file and assert snapshot.
/// This writes to `tests/snapshots/svg/{name}.svg` for easy viewing.
fn assert_svg_snapshot(name: &str, svg: &str) {
    // Write SVG file for easy viewing
    let svg_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/svg");
    fs::create_dir_all(&svg_dir).expect("Failed to create svg directory");
    let svg_path = svg_dir.join(format!("{}.svg", name));
    fs::write(&svg_path, svg).expect("Failed to write SVG file");

    // Also use insta for regression testing
    insta::assert_snapshot!(name, svg);
}

// =============================================================================
// Test Scenarios
// =============================================================================

#[test]
fn test_empty_scenario() {
    let input = RouterInput::new();
    let svg = render_scenario(&input);
    assert_svg_snapshot("empty_scenario", &svg);
}

#[test]
fn test_single_obstacle() {
    let mut fixture = TestFixture::new();
    fixture.add_obstacle("obs1", 50.0, 50.0, 100.0, 80.0);

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("single_obstacle", &svg);
}

#[test]
fn test_multiple_obstacles() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 50.0, 50.0, 80.0, 60.0)
        .add_obstacle("obs2", 200.0, 50.0, 80.0, 60.0)
        .add_obstacle("obs3", 125.0, 150.0, 80.0, 60.0);

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("multiple_obstacles", &svg);
}

#[test]
fn test_single_port() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 50.0, 50.0, 100.0, 80.0)
        .add_port_on_obstacle("p1", 100.0, 50.0, ConnDirFlags::UP, "obs1");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("single_port", &svg);
}

#[test]
fn test_ports_all_directions() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 100.0, 100.0, 100.0, 100.0)
        // Ports on each side with appropriate visibility, attached to obstacle
        .add_port_on_obstacle("p_top", 150.0, 100.0, ConnDirFlags::UP, "obs1")
        .add_port_on_obstacle("p_bottom", 150.0, 200.0, ConnDirFlags::DOWN, "obs1")
        .add_port_on_obstacle("p_left", 100.0, 150.0, ConnDirFlags::LEFT, "obs1")
        .add_port_on_obstacle("p_right", 200.0, 150.0, ConnDirFlags::RIGHT, "obs1");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("ports_all_directions", &svg);
}

#[test]
fn test_port_with_all_visibility() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 100.0, 100.0, 60.0, 60.0)
        .add_port("p1", 130.0, 80.0, ConnDirFlags::ALL);

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("port_with_all_visibility", &svg);
}

#[test]
fn test_two_ports_horizontal() {
    let mut fixture = TestFixture::new();
    fixture
        .add_port("p1", 50.0, 100.0, ConnDirFlags::RIGHT)
        .add_port("p2", 250.0, 100.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("two_ports_horizontal", &svg);
}

#[test]
fn test_two_ports_vertical() {
    let mut fixture = TestFixture::new();
    fixture
        .add_port("p1", 100.0, 50.0, ConnDirFlags::DOWN)
        .add_port("p2", 100.0, 200.0, ConnDirFlags::UP)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("two_ports_vertical", &svg);
}

#[test]
fn test_simple_obstacle_avoidance() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 100.0, 80.0, 80.0, 60.0)
        .add_port("p1", 50.0, 100.0, ConnDirFlags::RIGHT)
        .add_port("p2", 230.0, 100.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("simple_obstacle_avoidance", &svg);
}

#[test]
fn test_l_shaped_route() {
    let mut fixture = TestFixture::new();
    fixture
        .add_port("p1", 50.0, 50.0, ConnDirFlags::RIGHT)
        .add_port("p2", 150.0, 150.0, ConnDirFlags::UP)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("l_shaped_route", &svg);
}

#[test]
fn test_multiple_connectors() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 100.0, 80.0, 80.0, 80.0)
        .add_port("p1a", 50.0, 90.0, ConnDirFlags::RIGHT)
        .add_port("p1b", 230.0, 90.0, ConnDirFlags::LEFT)
        .add_port("p2a", 50.0, 140.0, ConnDirFlags::RIGHT)
        .add_port("p2b", 230.0, 140.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1a", "p1b")
        .add_connector("c2", "p2a", "p2b");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("multiple_connectors", &svg);
}

#[test]
fn test_schematic_like_layout() {
    // Simulate a simple schematic with components and connections
    let mut fixture = TestFixture::new();

    // Component 1 (left side)
    fixture
        .add_obstacle("comp1", 50.0, 100.0, 60.0, 80.0)
        .add_port_on_obstacle("comp1_out1", 110.0, 120.0, ConnDirFlags::RIGHT, "comp1")
        .add_port_on_obstacle("comp1_out2", 110.0, 160.0, ConnDirFlags::RIGHT, "comp1");

    // Component 2 (right side, top)
    fixture
        .add_obstacle("comp2", 250.0, 80.0, 60.0, 50.0)
        .add_port_on_obstacle("comp2_in", 250.0, 105.0, ConnDirFlags::LEFT, "comp2");

    // Component 3 (right side, bottom)
    fixture
        .add_obstacle("comp3", 250.0, 150.0, 60.0, 50.0)
        .add_port_on_obstacle("comp3_in", 250.0, 175.0, ConnDirFlags::LEFT, "comp3");

    // Connections
    fixture
        .add_connector("net1", "comp1_out1", "comp2_in")
        .add_connector("net2", "comp1_out2", "comp3_in");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("schematic_like_layout", &svg);
}

#[test]
fn test_grid_of_obstacles() {
    let mut fixture = TestFixture::new();

    // Create a 3x3 grid of obstacles
    for row in 0..3 {
        for col in 0..3 {
            let x = 50.0 + col as f64 * 80.0;
            let y = 50.0 + row as f64 * 70.0;
            fixture.add_obstacle(&format!("obs_{}_{}", row, col), x, y, 50.0, 40.0);
        }
    }

    // Add ports on the sides
    fixture
        .add_port("p_left", 20.0, 120.0, ConnDirFlags::RIGHT)
        .add_port("p_right", 310.0, 120.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p_left", "p_right");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("grid_of_obstacles", &svg);
}

#[test]
fn test_nested_obstacles() {
    // Test with obstacles of varying sizes arranged to create corridors
    let mut fixture = TestFixture::new();

    fixture
        .add_obstacle("outer_top", 50.0, 50.0, 200.0, 30.0)
        .add_obstacle("outer_bottom", 50.0, 170.0, 200.0, 30.0)
        .add_obstacle("inner", 120.0, 100.0, 60.0, 50.0)
        .add_port("p1", 30.0, 125.0, ConnDirFlags::RIGHT)
        .add_port("p2", 270.0, 125.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario(&fixture.build());
    assert_svg_snapshot("nested_obstacles", &svg);
}

// =============================================================================
// Visibility Graph Tests
// =============================================================================

#[test]
fn test_visibility_graph_simple() {
    // Simple scenario to visualize the visibility graph
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 100.0, 80.0, 80.0, 60.0)
        .add_port("p1", 50.0, 100.0, ConnDirFlags::RIGHT)
        .add_port("p2", 230.0, 100.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario_with_graph(&fixture.build());
    assert_svg_snapshot("visibility_graph_simple", &svg);
}

#[test]
fn test_visibility_graph_multiple_obstacles() {
    let mut fixture = TestFixture::new();
    fixture
        .add_obstacle("obs1", 80.0, 40.0, 60.0, 50.0)
        .add_obstacle("obs2", 160.0, 100.0, 60.0, 50.0)
        .add_port("p1", 30.0, 80.0, ConnDirFlags::RIGHT)
        .add_port("p2", 250.0, 80.0, ConnDirFlags::LEFT)
        .add_connector("c1", "p1", "p2");

    let svg = render_scenario_with_graph(&fixture.build());
    assert_svg_snapshot("visibility_graph_multiple_obstacles", &svg);
}

// =============================================================================
// Net-Aware Routing Tests
// =============================================================================

#[test]
fn test_same_net_connectors() {
    // Test that connectors on the same net can share/overlap segments.
    // Two connectors going from left ports to right ports, both on net "VCC".
    // They should be able to share the horizontal segment.
    let mut fixture = TestFixture::new();

    // Left side ports
    fixture
        .add_port("p1", 30.0, 80.0, ConnDirFlags::RIGHT)
        .add_port("p2", 30.0, 120.0, ConnDirFlags::RIGHT)
        // Right side ports
        .add_port("p3", 250.0, 80.0, ConnDirFlags::LEFT)
        .add_port("p4", 250.0, 120.0, ConnDirFlags::LEFT)
        // Both connectors on the same net
        .add_connector_with_net("c1", "p1", "p3", "VCC")
        .add_connector_with_net("c2", "p2", "p4", "VCC");

    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("same_net_connectors", &svg);
}

#[test]
fn test_different_net_connectors() {
    // Test that connectors on different nets avoid overlapping.
    // Two connectors going similar paths, but different nets.
    // The router should prefer paths that don't overlap.
    let mut fixture = TestFixture::new();

    // Obstacle in the middle
    fixture.add_obstacle("obs1", 100.0, 60.0, 80.0, 80.0);

    // Left side ports
    fixture
        .add_port("p1", 30.0, 80.0, ConnDirFlags::RIGHT)
        .add_port("p2", 30.0, 120.0, ConnDirFlags::RIGHT)
        // Right side ports
        .add_port("p3", 250.0, 80.0, ConnDirFlags::LEFT)
        .add_port("p4", 250.0, 120.0, ConnDirFlags::LEFT)
        // Connectors on DIFFERENT nets
        .add_connector_with_net("c1", "p1", "p3", "VCC")
        .add_connector_with_net("c2", "p2", "p4", "GND");

    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("different_net_connectors", &svg);
}

#[test]
fn test_mixed_net_routing() {
    // Test a more complex scenario with multiple nets.
    // Some connectors share nets, others don't.
    let mut fixture = TestFixture::new();

    // Create a component-like layout
    fixture
        .add_obstacle("comp1", 50.0, 60.0, 60.0, 80.0)
        .add_obstacle("comp2", 200.0, 60.0, 60.0, 80.0);

    // Ports on left component (output side)
    fixture
        .add_port_on_obstacle("comp1_out1", 110.0, 80.0, ConnDirFlags::RIGHT, "comp1")
        .add_port_on_obstacle("comp1_out2", 110.0, 100.0, ConnDirFlags::RIGHT, "comp1")
        .add_port_on_obstacle("comp1_out3", 110.0, 120.0, ConnDirFlags::RIGHT, "comp1");

    // Ports on right component (input side)
    fixture
        .add_port_on_obstacle("comp2_in1", 200.0, 80.0, ConnDirFlags::LEFT, "comp2")
        .add_port_on_obstacle("comp2_in2", 200.0, 100.0, ConnDirFlags::LEFT, "comp2")
        .add_port_on_obstacle("comp2_in3", 200.0, 120.0, ConnDirFlags::LEFT, "comp2");

    // Net 1: top and bottom ports connected (same net - can share)
    // Net 2: middle port (separate net - should avoid Net 1)
    fixture
        .add_connector_with_net("net1_a", "comp1_out1", "comp2_in1", "NET1")
        .add_connector_with_net("net1_b", "comp1_out3", "comp2_in3", "NET1")
        .add_connector_with_net("net2", "comp1_out2", "comp2_in2", "NET2");

    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("mixed_net_routing", &svg);
}

#[test]
fn test_three_nets_crossing() {
    // Test three different nets that need to cross paths.
    // Each should take a unique route to avoid overlapping.
    let mut fixture = TestFixture::new();

    // Three ports on each side at different heights
    fixture
        .add_port("left_top", 30.0, 50.0, ConnDirFlags::RIGHT)
        .add_port("left_mid", 30.0, 100.0, ConnDirFlags::RIGHT)
        .add_port("left_bot", 30.0, 150.0, ConnDirFlags::RIGHT)
        .add_port("right_top", 250.0, 50.0, ConnDirFlags::LEFT)
        .add_port("right_mid", 250.0, 100.0, ConnDirFlags::LEFT)
        .add_port("right_bot", 250.0, 150.0, ConnDirFlags::LEFT);

    // Cross-connect: top-left to bottom-right, etc. (different nets)
    fixture
        .add_connector_with_net("net_a", "left_top", "right_bot", "NET_A")
        .add_connector_with_net("net_b", "left_mid", "right_mid", "NET_B")
        .add_connector_with_net("net_c", "left_bot", "right_top", "NET_C");

    let svg = render_scenario_with_options(
        &fixture.build(),
        RenderOptions {
            color_by_net: true,
            show_visibility_graph: true,
        },
    );
    assert_svg_snapshot("three_nets_crossing", &svg);
}

#[test]
fn test_bus_like_routing() {
    // Test bus-like routing where multiple connectors from the same net
    // should share a common "bus" segment.
    let mut fixture = TestFixture::new();

    // Source component with multiple outputs on same net (like a data bus)
    fixture.add_obstacle("source", 30.0, 60.0, 60.0, 100.0);

    // Multiple destination components
    fixture
        .add_obstacle("dest1", 180.0, 40.0, 50.0, 40.0)
        .add_obstacle("dest2", 180.0, 100.0, 50.0, 40.0)
        .add_obstacle("dest3", 180.0, 160.0, 50.0, 40.0);

    // Source ports
    fixture
        .add_port_on_obstacle("src_out1", 90.0, 80.0, ConnDirFlags::RIGHT, "source")
        .add_port_on_obstacle("src_out2", 90.0, 110.0, ConnDirFlags::RIGHT, "source")
        .add_port_on_obstacle("src_out3", 90.0, 140.0, ConnDirFlags::RIGHT, "source");

    // Destination ports
    fixture
        .add_port_on_obstacle("dest1_in", 180.0, 60.0, ConnDirFlags::LEFT, "dest1")
        .add_port_on_obstacle("dest2_in", 180.0, 120.0, ConnDirFlags::LEFT, "dest2")
        .add_port_on_obstacle("dest3_in", 180.0, 180.0, ConnDirFlags::LEFT, "dest3");

    // All on the same "DATA_BUS" net - they can share segments
    fixture
        .add_connector_with_net("bus1", "src_out1", "dest1_in", "DATA_BUS")
        .add_connector_with_net("bus2", "src_out2", "dest2_in", "DATA_BUS")
        .add_connector_with_net("bus3", "src_out3", "dest3_in", "DATA_BUS");

    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("bus_like_routing", &svg);
}

#[test]
fn test_power_ground_separation() {
    // Classic VCC/GND separation test - power rails should never overlap.
    let mut fixture = TestFixture::new();

    // IC-like component in the center
    fixture.add_obstacle("ic", 100.0, 60.0, 80.0, 80.0);

    // VCC ports (top)
    fixture
        .add_port("vcc_supply", 30.0, 50.0, ConnDirFlags::RIGHT)
        .add_port_on_obstacle("ic_vcc", 140.0, 60.0, ConnDirFlags::UP, "ic");

    // GND ports (bottom)
    fixture
        .add_port("gnd_supply", 30.0, 150.0, ConnDirFlags::RIGHT)
        .add_port_on_obstacle("ic_gnd", 140.0, 140.0, ConnDirFlags::DOWN, "ic");

    // Signal port
    fixture
        .add_port("signal_in", 250.0, 100.0, ConnDirFlags::LEFT)
        .add_port_on_obstacle("ic_sig", 180.0, 100.0, ConnDirFlags::RIGHT, "ic");

    // VCC, GND, and signal connections
    fixture
        .add_connector_with_net("vcc_conn", "vcc_supply", "ic_vcc", "VCC")
        .add_connector_with_net("gnd_conn", "gnd_supply", "ic_gnd", "GND")
        .add_connector_with_net("sig_conn", "signal_in", "ic_sig", "SIGNAL");

    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("power_ground_separation", &svg);
}

#[test]
fn test_multi_fanout_same_net() {
    // Test one source fanning out to multiple destinations on the same net.
    // All routes should be able to share segments.
    let mut fixture = TestFixture::new();

    // Single source on left
    fixture.add_port("source", 30.0, 100.0, ConnDirFlags::RIGHT);

    // Multiple destinations on right at different heights
    fixture
        .add_port("dest1", 250.0, 50.0, ConnDirFlags::LEFT)
        .add_port("dest2", 250.0, 100.0, ConnDirFlags::LEFT)
        .add_port("dest3", 250.0, 150.0, ConnDirFlags::LEFT);

    // All on the same net (like a clock signal fanning out)
    fixture
        .add_connector_with_net("fanout1", "source", "dest1", "CLK")
        .add_connector_with_net("fanout2", "source", "dest2", "CLK")
        .add_connector_with_net("fanout3", "source", "dest3", "CLK");

    // Junctions will be detected and rendered automatically
    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("multi_fanout_same_net", &svg);
}

#[test]
fn test_junction_detection() {
    // Test junction detection where same-net routes meet.
    // A junction appears where one route's endpoint meets another route's segment.
    let mut fixture = TestFixture::new();

    // Create a T-junction scenario:
    // - Main horizontal bus from left to right
    // - Branch coming from below meeting the bus
    fixture
        .add_port("bus_left", 30.0, 100.0, ConnDirFlags::RIGHT)
        .add_port("bus_right", 250.0, 100.0, ConnDirFlags::LEFT)
        .add_port("branch_bottom", 140.0, 180.0, ConnDirFlags::UP);

    // All on the same net - the branch should join the bus
    fixture
        .add_connector_with_net("bus_main", "bus_left", "bus_right", "BUS")
        .add_connector_with_net("bus_branch", "branch_bottom", "bus_right", "BUS");

    // Junctions will be detected and rendered automatically
    let svg = render_scenario_with_net_colors(&fixture.build());
    assert_svg_snapshot("junction_detection", &svg);
}

// =============================================================================
// Power Rail Routing Tests (Based on DM0001.zen analysis)
// =============================================================================

/// Render a test scenario with debug logging of graph structure.
fn render_scenario_with_debug(input: &RouterInput) -> String {
    let router_config = RouterConfig::default();

    // Build visibility graph and log its structure
    let graph = VisibilityGraph::build(input, &router_config);

    let stats = graph.stats();
    println!("\n=== VISIBILITY GRAPH DEBUG ===");
    println!("Total vertices: {}", stats.vertex_count);
    println!("Total edges: {}", stats.edge_count);

    // Log vertices grouped by type
    let port_vertices: Vec<_> = graph
        .vertices
        .iter()
        .filter(|v| v.port_id.is_some())
        .collect();
    println!("\nPort vertices ({}):", port_vertices.len());
    for v in &port_vertices {
        let edges = graph.get_edges(v.id);
        println!(
            "  {:?} at ({:.1}, {:.1}) port={:?} -> {} edges",
            v.id,
            v.position.x,
            v.position.y,
            v.port_id,
            edges.len()
        );
        for e in edges.iter().take(5) {
            if let Some(target) = graph.get_vertex(e.to) {
                println!(
                    "    -> ({:.1}, {:.1}) dir={:?}",
                    target.position.x, target.position.y, e.direction
                );
            }
        }
        if edges.len() > 5 {
            println!("    ... and {} more", edges.len() - 5);
        }
    }

    // Log grid coordinates
    println!("\nX coordinates ({}):", graph.x_coords.len());
    for (i, x) in graph.x_coords.iter().enumerate().take(20) {
        println!("  x[{}] = {:.1}", i, x);
    }
    if graph.x_coords.len() > 20 {
        println!("  ... and {} more", graph.x_coords.len() - 20);
    }

    println!("\nY coordinates ({}):", graph.y_coords.len());
    for (i, y) in graph.y_coords.iter().enumerate().take(20) {
        println!("  y[{}] = {:.1}", i, y);
    }
    if graph.y_coords.len() > 20 {
        println!("  ... and {} more", graph.y_coords.len() - 20);
    }

    // Now route and log results
    let router = OrthoRouter::new(router_config.clone());
    let output = router.route(input);

    println!("\n=== ROUTING RESULTS ===");
    println!("Total paths: {}", output.paths.len());
    for path in &output.paths {
        let points_str: Vec<String> = path
            .points
            .iter()
            .map(|p| format!("({:.1},{:.1})", p.x, p.y))
            .collect();
        println!(
            "  {}: {} points, {} bends",
            path.connector_id,
            path.points.len(),
            path.bend_count()
        );
        println!("    {}", points_str.join(" -> "));
    }

    // Build net_ids for junction detection
    let net_ids: Vec<String> = output
        .paths
        .iter()
        .map(|p| {
            input
                .connectors
                .iter()
                .find(|c| c.id == p.connector_id)
                .map(|c| c.effective_net_id().to_string())
                .unwrap_or_else(|| p.connector_id.clone())
        })
        .collect();

    let junctions = detect_junctions(&output.paths, &net_ids);

    let renderer = SvgRenderer::new(RenderConfig {
        padding: 30.0,
        color_by_net: true,
        show_visibility_graph: true,
        ..Default::default()
    });

    renderer.render_full(input, &output, Some(&graph), &junctions)
}

#[test]
fn test_power_rail_fanin() {
    // Test scenario mimicking DM0001.zen power rail area:
    // - Power symbols on the left (VPLUS, V10V, V5V, V3V3)
    // - Components on the right with multiple connections to each power rail
    // - This tests fan-in routing: multiple sources -> single destination
    let mut fixture = TestFixture::new();

    // Power symbols on the left (like VPLUS, V10V, V5V, V3V3)
    // These receive connections from the right
    fixture
        .add_port("VPLUS", 30.0, 30.0, ConnDirFlags::RIGHT) // Top power rail
        .add_port("V10V", 30.0, 80.0, ConnDirFlags::RIGHT) // Second rail
        .add_port("V5V", 30.0, 130.0, ConnDirFlags::RIGHT) // Third rail
        .add_port("V3V3", 30.0, 180.0, ConnDirFlags::RIGHT); // Bottom rail

    // Buck converter component (converts VPLUS -> V10V)
    fixture.add_obstacle("buck", 100.0, 20.0, 60.0, 40.0);
    fixture
        .add_port_on_obstacle("buck_vin", 100.0, 40.0, ConnDirFlags::LEFT, "buck") // Input from VPLUS
        .add_port_on_obstacle("buck_vout", 160.0, 40.0, ConnDirFlags::RIGHT, "buck"); // Output (internal)

    // LDO 5V component (converts V10V -> V5V)
    fixture.add_obstacle("ldo_5v", 100.0, 70.0, 60.0, 40.0);
    fixture
        .add_port_on_obstacle("ldo5v_vin", 100.0, 90.0, ConnDirFlags::LEFT, "ldo_5v")
        .add_port_on_obstacle("ldo5v_vout", 160.0, 90.0, ConnDirFlags::RIGHT, "ldo_5v");

    // LDO 3V3 component (converts V5V -> V3V3)
    fixture.add_obstacle("ldo_3v3", 100.0, 120.0, 60.0, 40.0);
    fixture
        .add_port_on_obstacle("ldo3v3_vin", 100.0, 140.0, ConnDirFlags::LEFT, "ldo_3v3")
        .add_port_on_obstacle("ldo3v3_vout", 160.0, 140.0, ConnDirFlags::RIGHT, "ldo_3v3");

    // Power capacitors connected to VPLUS (like PowerCaps module)
    fixture.add_obstacle("cap1", 100.0, 170.0, 30.0, 30.0);
    fixture.add_obstacle("cap2", 140.0, 170.0, 30.0, 30.0);
    fixture
        .add_port_on_obstacle("cap1_vplus", 100.0, 185.0, ConnDirFlags::LEFT, "cap1")
        .add_port_on_obstacle("cap2_vplus", 140.0, 185.0, ConnDirFlags::LEFT, "cap2");

    // Additional V10V consumers (like phase drivers)
    fixture.add_obstacle("driver1", 200.0, 50.0, 40.0, 30.0);
    fixture.add_obstacle("driver2", 200.0, 90.0, 40.0, 30.0);
    fixture
        .add_port_on_obstacle("driver1_v10v", 200.0, 65.0, ConnDirFlags::LEFT, "driver1")
        .add_port_on_obstacle("driver2_v10v", 200.0, 105.0, ConnDirFlags::LEFT, "driver2");

    // Power rail connections (fan-in to each power symbol)
    // VPLUS connections
    fixture
        .add_connector_with_net("vplus_to_buck", "VPLUS", "buck_vin", "VPLUS")
        .add_connector_with_net("vplus_to_cap1", "VPLUS", "cap1_vplus", "VPLUS")
        .add_connector_with_net("vplus_to_cap2", "VPLUS", "cap2_vplus", "VPLUS");

    // V10V connections (multiple things need 10V)
    fixture
        .add_connector_with_net("v10v_to_ldo5v", "V10V", "ldo5v_vin", "V10V")
        .add_connector_with_net("v10v_to_driver1", "V10V", "driver1_v10v", "V10V")
        .add_connector_with_net("v10v_to_driver2", "V10V", "driver2_v10v", "V10V");

    // V5V connection
    fixture.add_connector_with_net("v5v_to_ldo3v3", "V5V", "ldo3v3_vin", "V5V");

    let input = fixture.build();
    let svg = render_scenario_with_debug(&input);
    assert_svg_snapshot("power_rail_fanin", &svg);
}

#[test]
fn test_gap_routing_diagnostic() {
    // Diagnostic test to understand why routes don't use gaps between obstacles.
    // Two stacked obstacles with a 10-unit gap, and a route that should go through the gap.
    let mut fixture = TestFixture::new();

    // Two stacked obstacles with a gap between them
    fixture
        .add_obstacle("top_obs", 100.0, 20.0, 60.0, 40.0) // Y: 20-60
        .add_obstacle("bot_obs", 100.0, 70.0, 60.0, 40.0); // Y: 70-110
                                                           // Gap is at Y: 60-70

    // Route from left (Y=65, in the gap) to right (Y=65)
    fixture
        .add_port("left", 30.0, 65.0, ConnDirFlags::RIGHT)
        .add_port("right", 200.0, 65.0, ConnDirFlags::LEFT)
        .add_connector("gap_route", "left", "right");

    let input = fixture.build();
    let svg = render_scenario_with_debug(&input);
    assert_svg_snapshot("gap_routing_diagnostic", &svg);
}

#[test]
fn test_gap_routing_from_outside() {
    // Test routing when source is NOT in the gap but destination IS.
    // This mimics the V10V -> driver scenario.
    let mut fixture = TestFixture::new();

    // Two stacked obstacles
    fixture
        .add_obstacle("top_obs", 100.0, 20.0, 60.0, 40.0) // Y: 20-60
        .add_obstacle("bot_obs", 100.0, 70.0, 60.0, 40.0); // Y: 70-110
                                                           // Gap is at Y: 60-70

    // Source at Y=80 (inside bot_obs Y range, but to the left of it)
    // Destination at Y=65 (in the gap, to the right of obstacles)
    fixture
        .add_port("source", 30.0, 80.0, ConnDirFlags::RIGHT)
        .add_port("dest", 200.0, 65.0, ConnDirFlags::LEFT)
        .add_connector("route", "source", "dest");

    let input = fixture.build();
    let svg = render_scenario_with_debug(&input);
    assert_svg_snapshot("gap_routing_from_outside", &svg);
}

#[test]
fn test_power_rail_stacked_symbols() {
    // Simpler test: multiple ports stacked vertically on the left,
    // multiple consumers on the right, all connecting to the same net.
    // This is the "power symbol" pattern where many things connect to one symbol.
    let mut fixture = TestFixture::new();

    // Single power symbol (e.g., VPLUS)
    fixture.add_port("VPLUS", 30.0, 100.0, ConnDirFlags::RIGHT);

    // Multiple consumers on the right at different Y positions
    // These would be component ports all needing VPLUS
    fixture
        .add_port("consumer1", 200.0, 40.0, ConnDirFlags::LEFT)
        .add_port("consumer2", 200.0, 80.0, ConnDirFlags::LEFT)
        .add_port("consumer3", 200.0, 120.0, ConnDirFlags::LEFT)
        .add_port("consumer4", 200.0, 160.0, ConnDirFlags::LEFT);

    // All on the same net - routes should merge nicely
    fixture
        .add_connector_with_net("vplus_c1", "VPLUS", "consumer1", "VPLUS")
        .add_connector_with_net("vplus_c2", "VPLUS", "consumer2", "VPLUS")
        .add_connector_with_net("vplus_c3", "VPLUS", "consumer3", "VPLUS")
        .add_connector_with_net("vplus_c4", "VPLUS", "consumer4", "VPLUS");

    let input = fixture.build();
    let svg = render_scenario_with_debug(&input);
    assert_svg_snapshot("power_rail_stacked_symbols", &svg);
}

/// Test that different-net routes with overlapping horizontal segments get separated.
///
/// This reproduces the exact bug from DM0001 where V10V and V5V routes share a horizontal
/// segment. The actual DM0001 routes are:
/// - V10V_edge_27: (38.1,-15.2) -> (26.7,-15.2) -> (26.7,-6.3) -> (25.4,-6.3) -> (25.4,-2.5)
/// - V5V_edge_44: (34.3,-43.2) -> (29.2,-43.2) -> (29.2,-15.2) -> (25.4,-15.2) -> (25.4,-12.7)
///
/// Both routes have a horizontal segment at Y=-15.2:
/// - V10V: X range [26.7, 38.1] at Y=-15.2
/// - V5V: X range [25.4, 29.2] at Y=-15.2
///
/// They overlap in X range [26.7, 29.2].
///
/// This test creates a simplified version of this scenario where:
/// - Power symbols are stacked vertically on the left
/// - Multiple source ports on the right connect to different power nets
/// - The routes must go through a common corridor, potentially overlapping
#[test]
fn test_different_nets_horizontal_overlap() {
    let mut fixture = TestFixture::new();

    // Power symbols stacked vertically on the left (like DM0001)
    // V10V symbol at Y=0
    fixture
        .add_obstacle("V10V_Symbol", 245.0, 0.0, 18.0, 28.0)
        .add_port_on_obstacle("V10V", 254.0, 28.0, ConnDirFlags::DOWN, "V10V_Symbol");

    // V5V symbol at Y=100
    fixture
        .add_obstacle("V5V_Symbol", 245.0, 100.0, 18.0, 28.0)
        .add_port_on_obstacle("V5V", 254.0, 128.0, ConnDirFlags::DOWN, "V5V_Symbol");

    // V3V3 symbol at Y=200
    fixture
        .add_obstacle("V3V3_Symbol", 245.0, 200.0, 18.0, 28.0)
        .add_port_on_obstacle("V3V3", 254.0, 228.0, ConnDirFlags::DOWN, "V3V3_Symbol");

    // Source ports on the right at different Y positions
    // These will create routes that fan out to the power symbols on the left
    // V10V source at Y=152 (close to but different from V5V source Y range)
    fixture.add_port("V10V_src", 381.0, 152.0, ConnDirFlags::LEFT);

    // V5V sources at Y=267 and Y=432 (below V3V3)
    fixture.add_port("V5V_src1", 381.0, 267.0, ConnDirFlags::LEFT);
    fixture.add_port("V5V_src2", 343.0, 432.0, ConnDirFlags::LEFT);

    // V3V3 source at Y=178 (between V10V and V5V_src1)
    fixture.add_port("V3V3_src", 381.0, 178.0, ConnDirFlags::LEFT);

    // The key constraint: All routes must go through the vertical corridor at X=254
    // to reach the power symbols. Without proper separation, the horizontal
    // segments in this corridor will overlap.

    fixture
        .add_connector_with_net("v10v_conn", "V10V", "V10V_src", "V10V")
        .add_connector_with_net("v5v_conn1", "V5V", "V5V_src1", "V5V")
        .add_connector_with_net("v5v_conn2", "V5V", "V5V_src2", "V5V")
        .add_connector_with_net("v3v3_conn", "V3V3", "V3V3_src", "V3V3");

    let input = fixture.build();
    let svg = render_scenario_with_net_colors(&input);
    assert_svg_snapshot("different_nets_horizontal_overlap", &svg);
}

/// Test that different-net routes meeting at a corner point get separated.
///
/// This reproduces an issue from DM0001 where 3V3 and GND routes appear to
/// meet at a corner, making them look connected even though they're different nets.
///
/// The issue occurs when:
/// - Route A (net1): turns at point P
/// - Route B (net2): passes through or turns at the same point P
///
/// The visual result is that the routes appear connected at P.
#[test]
fn test_different_nets_corner_meeting() {
    let mut fixture = TestFixture::new();

    // Net symbol destinations on the left
    fixture
        .add_port("NET_A_sym", 30.0, 50.0, ConnDirFlags::RIGHT)
        .add_port("NET_B_sym", 30.0, 150.0, ConnDirFlags::RIGHT);

    // Source components on the right, arranged so routes cross paths
    // NET_A source at bottom-right, going to top-left symbol
    fixture.add_port("NET_A_src", 200.0, 120.0, ConnDirFlags::LEFT);

    // NET_B source at top-right, going to bottom-left symbol
    fixture.add_port("NET_B_src", 200.0, 80.0, ConnDirFlags::LEFT);

    // The routes will naturally want to share the vertical corridor around X=100
    // NET_A: (200, 120) -> (~100, 120) -> (~100, 50) -> (30, 50)
    // NET_B: (200, 80) -> (~100, 80) -> (~100, 150) -> (30, 150)
    //
    // Without proper separation, both might use X=100 for their vertical segment,
    // causing them to meet at point (~100, ~100) or share corners.

    fixture
        .add_connector_with_net("net_a_conn", "NET_A_sym", "NET_A_src", "NET_A")
        .add_connector_with_net("net_b_conn", "NET_B_sym", "NET_B_src", "NET_B");

    let input = fixture.build();
    let svg = render_scenario_with_net_colors(&input);
    assert_svg_snapshot("different_nets_corner_meeting", &svg);
}

/// Minimal reproduction of the V5V "divet" issue from DM0001.
///
/// The exact issue from DM0001:
/// - Route from Ldo_5V creates a 3-point L-shape at Y=177.8 (anchored, no Y bend point)
/// - Routes from Ldo_3V3 and HallEncoder create 5-point paths with Y-nudgeable segments
/// - After nudging, the 5-point paths align to each other but NOT to the 3-point path
/// - Result: A "divet" between the 3-point path and the 5-point paths
///
/// Root cause: The 3-point path's horizontal segment Y coordinate has no representation
/// in the nudging system, so other routes can't align to it.
#[test]
fn test_same_net_fanin_divet() {
    let mut fixture = TestFixture::new();

    // === Simplified DM0001 V5V layout ===

    // V5V.1 net symbol - destination for all routes
    // Port faces DOWN at Y=128
    fixture
        .add_obstacle("V5V_Symbol", 245.0, 100.0, 18.0, 28.0)
        .add_port_on_obstacle("V5V", 254.0, 128.0, ConnDirFlags::DOWN, "V5V_Symbol");

    // Blocker obstacle - positioned BELOW Ldo_5V's Y=177.8
    // This blocks direct horizontal routes at Y > 200 but NOT at Y=177.8
    // So Ldo_5V can go direct (3-point), but Ldo_3V3 and HallEncoder must go around (5-point)
    fixture.add_obstacle("Blocker", 270.0, 210.0, 80.0, 180.0);

    // Ldo_5V module - source 1
    // At Y=177.8, which is BLOCKED by the blocker (blocker is at Y=140-260)
    // But since Ldo_5V is to the RIGHT of the blocker, it can go direct to V5V
    // Creates a 3-point L-shape: (381,177.8) -> (254,177.8) -> (254,128)
    fixture
        .add_obstacle("Ldo_5V", 381.0, 127.0, 153.0, 77.0)
        .add_port_on_obstacle("Ldo_5V_Out", 381.0, 177.8, ConnDirFlags::LEFT, "Ldo_5V");

    // Ldo_3V3 module - source 2
    // At Y=266.7, which is BELOW the blocker, so direct route is blocked
    // Must go around the blocker, creating a 5-point path
    fixture
        .add_obstacle("Ldo_3V3", 381.0, 241.0, 153.0, 77.0)
        .add_port_on_obstacle("Ldo_3V3_In", 381.0, 266.7, ConnDirFlags::LEFT, "Ldo_3V3");

    // HallEncoder - source 3
    // Also below the blocker, creates a 5-point path
    fixture
        .add_obstacle("HallEncoder", 343.0, 406.0, 229.0, 102.0)
        .add_port_on_obstacle("Hall_V5V", 343.0, 431.8, ConnDirFlags::LEFT, "HallEncoder");

    // All connectors on V5V net
    // After routing and nudging:
    // - Route 1 (3-point) should be at Y=177.8
    // - Routes 2&3 (5-point) should ALSO align to Y=177.8
    // - But currently they center at a different Y, creating a divet
    fixture
        .add_connector_with_net("v5v_ldo5v_out", "V5V", "Ldo_5V_Out", "V5V")
        .add_connector_with_net("v5v_to_ldo3v3", "V5V", "Ldo_3V3_In", "V5V")
        .add_connector_with_net("v5v_to_hall", "V5V", "Hall_V5V", "V5V");

    let input = fixture.build();
    let svg = render_scenario(&input);
    assert_svg_snapshot("same_net_fanin_divet", &svg);
}

/// Reproduces the FANS ALERT redundant box from ControlHubBoard.
///
/// The endpoints are fixed: both binary MST edges must still terminate at the
/// hlabel target. The cleaner route for the second edge should join the first
/// edge at the first same-net intersection, then follow the existing trunk to
/// the target instead of creating a rectangular detour.
#[test]
fn test_same_net_mid_segment_join_follows_trunk_to_fixed_target() {
    let mut input = RouterInput::new();

    input.add_port(Port::new(
        "alert_source_a",
        Point::new(-660.4, -571.5),
        ConnDirFlags::DOWN,
    ));
    input.add_port(Port::new(
        "alert_source_b",
        Point::new(-457.2, -508.0),
        ConnDirFlags::LEFT,
    ));
    input.add_port(Port::new(
        "alert_hlabel",
        Point::new(-1054.1, -495.3),
        ConnDirFlags::RIGHT,
    ));

    input.add_connector(Connector::with_net(
        "alert_a_to_label",
        "alert_source_a",
        "alert_hlabel",
        "ALERT",
    ));
    input.add_connector(Connector::with_net(
        "alert_b_to_label",
        "alert_source_b",
        "alert_hlabel",
        "ALERT",
    ));

    let router = OrthoRouter::new(RouterConfig::default());
    let output = router.route(&input);
    let branch = output
        .paths
        .iter()
        .find(|path| path.connector_id == "alert_b_to_label")
        .expect("branch route should be present");

    let rounded_points: Vec<_> = branch
        .points
        .iter()
        .map(|point| format!("({:.1},{:.1})", point.x, point.y))
        .collect();

    assert_eq!(
        rounded_points,
        vec![
            "(-457.2,-508.0)",
            "(-660.4,-508.0)",
            "(-660.4,-495.3)",
            "(-1054.1,-495.3)",
        ]
    );
}
