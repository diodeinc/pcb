use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use ipc2581::types::ecad::Layer;
use minijinja::{Environment, context};
use pcb_ir::dialects::ipc::{ProfileSet, View};
use serde::Serialize;

use crate::UnitFormat;
use crate::accessors::{ColorInfo, IpcAccessor, StackupLayerType, SurfaceFinishInfo};
use crate::geometry;
use crate::utils::file as file_utils;

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

pub fn execute(
    input_file: &Path,
    output_file: Option<&Path>,
    unit_format: UnitFormat,
) -> Result<()> {
    // Load and parse IPC-2581 file
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);

    // Generate HTML
    let html = generate_html(&accessor, unit_format)?;

    // Determine output path
    let output_path = match output_file {
        Some(path) => path.to_path_buf(),
        None => {
            let mut path = input_file.to_path_buf();
            path.set_extension("html");
            path
        }
    };

    // Write HTML to file
    std::fs::write(&output_path, html)
        .with_context(|| format!("Failed to write HTML to {}", output_path.display()))?;

    println!("✓ HTML exported to {}", output_path.display());

    Ok(())
}

pub fn generate_html(accessor: &IpcAccessor, unit_format: UnitFormat) -> Result<String> {
    let mut env = Environment::new();
    env.add_template("html", HTML_TEMPLATE)
        .context("Failed to add HTML template")?;

    let template = env.get_template("html")?;

    // Extract data
    let board_summary = extract_board_summary(accessor, unit_format)?;
    let stackup = extract_stackup_data(accessor, unit_format);
    let rendered_layers = extract_rendered_layers(accessor)?;
    let version = env!("CARGO_PKG_VERSION");

    // Extract file metadata
    let ipc = accessor.ipc();
    let content = ipc.content();
    let ipc_revision = ipc.revision();
    let mode_str = if let Some(level) = content.function_mode.level {
        format!("{}/{:?}", content.function_mode.mode.as_str(), level)
    } else {
        content.function_mode.mode.as_str().to_string()
    };
    let file_metadata = accessor.file_metadata();

    // Format software string in Rust
    let software_str = file_metadata
        .as_ref()
        .and_then(|m| m.software.as_ref())
        .and_then(|s| s.format());
    let source_units = file_metadata.as_ref().and_then(|m| m.source_units.clone());
    let created = file_metadata.as_ref().and_then(|m| m.created.clone());
    let last_modified = file_metadata.as_ref().and_then(|m| m.last_modified.clone());

    let html = template
        .render(context! {
            board_summary,
            stackup,
            rendered_layers,
            css_styles => CSS_STYLES,
            version,
            ipc_revision,
            mode_str,
            source_units,
            created,
            last_modified,
            software_str,
        })
        .context("Failed to render HTML template")?;

    Ok(html)
}

#[derive(Serialize)]
struct BoardSummary {
    design_name: Option<String>,
    width: Option<String>,
    height: Option<String>,
    board_array: Option<BoardArraySummary>,
    thickness: Option<String>,
    copper_layers: Option<usize>,
    components: Option<usize>,
    nets: Option<usize>,
    drill_holes: Option<String>,
}

#[derive(Serialize)]
struct BoardArraySummary {
    width: Option<String>,
    height: Option<String>,
    grid: Option<BoardArrayGridSummary>,
    drill_holes: Option<String>,
    overview_svg: Option<String>,
}

#[derive(Serialize)]
struct BoardArrayGridSummary {
    columns: u32,
    rows: u32,
    board_margin: Option<String>,
    edge_rail: Option<String>,
}

#[derive(Serialize)]
struct StackupData {
    name: String,
    overall_thickness: Option<String>,
    layers: Vec<StackupLayer>,
    soldermask_color: Option<Color>,
    silkscreen_color: Option<Color>,
    surface_finish: Option<SurfaceFinish>,
    outer_copper: Option<String>,
    inner_copper: Option<String>,
}

#[derive(Serialize)]
struct SurfaceFinish {
    name: String,
    hex: String,
    is_standard: bool,
}

#[derive(Serialize)]
struct StackupLayer {
    number: String,
    name: String,
    layer_type: String,
    thickness_mm: Option<String>,
    thickness_mil: Option<String>,
    material: Option<String>,
    dk: Option<String>,
    loss_tangent: Option<String>,
    is_conductor: bool,
    is_dielectric: bool,
}

#[derive(Default, Serialize)]
struct RenderedLayers {
    stackup: Vec<RenderedLayer>,
    non_stackup: Vec<RenderedLayer>,
}

#[derive(Serialize)]
struct RenderedLayer {
    name: String,
    function: String,
    side: String,
    sequence: Option<String>,
    svg: Option<String>,
    warning: Option<String>,
    #[serde(skip_serializing)]
    has_native_content: bool,
}

#[derive(Serialize)]
struct Color {
    name: String,
    hex: String,
}

fn extract_board_summary(accessor: &IpcAccessor, unit_format: UnitFormat) -> Result<BoardSummary> {
    let layout = accessor.board_layout_info();
    let design_name = layout.as_ref().and_then(|layout| layout.board_name.clone());
    let array_overview_svg = crate::board_array::render_board_array_overview_svg(accessor)?;

    let (width, height) = if let Some(dims) = layout
        .as_ref()
        .and_then(|layout| layout.board_dimensions.as_ref())
    {
        formatted_dimensions(dims.width_mm(), dims.height_mm(), unit_format)
    } else {
        (None, None)
    };

    let board_array = layout
        .as_ref()
        .and_then(|layout| layout.board_array.as_ref())
        .map(|board_array| {
            let (width, height) = board_array
                .dimensions
                .as_ref()
                .map(|dims| formatted_dimensions(dims.width_mm(), dims.height_mm(), unit_format))
                .unwrap_or((None, None));
            BoardArraySummary {
                width,
                height,
                grid: board_array.grid.as_ref().map(|grid| BoardArrayGridSummary {
                    columns: grid.columns,
                    rows: grid.rows,
                    board_margin: grid.board_margin.as_ref().map(|margin| {
                        margin.format_shorthand(|value| format_length(value, unit_format))
                    }),
                    edge_rail: Some(
                        grid.edge_rail
                            .format_shorthand(|value| format_length(value, unit_format)),
                    ),
                }),
                drill_holes: accessor
                    .board_array_drill_stats()
                    .and_then(format_drill_count),
                overview_svg: array_overview_svg,
            }
        });

    let thickness = if let Some(stackup) = accessor.stackup_details() {
        stackup.overall_thickness_mm.map(|t| match unit_format {
            UnitFormat::Mm => format!("{:.2} mm", t),
            UnitFormat::Mil => format!("{:.1} mil", t / 0.0254),
            UnitFormat::Inch => format!("{:.4} in", t / 25.4),
        })
    } else {
        None
    };

    let copper_layers = accessor.stackup_details().map(|s| {
        s.layers
            .iter()
            .filter(|l| l.layer_type == StackupLayerType::Conductor)
            .count()
    });

    let components = accessor.component_stats().map(|stats| stats.total);
    let nets = accessor.net_stats().map(|stats| stats.count);
    let drill_holes = accessor.board_drill_stats().and_then(format_drill_count);

    Ok(BoardSummary {
        design_name,
        width,
        height,
        board_array,
        thickness,
        copper_layers,
        components,
        nets,
        drill_holes,
    })
}

fn format_drill_count(drills: crate::accessors::DrillStats) -> Option<String> {
    (drills.total_holes > 0).then(|| {
        format!(
            "{} total ({} sizes)",
            drills.total_holes, drills.unique_sizes
        )
    })
}

fn format_length(value_mm: f64, unit_format: UnitFormat) -> String {
    match unit_format {
        UnitFormat::Mm => format!("{value_mm:.2} mm"),
        UnitFormat::Mil => format!("{:.1} mil", value_mm / 0.0254),
        UnitFormat::Inch => format!("{:.3} in", value_mm / 25.4),
    }
}

fn formatted_dimensions(
    width_mm: f64,
    height_mm: f64,
    unit_format: UnitFormat,
) -> (Option<String>, Option<String>) {
    match unit_format {
        UnitFormat::Mm => (
            Some(format!("{width_mm:.2} mm")),
            Some(format!("{height_mm:.2} mm")),
        ),
        UnitFormat::Mil => (
            Some(format!("{:.1} mil", width_mm / 0.0254)),
            Some(format!("{:.1} mil", height_mm / 0.0254)),
        ),
        UnitFormat::Inch => (
            Some(format!("{:.3} in", width_mm / 25.4)),
            Some(format!("{:.3} in", height_mm / 25.4)),
        ),
    }
}

fn extract_rendered_layers(accessor: &IpcAccessor) -> Result<RenderedLayers> {
    let ipc = accessor.ipc();
    let Some(ecad) = ipc.ecad() else {
        return Ok(RenderedLayers::default());
    };
    let layers_by_ref = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| (layer.name, layer))
        .collect::<HashMap<_, _>>();

    let mut stackup_layer_refs = HashSet::new();
    let mut stackup_layers = Vec::new();

    if let Some(stackup) = ecad.cad_data.stackups.first() {
        for stackup_layer in &stackup.layers {
            stackup_layer_refs.insert(stackup_layer.layer_ref);
            let Some(layer) = layers_by_ref.get(&stackup_layer.layer_ref).copied() else {
                continue;
            };
            if layer.layer_function.is_dielectric() {
                continue;
            }

            let rendered = rendered_source_layer(
                ipc,
                layer,
                stackup_layer.layer_number.map(|number| number.to_string()),
            );
            if layer.layer_function.is_coating() && !rendered.has_native_content {
                continue;
            }
            stackup_layers.push(rendered);
        }
    } else {
        for layer in &ecad.cad_data.layers {
            if layer.layer_function.is_fabrication()
                || layer.layer_function.is_dielectric()
                || layer.layer_function.is_coating()
            {
                continue;
            }
            stackup_layer_refs.insert(layer.name);
            stackup_layers.push(rendered_source_layer(ipc, layer, None));
        }
    }

    let mut non_stackup_layers = Vec::new();

    for layer in &ecad.cad_data.layers {
        if stackup_layer_refs.contains(&layer.name) {
            continue;
        }

        let rendered = rendered_source_layer(ipc, layer, None);
        if rendered.has_native_content {
            non_stackup_layers.push(rendered);
        }
    }

    Ok(RenderedLayers {
        stackup: stackup_layers,
        non_stackup: non_stackup_layers,
    })
}

fn rendered_source_layer(
    ipc: &ipc2581::Ipc2581,
    layer: &Layer,
    sequence: Option<String>,
) -> RenderedLayer {
    let name = ipc.resolve(layer.name).to_string();
    let mut rendered = RenderedLayer {
        name: name.clone(),
        function: layer.layer_function.as_str().to_string(),
        side: layer
            .side
            .map(|side| side.as_str())
            .unwrap_or("None")
            .to_string(),
        sequence,
        svg: None,
        warning: None,
        has_native_content: false,
    };

    match geometry::extract_layer_for_view(ipc, &name, View::Board) {
        Ok(geometry) => render_extracted_layer(&mut rendered, geometry, View::Board.profile_set()),
        Err(error) => {
            rendered.warning = Some(format!("Render unavailable: {error}"));
        }
    }

    rendered
}

fn render_extracted_layer(
    rendered: &mut RenderedLayer,
    mut geometry: GeometryDocument,
    profile_set: ProfileSet,
) {
    rendered.has_native_content = geometry::render::layer_has_native_content(&geometry);
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut geometry);
    rendered.svg = Some(geometry::render::render_layer_svg(
        &geometry,
        true,
        profile_set,
    ));
    if !geometry.diagnostics.is_empty() {
        rendered.warning = Some(format!("{} warning(s)", geometry.diagnostics.len()));
    }
}

/// Format a decimal number with engineering precision:
/// - Removes unnecessary trailing zeros
/// - Maintains minimum decimal places
fn format_decimal(value: f64, min_decimals: usize, max_decimals: usize) -> String {
    let formatted = format!("{:.prec$}", value, prec = max_decimals);
    let trimmed = formatted.trim_end_matches('0');

    // Ensure minimum decimal places
    if let Some(dot_pos) = trimmed.find('.') {
        let current_decimals = trimmed.len() - dot_pos - 1;
        if current_decimals < min_decimals {
            let zeros_needed = min_decimals - current_decimals;
            format!("{}{}", trimmed, "0".repeat(zeros_needed))
        } else {
            trimmed.to_string()
        }
    } else {
        // No decimal point, add it with min decimals
        format!("{}.{}", trimmed, "0".repeat(min_decimals))
    }
}

fn extract_stackup_data(accessor: &IpcAccessor, unit_format: UnitFormat) -> Option<StackupData> {
    let stackup = accessor.stackup_details()?;

    // Format total thickness like: "1.61 mm (63.2 mil)"
    let overall_thickness = stackup.overall_thickness_mm.map(|t| match unit_format {
        UnitFormat::Mm => {
            let mm = format_decimal(t, 2, 2);
            let mil = format_decimal(t / 0.0254, 1, 1);
            format!("{} mm ({} mil)", mm, mil)
        }
        UnitFormat::Mil => {
            let mil = format_decimal(t / 0.0254, 1, 2);
            let mm = format_decimal(t, 2, 2);
            format!("{} mil ({} mm)", mil, mm)
        }
        UnitFormat::Inch => {
            let inch = format_decimal(t / 25.4, 3, 4);
            let mm = format_decimal(t, 2, 2);
            format!("{} in ({} mm)", inch, mm)
        }
    });

    let layers = stackup
        .layers
        .iter()
        .filter(|layer| {
            // Only include physical stackup layers (conductor, dielectric, soldermask)
            // Filter out "Other" layers (silkscreen, paste, etc.) as they're not part of the board structure
            layer.layer_type != StackupLayerType::Other
        })
        .map(|layer| {
            let is_conductor = layer.layer_type == StackupLayerType::Conductor;
            let is_dielectric = layer.layer_type.is_dielectric();
            let is_soldermask = layer.layer_type == StackupLayerType::Soldermask;

            // Only show thickness for conductor, dielectric, and soldermask layers
            let (thickness_mm, thickness_mil) = if is_conductor || is_dielectric || is_soldermask {
                (
                    layer.thickness_mm.map(|t| format_decimal(t, 2, 4)),
                    layer.thickness_mm.map(|t| format_decimal(t / 0.0254, 1, 2)),
                )
            } else {
                (None, None)
            };

            StackupLayer {
                number: layer.layer_number.unwrap_or(0).to_string(),
                name: layer.name.clone(),
                layer_type: layer.layer_type.as_str().to_string(),
                thickness_mm,
                thickness_mil,
                material: layer.material.clone(),
                dk: layer.dielectric_constant.map(|dk| format_decimal(dk, 1, 2)),
                loss_tangent: layer.loss_tangent.map(|lt| format_decimal(lt, 3, 4)),
                is_conductor,
                is_dielectric,
            }
        })
        .collect();

    // Calculate copper weights using helper methods
    let outer_copper = stackup.outer_copper_weight();
    let inner_copper = stackup.inner_copper_weight();

    let soldermask_color = stackup.soldermask_color.as_ref().and_then(color_to_html);
    let silkscreen_color = stackup.silkscreen_color.as_ref().and_then(color_to_html);
    let surface_finish = stackup.surface_finish.as_ref().map(surface_finish_to_html);

    Some(StackupData {
        name: stackup.name,
        overall_thickness,
        layers,
        soldermask_color,
        silkscreen_color,
        surface_finish,
        outer_copper,
        inner_copper,
    })
}

fn color_to_html(color: &ColorInfo) -> Option<Color> {
    let name = color.name.clone()?;
    let hex = color.hex_color()?;
    Some(Color { name, hex })
}

fn surface_finish_to_html(finish: &SurfaceFinishInfo) -> SurfaceFinish {
    SurfaceFinish {
        name: finish.name.clone(),
        hex: finish.hex_color(),
        is_standard: finish.is_standard, // Track but don't render
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_board_summary_separate_from_board_array_summary() {
        let ipc = ipc2581::Ipc2581::parse(board_array_design_name_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let html = generate_html(&accessor, UnitFormat::Mm).unwrap();
        let design_start = html.find("Design Name:").unwrap();
        let dimensions_start = html.find("Board Dimensions:").unwrap();
        let design_row = &html[design_start..dimensions_start];

        assert!(design_row.contains(r#"<span class="summary-value">board</span>"#));
        assert!(!design_row.contains(r#"<span class="summary-value">board_array</span>"#));
        let board_summary = html.find("<h2>Board Summary</h2>").unwrap();
        let array_summary = html.find("<h2>Board Array Summary</h2>").unwrap();
        let file_info = html.find(r#"<div class="file-info">"#).unwrap();
        assert!(board_summary < array_summary);
        assert!(array_summary < file_info);
        assert!(!html[board_summary..array_summary].contains("Array Step:"));
        assert!(!html.contains("Array Step:"));
        assert!(!html.contains("Array Boards:"));
        assert!(!html.contains("1 instance from 1 board step"));
        assert!(html.contains("Array Grid:"));
        assert!(html.contains("data-board-array-overview='true'"));
    }

    #[test]
    fn html_renders_stackup_layers_then_separate_drills_and_outline() {
        let ipc = ipc2581::Ipc2581::parse(layer_render_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let html = generate_html(&accessor, UnitFormat::Mm).unwrap();

        assert!(html.contains("<h2>Layers</h2>"));
        assert!(html.contains("<h3>Stackup Layers</h3>"));
        assert!(html.contains("<h3>Non-Stackup Layers</h3>"));
        assert!(html.contains("data-board-outline='true'"));
        assert!(!html.contains("<span>DIELECTRIC_1</span>"));
        assert!(!html.contains("<span>COATING_TOP</span>"));
        assert!(!html.contains("<span>Board Outline</span>"));

        let stackup_heading = html.find("<h3>Stackup Layers</h3>").unwrap();
        let non_stackup_heading = html.find("<h3>Non-Stackup Layers</h3>").unwrap();
        let top_copper = html.find("<span>F.Cu</span>").unwrap();
        let bottom_copper = html.find("<span>B.Cu</span>").unwrap();
        let edge_cuts = html.find("<span>Edge.Cuts</span>").unwrap();
        let drill = html.find("<span>F.Cu_B.Cu</span>").unwrap();

        assert!(stackup_heading < top_copper);
        assert!(top_copper < bottom_copper);
        assert!(bottom_copper < non_stackup_heading);
        assert!(non_stackup_heading < edge_cuts);
        assert!(edge_cuts < drill);
        assert!(html.matches("<svg ").count() >= 4);
    }

    #[test]
    fn html_renders_array_support_layers_in_board_array_summary_only() {
        let ipc = ipc2581::Ipc2581::parse(array_layer_render_fixture()).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let html = generate_html(&accessor, UnitFormat::Mm).unwrap();

        let array_summary = html.find("<h2>Board Array Summary</h2>").unwrap();
        let file_info = html.find(r#"<div class="file-info">"#).unwrap();
        assert!(array_summary < file_info);
        assert!(!html.contains("<h3>Board Array Layers</h3>"));
        assert!(!html.contains("board-array-layer-render"));

        let summary_section = &html[array_summary..file_info];
        assert!(summary_section.contains("data-board-array-overview='true'"));
        assert!(summary_section.contains("array-layer-copper"));
        assert!(summary_section.contains("vcut-guide"));
        assert!(summary_section.contains("array-layer-drill"));
        assert!(summary_section.contains("Array Drill Holes:"));
    }

    fn board_array_design_name_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board_array"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="board_array" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1" dx="0" dy="0"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn array_layer_render_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <LayerRef name="F.Cu"/>
    <LayerRef name="B.Cu"/>
    <LayerRef name="V-Score"/>
    <LayerRef name="Board_Array_Drill"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <Circle diameter="1"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="V-Score" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>
      <Layer name="Board_Array_Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="B.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="B.Cu">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="array" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="20" y="0"/>
            <PolyStepSegment x="20" y="15"/>
            <PolyStepSegment x="0" y="15"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5" nx="1" ny="1" dx="0" dy="0"/>
        <LayerFeature layerRef="F.Cu">
          <Set polarity="POSITIVE">
            <GlobalFiducial>
              <Location x="5" y="12"/>
              <Circle diameter="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="V-Score">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="5" startY="0" endX="5" endY="15">
                <LineDesc lineWidth="0.025" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Board_Array_Drill">
          <Set polarity="POSITIVE">
            <Hole name="array_tooling_hole_0" type="CIRCLE" diameter="2" platingStatus="NONPLATED" x="5" y="2.5"/>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn layer_render_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="F.Cu"/>
    <LayerRef name="DIELECTRIC_1"/>
    <LayerRef name="B.Cu"/>
    <LayerRef name="COATING_TOP"/>
    <LayerRef name="Edge.Cuts"/>
    <LayerRef name="F.Cu_B.Cu"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad">
        <Circle diameter="1"/>
      </EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="DIELECTRIC_1" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="COATING_TOP" layerFunction="COATINGCOND" side="TOP" polarity="POSITIVE"/>
      <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>
      <Layer name="F.Cu_B.Cu" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="1.6">
        <StackupGroup name="Primary_Group">
          <StackupLayer layerOrGroupRef="F.Cu" thickness="0.035" sequence="1"/>
          <StackupLayer layerOrGroupRef="DIELECTRIC_1" thickness="1.53" sequence="2"/>
          <StackupLayer layerOrGroupRef="COATING_TOP" thickness="0" sequence="3"/>
          <StackupLayer layerOrGroupRef="B.Cu" thickness="0.035" sequence="4"/>
        </StackupGroup>
      </Stackup>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="F.Cu" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Cu">
          <Set>
            <Pad padstackDefRef="padstack">
              <Location x="2" y="3"/>
            </Pad>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Cu_B.Cu">
          <Set>
            <Hole name="H1" diameter="0.8" platingStatus="VIA" x="5" y="2.5"/>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Edge.Cuts">
          <Set>
            <Features>
              <Line startX="0" startY="0" endX="10" endY="0">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}

const HTML_TEMPLATE: &str = include_str!("html_template.html.jinja");
const CSS_STYLES: &str = include_str!("style.css");
