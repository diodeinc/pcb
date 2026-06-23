use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    Units,
    ecad::{
        Fiducial, FiducialKind, FiducialShape, Hole, LayerFunction, Line, PlatingStatus, Polarity,
        SetFeature, Side, StepType,
    },
    primitives::{Circle, LineEnd, StandardPrimitive, Styled},
    transform::Location,
};
use pcb_ir::dialects::ipc::{LayoutStepKind, root_step};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};

use crate::accessors::IpcAccessor;
use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

const EPSILON: f64 = 1e-9;
const MIN_PANEL_DIMENSION_MM: f64 = 70.0;
const MAX_PANEL_DIMENSION_MM: f64 = 260.0;
const MAX_VCUT_LINES_PER_AXIS: usize = 25;
const MIN_VCUT_CLEARANCE_MM: f64 = 5.0;
const MIN_EDGE_RAIL_WIDTH_MM: f64 = 5.0;
const VCUT_LAYER_BASE_NAME: &str = "V-Score";
const VCUT_LINE_WIDTH_MM: f64 = 0.025;
const GENERATED_HOLE_NAME_PREFIX: &str = "array_tooling_hole";

#[derive(Debug, Clone, PartialEq)]
enum BoardArrayCreateValidationError {
    U32Range {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    MmRange {
        field: &'static str,
        value: f64,
        min: f64,
        max: f64,
    },
    ZeroOrMinMm {
        field: &'static str,
        value: f64,
        min: f64,
    },
    ArrayDimensionMin {
        axis: &'static str,
        value: f64,
        min: f64,
    },
    ArrayDimensionMax {
        axis: &'static str,
        value: f64,
        max: f64,
    },
    VcutLineCount {
        axis: &'static str,
        count: usize,
        max: usize,
    },
}

impl std::fmt::Display for BoardArrayCreateValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::U32Range {
                field,
                value,
                min,
                max,
            } => write!(f, "{field} must be between {min} and {max}; got {value}"),
            Self::MmRange {
                field,
                value,
                min,
                max,
            } => write!(
                f,
                "{field} must be between {} and {} mm; got {} mm",
                fmt_num(*min),
                fmt_num(*max),
                fmt_num(*value)
            ),
            Self::ZeroOrMinMm { field, value, min } => write!(
                f,
                "{field} must be 0 mm or at least {} mm; got {} mm",
                fmt_num(*min),
                fmt_num(*value)
            ),
            Self::ArrayDimensionMin { axis, value, min } => write!(
                f,
                "array {axis} must be at least {} mm; got {} mm",
                fmt_num(*min),
                fmt_num(*value)
            ),
            Self::ArrayDimensionMax { axis, value, max } => write!(
                f,
                "array {axis} must be at most {} mm; got {} mm",
                fmt_num(*max),
                fmt_num(*value)
            ),
            Self::VcutLineCount { axis, count, max } => {
                write!(
                    f,
                    "{axis}-axis V-cut line count must be at most {max}; got {count}"
                )
            }
        }
    }
}

impl std::error::Error for BoardArrayCreateValidationError {}

#[derive(Debug, Clone)]
pub struct BoardArrayCreateOptions {
    pub columns: u32,
    pub rows: u32,
    pub column_spacing_mm: f64,
    pub row_spacing_mm: f64,
    pub edge_rail_width_mm: f64,
}

#[derive(Debug, Clone)]
struct BoardArraySpec {
    array_name: String,
    board_name: String,
    content_step_refs: Vec<String>,
    content_layer_refs: Vec<String>,
    columns: u32,
    rows: u32,
    repeat_x_mm: f64,
    repeat_y_mm: f64,
    pitch_x_mm: f64,
    pitch_y_mm: f64,
    array_width_mm: f64,
    array_height_mm: f64,
    generated_geometry: BoardArrayGeneratedGeometry,
    units: Units,
}

#[derive(Debug, Clone, Default)]
pub struct BoardArrayGeneratedGeometry {
    pub layers: Vec<GeneratedLayer>,
    pub layer_features: Vec<GeneratedLayerFeature>,
}

impl BoardArrayGeneratedGeometry {
    pub fn add_layer(&mut self, layer: GeneratedLayer) {
        self.layers.push(layer);
    }

    pub fn add_layer_feature(
        &mut self,
        layer_name: impl Into<String>,
        polarity: Polarity,
        features: Vec<SetFeature>,
    ) {
        self.layer_features.push(GeneratedLayerFeature {
            layer_name: layer_name.into(),
            polarity,
            features,
        });
    }

    pub fn add_round_global_fiducial(
        &mut self,
        layer_name: impl Into<String>,
        x_mm: f64,
        y_mm: f64,
        diameter_mm: f64,
    ) {
        self.add_layer_feature(
            layer_name,
            Polarity::Positive,
            vec![SetFeature::Fiducial(round_global_fiducial(
                x_mm,
                y_mm,
                diameter_mm,
            ))],
        );
    }

    pub fn add_round_nonplated_hole(
        &mut self,
        layer_name: impl Into<String>,
        x_mm: f64,
        y_mm: f64,
        diameter_mm: f64,
    ) {
        self.add_layer_feature(
            layer_name,
            Polarity::Positive,
            vec![SetFeature::Hole(Hole {
                name: None,
                diameter: diameter_mm,
                plating_status: PlatingStatus::NonPlated,
                x: x_mm,
                y: y_mm,
            })],
        );
    }

    fn referenced_layer_names(&self) -> impl Iterator<Item = &str> {
        self.layers.iter().map(|layer| layer.name.as_str()).chain(
            self.layer_features
                .iter()
                .map(|layer_feature| layer_feature.layer_name.as_str()),
        )
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedLayer {
    pub name: String,
    pub layer_function: LayerFunction,
    pub side: Option<Side>,
    pub polarity: Option<Polarity>,
}

impl GeneratedLayer {
    pub fn new(
        name: impl Into<String>,
        layer_function: LayerFunction,
        side: Option<Side>,
        polarity: Option<Polarity>,
    ) -> Self {
        Self {
            name: name.into(),
            layer_function,
            side,
            polarity,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedLayerFeature {
    pub layer_name: String,
    pub polarity: Polarity,
    pub features: Vec<SetFeature>,
}

#[derive(Debug, Clone, Copy)]
struct VcutLine {
    start_x_mm: f64,
    start_y_mm: f64,
    end_x_mm: f64,
    end_y_mm: f64,
}

pub fn execute(input: &Path, output: &Path, options: &BoardArrayCreateOptions) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    let updated_xml = create_board_array_xml(&content, options)?;
    file_utils::save_ipc_file(output, &updated_xml)?;
    eprintln!("✓ Created IPC-2581 board array at {}", output.display());
    Ok(())
}

pub fn execute_svg(input: &Path, output: &Path) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    let ipc = Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);
    let svg = crate::panel::render_board_array_overview_svg(&accessor)
        .context("IPC-2581 file has no renderable board array overview")?;

    std::fs::write(output, svg)
        .with_context(|| format!("failed to write board array SVG to {}", output.display()))?;
    println!("✓ Board array SVG exported to {}", output.display());
    Ok(())
}

fn create_board_array_xml(xml: &str, options: &BoardArrayCreateOptions) -> Result<String> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let spec = build_board_array_spec(&ipc, options)?;
    write_board_array_xml(xml, &spec)
}

fn write_board_array_xml(xml: &str, spec: &BoardArraySpec) -> Result<String> {
    let generated_layer_xml = write_generated_layers_xml(&spec.generated_geometry)?;
    let array_step_xml = write_array_step_xml(spec)?;
    let xml = update_content_refs(xml, &spec.content_step_refs, &spec.content_layer_refs)?;
    let xml = insert_array_cad_data(&xml, generated_layer_xml.as_deref(), &array_step_xml)?;
    let xml = crate::utils::history::append_file_revision(&xml, "Created board array")?;
    let xml = crate::utils::format::reformat_xml(&xml)?;

    Ipc2581::parse(&xml).context("Generated IPC-2581 board array XML did not parse")?;
    Ok(xml)
}

fn build_board_array_spec(
    ipc: &Ipc2581,
    options: &BoardArrayCreateOptions,
) -> Result<BoardArraySpec> {
    validate_options(options)?;

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let primary_step = crate::steps::primary_step(ipc, &ecad.cad_data.steps)
        .context("IPC-2581 ECAD section has no Step")?;

    if is_panel_step(primary_step) {
        bail!("primary IPC-2581 step is already a panel; board array create expects a board step");
    }
    if !is_board_step(primary_step) {
        bail!("primary IPC-2581 step is not a board step");
    }

    let layout = geometry::extract_layout(ipc)?;
    let (_, root) = root_step(&layout).context("IPC-2581 board step has no layout root")?;
    if root.kind != LayoutStepKind::Board {
        bail!("primary IPC-2581 layout root is not a board step");
    }
    if root.bbox.is_empty() {
        bail!("primary IPC-2581 board step has no Profile outline");
    }

    let board_width = root.bbox.width();
    let board_height = root.bbox.height();
    if board_width <= EPSILON || board_height <= EPSILON {
        bail!("primary IPC-2581 board Profile outline has zero size");
    }

    let columns = options.columns;
    let rows = options.rows;
    let margin_x = options.column_spacing_mm + options.edge_rail_width_mm;
    let margin_y = options.row_spacing_mm + options.edge_rail_width_mm;
    let pitch_x = board_width + options.column_spacing_mm;
    let pitch_y = board_height + options.row_spacing_mm;
    let array_width = columns as f64 * board_width
        + (columns + 1) as f64 * options.column_spacing_mm
        + 2.0 * options.edge_rail_width_mm;
    let array_height = rows as f64 * board_height
        + (rows + 1) as f64 * options.row_spacing_mm
        + 2.0 * options.edge_rail_width_mm;
    validate_array_dimensions(array_width, array_height)?;

    let board_name = ipc.resolve(root.source_step_ref).to_string();
    let existing_step_names = ecad
        .cad_data
        .steps
        .iter()
        .map(|step| ipc.resolve(step.name).to_string())
        .collect::<HashSet<_>>();
    let array_name = unique_name(&existing_step_names, "array");
    let existing_layer_names = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<HashSet<_>>();
    let mut generated_geometry = BoardArrayGeneratedGeometry::default();
    add_vcut_lines(
        &mut generated_geometry,
        &existing_layer_names,
        vcut_lines(VcutLineSpec {
            columns,
            rows,
            board_width_mm: board_width,
            board_height_mm: board_height,
            margin_x_mm: margin_x,
            margin_y_mm: margin_y,
            pitch_x_mm: pitch_x,
            pitch_y_mm: pitch_y,
            array_width_mm: array_width,
            array_height_mm: array_height,
        })?,
    );

    Ok(BoardArraySpec {
        array_name: array_name.clone(),
        board_name: board_name.clone(),
        content_step_refs: content_step_refs(ipc, &array_name, &board_name),
        content_layer_refs: content_layer_refs(ipc, &generated_geometry),
        columns,
        rows,
        repeat_x_mm: margin_x - root.bbox.min.x,
        repeat_y_mm: margin_y - root.bbox.min.y,
        pitch_x_mm: if columns > 1 { pitch_x } else { 0.0 },
        pitch_y_mm: if rows > 1 { pitch_y } else { 0.0 },
        array_width_mm: array_width,
        array_height_mm: array_height,
        generated_geometry,
        units: ecad.cad_header.units,
    })
}

fn validate_options(options: &BoardArrayCreateOptions) -> Result<()> {
    validate_u32_range("columns", options.columns, 1, 10)?;
    validate_u32_range("rows", options.rows, 1, 10)?;
    validate_mm_range("column spacing", options.column_spacing_mm, 0.0, 20.0)?;
    validate_mm_range("row spacing", options.row_spacing_mm, 0.0, 20.0)?;
    validate_mm_range(
        "edge rail width",
        options.edge_rail_width_mm,
        MIN_EDGE_RAIL_WIDTH_MM,
        30.0,
    )?;
    validate_zero_or_min_mm(
        "column spacing",
        options.column_spacing_mm,
        MIN_VCUT_CLEARANCE_MM,
    )?;
    validate_zero_or_min_mm("row spacing", options.row_spacing_mm, MIN_VCUT_CLEARANCE_MM)?;
    Ok(())
}

fn validate_u32_range(field: &'static str, value: u32, min: u32, max: u32) -> Result<()> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::U32Range {
            field,
            value,
            min,
            max,
        }
        .into())
    }
}

fn validate_mm_range(field: &'static str, value: f64, min: f64, max: f64) -> Result<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::MmRange {
            field,
            value,
            min,
            max,
        }
        .into())
    }
}

fn validate_zero_or_min_mm(field: &'static str, value: f64, min: f64) -> Result<()> {
    if value.abs() <= EPSILON || value + EPSILON >= min {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::ZeroOrMinMm { field, value, min }.into())
    }
}

fn validate_array_dimensions(width_mm: f64, height_mm: f64) -> Result<()> {
    validate_array_dimension("width", width_mm)?;
    validate_array_dimension("height", height_mm)
}

fn validate_array_dimension(axis: &'static str, value: f64) -> Result<()> {
    if !value.is_finite() || value + EPSILON < MIN_PANEL_DIMENSION_MM {
        Err(BoardArrayCreateValidationError::ArrayDimensionMin {
            axis,
            value,
            min: MIN_PANEL_DIMENSION_MM,
        }
        .into())
    } else if value > MAX_PANEL_DIMENSION_MM + EPSILON {
        Err(BoardArrayCreateValidationError::ArrayDimensionMax {
            axis,
            value,
            max: MAX_PANEL_DIMENSION_MM,
        }
        .into())
    } else {
        Ok(())
    }
}

fn is_panel_step(step: &ipc2581::types::ecad::Step) -> bool {
    step.step_type == Some(StepType::Pallet)
        || (step.step_type.is_none() && !step.step_repeats.is_empty())
}

fn is_board_step(step: &ipc2581::types::ecad::Step) -> bool {
    step.step_type == Some(StepType::Board)
        || (step.step_type.is_none() && step.step_repeats.is_empty())
}

fn unique_name(existing_names: &HashSet<String>, base: &str) -> String {
    if !existing_names.contains(base) {
        return base.to_string();
    }

    (1..)
        .map(|index| format!("{base}_{index}"))
        .find(|name| !existing_names.contains(name))
        .expect("unbounded name search should find an unused name")
}

fn content_step_refs(ipc: &Ipc2581, array_name: &str, board_name: &str) -> Vec<String> {
    let mut refs = vec![array_name.to_string()];
    let mut seen = HashSet::from([array_name.to_string()]);
    for step_ref in &ipc.content().step_refs {
        let name = ipc.resolve(*step_ref).to_string();
        if seen.insert(name.clone()) {
            refs.push(name);
        }
    }
    if seen.insert(board_name.to_string()) {
        refs.push(board_name.to_string());
    }
    refs
}

fn content_layer_refs(
    ipc: &Ipc2581,
    generated_geometry: &BoardArrayGeneratedGeometry,
) -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for layer_ref in &ipc.content().layer_refs {
        let name = ipc.resolve(*layer_ref).to_string();
        if seen.insert(name.clone()) {
            refs.push(name);
        }
    }
    for layer_name in generated_geometry.referenced_layer_names() {
        if seen.insert(layer_name.to_string()) {
            refs.push(layer_name.to_string());
        }
    }
    refs
}

fn add_vcut_lines(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    existing_layer_names: &HashSet<String>,
    lines: Vec<VcutLine>,
) {
    if lines.is_empty() {
        return;
    }

    let layer_name = unique_name(existing_layer_names, VCUT_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        layer_name.clone(),
        LayerFunction::VCut,
        Some(Side::None),
        Some(Polarity::Positive),
    ));
    generated_geometry.add_layer_feature(
        layer_name,
        Polarity::Positive,
        lines.into_iter().map(vcut_line_feature).collect(),
    );
}

fn vcut_line_feature(line: VcutLine) -> SetFeature {
    SetFeature::Line(Line {
        start_x: line.start_x_mm,
        start_y: line.start_y_mm,
        end_x: line.end_x_mm,
        end_y: line.end_y_mm,
        line_desc_ref: None,
        line_width: VCUT_LINE_WIDTH_MM,
        line_end: Some(LineEnd::Round),
    })
}

struct VcutLineSpec {
    columns: u32,
    rows: u32,
    board_width_mm: f64,
    board_height_mm: f64,
    margin_x_mm: f64,
    margin_y_mm: f64,
    pitch_x_mm: f64,
    pitch_y_mm: f64,
    array_width_mm: f64,
    array_height_mm: f64,
}

fn vcut_lines(spec: VcutLineSpec) -> Result<Vec<VcutLine>> {
    let x_positions = board_edge_positions(
        spec.columns,
        spec.margin_x_mm,
        spec.pitch_x_mm,
        spec.board_width_mm,
        spec.array_width_mm,
    );
    validate_vcut_line_count("X", x_positions.len())?;

    let y_positions = board_edge_positions(
        spec.rows,
        spec.margin_y_mm,
        spec.pitch_y_mm,
        spec.board_height_mm,
        spec.array_height_mm,
    );
    validate_vcut_line_count("Y", y_positions.len())?;

    let mut lines = Vec::new();
    for x in x_positions {
        lines.push(VcutLine {
            start_x_mm: x,
            start_y_mm: 0.0,
            end_x_mm: x,
            end_y_mm: spec.array_height_mm,
        });
    }
    for y in y_positions {
        lines.push(VcutLine {
            start_x_mm: 0.0,
            start_y_mm: y,
            end_x_mm: spec.array_width_mm,
            end_y_mm: y,
        });
    }
    Ok(lines)
}

fn validate_vcut_line_count(axis: &'static str, count: usize) -> Result<()> {
    if count <= MAX_VCUT_LINES_PER_AXIS {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::VcutLineCount {
            axis,
            count,
            max: MAX_VCUT_LINES_PER_AXIS,
        }
        .into())
    }
}

fn board_edge_positions(
    count: u32,
    margin: f64,
    pitch: f64,
    size: f64,
    panel_size: f64,
) -> Vec<f64> {
    let mut positions = Vec::new();
    for index in 0..count {
        let start = margin + index as f64 * pitch;
        positions.push(start);
        positions.push(start + size);
    }
    positions.retain(|position| {
        position.is_finite() && *position > EPSILON && *position < panel_size - EPSILON
    });
    positions.sort_by(f64::total_cmp);
    positions.dedup_by(|left, right| (*left - *right).abs() <= EPSILON);
    positions
}

fn update_content_refs(xml: &str, step_refs: &[String], layer_refs: &[String]) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut in_content = false;
    let mut content_depth = 0usize;
    let mut skip_depth = 0usize;
    let mut function_mode_open = false;
    let mut refs_written = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::Empty(_) if skip_depth > 0 => {}
            Event::End(_) if skip_depth > 0 => skip_depth -= 1,
            Event::Start(ref e) if e.name().as_ref() == b"Content" => {
                in_content = true;
                content_depth = 1;
                function_mode_open = false;
                refs_written = false;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::End(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"Content" =>
            {
                if !refs_written {
                    write_content_refs(&mut writer, step_refs, layer_refs)?;
                }
                writer.write_event(Event::End(e.to_owned()))?;
                in_content = false;
                content_depth = 0;
            }
            Event::Start(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"StepRef" =>
            {
                skip_depth = 1;
            }
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"StepRef" => {}
            Event::Start(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"LayerRef" =>
            {
                skip_depth = 1;
            }
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"LayerRef" => {}
            Event::Empty(ref e)
                if in_content && content_depth == 1 && e.name().as_ref() == b"FunctionMode" =>
            {
                writer.write_event(Event::Empty(e.to_owned()))?;
                write_content_refs(&mut writer, step_refs, layer_refs)?;
                refs_written = true;
            }
            Event::Start(ref e) if in_content => {
                if content_depth == 1 && e.name().as_ref() == b"FunctionMode" {
                    function_mode_open = true;
                }
                writer.write_event(Event::Start(e.to_owned()))?;
                content_depth += 1;
            }
            Event::End(ref e) if in_content => {
                writer.write_event(Event::End(e.to_owned()))?;
                if function_mode_open && content_depth == 2 && e.name().as_ref() == b"FunctionMode"
                {
                    write_content_refs(&mut writer, step_refs, layer_refs)?;
                    refs_written = true;
                    function_mode_open = false;
                }
                content_depth -= 1;
            }
            Event::Empty(ref e) if in_content => {
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_content_refs(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    step_refs: &[String],
    layer_refs: &[String],
) -> Result<()> {
    write_step_refs(writer, step_refs)?;
    write_layer_refs(writer, layer_refs)?;
    Ok(())
}

fn write_step_refs(writer: &mut Writer<Cursor<Vec<u8>>>, step_refs: &[String]) -> Result<()> {
    for step_ref in step_refs {
        let mut elem = BytesStart::new("StepRef");
        elem.push_attribute(("name", step_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

fn write_layer_refs(writer: &mut Writer<Cursor<Vec<u8>>>, layer_refs: &[String]) -> Result<()> {
    for layer_ref in layer_refs {
        let mut elem = BytesStart::new("LayerRef");
        elem.push_attribute(("name", layer_ref.as_str()));
        writer.write_event(Event::Empty(elem))?;
    }
    Ok(())
}

fn insert_array_cad_data(
    xml: &str,
    generated_layer_xml: Option<&str>,
    array_step_xml: &str,
) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut in_cad_data = false;
    let mut cad_data_depth = 0usize;
    let mut panel_step_inserted = false;
    let mut generated_layers_inserted = generated_layer_xml.is_none();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(ref e) if e.name().as_ref() == b"CadData" => {
                in_cad_data = true;
                cad_data_depth = 1;
                writer.write_event(Event::Start(e.to_owned()))?;
            }
            Event::Start(ref e) if in_cad_data => {
                if cad_data_depth == 1
                    && !generated_layers_inserted
                    && e.name().as_ref() != b"Layer"
                {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                writer.write_event(Event::Start(e.to_owned()))?;
                cad_data_depth += 1;
            }
            Event::Empty(ref e) if in_cad_data => {
                if cad_data_depth == 1
                    && !generated_layers_inserted
                    && e.name().as_ref() != b"Layer"
                {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::End(ref e) if e.name().as_ref() == b"CadData" => {
                if !generated_layers_inserted {
                    write_raw_xml(&mut writer, generated_layer_xml)?;
                    generated_layers_inserted = true;
                }
                writer.get_mut().write_all(array_step_xml.as_bytes())?;
                writer.write_event(Event::End(e.to_owned()))?;
                panel_step_inserted = true;
                in_cad_data = false;
                cad_data_depth = 0;
            }
            Event::End(ref e) if in_cad_data => {
                writer.write_event(Event::End(e.to_owned()))?;
                cad_data_depth -= 1;
            }
            event => writer.write_event(event)?,
        }
        buf.clear();
    }

    if !panel_step_inserted {
        bail!("IPC-2581 file has no CadData section");
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_raw_xml(writer: &mut Writer<Cursor<Vec<u8>>>, xml: Option<&str>) -> Result<()> {
    if let Some(xml) = xml {
        writer.get_mut().write_all(xml.as_bytes())?;
    }
    Ok(())
}

fn write_generated_layers_xml(geometry: &BoardArrayGeneratedGeometry) -> Result<Option<String>> {
    if geometry.layers.is_empty() {
        return Ok(None);
    }

    let mut writer = Writer::new(Cursor::new(Vec::new()));
    for generated_layer in &geometry.layers {
        write_generated_layer_xml(&mut writer, generated_layer)?;
    }
    Ok(Some(String::from_utf8(writer.into_inner().into_inner())?))
}

fn write_generated_layer_xml(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    generated_layer: &GeneratedLayer,
) -> Result<()> {
    let mut layer = BytesStart::new("Layer");
    layer.push_attribute(("name", generated_layer.name.as_str()));
    layer.push_attribute(("layerFunction", generated_layer.layer_function.as_str()));
    if let Some(side) = generated_layer.side {
        layer.push_attribute(("side", side_attr(side)));
    }
    if let Some(polarity) = generated_layer.polarity {
        layer.push_attribute(("polarity", polarity_attr(polarity)));
    }
    writer.write_event(Event::Empty(layer))?;
    Ok(())
}

fn write_array_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.array_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;

    writer.write_event(Event::Start(BytesStart::new("Profile")))?;
    writer.write_event(Event::Start(BytesStart::new("Polygon")))?;
    write_location_empty(&mut writer, "PolyBegin", 0.0, 0.0, spec.units)?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        0.0,
        spec.array_height_mm,
        spec.units,
    )?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        spec.array_width_mm,
        spec.array_height_mm,
        spec.units,
    )?;
    write_location_empty(
        &mut writer,
        "PolyStepSegment",
        spec.array_width_mm,
        0.0,
        spec.units,
    )?;
    write_location_empty(&mut writer, "PolyStepSegment", 0.0, 0.0, spec.units)?;
    writer.write_event(Event::End(BytesStart::new("Polygon").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("Profile").to_end()))?;

    write_step_repeat(&mut writer, spec)?;
    write_generated_layer_features(&mut writer, spec)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_generated_layer_features(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let mut generated_hole_index = 0usize;
    for layer_feature in &spec.generated_geometry.layer_features {
        write_generated_layer_feature(
            writer,
            spec.units,
            layer_feature,
            &mut generated_hole_index,
        )?;
    }
    Ok(())
}

fn write_generated_layer_feature(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    layer_feature: &GeneratedLayerFeature,
    generated_hole_index: &mut usize,
) -> Result<()> {
    if layer_feature.features.is_empty() {
        return Ok(());
    }

    let mut elem = BytesStart::new("LayerFeature");
    elem.push_attribute(("layerRef", layer_feature.layer_name.as_str()));
    writer.write_event(Event::Start(elem))?;

    let mut set = BytesStart::new("Set");
    set.push_attribute(("polarity", polarity_attr(layer_feature.polarity)));
    writer.write_event(Event::Start(set))?;
    write_set_features(writer, units, &layer_feature.features, generated_hole_index)?;
    writer.write_event(Event::End(BytesStart::new("Set").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("LayerFeature").to_end()))?;
    Ok(())
}

fn write_set_features(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    features: &[SetFeature],
    generated_hole_index: &mut usize,
) -> Result<()> {
    let mut features_open = false;
    for feature in features {
        match feature {
            SetFeature::Line(line) => {
                if !features_open {
                    writer.write_event(Event::Start(BytesStart::new("Features")))?;
                    features_open = true;
                }
                write_line(writer, units, line)?;
            }
            SetFeature::Fiducial(fiducial) => {
                close_features_element(writer, &mut features_open)?;
                write_fiducial(writer, units, fiducial)?;
            }
            SetFeature::Hole(hole) => {
                close_features_element(writer, &mut features_open)?;
                write_hole(writer, units, hole, generated_hole_index)?;
            }
            _ => bail!("generated board array layer feature has unsupported feature kind"),
        }
    }
    close_features_element(writer, &mut features_open)?;
    Ok(())
}

fn close_features_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    features_open: &mut bool,
) -> Result<()> {
    if *features_open {
        writer.write_event(Event::End(BytesStart::new("Features").to_end()))?;
        *features_open = false;
    }
    Ok(())
}

fn write_line(writer: &mut Writer<Cursor<Vec<u8>>>, units: Units, line: &Line) -> Result<()> {
    if line.line_desc_ref.is_some() {
        bail!("generated board array lines must carry inline LineDesc values");
    }

    let start_x = fmt_units(line.start_x, units);
    let start_y = fmt_units(line.start_y, units);
    let end_x = fmt_units(line.end_x, units);
    let end_y = fmt_units(line.end_y, units);
    let mut elem = BytesStart::new("Line");
    elem.push_attribute(("startX", start_x.as_str()));
    elem.push_attribute(("startY", start_y.as_str()));
    elem.push_attribute(("endX", end_x.as_str()));
    elem.push_attribute(("endY", end_y.as_str()));
    writer.write_event(Event::Start(elem))?;

    let line_width = fmt_units(line.line_width, units);
    let mut line_desc = BytesStart::new("LineDesc");
    line_desc.push_attribute(("lineWidth", line_width.as_str()));
    if let Some(line_end) = line.line_end {
        line_desc.push_attribute(("lineEnd", line_end_attr(line_end)));
    }
    writer.write_event(Event::Empty(line_desc))?;
    writer.write_event(Event::End(BytesStart::new("Line").to_end()))?;
    Ok(())
}

fn write_fiducial(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    fiducial: &Fiducial,
) -> Result<()> {
    if fiducial.xform.is_some() || fiducial.pin_ref.is_some() {
        bail!("generated board array fiducials must use location-only round geometry");
    }

    let elem_name = fiducial_element_name(fiducial.kind);
    writer.write_event(Event::Start(BytesStart::new(elem_name)))?;
    write_location_empty(
        writer,
        "Location",
        fiducial.location.x,
        fiducial.location.y,
        units,
    )?;
    match &fiducial.shape {
        FiducialShape::Primitive(StandardPrimitive::Circle(circle)) => {
            write_circle(writer, units, circle.shape.diameter)?;
        }
        _ => bail!("generated board array fiducials must use inline Circle geometry"),
    }
    writer.write_event(Event::End(BytesStart::new(elem_name).to_end()))?;
    Ok(())
}

fn write_hole(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    hole: &Hole,
    generated_hole_index: &mut usize,
) -> Result<()> {
    let diameter = fmt_units(hole.diameter, units);
    let x = fmt_units(hole.x, units);
    let y = fmt_units(hole.y, units);
    let fallback_name = format!("{GENERATED_HOLE_NAME_PREFIX}_{}", *generated_hole_index);
    *generated_hole_index += 1;

    let mut elem = BytesStart::new("Hole");
    elem.push_attribute(("name", fallback_name.as_str()));
    elem.push_attribute(("type", "CIRCLE"));
    elem.push_attribute(("diameter", diameter.as_str()));
    elem.push_attribute(("platingStatus", plating_status_attr(hole.plating_status)));
    elem.push_attribute(("plusTol", "0"));
    elem.push_attribute(("minusTol", "0"));
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

fn write_circle(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    diameter_mm: f64,
) -> Result<()> {
    let diameter = fmt_units(diameter_mm, units);
    let mut elem = BytesStart::new("Circle");
    elem.push_attribute(("diameter", diameter.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

fn round_global_fiducial(x_mm: f64, y_mm: f64, diameter_mm: f64) -> Fiducial {
    Fiducial {
        kind: FiducialKind::Global,
        location: Location { x: x_mm, y: y_mm },
        xform: None,
        shape: FiducialShape::Primitive(StandardPrimitive::Circle(Styled {
            shape: Circle {
                diameter: diameter_mm,
            },
            fill_property: None,
            line_desc_ref: None,
        })),
        pin_ref: None,
    }
}

fn fiducial_element_name(kind: FiducialKind) -> &'static str {
    match kind {
        FiducialKind::BadBoardMark => "BadBoardMark",
        FiducialKind::Global => "GlobalFiducial",
        FiducialKind::GoodPanelMark => "GoodPanelMark",
        FiducialKind::Local => "LocalFiducial",
    }
}

fn side_attr(side: Side) -> &'static str {
    match side {
        Side::Top => "TOP",
        Side::Bottom => "BOTTOM",
        Side::Both => "BOTH",
        Side::Internal => "INTERNAL",
        Side::All => "ALL",
        Side::None => "NONE",
    }
}

fn polarity_attr(polarity: Polarity) -> &'static str {
    match polarity {
        Polarity::Positive => "POSITIVE",
        Polarity::Negative => "NEGATIVE",
    }
}

fn line_end_attr(line_end: LineEnd) -> &'static str {
    match line_end {
        LineEnd::Round => "ROUND",
        LineEnd::Square => "SQUARE",
        LineEnd::Flat => "FLAT",
    }
}

fn plating_status_attr(plating_status: PlatingStatus) -> &'static str {
    match plating_status {
        PlatingStatus::Plated => "PLATED",
        PlatingStatus::NonPlated => "NONPLATED",
        PlatingStatus::Via => "VIA",
    }
}

fn write_location_empty(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    name: &str,
    x_mm: f64,
    y_mm: f64,
    units: Units,
) -> Result<()> {
    let x = fmt_units(x_mm, units);
    let y = fmt_units(y_mm, units);
    let mut elem = BytesStart::new(name);
    elem.push_attribute(("x", x.as_str()));
    elem.push_attribute(("y", y.as_str()));
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

fn write_step_repeat(writer: &mut Writer<Cursor<Vec<u8>>>, spec: &BoardArraySpec) -> Result<()> {
    let x = fmt_units(spec.repeat_x_mm, spec.units);
    let y = fmt_units(spec.repeat_y_mm, spec.units);
    let dx = fmt_units(spec.pitch_x_mm, spec.units);
    let dy = fmt_units(spec.pitch_y_mm, spec.units);
    let nx = spec.columns.to_string();
    let ny = spec.rows.to_string();

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_name.as_str()));
    repeat.push_attribute(("x", x.as_str()));
    repeat.push_attribute(("y", y.as_str()));
    repeat.push_attribute(("nx", nx.as_str()));
    repeat.push_attribute(("ny", ny.as_str()));
    repeat.push_attribute(("dx", dx.as_str()));
    repeat.push_attribute(("dy", dy.as_str()));
    repeat.push_attribute(("angle", "0.00"));
    repeat.push_attribute(("mirror", "false"));
    writer.write_event(Event::Empty(repeat))?;
    Ok(())
}

fn fmt_units(value_mm: f64, units: Units) -> String {
    fmt_num(ipc2581::units::from_mm(value_mm, units))
}

fn fmt_num(value: f64) -> String {
    if value.abs() < EPSILON {
        return "0".to_string();
    }
    let mut s = format!("{value:.6}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessors::IpcAccessor;
    use crate::gerber::{GerberExportOptions, export_gerber_x2};
    use pcb_ir::common::Point;
    use pcb_ir::dialects::ipc::{FeatureBucket, FeatureKind, FeatureSemantic};

    use crate::LayoutTarget;

    #[test]
    fn creates_rectangular_panel_step_from_board_bbox() {
        let xml = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<StepRef name="array"/>"#));
        assert!(xml.contains(r#"<StepRef name="board"/>"#));
        assert!(xml.contains(r#"<LayerRef name="V-Score"/>"#));
        assert!(xml.contains(
            r#"<Layer name="V-Score" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>"#
        ));
        assert!(xml.contains(r#"<Step name="array" type="PALLET">"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="12" y="13" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#
        ));
        assert!(xml.contains(r#"<LayerFeature layerRef="V-Score">"#));
        assert!(xml.contains(r#"<Line startX="10" startY="0" endX="10" endY="105">"#));
        assert!(xml.contains(r#"<Line startX="0" startY="10" endX="105" endY="10">"#));

        let ipc = Ipc2581::parse(&xml).unwrap();
        let layout = geometry::extract_layout(&ipc).unwrap();
        let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
        assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
        assert_point_close(panel_step.bbox.max, Point::new(105.0, 105.0));
        assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 36);

        let first_instance = &layout.layout.instances[0];
        assert_point_close(first_instance.bbox.min, Point::new(10.0, 10.0));
        assert_point_close(first_instance.bbox.max, Point::new(20.0, 20.0));

        let vcut = geometry::extract_layer_for_layout_target(&ipc, "V-Score", LayoutTarget::Panel)
            .unwrap();
        assert_eq!(vcut.features.len(), 24);
        assert!(
            vcut.features
                .iter()
                .all(|feature| feature.semantic == FeatureSemantic::VCut)
        );
    }

    #[test]
    fn created_panel_vcuts_flow_to_svg_and_gerber() {
        let xml = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();
        let ipc = Ipc2581::parse(&xml).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = crate::panel::render_board_array_overview_svg(&accessor).unwrap();
        assert_eq!(svg.matches("class='vcut-guide'").count(), 24);
        assert!(!svg.contains("class='score-guide'"));

        let output_dir = std::env::temp_dir().join(format!(
            "pcb-ipc-created-panel-vcuts-gerber-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&output_dir);
        let set = export_gerber_x2(
            &ipc,
            &GerberExportOptions {
                output: output_dir,
                layout_target: LayoutTarget::Panel,
            },
        )
        .unwrap();

        let vcut = set
            .files
            .iter()
            .find(|file| file.filename == "V_Cut.gbr")
            .unwrap();
        assert!(vcut.contents.contains("%TF.FileFunction,Vcut,Top/Bot*%"));
        assert!(vcut.contents.contains("%TF.Part,Array*%"));
        assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));
        assert_eq!(vcut.contents.matches("D01*").count(), 24);
    }

    #[test]
    fn board_array_creation_preserves_board_target_geometry() {
        let input = board_fixture_with_top_line_mm();
        let before_ipc = Ipc2581::parse(input).unwrap();
        let before =
            geometry::extract_layer_for_layout_target(&before_ipc, "TOP", LayoutTarget::Board)
                .unwrap();

        let xml = create_board_array_xml(
            input,
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();
        let after_ipc = Ipc2581::parse(&xml).unwrap();
        let after =
            geometry::extract_layer_for_layout_target(&after_ipc, "TOP", LayoutTarget::Board)
                .unwrap();

        assert_eq!(before.features.len(), after.features.len());
        assert_eq!(before.paths.len(), after.paths.len());
        assert_eq!(before.contours.len(), after.contours.len());
        assert_eq!(before.path_cmds, after.path_cmds);

        for (before_feature, after_feature) in before.features.iter().zip(&after.features) {
            assert_eq!(before_feature.kind, after_feature.kind);
            assert_eq!(before_feature.bucket, after_feature.bucket);
            assert_eq!(before_feature.polarity, after_feature.polarity);
            assert_eq!(before_feature.semantic, after_feature.semantic);
            assert_eq!(before_feature.bbox, after_feature.bbox);
            assert_eq!(before_feature.path_count, after_feature.path_count);
        }
    }

    #[test]
    fn generated_array_geometry_writes_fiducials_and_nonplated_holes() {
        let input = board_fixture_with_mask_mm();
        let ipc = Ipc2581::parse(input).unwrap();
        let mut spec = build_board_array_spec(
            &ipc,
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        spec.generated_geometry
            .add_round_global_fiducial("TOP", 12.5, 12.5, 1.0);
        spec.generated_geometry
            .add_round_global_fiducial("F.Mask", 12.5, 12.5, 2.0);
        spec.generated_geometry.add_layer(GeneratedLayer::new(
            "Array_Drill",
            LayerFunction::Drill,
            Some(Side::All),
            Some(Polarity::Positive),
        ));
        spec.generated_geometry
            .add_round_nonplated_hole("Array_Drill", 20.0, 20.0, 2.0);
        spec.content_layer_refs = content_layer_refs(&ipc, &spec.generated_geometry);

        let xml = write_board_array_xml(input, &spec).unwrap();

        assert!(xml.contains(r#"<LayerRef name="F.Mask"/>"#));
        assert!(xml.contains(r#"<LayerRef name="Array_Drill"/>"#));
        assert!(xml.contains(
            r#"<Layer name="Array_Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>"#
        ));
        assert_eq!(xml.matches("<GlobalFiducial>").count(), 2);
        assert!(xml.contains(r#"<Circle diameter="1"/>"#));
        assert!(xml.contains(r#"<Circle diameter="2"/>"#));
        assert!(xml.contains(
            r#"<Hole name="array_tooling_hole_0" type="CIRCLE" diameter="2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="20" y="20"/>"#
        ));

        let parsed = Ipc2581::parse(&xml).unwrap();
        let top =
            geometry::extract_layer_for_layout_target(&parsed, "TOP", LayoutTarget::Panel).unwrap();
        assert!(top.features.iter().any(|feature| matches!(
            feature.semantic,
            FeatureSemantic::Fiducial(pcb_ir::dialects::ipc::FiducialKind::Global)
        )));

        let drill =
            geometry::extract_layer_for_layout_target(&parsed, "Array_Drill", LayoutTarget::Panel)
                .unwrap();
        assert_eq!(drill.features.len(), 1);
        assert_eq!(drill.features[0].kind, FeatureKind::Hole);
        assert_eq!(drill.features[0].bucket, FeatureBucket::Cutout);
    }

    #[test]
    fn exports_board_array_overview_svg_file() {
        let xml = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("pcb-ipc-panel-svg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();
        let input = temp_dir.join("panel.ipc2581.xml");
        let output = temp_dir.join("panel.svg");
        std::fs::write(&input, xml).unwrap();

        execute_svg(&input, &output).unwrap();

        let svg = std::fs::read_to_string(output).unwrap();
        assert!(svg.contains("data-board-array-overview='true'"));
        assert!(svg.contains("Board array overview: 6 columns by 6 rows"));
    }

    #[test]
    fn writes_generated_panel_values_in_cad_header_units() {
        let xml = create_board_array_xml(
            board_fixture_inch(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 25.4,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<PolyStepSegment x="0" y="3"/>"#));
        assert!(xml.contains(r#"<PolyStepSegment x="3" y="3"/>"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="1" y="1" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
        ));
    }

    #[test]
    fn rejects_primary_panel_step() {
        let error = create_board_array_xml(
            panel_fixture(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("primary IPC-2581 step is already a panel")
        );
    }

    #[test]
    fn validates_simple_api_ranges() {
        let error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 11,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("columns must be between 1 and 10")
        );
    }

    #[test]
    fn rejects_small_spacing_and_edge_rail_width() {
        let column_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                column_spacing_mm: 4.99,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            column_error
                .to_string()
                .contains("column spacing must be 0 mm or at least 5 mm")
        );

        let row_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 2,
                column_spacing_mm: 0.0,
                row_spacing_mm: 4.99,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            row_error
                .to_string()
                .contains("row spacing must be 0 mm or at least 5 mm")
        );

        let rail_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                column_spacing_mm: 0.0,
                row_spacing_mm: 0.0,
                edge_rail_width_mm: 0.0,
            },
        )
        .unwrap_err();
        assert!(
            rail_error
                .to_string()
                .contains("edge rail width must be between 5 and 30 mm; got 0 mm")
        );
    }

    #[test]
    fn rejects_more_than_25_vcut_lines_per_axis() {
        let x_error = vcut_lines(VcutLineSpec {
            columns: 13,
            rows: 1,
            board_width_mm: 10.0,
            board_height_mm: 10.0,
            margin_x_mm: 5.0,
            margin_y_mm: 5.0,
            pitch_x_mm: 15.0,
            pitch_y_mm: 15.0,
            array_width_mm: 210.0,
            array_height_mm: 25.0,
        })
        .unwrap_err();
        assert!(
            x_error
                .to_string()
                .contains("X-axis V-cut line count must be at most 25; got 26")
        );

        let y_error = vcut_lines(VcutLineSpec {
            columns: 1,
            rows: 13,
            board_width_mm: 10.0,
            board_height_mm: 10.0,
            margin_x_mm: 5.0,
            margin_y_mm: 5.0,
            pitch_x_mm: 15.0,
            pitch_y_mm: 15.0,
            array_width_mm: 25.0,
            array_height_mm: 210.0,
        })
        .unwrap_err();
        assert!(
            y_error
                .to_string()
                .contains("Y-axis V-cut line count must be at most 25; got 26")
        );
    }

    #[test]
    fn rejects_array_dimensions_outside_limits() {
        let narrow_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 3,
                rows: 2,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            narrow_error
                .to_string()
                .contains("array width must be at least 70 mm; got 60 mm")
        );

        let short_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 4,
                rows: 2,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            short_error
                .to_string()
                .contains("array height must be at least 70 mm; got 45 mm")
        );

        let wide_error = create_board_array_xml(
            large_board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 1,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            wide_error
                .to_string()
                .contains("array width must be at most 260 mm; got 405 mm")
        );

        let tall_error = create_board_array_xml(
            large_board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 6,
                column_spacing_mm: 5.0,
                row_spacing_mm: 5.0,
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            tall_error
                .to_string()
                .contains("array height must be at most 260 mm; got 405 mm")
        );
    }

    fn assert_point_close(actual: Point, expected: Point) {
        assert!(
            (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn board_fixture_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="-2" y="-3"/>
            <PolyStepSegment x="8" y="-3"/>
            <PolyStepSegment x="8" y="7"/>
            <PolyStepSegment x="-2" y="7"/>
            <PolyStepSegment x="-2" y="-3"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn board_fixture_with_mask_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="-2" y="-3"/>
            <PolyStepSegment x="8" y="-3"/>
            <PolyStepSegment x="8" y="7"/>
            <PolyStepSegment x="-2" y="7"/>
            <PolyStepSegment x="-2" y="-3"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn board_fixture_with_top_line_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="-2" y="-3"/>
            <PolyStepSegment x="8" y="-3"/>
            <PolyStepSegment x="8" y="7"/>
            <PolyStepSegment x="-2" y="7"/>
            <PolyStepSegment x="-2" y="-3"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="0" startY="0" endX="5" endY="0">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn board_fixture_inch() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="INCH"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="1" y="0"/>
            <PolyStepSegment x="1" y="1"/>
            <PolyStepSegment x="0" y="1"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn large_board_fixture_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="60" y="0"/>
            <PolyStepSegment x="60" y="60"/>
            <PolyStepSegment x="0" y="60"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn panel_fixture() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="panel" type="PALLET">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }
}
