//! SVG rendering for visualization and snapshot testing.
//!
//! This module provides SVG output for visualizing routing inputs and outputs,
//! useful for debugging and snapshot testing.

use crate::junction::Junction;
use crate::types::{ConnDirFlags, Direction, Point, Rect, RouterInput, RouterOutput};
use crate::visibility::VisibilityGraph;
use std::collections::HashMap;
use std::fmt::Write;

/// Escape special XML characters in a string for use in SVG attributes.
fn escape_xml(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '&' => result.push_str("&amp;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            _ => result.push(c),
        }
    }
    result
}

/// Configuration for SVG rendering.
#[derive(Debug, Clone)]
pub struct RenderConfig {
    /// Padding around the content.
    pub padding: f64,
    /// Scale factor for coordinates.
    pub scale: f64,
    /// Stroke width for obstacles.
    pub obstacle_stroke_width: f64,
    /// Fill color for obstacles.
    pub obstacle_fill: String,
    /// Stroke color for obstacles.
    pub obstacle_stroke: String,
    /// Radius for port circles.
    pub port_radius: f64,
    /// Fill color for ports.
    pub port_fill: String,
    /// Stroke color for ports.
    pub port_stroke: String,
    /// Length of visibility direction indicator.
    pub visibility_indicator_length: f64,
    /// Stroke width for routes.
    pub route_stroke_width: f64,
    /// Stroke color for routes.
    pub route_stroke: String,
    /// Whether to show the visibility graph.
    pub show_visibility_graph: bool,
    /// Stroke color for visibility graph edges.
    pub visibility_graph_stroke: String,
    /// Stroke width for visibility graph edges.
    pub visibility_graph_stroke_width: f64,
    /// Radius for visibility graph vertices.
    pub visibility_graph_vertex_radius: f64,
    /// Whether to color ports and routes by net.
    pub color_by_net: bool,
    /// Color palette for nets (cycles through if more nets than colors).
    pub net_colors: Vec<String>,
    /// Radius for junction dots.
    pub junction_radius: f64,
    /// Fill color for junction dots.
    pub junction_fill: String,
    /// Whether to show channel limits as bands.
    pub show_channel_limits: bool,
    /// Fill color for channel limit bands.
    pub channel_limit_fill: String,
    /// Opacity for channel limit bands.
    pub channel_limit_opacity: f64,
    /// Colors for segment types (fixed, final, zigzag, free).
    pub segment_type_colors: SegmentTypeColors,
}

/// Colors for different segment types in nudging visualization.
#[derive(Debug, Clone)]
pub struct SegmentTypeColors {
    /// Color for fixed segments (cannot move).
    pub fixed: String,
    /// Color for final segments (first/last, resist movement).
    pub final_seg: String,
    /// Color for zigzag segments (S/Z-bend, prefer centering).
    pub zigzag: String,
    /// Color for free segments (other movable).
    pub free: String,
}

impl Default for SegmentTypeColors {
    fn default() -> Self {
        Self {
            fixed: "#e74c3c".to_string(),     // Red
            final_seg: "#f39c12".to_string(), // Orange
            zigzag: "#3498db".to_string(),    // Blue
            free: "#2ecc71".to_string(),      // Green
        }
    }
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            padding: 20.0,
            scale: 1.0,
            obstacle_stroke_width: 1.0,
            obstacle_fill: "#e0e0e0".to_string(),
            obstacle_stroke: "#333333".to_string(),
            port_radius: 4.0,
            port_fill: "#4a90d9".to_string(),
            port_stroke: "#2d5a87".to_string(),
            visibility_indicator_length: 10.0,
            route_stroke_width: 2.0,
            route_stroke: "#d94a4a".to_string(),
            show_visibility_graph: false,
            visibility_graph_stroke: "#aaaaaa".to_string(),
            visibility_graph_stroke_width: 0.5,
            visibility_graph_vertex_radius: 2.0,
            color_by_net: false,
            net_colors: vec![
                "#e74c3c".to_string(), // Red
                "#3498db".to_string(), // Blue
                "#2ecc71".to_string(), // Green
                "#9b59b6".to_string(), // Purple
                "#f39c12".to_string(), // Orange
                "#1abc9c".to_string(), // Teal
                "#e91e63".to_string(), // Pink
                "#00bcd4".to_string(), // Cyan
            ],
            junction_radius: 3.0,
            junction_fill: "#333333".to_string(),
            show_channel_limits: false,
            channel_limit_fill: "#90EE90".to_string(), // Light green - more visible
            channel_limit_opacity: 0.5,
            segment_type_colors: SegmentTypeColors::default(),
        }
    }
}

/// Render routing input and output to SVG.
pub struct SvgRenderer {
    config: RenderConfig,
}

impl SvgRenderer {
    pub fn new(config: RenderConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(RenderConfig::default())
    }

    /// Render input (obstacles and ports) to SVG.
    pub fn render_input(&self, input: &RouterInput) -> String {
        self.render(input, &RouterOutput::default())
    }

    /// Render input, output, and optionally the visibility graph to SVG.
    pub fn render_with_graph(
        &self,
        input: &RouterInput,
        output: &RouterOutput,
        graph: Option<&VisibilityGraph>,
    ) -> String {
        self.render_internal(input, output, graph)
    }

    /// Render input and output (including routes) to SVG.
    pub fn render(&self, input: &RouterInput, output: &RouterOutput) -> String {
        self.render_full(input, output, None, &[])
    }

    /// Render input, output, and junctions to SVG.
    pub fn render_with_junctions(
        &self,
        input: &RouterInput,
        output: &RouterOutput,
        junctions: &[Junction],
    ) -> String {
        self.render_full(input, output, None, junctions)
    }

    fn render_internal(
        &self,
        input: &RouterInput,
        output: &RouterOutput,
        graph: Option<&VisibilityGraph>,
    ) -> String {
        self.render_full(input, output, graph, &[])
    }

    /// Render input, output, optional visibility graph, and junctions to SVG.
    pub fn render_full(
        &self,
        input: &RouterInput,
        output: &RouterOutput,
        graph: Option<&VisibilityGraph>,
        junctions: &[Junction],
    ) -> String {
        let bounds = self.calculate_bounds(input);
        let (view_width, view_height) = self.calculate_view_size(&bounds);

        // Build net color mappings if color_by_net is enabled
        let (port_to_net, connector_to_net, net_to_color) = if self.config.color_by_net {
            self.build_net_mappings(input)
        } else {
            (HashMap::new(), HashMap::new(), HashMap::new())
        };

        let mut svg = String::new();

        // SVG header
        writeln!(
            &mut svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="{} {} {} {}">"#,
            view_width,
            view_height,
            bounds.min_x - self.config.padding,
            bounds.min_y - self.config.padding,
            view_width,
            view_height
        )
        .unwrap();

        // Background
        writeln!(
            &mut svg,
            r#"  <rect x="{}" y="{}" width="{}" height="{}" fill="white"/>"#,
            bounds.min_x - self.config.padding,
            bounds.min_y - self.config.padding,
            view_width,
            view_height
        )
        .unwrap();

        // Obstacles
        writeln!(&mut svg, r#"  <g id="obstacles">"#).unwrap();
        for obstacle in &input.obstacles {
            self.render_obstacle(&mut svg, &obstacle.bounds, &obstacle.id);
        }
        writeln!(&mut svg, "  </g>").unwrap();

        // Visibility graph (if provided and enabled)
        if self.config.show_visibility_graph {
            if let Some(graph) = graph {
                self.render_visibility_graph(&mut svg, graph);
            }
        }

        // Routes (if any)
        if !output.paths.is_empty() {
            writeln!(&mut svg, r#"  <g id="routes">"#).unwrap();
            for path in &output.paths {
                let color = if self.config.color_by_net {
                    connector_to_net
                        .get(&path.connector_id)
                        .and_then(|net| net_to_color.get(net))
                        .cloned()
                } else {
                    None
                };
                self.render_route(&mut svg, &path.points, &path.connector_id, color.as_deref());
            }
            writeln!(&mut svg, "  </g>").unwrap();
        }

        // Junctions (if any)
        if !junctions.is_empty() {
            writeln!(&mut svg, r#"  <g id="junctions">"#).unwrap();
            for junction in junctions {
                let color = if self.config.color_by_net {
                    net_to_color.get(&junction.net_id).cloned()
                } else {
                    None
                };
                self.render_junction(&mut svg, &junction.position, color.as_deref());
            }
            writeln!(&mut svg, "  </g>").unwrap();
        }

        // Ports
        writeln!(&mut svg, r#"  <g id="ports">"#).unwrap();
        for port in &input.ports {
            let color = if self.config.color_by_net {
                port_to_net
                    .get(&port.id)
                    .and_then(|net| net_to_color.get(net))
                    .cloned()
            } else {
                None
            };
            self.render_port(
                &mut svg,
                &port.position,
                port.visibility,
                &port.id,
                color.as_deref(),
            );
        }
        writeln!(&mut svg, "  </g>").unwrap();

        // SVG footer
        writeln!(&mut svg, "</svg>").unwrap();

        svg
    }

    /// Build mappings from ports/connectors to nets and nets to colors.
    fn build_net_mappings(
        &self,
        input: &RouterInput,
    ) -> (
        HashMap<String, String>,
        HashMap<String, String>,
        HashMap<String, String>,
    ) {
        let mut port_to_net: HashMap<String, String> = HashMap::new();
        let mut connector_to_net: HashMap<String, String> = HashMap::new();
        let mut net_ids: Vec<String> = Vec::new();

        // Build mappings from connectors
        for connector in &input.connectors {
            let net_id = connector.effective_net_id().to_string();

            // Track unique net IDs
            if !net_ids.contains(&net_id) {
                net_ids.push(net_id.clone());
            }

            // Map connector to net
            connector_to_net.insert(connector.id.clone(), net_id.clone());

            // Map ports to net
            port_to_net.insert(connector.source_port_id.clone(), net_id.clone());
            port_to_net.insert(connector.target_port_id.clone(), net_id);
        }

        // Assign colors to nets
        let mut net_to_color: HashMap<String, String> = HashMap::new();
        for (i, net_id) in net_ids.iter().enumerate() {
            let color_idx = i % self.config.net_colors.len();
            net_to_color.insert(net_id.clone(), self.config.net_colors[color_idx].clone());
        }

        (port_to_net, connector_to_net, net_to_color)
    }

    fn render_obstacle(&self, svg: &mut String, bounds: &Rect, id: &str) {
        writeln!(
            svg,
            r#"    <rect x="{}" y="{}" width="{}" height="{}" fill="{}" stroke="{}" stroke-width="{}" data-id="{}"/>"#,
            bounds.min_x,
            bounds.min_y,
            bounds.width(),
            bounds.height(),
            self.config.obstacle_fill,
            self.config.obstacle_stroke,
            self.config.obstacle_stroke_width,
            escape_xml(id)
        )
        .unwrap();
    }

    fn render_port(
        &self,
        svg: &mut String,
        pos: &Point,
        visibility: ConnDirFlags,
        id: &str,
        color_override: Option<&str>,
    ) {
        let fill_color = color_override.unwrap_or(&self.config.port_fill);
        let stroke_color = color_override
            .map(darken_color)
            .unwrap_or_else(|| self.config.port_stroke.clone());

        // Port circle
        writeln!(
            svg,
            r#"    <circle cx="{}" cy="{}" r="{}" fill="{}" stroke="{}" stroke-width="1" data-id="{}"/>"#,
            pos.x,
            pos.y,
            self.config.port_radius,
            fill_color,
            stroke_color,
            escape_xml(id)
        )
        .unwrap();

        // Visibility direction indicators
        let len = self.config.visibility_indicator_length;
        let directions = [
            (Direction::Up, 0.0, -len),
            (Direction::Down, 0.0, len),
            (Direction::Left, -len, 0.0),
            (Direction::Right, len, 0.0),
        ];

        for (dir, dx, dy) in directions {
            if visibility.allows(dir) {
                writeln!(
                    svg,
                    r#"    <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}" stroke-width="1.5" stroke-linecap="round"/>"#,
                    pos.x,
                    pos.y,
                    pos.x + dx,
                    pos.y + dy,
                    stroke_color
                )
                .unwrap();
            }
        }
    }

    fn render_route(
        &self,
        svg: &mut String,
        points: &[Point],
        id: &str,
        color_override: Option<&str>,
    ) {
        if points.len() < 2 {
            return;
        }

        let stroke_color = color_override.unwrap_or(&self.config.route_stroke);

        let mut path_data = format!("M {} {}", points[0].x, points[0].y);
        for point in &points[1..] {
            write!(&mut path_data, " L {} {}", point.x, point.y).unwrap();
        }

        writeln!(
            svg,
            r#"    <path d="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linecap="round" stroke-linejoin="round" data-id="{}"/>"#,
            path_data,
            stroke_color,
            self.config.route_stroke_width,
            escape_xml(id)
        )
        .unwrap();
    }

    fn render_junction(&self, svg: &mut String, pos: &Point, color_override: Option<&str>) {
        let fill_color = color_override.unwrap_or(&self.config.junction_fill);

        writeln!(
            svg,
            r#"    <circle cx="{}" cy="{}" r="{}" fill="{}"/>"#,
            pos.x, pos.y, self.config.junction_radius, fill_color
        )
        .unwrap();
    }

    fn render_visibility_graph(&self, svg: &mut String, graph: &VisibilityGraph) {
        writeln!(svg, r#"  <g id="visibility-graph" opacity="0.5">"#).unwrap();

        // Render edges
        for vertex in &graph.vertices {
            for edge in graph.get_edges(vertex.id) {
                if let Some(to_vertex) = graph.get_vertex(edge.to) {
                    // Only render each edge once (from lower to higher vertex ID)
                    if vertex.id.0 < edge.to.0 {
                        writeln!(
                            svg,
                            r#"    <line x1="{}" y1="{}" x2="{}" y2="{}" stroke="{}" stroke-width="{}"/>"#,
                            vertex.position.x,
                            vertex.position.y,
                            to_vertex.position.x,
                            to_vertex.position.y,
                            self.config.visibility_graph_stroke,
                            self.config.visibility_graph_stroke_width
                        )
                        .unwrap();
                    }
                }
            }
        }

        // Render vertices (small dots)
        for vertex in &graph.vertices {
            // Skip port vertices (they'll be rendered as ports)
            if vertex.port_id.is_some() {
                continue;
            }
            writeln!(
                svg,
                r#"    <circle cx="{}" cy="{}" r="{}" fill="{}"/>"#,
                vertex.position.x,
                vertex.position.y,
                self.config.visibility_graph_vertex_radius,
                self.config.visibility_graph_stroke
            )
            .unwrap();
        }

        writeln!(svg, "  </g>").unwrap();
    }

    fn calculate_bounds(&self, input: &RouterInput) -> Rect {
        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;

        // Include obstacles
        for obstacle in &input.obstacles {
            min_x = min_x.min(obstacle.bounds.min_x);
            min_y = min_y.min(obstacle.bounds.min_y);
            max_x = max_x.max(obstacle.bounds.max_x);
            max_y = max_y.max(obstacle.bounds.max_y);
        }

        // Include ports
        for port in &input.ports {
            min_x = min_x.min(port.position.x);
            min_y = min_y.min(port.position.y);
            max_x = max_x.max(port.position.x);
            max_y = max_y.max(port.position.y);
        }

        // Handle empty input
        if min_x == f64::MAX {
            return Rect::new(0.0, 0.0, 100.0, 100.0);
        }

        Rect::new(min_x, min_y, max_x, max_y)
    }

    fn calculate_view_size(&self, bounds: &Rect) -> (f64, f64) {
        let width = (bounds.width() + 2.0 * self.config.padding) * self.config.scale;
        let height = (bounds.height() + 2.0 * self.config.padding) * self.config.scale;
        (width, height)
    }

    /// Render a nudging pass with segment visualization.
    ///
    /// Shows channel limits as shaded bands and colors segments by type.
    pub fn render_nudging_pass(
        &self,
        input: &RouterInput,
        pass_info: &crate::nudging_libavoid::NudgingPassDebugInfo,
    ) -> String {
        use crate::nudging_libavoid::SegmentType;

        let bounds = self.calculate_bounds(input);
        let (view_width, base_view_height) = self.calculate_view_size(&bounds);
        // Add extra space for the legend at the bottom (pass name + color legend + 2 lines of explainer)
        let legend_height = 60.0;
        let view_height = base_view_height + legend_height;

        let mut svg = String::new();

        // SVG header
        writeln!(
            &mut svg,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="{} {} {} {}">"#,
            view_width,
            view_height,
            bounds.min_x - self.config.padding,
            bounds.min_y - self.config.padding,
            view_width,
            view_height
        )
        .unwrap();

        // Background
        writeln!(
            &mut svg,
            r#"  <rect x="{}" y="{}" width="{}" height="{}" fill="white"/>"#,
            bounds.min_x - self.config.padding,
            bounds.min_y - self.config.padding,
            view_width,
            view_height
        )
        .unwrap();

        // Obstacles
        writeln!(&mut svg, r#"  <g id="obstacles">"#).unwrap();
        for obstacle in &input.obstacles {
            self.render_obstacle(&mut svg, &obstacle.bounds, &obstacle.id);
        }
        writeln!(&mut svg, "  </g>").unwrap();

        // Channel limits (if enabled)
        if self.config.show_channel_limits {
            writeln!(
                &mut svg,
                r#"  <g id="channel-limits" opacity="{}">"#,
                self.config.channel_limit_opacity
            )
            .unwrap();

            for seg in &pass_info.segments {
                // Skip segments with infinite limits
                if seg.min_space_limit <= -1e8 || seg.max_space_limit >= 1e8 {
                    continue;
                }

                // Render as a rectangle showing the movement range
                let (x, y, width, height) = if pass_info.dimension == 0 {
                    // X dimension (vertical segments) - show horizontal band
                    (
                        seg.min_space_limit,
                        seg.alt_range.0,
                        seg.max_space_limit - seg.min_space_limit,
                        seg.alt_range.1 - seg.alt_range.0,
                    )
                } else {
                    // Y dimension (horizontal segments) - show vertical band
                    (
                        seg.alt_range.0,
                        seg.min_space_limit,
                        seg.alt_range.1 - seg.alt_range.0,
                        seg.max_space_limit - seg.min_space_limit,
                    )
                };

                // Only render if dimensions are positive
                if width > 0.0 && height > 0.0 {
                    writeln!(
                        &mut svg,
                        r#"    <rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}" stroke="none"/>"#,
                        x, y, width, height, self.config.channel_limit_fill
                    )
                    .unwrap();
                }
            }

            writeln!(&mut svg, "  </g>").unwrap();
        }

        // Routes with segment type coloring
        writeln!(&mut svg, r#"  <g id="routes">"#).unwrap();
        for path in &pass_info.paths_after {
            for i in 0..path.points.len().saturating_sub(1) {
                let p1 = &path.points[i];
                let p2 = &path.points[i + 1];

                // Determine segment type by finding matching debug info
                let seg_type =
                    self.find_segment_type_for_path_segment(&path.connector_id, p1, p2, pass_info);

                let color = match seg_type {
                    SegmentType::Fixed => &self.config.segment_type_colors.fixed,
                    SegmentType::Final => &self.config.segment_type_colors.final_seg,
                    SegmentType::Zigzag => &self.config.segment_type_colors.zigzag,
                    SegmentType::Free => &self.config.segment_type_colors.free,
                };

                writeln!(
                    &mut svg,
                    r#"    <line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="{}" stroke-linecap="round"/>"#,
                    p1.x, p1.y, p2.x, p2.y, color, self.config.route_stroke_width
                )
                .unwrap();
            }
        }
        writeln!(&mut svg, "  </g>").unwrap();

        // Ports
        writeln!(&mut svg, r#"  <g id="ports">"#).unwrap();
        for port in &input.ports {
            self.render_port(&mut svg, &port.position, port.visibility, &port.id, None);
        }
        writeln!(&mut svg, "  </g>").unwrap();

        // Legend with pass name
        self.render_segment_type_legend(&mut svg, &bounds, &pass_info.pass_name);

        writeln!(&mut svg, "</svg>").unwrap();
        svg
    }

    /// Find the segment type for a path segment based on debug info.
    fn find_segment_type_for_path_segment(
        &self,
        connector_id: &str,
        p1: &Point,
        p2: &Point,
        pass_info: &crate::nudging_libavoid::NudgingPassDebugInfo,
    ) -> crate::nudging_libavoid::SegmentType {
        use crate::nudging_libavoid::SegmentType;

        // Check if this segment is in the dimension being processed
        let is_in_dim = if pass_info.dimension == 0 {
            (p1.x - p2.x).abs() < 1e-9 // Vertical segment
        } else {
            (p1.y - p2.y).abs() < 1e-9 // Horizontal segment
        };

        if !is_in_dim {
            // Segment not in this dimension - color as free
            return SegmentType::Free;
        }

        // Find matching segment in debug info
        for seg in &pass_info.segments {
            if seg.connector_id != connector_id {
                continue;
            }

            // Check if positions match
            let seg_pos = seg.position_after;
            let path_pos = if pass_info.dimension == 0 { p1.x } else { p1.y };

            if (seg_pos - path_pos).abs() < 1.0 {
                // Check if alt ranges overlap
                let (alt_min, alt_max) = if pass_info.dimension == 0 {
                    (p1.y.min(p2.y), p1.y.max(p2.y))
                } else {
                    (p1.x.min(p2.x), p1.x.max(p2.x))
                };

                let overlap = alt_min <= seg.alt_range.1 + 1.0 && alt_max >= seg.alt_range.0 - 1.0;
                if overlap {
                    return seg.segment_type;
                }
            }
        }

        SegmentType::Free
    }

    /// Render a legend and explainer for segment types.
    fn render_segment_type_legend(&self, svg: &mut String, bounds: &Rect, pass_name: &str) {
        let legend_x = bounds.min_x - self.config.padding + 5.0;
        let legend_y = bounds.max_y + 10.0;

        writeln!(svg, r#"  <g id="legend">"#).unwrap();

        // Pass name
        writeln!(
            svg,
            r#"    <text x="{:.2}" y="{:.2}" font-size="11" font-weight="bold" font-family="sans-serif">Pass: {}</text>"#,
            legend_x, legend_y, pass_name
        )
        .unwrap();

        // Segment type legend
        let entries = [
            ("Fixed", &self.config.segment_type_colors.fixed),
            ("Final", &self.config.segment_type_colors.final_seg),
            ("Zigzag", &self.config.segment_type_colors.zigzag),
            ("Free", &self.config.segment_type_colors.free),
        ];

        let legend_row_y = legend_y + 14.0;
        for (i, (label, color)) in entries.iter().enumerate() {
            let x = legend_x + (i as f64 * 70.0);

            writeln!(
                svg,
                r#"    <line x1="{:.2}" y1="{:.2}" x2="{:.2}" y2="{:.2}" stroke="{}" stroke-width="3"/>"#,
                x, legend_row_y, x + 15.0, legend_row_y, color
            )
            .unwrap();
            writeln!(
                svg,
                r#"    <text x="{:.2}" y="{:.2}" font-size="9" font-family="sans-serif">{}</text>"#,
                x + 18.0,
                legend_row_y + 3.0,
                label
            )
            .unwrap();
        }

        // Explainer text - two lines
        let explainer_y = legend_row_y + 16.0;
        writeln!(
            svg,
            "    <text x=\"{:.2}\" y=\"{:.2}\" font-size=\"8\" font-family=\"sans-serif\" fill=\"#666\">Green bands = channel limits (allowed movement range). Fixed=immovable, Final=resist movement, Zigzag=prefer centering, Free=movable.</text>",
            legend_x,
            explainer_y
        )
        .unwrap();
        writeln!(
            svg,
            "    <text x=\"{:.2}\" y=\"{:.2}\" font-size=\"8\" font-family=\"sans-serif\" fill=\"#666\">Unify pass = light constraints to establish segment ordering. Nudge pass = full separation constraints to push segments apart.</text>",
            legend_x,
            explainer_y + 10.0
        )
        .unwrap();

        writeln!(svg, "  </g>").unwrap();
    }
}

impl Default for SvgRenderer {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Darken a hex color by reducing its brightness.
fn darken_color(hex: &str) -> String {
    // Parse hex color
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return format!("#{}", hex); // Return as-is if not valid
    }

    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);

    // Darken by 30%
    let factor = 0.7;
    let r = (r as f64 * factor) as u8;
    let g = (g as f64 * factor) as u8;
    let b = (b as f64 * factor) as u8;

    format!("#{:02x}{:02x}{:02x}", r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Obstacle, Port};

    #[test]
    fn test_render_empty_input() {
        let renderer = SvgRenderer::with_defaults();
        let input = RouterInput::new();
        let svg = renderer.render_input(&input);
        assert!(svg.contains("<svg"));
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn test_render_obstacle() {
        let renderer = SvgRenderer::with_defaults();
        let mut input = RouterInput::new();
        input.add_obstacle(Obstacle::new("test_obs", Rect::new(10.0, 10.0, 50.0, 50.0)));

        let svg = renderer.render_input(&input);
        assert!(svg.contains("data-id=\"test_obs\""));
        assert!(svg.contains("<rect"));
    }

    #[test]
    fn test_render_port() {
        let renderer = SvgRenderer::with_defaults();
        let mut input = RouterInput::new();
        input.add_port(Port::new(
            "test_port",
            Point::new(25.0, 25.0),
            ConnDirFlags::RIGHT,
        ));

        let svg = renderer.render_input(&input);
        assert!(svg.contains("data-id=\"test_port\""));
        assert!(svg.contains("<circle"));
    }
}
