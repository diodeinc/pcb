//! Debug utilities for routing visualization and diagnostics.
//!
//! This module provides functions to render routing steps to SVG files
//! for debugging and analysis.

use crate::render::{RenderConfig, SvgRenderer};
use crate::router::RoutingSteps;
use crate::types::{RouterInput, RouterOutput, RoutingTiming};
use std::fs;
use std::io;
use std::path::Path;

/// Render all routing steps to SVG files in the given directory.
///
/// Creates the following files:
/// - `01_input.svg` - Obstacles and ports only
/// - `02_visibility_graph.svg` - With visibility graph overlay
/// - `03_pathfinding.svg` - Paths after A* search
/// - `04_improve_crossings.svg` - Paths after crossing improvement
/// - `05a_nudge_x_unify.svg` - After X-dimension unifying pass
/// - `05b_nudge_x_nudge.svg` - After X-dimension nudging pass
/// - `05c_nudge_y_unify.svg` - After Y-dimension unifying pass
/// - `05d_nudge_y_nudge.svg` - After Y-dimension nudging pass
/// - `05e_nudge_merge.svg` - After same-net path merging
/// - `06_final.svg` - Final paths after all processing
/// - `timing.json` - Timing breakdown
///
/// # Arguments
/// * `input` - The router input (obstacles, ports, connectors)
/// * `steps` - The routing steps captured by `route_with_steps`
/// * `output_dir` - Directory to write files to (created if doesn't exist)
pub fn render_routing_steps(
    input: &RouterInput,
    steps: &RoutingSteps,
    output_dir: &Path,
) -> io::Result<()> {
    // Create output directory if it doesn't exist
    fs::create_dir_all(output_dir)?;

    // Configure renderer with color-by-net for better visualization
    let config = RenderConfig {
        color_by_net: true,
        ..Default::default()
    };

    let renderer = SvgRenderer::new(config.clone());

    // 01_input.svg - Just obstacles and ports
    let svg_input = renderer.render_input(input);
    fs::write(output_dir.join("01_input.svg"), svg_input)?;

    // 02_visibility_graph.svg - With visibility graph overlay
    let mut config_with_graph = config.clone();
    config_with_graph.show_visibility_graph = true;
    let renderer_with_graph = SvgRenderer::new(config_with_graph);
    let svg_graph =
        renderer_with_graph.render_with_graph(input, &RouterOutput::default(), Some(&steps.graph));
    fs::write(output_dir.join("02_visibility_graph.svg"), svg_graph)?;

    // 03_pathfinding.svg - Paths after A* search
    let output_pathfinding = RouterOutput {
        paths: steps.paths_after_pathfinding.clone(),
        junctions: Vec::new(),
    };
    let svg_pathfinding = renderer.render(input, &output_pathfinding);
    fs::write(output_dir.join("03_pathfinding.svg"), svg_pathfinding)?;

    // 04_improve_crossings.svg - Paths after crossing improvement
    let output_crossings = RouterOutput {
        paths: steps.paths_after_improve_crossings.clone(),
        junctions: Vec::new(),
    };
    let svg_crossings = renderer.render(input, &output_crossings);
    fs::write(output_dir.join("04_improve_crossings.svg"), svg_crossings)?;

    // Render nudging passes if debug info is available
    if let Some(nudging_debug) = &steps.nudging_debug {
        render_nudging_passes(input, nudging_debug, output_dir, &config)?;
    }

    // 06_final.svg - Final paths after nudging (numbered 06 to come after nudging passes)
    let output_final = RouterOutput {
        paths: steps.paths_final.clone(),
        junctions: Vec::new(),
    };
    let svg_final = renderer.render(input, &output_final);
    fs::write(output_dir.join("06_final.svg"), svg_final)?;

    // timing.json - Timing breakdown
    let timing_json = serde_json::to_string_pretty(&steps.timing).map_err(io::Error::other)?;
    fs::write(output_dir.join("timing.json"), timing_json)?;

    Ok(())
}

/// Render all nudging pass visualizations.
fn render_nudging_passes(
    input: &RouterInput,
    nudging_debug: &crate::nudging_libavoid::NudgingDebugInfo,
    output_dir: &Path,
    base_config: &RenderConfig,
) -> io::Result<()> {
    // Configure renderer for nudging visualization
    let mut config = base_config.clone();
    config.show_channel_limits = true;

    let renderer = SvgRenderer::new(config);

    // Map pass names to file names
    let file_mapping = [
        ("x_unify", "05a_nudge_x_unify.svg"),
        ("x_nudge", "05b_nudge_x_nudge.svg"),
        ("y_unify", "05c_nudge_y_unify.svg"),
        ("y_nudge", "05d_nudge_y_nudge.svg"),
    ];

    for pass_info in &nudging_debug.passes {
        let filename = file_mapping
            .iter()
            .find(|(name, _)| pass_info.pass_name == *name)
            .map(|(_, file)| *file)
            .unwrap_or_else(|| {
                // Fallback for unknown pass names
                Box::leak(format!("05_nudge_{}.svg", pass_info.pass_name).into_boxed_str())
            });

        let svg = renderer.render_nudging_pass(input, pass_info);
        fs::write(output_dir.join(filename), svg)?;
    }

    // 05e_nudge_merge.svg - After same-net merging
    let output_merge = RouterOutput {
        paths: nudging_debug.paths_after_merge.clone(),
        junctions: Vec::new(),
    };
    let base_renderer = SvgRenderer::new(base_config.clone());
    let svg_merge = base_renderer.render(input, &output_merge);
    fs::write(output_dir.join("05e_nudge_merge.svg"), svg_merge)?;

    Ok(())
}

/// Load a RouterInput from a JSON file.
pub fn load_router_input(path: &Path) -> io::Result<RouterInput> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Process a single JSON input file and write debug outputs.
///
/// # Arguments
/// * `input_path` - Path to the JSON input file
/// * `output_dir` - Base output directory (a subdirectory named after the input file will be created)
///
/// # Returns
/// The timing information for the routing.
pub fn process_debug_input(input_path: &Path, output_dir: &Path) -> io::Result<RoutingTiming> {
    // Load input
    let input = load_router_input(input_path)?;

    // Create output subdirectory based on input filename
    let input_name = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let output_subdir = output_dir.join(input_name);

    // Route with step capture
    let router = crate::OrthoRouter::with_defaults();
    let (_output, steps) = router.route_with_steps(&input);

    // Render all steps
    render_routing_steps(&input, &steps, &output_subdir)?;

    Ok(steps.timing)
}

/// Process all JSON files in a directory and write debug outputs.
///
/// # Arguments
/// * `input_dir` - Directory containing JSON input files
/// * `output_dir` - Base output directory
///
/// # Returns
/// A vector of (filename, timing) pairs for all processed files.
pub fn process_debug_inputs(
    input_dir: &Path,
    output_dir: &Path,
) -> io::Result<Vec<(String, RoutingTiming)>> {
    let mut results = Vec::new();

    // Find all .json files in input directory
    for entry in fs::read_dir(input_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            match process_debug_input(&path, output_dir) {
                Ok(timing) => {
                    results.push((filename, timing));
                }
                Err(e) => {
                    eprintln!("Error processing {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ConnDirFlags, Connector, Obstacle, Point, Port, Rect};
    use tempfile::TempDir;

    #[test]
    fn test_render_routing_steps() {
        // Create a simple test input
        let mut input = RouterInput::new();
        input.add_obstacle(Obstacle::new("obs1", Rect::new(50.0, 50.0, 100.0, 100.0)));
        input.add_port(Port::new("p1", Point::new(0.0, 75.0), ConnDirFlags::RIGHT));
        input.add_port(Port::new("p2", Point::new(150.0, 75.0), ConnDirFlags::LEFT));
        input.add_connector(Connector::new("c1", "p1", "p2"));

        // Route with steps
        let router = crate::OrthoRouter::with_defaults();
        let (_output, steps) = router.route_with_steps(&input);

        // Render to temp directory
        let temp_dir = TempDir::new().unwrap();
        render_routing_steps(&input, &steps, temp_dir.path()).unwrap();

        // Verify files were created
        assert!(temp_dir.path().join("01_input.svg").exists());
        assert!(temp_dir.path().join("02_visibility_graph.svg").exists());
        assert!(temp_dir.path().join("03_pathfinding.svg").exists());
        assert!(temp_dir.path().join("04_improve_crossings.svg").exists());
        assert!(temp_dir.path().join("06_final.svg").exists());
        assert!(temp_dir.path().join("timing.json").exists());

        // Verify timing.json is valid JSON
        let timing_content = fs::read_to_string(temp_dir.path().join("timing.json")).unwrap();
        let _: RoutingTiming = serde_json::from_str(&timing_content).unwrap();
    }
}
