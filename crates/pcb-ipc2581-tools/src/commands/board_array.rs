use std::collections::HashSet;
use std::io::{Cursor, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    Units,
    ecad::{
        Fiducial, FiducialKind as IpcFiducialKind, FiducialShape, Hole, LayerFunction, Line,
        PlatingStatus, Polarity, SetFeature, Side, StepType,
    },
    primitives::{
        Circle, LineEnd, Point as IpcPoint, PolyStep, PolyStepSegment, Polygon, StandardPrimitive,
        Styled,
    },
    transform::Location,
};
use pcb_ir::{
    common::Point,
    dialects::ipc::{LayoutStepKind, root_step},
};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};

use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

const EPSILON: f64 = 1e-9;
const MIN_BOARD_ARRAY_DIMENSION_MM: f64 = 70.0;
const MAX_BOARD_ARRAY_DIMENSION_MM: f64 = 260.0;
const MAX_VCUT_LINES_PER_AXIS: usize = 25;
const MIN_VCUT_CLEARANCE_MM: f64 = 5.0;
const MIN_EDGE_RAIL_WIDTH_MM: f64 = 5.0;
const VCUT_LAYER_BASE_NAME: &str = "V-Score";
const VCUT_LINE_WIDTH_MM: f64 = 0.025;
const TOP_COPPER_LAYER_BASE_NAME: &str = "F.Cu";
const TOP_SOLDERMASK_LAYER_BASE_NAME: &str = "F.Mask";
const TOOLING_HOLE_LAYER_BASE_NAME: &str = "Board_Array_Drill";
const GENERATED_HOLE_NAME_PREFIX: &str = "array_tooling_hole";
const FIDUCIAL_COPPER_DIAMETER_MM: f64 = 1.0;
const FIDUCIAL_MASK_OPENING_DIAMETER_MM: f64 = 2.0;
const TOOLING_HOLE_DIAMETER_MM: f64 = 2.0;
const TOOLING_HOLE_EDGE_OFFSET_MM: f64 = 2.5;
const FIDUCIAL_EDGE_OFFSET_MM: f64 = 3.85;
const FIDUCIAL_FROM_TOOLING_HOLE_MM: f64 = 5.0;
const TOP_TOOLING_HOLE_X_INSET_MM: f64 = 5.0;
const BOTTOM_TOOLING_HOLE_X_INSET_MM: f64 = 10.0;
const SINGLE_COLUMN_TOOLING_MIN_BOARD_WIDTH_MM: f64 = 35.0;
const MULTI_COLUMN_TOOLING_MIN_BOARD_WIDTH_MM: f64 = 20.0;
const MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM: f64 = 5.0;
const MIN_BOARD_CELL_FIDUCIAL_SPAN_MM: f64 = 30.0;
const BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM: f64 = 2.0;
const PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = TOP_TOOLING_HOLE_X_INSET_MM;
const SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = BOTTOM_TOOLING_HOLE_X_INSET_MM;

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
    pub board_margin_mm: BoardMarginMm,
    pub edge_rail_width_mm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardMarginMm {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl BoardMarginMm {
    pub fn all(value: f64) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn from_css_shorthand(values: &[f64]) -> Result<Self> {
        match values {
            [all] => Ok(Self::all(*all)),
            [vertical, horizontal] => Ok(Self {
                top: *vertical,
                right: *horizontal,
                bottom: *vertical,
                left: *horizontal,
            }),
            [top, horizontal, bottom] => Ok(Self {
                top: *top,
                right: *horizontal,
                bottom: *bottom,
                left: *horizontal,
            }),
            [top, right, bottom, left] => Ok(Self {
                top: *top,
                right: *right,
                bottom: *bottom,
                left: *left,
            }),
            _ => bail!("board margin expects 1 to 4 values"),
        }
    }

    fn horizontal_gap(self) -> f64 {
        self.left + self.right
    }

    fn vertical_gap(self) -> f64 {
        self.top + self.bottom
    }

    fn sides(self) -> [(&'static str, f64); 4] {
        [
            ("board margin top", self.top),
            ("board margin right", self.right),
            ("board margin bottom", self.bottom),
            ("board margin left", self.left),
        ]
    }
}

#[derive(Debug, Clone)]
struct BoardArraySpec {
    array_name: String,
    board_cell_name: String,
    board_name: String,
    board_outline_layer_names: Vec<String>,
    content_step_refs: Vec<String>,
    content_layer_refs: Vec<String>,
    columns: u32,
    rows: u32,
    array_repeat_x_mm: f64,
    array_repeat_y_mm: f64,
    board_repeat_x_mm: f64,
    board_repeat_y_mm: f64,
    pitch_x_mm: f64,
    pitch_y_mm: f64,
    array_width_mm: f64,
    array_height_mm: f64,
    generated_geometry: BoardArrayGeneratedGeometry,
    units: Units,
}

#[derive(Debug, Clone, Default)]
struct BoardArrayGeneratedGeometry {
    layers: Vec<GeneratedLayer>,
    layer_features: Vec<GeneratedLayerFeature>,
}

impl BoardArrayGeneratedGeometry {
    fn add_layer(&mut self, layer: GeneratedLayer) {
        self.layers.push(layer);
    }

    fn add_layer_feature(
        &mut self,
        scope: GeneratedFeatureScope,
        layer_name: impl Into<String>,
        polarity: Polarity,
        features: Vec<SetFeature>,
    ) {
        self.layer_features.push(GeneratedLayerFeature {
            scope,
            layer_name: layer_name.into(),
            polarity,
            features,
        });
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
struct GeneratedLayer {
    name: String,
    layer_function: LayerFunction,
    side: Option<Side>,
    polarity: Option<Polarity>,
}

impl GeneratedLayer {
    fn new(
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
struct GeneratedLayerFeature {
    scope: GeneratedFeatureScope,
    layer_name: String,
    polarity: Polarity,
    features: Vec<SetFeature>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneratedFeatureScope {
    Array,
    BoardCell,
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

fn create_board_array_xml(xml: &str, options: &BoardArrayCreateOptions) -> Result<String> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let spec = build_board_array_spec(&ipc, options)?;
    write_board_array_xml(xml, &spec)
}

fn write_board_array_xml(xml: &str, spec: &BoardArraySpec) -> Result<String> {
    let generated_layer_xml = write_generated_layers_xml(&spec.generated_geometry)?;
    let generated_steps_xml = write_generated_steps_xml(spec)?;
    let xml = update_content_refs(xml, &spec.content_step_refs, &spec.content_layer_refs)?;
    let xml = insert_array_cad_data(
        &xml,
        spec,
        generated_layer_xml.as_deref(),
        &generated_steps_xml,
    )?;
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
        bail!(
            "primary IPC-2581 step is already a board array; board array create expects a board step"
        );
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
    let board_margin = options.board_margin_mm;
    let margin_x = options.edge_rail_width_mm + board_margin.left;
    let margin_y = options.edge_rail_width_mm + board_margin.bottom;
    let pitch_x = board_width + board_margin.horizontal_gap();
    let pitch_y = board_height + board_margin.vertical_gap();
    let array_width = columns as f64 * board_width
        + columns as f64 * board_margin.horizontal_gap()
        + 2.0 * options.edge_rail_width_mm;
    let array_height = rows as f64 * board_height
        + rows as f64 * board_margin.vertical_gap()
        + 2.0 * options.edge_rail_width_mm;
    validate_array_dimensions(array_width, array_height)?;
    let board_repeat_x = board_margin.left - root.bbox.min.x;
    let board_repeat_y = board_margin.bottom - root.bbox.min.y;

    let board_name = ipc.resolve(root.source_step_ref).to_string();
    let existing_step_names = ecad
        .cad_data
        .steps
        .iter()
        .map(|step| ipc.resolve(step.name).to_string())
        .collect::<HashSet<_>>();
    let array_name = unique_name(&existing_step_names, "array");
    let mut used_step_names = existing_step_names;
    used_step_names.insert(array_name.clone());
    let board_cell_name = unique_name(&used_step_names, "board_cell");
    let mut used_layer_names = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<HashSet<_>>();
    let mut generated_geometry = BoardArrayGeneratedGeometry::default();
    add_vcut_lines(
        &mut generated_geometry,
        &mut used_layer_names,
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
    add_board_array_tooling(
        &mut generated_geometry,
        ipc,
        ecad,
        &mut used_layer_names,
        BoardArrayToolingSpec {
            columns,
            board_width_mm: board_width,
            margin_x_mm: margin_x,
            pitch_x_mm: pitch_x,
            array_height_mm: array_height,
        },
    );
    if columns > 1 || rows > 1 {
        add_board_cell_fiducials(
            &mut generated_geometry,
            ipc,
            ecad,
            &mut used_layer_names,
            BoardCellFiducialSpec {
                board_width_mm: board_width,
                board_height_mm: board_height,
                board_margin,
            },
        );
    }
    let board_outline_layer_names = board_outline_layer_names(ipc, ecad);
    let content_step_refs = content_step_refs(ipc, &array_name, &board_cell_name, &board_name);
    let content_layer_refs =
        content_layer_refs(ipc, &generated_geometry, &board_outline_layer_names);

    Ok(BoardArraySpec {
        array_name,
        board_cell_name,
        board_name,
        board_outline_layer_names,
        content_step_refs,
        content_layer_refs,
        columns,
        rows,
        array_repeat_x_mm: options.edge_rail_width_mm,
        array_repeat_y_mm: options.edge_rail_width_mm,
        board_repeat_x_mm: board_repeat_x,
        board_repeat_y_mm: board_repeat_y,
        pitch_x_mm: pitch_x,
        pitch_y_mm: pitch_y,
        array_width_mm: array_width,
        array_height_mm: array_height,
        generated_geometry,
        units: ecad.cad_header.units,
    })
}

fn validate_options(options: &BoardArrayCreateOptions) -> Result<()> {
    validate_u32_range("columns", options.columns, 1, 10)?;
    validate_u32_range("rows", options.rows, 1, 10)?;
    for (field, value) in options.board_margin_mm.sides() {
        validate_mm_range(field, value, 0.0, 20.0)?;
    }
    validate_mm_range(
        "edge rail width",
        options.edge_rail_width_mm,
        MIN_EDGE_RAIL_WIDTH_MM,
        30.0,
    )?;
    if options.columns > 1 {
        validate_zero_or_min_mm(
            "horizontal board clearance",
            options.board_margin_mm.horizontal_gap(),
            MIN_VCUT_CLEARANCE_MM,
        )?;
    }
    if options.rows > 1 {
        validate_zero_or_min_mm(
            "vertical board clearance",
            options.board_margin_mm.vertical_gap(),
            MIN_VCUT_CLEARANCE_MM,
        )?;
    }
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
    if !value.is_finite() || value + EPSILON < MIN_BOARD_ARRAY_DIMENSION_MM {
        Err(BoardArrayCreateValidationError::ArrayDimensionMin {
            axis,
            value,
            min: MIN_BOARD_ARRAY_DIMENSION_MM,
        }
        .into())
    } else if value > MAX_BOARD_ARRAY_DIMENSION_MM + EPSILON {
        Err(BoardArrayCreateValidationError::ArrayDimensionMax {
            axis,
            value,
            max: MAX_BOARD_ARRAY_DIMENSION_MM,
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

fn content_step_refs(
    ipc: &Ipc2581,
    array_name: &str,
    board_cell_name: &str,
    board_name: &str,
) -> Vec<String> {
    let mut refs = vec![array_name.to_string()];
    let mut seen = HashSet::from([array_name.to_string()]);
    refs.push(board_cell_name.to_string());
    seen.insert(board_cell_name.to_string());
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
    removed_layer_names: &[String],
) -> Vec<String> {
    let mut refs = Vec::new();
    let mut seen = HashSet::new();
    for layer_ref in &ipc.content().layer_refs {
        let name = ipc.resolve(*layer_ref).to_string();
        if removed_layer_names.iter().any(|removed| removed == &name) {
            continue;
        }
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

fn board_outline_layer_names(ipc: &Ipc2581, ecad: &ipc2581::types::Ecad) -> Vec<String> {
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::BoardOutline)
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect()
}

fn add_vcut_lines(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    lines: Vec<VcutLine>,
) {
    if lines.is_empty() {
        return;
    }

    let layer_name = reserve_unique_name(used_layer_names, VCUT_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        layer_name.clone(),
        LayerFunction::VCut,
        Some(Side::None),
        Some(Polarity::Positive),
    ));
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
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

fn polygon_from_points(points: Vec<Point>) -> Option<Polygon> {
    let mut points = points
        .into_iter()
        .filter(|point| point.is_finite())
        .collect::<Vec<_>>();
    if points.len() > 1 && points[0].distance_to(*points.last().unwrap()) <= EPSILON {
        points.pop();
    }
    if points.len() < 3 {
        return None;
    }
    let begin = IpcPoint {
        x: points[0].x,
        y: points[0].y,
    };
    let mut steps = points[1..]
        .iter()
        .map(|point| {
            PolyStep::Segment(PolyStepSegment {
                point: IpcPoint {
                    x: point.x,
                    y: point.y,
                },
            })
        })
        .collect::<Vec<_>>();
    steps.push(PolyStep::Segment(PolyStepSegment { point: begin }));
    Some(Polygon { begin, steps })
}

struct BoardArrayToolingSpec {
    columns: u32,
    board_width_mm: f64,
    margin_x_mm: f64,
    pitch_x_mm: f64,
    array_height_mm: f64,
}

fn add_board_array_tooling(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    used_layer_names: &mut HashSet<String>,
    spec: BoardArrayToolingSpec,
) {
    let min_width = if spec.columns == 1 {
        SINGLE_COLUMN_TOOLING_MIN_BOARD_WIDTH_MM
    } else {
        MULTI_COLUMN_TOOLING_MIN_BOARD_WIDTH_MM
    };
    if spec.board_width_mm + EPSILON < min_width {
        return;
    }

    let top_copper_layer_name =
        ensure_top_copper_layer_name(generated_geometry, ipc, ecad, used_layer_names);
    let top_soldermask_layer_name =
        ensure_top_soldermask_layer_name(generated_geometry, ipc, ecad, used_layer_names);
    let tooling_hole_layer_name =
        reserve_unique_name(used_layer_names, TOOLING_HOLE_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        tooling_hole_layer_name.clone(),
        LayerFunction::Drill,
        Some(Side::All),
        Some(Polarity::Positive),
    ));

    let fiducials = board_array_tooling_fiducials(&spec);
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        top_copper_layer_name,
        Polarity::Positive,
        round_fiducial_features(
            IpcFiducialKind::Global,
            fiducials,
            FIDUCIAL_COPPER_DIAMETER_MM,
        ),
    );
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        top_soldermask_layer_name,
        Polarity::Positive,
        round_fiducial_features(
            IpcFiducialKind::Global,
            fiducials,
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
        ),
    );
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        tooling_hole_layer_name,
        Polarity::Positive,
        round_nonplated_hole_features(board_array_tooling_holes(&spec), TOOLING_HOLE_DIAMETER_MM),
    );
}

struct BoardCellFiducialSpec {
    board_width_mm: f64,
    board_height_mm: f64,
    board_margin: BoardMarginMm,
}

fn add_board_cell_fiducials(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    used_layer_names: &mut HashSet<String>,
    spec: BoardCellFiducialSpec,
) {
    let Some(fiducials) = board_cell_fiducials(&spec) else {
        return;
    };

    let top_copper_layer_name =
        ensure_top_copper_layer_name(generated_geometry, ipc, ecad, used_layer_names);
    let top_soldermask_layer_name =
        ensure_top_soldermask_layer_name(generated_geometry, ipc, ecad, used_layer_names);

    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::BoardCell,
        top_copper_layer_name,
        Polarity::Positive,
        round_fiducial_features(
            IpcFiducialKind::Local,
            fiducials,
            FIDUCIAL_COPPER_DIAMETER_MM,
        ),
    );
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::BoardCell,
        top_soldermask_layer_name,
        Polarity::Positive,
        round_fiducial_features(
            IpcFiducialKind::Local,
            fiducials,
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
        ),
    );
}

/// Place board array tooling on the top and bottom edge rails over board columns.
///
/// The generated board array uses a rectangular profile with the lower-left
/// array corner at (0, 0). Fiducials and tooling holes live in the outer 5 mm
/// rail band even when the configured edge rail is wider. They are not placed
/// on left/right rails or over horizontal board gaps, so removing side rails
/// and gaps keeps the top/bottom rail tooling attached to board material.
///
/// Horizontal rules:
/// - one column requires at least 35 mm board width, because both left and
///   right pairs share the same board span;
/// - multiple columns require at least 20 mm board width, because each side's
///   pair sits over a different outer board column;
/// - top tooling holes are 5 mm inward from the outer board-column edge;
/// - bottom tooling holes are 10 mm inward from the outer board-column edge;
/// - each fiducial is another 5 mm inward from its paired tooling hole.
///
/// Vertical rules:
/// - tooling hole centers are 2.5 mm from the top/bottom array edge;
/// - fiducial centers are 3.85 mm from the top/bottom array edge.
fn board_array_tooling_fiducials(spec: &BoardArrayToolingSpec) -> [(f64, f64); 4] {
    let left_edge = spec.margin_x_mm;
    let right_edge =
        spec.margin_x_mm + (spec.columns - 1) as f64 * spec.pitch_x_mm + spec.board_width_mm;
    let top_y = spec.array_height_mm - FIDUCIAL_EDGE_OFFSET_MM;
    let bottom_y = FIDUCIAL_EDGE_OFFSET_MM;

    [
        (
            left_edge + TOP_TOOLING_HOLE_X_INSET_MM + FIDUCIAL_FROM_TOOLING_HOLE_MM,
            top_y,
        ),
        (
            right_edge - TOP_TOOLING_HOLE_X_INSET_MM - FIDUCIAL_FROM_TOOLING_HOLE_MM,
            top_y,
        ),
        (
            left_edge + BOTTOM_TOOLING_HOLE_X_INSET_MM + FIDUCIAL_FROM_TOOLING_HOLE_MM,
            bottom_y,
        ),
        (
            right_edge - BOTTOM_TOOLING_HOLE_X_INSET_MM - FIDUCIAL_FROM_TOOLING_HOLE_MM,
            bottom_y,
        ),
    ]
}

fn board_array_tooling_holes(spec: &BoardArrayToolingSpec) -> [(f64, f64); 4] {
    let left_edge = spec.margin_x_mm;
    let right_edge =
        spec.margin_x_mm + (spec.columns - 1) as f64 * spec.pitch_x_mm + spec.board_width_mm;
    let top_y = spec.array_height_mm - TOOLING_HOLE_EDGE_OFFSET_MM;
    let bottom_y = TOOLING_HOLE_EDGE_OFFSET_MM;

    [
        (left_edge + TOP_TOOLING_HOLE_X_INSET_MM, top_y),
        (right_edge - TOP_TOOLING_HOLE_X_INSET_MM, top_y),
        (left_edge + BOTTOM_TOOLING_HOLE_X_INSET_MM, bottom_y),
        (right_edge - BOTTOM_TOOLING_HOLE_X_INSET_MM, bottom_y),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardCellFiducialOrientation {
    TopBottom,
    LeftRight,
}

/// Place four board fiducials in each board cell's margin.
///
/// Eligibility is checked per orientation: top/bottom needs enough horizontal
/// board span and top/bottom margins; left/right needs enough vertical board
/// span and left/right margins. Prefer the board's longer dimension, then fall
/// back to the other eligible orientation. Offsets along the board span are
/// measured from the board bbox; offsets into the margin are measured from the
/// board-cell outer edge. The primary side is top/left and uses a 5 mm span
/// inset; the opposite side uses 10 mm, matching the array-level tooling pattern.
fn board_cell_fiducials(spec: &BoardCellFiducialSpec) -> Option<[(f64, f64); 4]> {
    let orientation = board_cell_fiducial_orientation(spec)?;
    let board_left = spec.board_margin.left;
    let board_right = spec.board_margin.left + spec.board_width_mm;
    let board_bottom = spec.board_margin.bottom;
    let board_top = spec.board_margin.bottom + spec.board_height_mm;
    let cell_right = board_right + spec.board_margin.right;
    let cell_top = board_top + spec.board_margin.top;

    match orientation {
        BoardCellFiducialOrientation::TopBottom => Some([
            (
                board_left + PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
                cell_top - BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
            ),
            (
                board_right - PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
                cell_top - BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
            ),
            (
                board_left + SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
                BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
            ),
            (
                board_right - SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
                BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
            ),
        ]),
        BoardCellFiducialOrientation::LeftRight => Some([
            (
                BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
                board_top - PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
            ),
            (
                BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
                board_bottom + PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
            ),
            (
                cell_right - BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
                board_top - SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
            ),
            (
                cell_right - BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM,
                board_bottom + SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
            ),
        ]),
    }
}

fn board_cell_fiducial_orientation(
    spec: &BoardCellFiducialSpec,
) -> Option<BoardCellFiducialOrientation> {
    let top_bottom = spec.board_width_mm + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_SPAN_MM
        && spec.board_margin.top + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM
        && spec.board_margin.bottom + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM;
    let left_right = spec.board_height_mm + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_SPAN_MM
        && spec.board_margin.left + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM
        && spec.board_margin.right + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM;

    if spec.board_width_mm >= spec.board_height_mm {
        if top_bottom {
            Some(BoardCellFiducialOrientation::TopBottom)
        } else {
            left_right.then_some(BoardCellFiducialOrientation::LeftRight)
        }
    } else if left_right {
        Some(BoardCellFiducialOrientation::LeftRight)
    } else {
        top_bottom.then_some(BoardCellFiducialOrientation::TopBottom)
    }
}

fn ensure_top_copper_layer_name(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    used_layer_names: &mut HashSet<String>,
) -> String {
    top_copper_layer_name(ipc, ecad).unwrap_or_else(|| {
        let layer_name = reserve_unique_name(used_layer_names, TOP_COPPER_LAYER_BASE_NAME);
        generated_geometry.add_layer(GeneratedLayer::new(
            layer_name.clone(),
            LayerFunction::Signal,
            Some(Side::Top),
            Some(Polarity::Positive),
        ));
        layer_name
    })
}

fn ensure_top_soldermask_layer_name(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    used_layer_names: &mut HashSet<String>,
) -> String {
    top_soldermask_layer_name(ipc, ecad).unwrap_or_else(|| {
        let layer_name = reserve_unique_name(used_layer_names, TOP_SOLDERMASK_LAYER_BASE_NAME);
        generated_geometry.add_layer(GeneratedLayer::new(
            layer_name.clone(),
            LayerFunction::Soldermask,
            Some(Side::Top),
            Some(Polarity::Positive),
        ));
        layer_name
    })
}

fn top_copper_layer_name(ipc: &Ipc2581, ecad: &ipc2581::types::Ecad) -> Option<String> {
    ecad.cad_data
        .layers
        .iter()
        .find(|layer| layer.side == Some(Side::Top) && is_copper_layer(layer.layer_function))
        .map(|layer| ipc.resolve(layer.name).to_string())
}

fn top_soldermask_layer_name(ipc: &Ipc2581, ecad: &ipc2581::types::Ecad) -> Option<String> {
    ecad.cad_data
        .layers
        .iter()
        .find(|layer| {
            layer.side == Some(Side::Top) && layer.layer_function == LayerFunction::Soldermask
        })
        .map(|layer| ipc.resolve(layer.name).to_string())
}

fn is_copper_layer(layer_function: LayerFunction) -> bool {
    matches!(
        layer_function,
        LayerFunction::Conductor
            | LayerFunction::CondFilm
            | LayerFunction::CondFoil
            | LayerFunction::Plane
            | LayerFunction::Signal
            | LayerFunction::Mixed
    )
}

fn reserve_unique_name(used_names: &mut HashSet<String>, base: &str) -> String {
    let name = unique_name(used_names, base);
    used_names.insert(name.clone());
    name
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
    spec: &BoardArraySpec,
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
    let mut source_board_step_depth = None;
    let mut skip_depth = 0usize;

    loop {
        let event = reader.read_event_into(&mut buf)?;
        if skip_depth > 0 {
            match event {
                Event::Start(_) => skip_depth += 1,
                Event::End(_) => skip_depth -= 1,
                Event::Eof => {
                    bail!("unexpected end of IPC-2581 while removing board outline layer feature")
                }
                _ => {}
            }
            buf.clear();
            continue;
        }

        match event {
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
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Layer"
                    && cad_data_layer_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    skip_depth = 1;
                    buf.clear();
                    continue;
                }
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Step"
                    && start_attr_eq(e, b"name", &spec.board_name)?
                {
                    source_board_step_depth = Some(cad_data_depth + 1);
                }
                if source_board_step_depth.is_some()
                    && e.name().as_ref() == b"LayerFeature"
                    && layer_feature_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    skip_depth = 1;
                    buf.clear();
                    continue;
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
                if cad_data_depth == 1
                    && e.name().as_ref() == b"Layer"
                    && cad_data_layer_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    buf.clear();
                    continue;
                }
                if source_board_step_depth.is_some()
                    && e.name().as_ref() == b"LayerFeature"
                    && layer_feature_is_board_outline(e, &spec.board_outline_layer_names)?
                {
                    buf.clear();
                    continue;
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
                if source_board_step_depth == Some(cad_data_depth) && e.name().as_ref() == b"Step" {
                    source_board_step_depth = None;
                }
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

fn cad_data_layer_is_board_outline(
    e: &BytesStart,
    board_outline_layer_names: &[String],
) -> Result<bool> {
    let Some(name) = start_attr_value(e, b"name")? else {
        return Ok(false);
    };
    Ok(board_outline_layer_names.iter().any(|layer| layer == &name))
}

fn layer_feature_is_board_outline(
    e: &BytesStart,
    board_outline_layer_names: &[String],
) -> Result<bool> {
    let Some(layer_ref) = start_attr_value(e, b"layerRef")? else {
        return Ok(false);
    };
    Ok(board_outline_layer_names
        .iter()
        .any(|name| name == &layer_ref))
}

fn start_attr_eq(e: &BytesStart, key: &[u8], value: &str) -> Result<bool> {
    Ok(start_attr_value(e, key)?.as_deref() == Some(value))
}

fn start_attr_value(e: &BytesStart, key: &[u8]) -> Result<Option<String>> {
    for attr in e.attributes() {
        let attr = attr?;
        if attr.key.as_ref() == key {
            return Ok(Some(String::from_utf8(attr.value.into_owned())?));
        }
    }
    Ok(None)
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

fn write_generated_steps_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut xml = write_board_cell_step_xml(spec)?;
    xml.push_str(&write_array_step_xml(spec)?);
    Ok(xml)
}

fn write_board_cell_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.board_cell_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;
    write_board_cell_step_repeat(&mut writer, spec)?;
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::BoardCell)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_array_step_xml(spec: &BoardArraySpec) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));

    let mut step = BytesStart::new("Step");
    step.push_attribute(("name", spec.array_name.as_str()));
    step.push_attribute(("type", "PALLET"));
    writer.write_event(Event::Start(step))?;

    write_location_empty(&mut writer, "Datum", 0.0, 0.0, spec.units)?;

    writer.write_event(Event::Start(BytesStart::new("Profile")))?;
    write_polygon(
        &mut writer,
        spec.units,
        &rectangle_polygon(spec.array_width_mm, spec.array_height_mm),
    )?;
    writer.write_event(Event::End(BytesStart::new("Profile").to_end()))?;

    write_array_step_repeat(&mut writer, spec)?;
    write_generated_layer_features(&mut writer, spec, GeneratedFeatureScope::Array)?;

    writer.write_event(Event::End(BytesStart::new("Step").to_end()))?;

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn write_generated_layer_features(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
    scope: GeneratedFeatureScope,
) -> Result<()> {
    let mut names = GeneratedNameState::default();
    for layer_feature in spec
        .generated_geometry
        .layer_features
        .iter()
        .filter(|layer_feature| layer_feature.scope == scope)
    {
        write_generated_layer_feature(writer, spec.units, layer_feature, &mut names)?;
    }
    Ok(())
}

fn write_generated_layer_feature(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    layer_feature: &GeneratedLayerFeature,
    names: &mut GeneratedNameState,
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
    write_set_features(writer, units, &layer_feature.features, names)?;
    writer.write_event(Event::End(BytesStart::new("Set").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("LayerFeature").to_end()))?;
    Ok(())
}

#[derive(Debug, Default)]
struct GeneratedNameState {
    hole_index: usize,
}

fn write_set_features(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    features: &[SetFeature],
    names: &mut GeneratedNameState,
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
                write_hole(writer, units, hole, names)?;
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
    names: &mut GeneratedNameState,
) -> Result<()> {
    let diameter = fmt_units(hole.diameter, units);
    let x = fmt_units(hole.x, units);
    let y = fmt_units(hole.y, units);
    let generated_name = format!("{GENERATED_HOLE_NAME_PREFIX}_{}", names.hole_index);
    names.hole_index += 1;

    let mut elem = BytesStart::new("Hole");
    elem.push_attribute(("name", generated_name.as_str()));
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

fn write_polygon(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    units: Units,
    polygon: &Polygon,
) -> Result<()> {
    writer.write_event(Event::Start(BytesStart::new("Polygon")))?;
    write_location_empty(writer, "PolyBegin", polygon.begin.x, polygon.begin.y, units)?;
    for step in &polygon.steps {
        match step {
            PolyStep::Segment(segment) => {
                write_location_empty(
                    writer,
                    "PolyStepSegment",
                    segment.point.x,
                    segment.point.y,
                    units,
                )?;
            }
            PolyStep::Curve(_) => bail!("generated board array polygons must be line segments"),
        }
    }
    writer.write_event(Event::End(BytesStart::new("Polygon").to_end()))?;
    Ok(())
}

fn rectangle_polygon(width_mm: f64, height_mm: f64) -> Polygon {
    polygon_from_points(vec![
        Point::new(0.0, 0.0),
        Point::new(0.0, height_mm),
        Point::new(width_mm, height_mm),
        Point::new(width_mm, 0.0),
    ])
    .expect("non-degenerate rectangle should produce a polygon")
}

fn round_fiducial(kind: IpcFiducialKind, x_mm: f64, y_mm: f64, diameter_mm: f64) -> Fiducial {
    Fiducial {
        kind,
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

fn round_fiducial_features(
    kind: IpcFiducialKind,
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    points
        .into_iter()
        .map(|(x, y)| SetFeature::Fiducial(round_fiducial(kind, x, y, diameter_mm)))
        .collect()
}

fn round_nonplated_hole(x_mm: f64, y_mm: f64, diameter_mm: f64) -> Hole {
    Hole {
        name: None,
        diameter: diameter_mm,
        plating_status: PlatingStatus::NonPlated,
        x: x_mm,
        y: y_mm,
    }
}

fn round_nonplated_hole_features(
    points: impl IntoIterator<Item = (f64, f64)>,
    diameter_mm: f64,
) -> Vec<SetFeature> {
    points
        .into_iter()
        .map(|(x, y)| SetFeature::Hole(round_nonplated_hole(x, y, diameter_mm)))
        .collect()
}

fn fiducial_element_name(kind: IpcFiducialKind) -> &'static str {
    match kind {
        IpcFiducialKind::BadBoardMark => "BadBoardMark",
        IpcFiducialKind::Global => "GlobalFiducial",
        IpcFiducialKind::GoodPanelMark => "GoodPanelMark",
        IpcFiducialKind::Local => "LocalFiducial",
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

fn write_array_step_repeat(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let x = fmt_units(spec.array_repeat_x_mm, spec.units);
    let y = fmt_units(spec.array_repeat_y_mm, spec.units);
    let dx = fmt_units(spec.pitch_x_mm, spec.units);
    let dy = fmt_units(spec.pitch_y_mm, spec.units);
    let nx = spec.columns.to_string();
    let ny = spec.rows.to_string();

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_cell_name.as_str()));
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

fn write_board_cell_step_repeat(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    spec: &BoardArraySpec,
) -> Result<()> {
    let x = fmt_units(spec.board_repeat_x_mm, spec.units);
    let y = fmt_units(spec.board_repeat_y_mm, spec.units);

    let mut repeat = BytesStart::new("StepRepeat");
    repeat.push_attribute(("stepRef", spec.board_name.as_str()));
    repeat.push_attribute(("x", x.as_str()));
    repeat.push_attribute(("y", y.as_str()));
    repeat.push_attribute(("nx", "1"));
    repeat.push_attribute(("ny", "1"));
    repeat.push_attribute(("dx", "0"));
    repeat.push_attribute(("dy", "0"));
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
    use crate::manufacturing::build_manufacturing_package;
    use pcb_ir::common::Point;
    use pcb_ir::dialects::ipc::{
        FeatureBucket, FeatureDomain, FeatureKind, FeatureOperation, FeatureRole, FiducialKind,
        GeometryView, LayoutStepKind, PlatingKind,
    };

    #[test]
    fn parses_board_margin_css_shorthand() {
        let cases = [
            (&[1.0][..], BoardMarginMm::all(1.0)),
            (
                &[1.0, 2.0][..],
                BoardMarginMm {
                    top: 1.0,
                    right: 2.0,
                    bottom: 1.0,
                    left: 2.0,
                },
            ),
            (
                &[1.0, 2.0, 3.0][..],
                BoardMarginMm {
                    top: 1.0,
                    right: 2.0,
                    bottom: 3.0,
                    left: 2.0,
                },
            ),
            (
                &[1.0, 2.0, 3.0, 4.0][..],
                BoardMarginMm {
                    top: 1.0,
                    right: 2.0,
                    bottom: 3.0,
                    left: 4.0,
                },
            ),
        ];

        for (values, expected) in cases {
            assert_eq!(BoardMarginMm::from_css_shorthand(values).unwrap(), expected);
        }
        assert!(BoardMarginMm::from_css_shorthand(&[]).is_err());
        assert!(BoardMarginMm::from_css_shorthand(&[1.0, 2.0, 3.0, 4.0, 5.0]).is_err());
    }

    #[test]
    fn creates_rectangular_panel_step_from_board_bbox() {
        let xml = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<StepRef name="array"/>"#));
        assert!(xml.contains(r#"<StepRef name="board_cell"/>"#));
        assert!(xml.contains(r#"<StepRef name="board"/>"#));
        assert!(xml.contains(r#"<LayerRef name="V-Score"/>"#));
        assert!(xml.contains(
            r#"<Layer name="V-Score" layerFunction="V_CUT" side="NONE" polarity="POSITIVE"/>"#
        ));
        assert!(xml.contains(r#"<Step name="array" type="PALLET">"#));
        assert!(xml.contains(r#"<Step name="board_cell" type="PALLET">"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board_cell" x="5" y="5" nx="6" ny="6" dx="15" dy="15" angle="0.00" mirror="false"/>"#
        ));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="4.5" y="5.5" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
        ));
        assert!(xml.contains(r#"<LayerFeature layerRef="V-Score">"#));
        assert!(xml.contains(r#"<Line startX="7.5" startY="0" endX="7.5" endY="100">"#));
        assert!(xml.contains(r#"<Line startX="0" startY="7.5" endX="100" endY="7.5">"#));

        let ipc = Ipc2581::parse(&xml).unwrap();
        let layout = geometry::extract_layout(&ipc).unwrap();
        let (_, panel_step) = pcb_ir::dialects::ipc::root_panel_step(&layout).unwrap();
        assert_point_close(panel_step.bbox.min, Point::new(0.0, 0.0));
        assert_point_close(panel_step.bbox.max, Point::new(100.0, 100.0));
        assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 36);

        let first_instance = layout
            .layout
            .instances
            .iter()
            .find(|instance| {
                layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Board
            })
            .unwrap();
        assert_point_close(first_instance.bbox.min, Point::new(7.5, 7.5));
        assert_point_close(first_instance.bbox.max, Point::new(17.5, 17.5));

        let vcut = geometry::extract_layer_for_view(&ipc, "V-Score", GeometryView::ArrayFlattened)
            .unwrap();
        assert_eq!(vcut.features.len(), 24);
        assert!(
            vcut.features
                .iter()
                .all(|feature| feature.intent.domain == FeatureDomain::VCut)
        );
    }

    #[test]
    fn created_board_array_vcuts_flow_to_svg_and_gerber() {
        let xml = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();
        let ipc = Ipc2581::parse(&xml).unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let svg = crate::board_array::render_board_array_overview_svg(&accessor)
            .unwrap()
            .unwrap();
        assert_eq!(svg.matches("vcut-guide").count(), 24);
        assert!(svg.contains("stroke='#dc2626'"));
        assert!(!svg.contains("stroke-dasharray"));
        assert!(!svg.contains("class='score-guide'"));

        let package = build_manufacturing_package(&ipc, GeometryView::ArrayFlattened).unwrap();

        let vcut = package
            .files
            .iter()
            .find(|file| file.filename == "V_Cut.gbr")
            .unwrap();
        assert!(vcut.contents.contains("%TF.FileFunction,Vcut,Top/Bot*%"));
        assert!(vcut.contents.contains("%TF.Part,Array*%"));
        assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));
        assert!(!vcut.contents.contains("G36*"));
        assert_eq!(vcut.contents.matches("D01*").count(), 24);

        let board_package = build_manufacturing_package(&ipc, GeometryView::Board).unwrap();
        assert!(
            board_package
                .files
                .iter()
                .all(|file| file.filename != "V_Cut.gbr")
        );
    }

    #[test]
    fn created_board_array_profile_gerber_derives_vscore_reliefs() {
        let xml = create_board_array_xml(
            rounded_corner_board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        assert!(!xml.contains("<SlotCavity"));

        let ipc = Ipc2581::parse(&xml).unwrap();
        let package = build_manufacturing_package(&ipc, GeometryView::ArrayFlattened).unwrap();
        let vcut = package
            .files
            .iter()
            .find(|file| file.filename == "V_Cut.gbr")
            .unwrap();
        assert!(!vcut.contents.contains("G36*"));
        assert!(
            package
                .files
                .iter()
                .all(|file| file.filename != "Edge_Cuts.gm1")
        );
        let profile = package
            .files
            .iter()
            .find(|file| file.filename == "Board_Array_Profile.gm1")
            .unwrap();
        assert!(profile.contents.contains("%TF.FileFunction,Profile,NP*%"));
        assert!(profile.contents.contains("%TF.Part,Array*%"));
        assert!(profile.contents.contains("%TA.AperFunction,Profile*%"));
        assert!(profile.contents.contains("%ADD10C,0.05*%"));
        assert!(!profile.contents.contains("%ADD11C,1*%"));
        assert!(!profile.contents.contains("G36*"));
        assert!(
            profile.contents.matches("D01*").count() > vcut.contents.matches("D01*").count(),
            "routed reliefs should emit closed contour strokes, not only the V-cut guide lines"
        );
        gerberx2::GerberX2::parse(&profile.contents).unwrap();
    }

    #[test]
    fn board_array_creation_drops_source_board_outline_layer_features() {
        let xml = create_board_array_xml(
            board_fixture_with_edge_cuts_layer_mm(),
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 2,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<LayerFeature layerRef="TOP">"#));
        assert!(!xml.contains(r#"<LayerRef name="Edge.Cuts""#));
        assert!(!xml.contains(r#"<Layer name="Edge.Cuts""#));
        assert!(!xml.contains(r#"<LayerFeature layerRef="Edge.Cuts">"#));

        let ipc = Ipc2581::parse(&xml).unwrap();
        let package = build_manufacturing_package(&ipc, GeometryView::ArrayFlattened).unwrap();
        assert!(
            package
                .files
                .iter()
                .all(|file| file.filename != "Edge_Cuts.gm1")
        );
        assert!(
            package
                .files
                .iter()
                .any(|file| file.filename == "Board_Array_Profile.gm1")
        );
    }

    #[test]
    fn board_array_creation_preserves_board_target_geometry() {
        let input = board_fixture_with_top_line_mm();
        let before_ipc = Ipc2581::parse(input).unwrap();
        let before =
            geometry::extract_layer_for_view(&before_ipc, "TOP", GeometryView::Board).unwrap();

        let xml = create_board_array_xml(
            input,
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 6,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();
        let after_ipc = Ipc2581::parse(&xml).unwrap();
        let after =
            geometry::extract_layer_for_view(&after_ipc, "TOP", GeometryView::Board).unwrap();

        assert_eq!(before.features.len(), after.features.len());
        assert_eq!(before.paths.len(), after.paths.len());
        assert_eq!(before.contours.len(), after.contours.len());
        assert_eq!(before.path_cmds, after.path_cmds);

        for (before_feature, after_feature) in before.features.iter().zip(&after.features) {
            assert_eq!(before_feature.kind, after_feature.kind);
            assert_eq!(before_feature.bucket, after_feature.bucket);
            assert_eq!(before_feature.polarity, after_feature.polarity);
            assert_eq!(before_feature.intent, after_feature.intent);
            assert_eq!(before_feature.fiducial_kind, after_feature.fiducial_kind);
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
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap();

        spec.generated_geometry.add_layer_feature(
            GeneratedFeatureScope::Array,
            "TOP",
            Polarity::Positive,
            round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 1.0),
        );
        spec.generated_geometry.add_layer_feature(
            GeneratedFeatureScope::Array,
            "F.Mask",
            Polarity::Positive,
            round_fiducial_features(IpcFiducialKind::Global, [(12.5, 12.5)], 2.0),
        );
        spec.generated_geometry.add_layer(GeneratedLayer::new(
            "Array_Drill",
            LayerFunction::Drill,
            Some(Side::All),
            Some(Polarity::Positive),
        ));
        spec.generated_geometry.add_layer_feature(
            GeneratedFeatureScope::Array,
            "Array_Drill",
            Polarity::Positive,
            round_nonplated_hole_features([(20.0, 20.0)], 2.0),
        );
        spec.content_layer_refs = content_layer_refs(
            &ipc,
            &spec.generated_geometry,
            &spec.board_outline_layer_names,
        );

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
            geometry::extract_layer_for_view(&parsed, "TOP", GeometryView::ArrayFlattened).unwrap();
        assert!(top.features.iter().any(|feature| {
            feature.intent.role == FeatureRole::Fiducial
                && feature.fiducial_kind == FiducialKind::Global
        }));

        let drill =
            geometry::extract_layer_for_view(&parsed, "Array_Drill", GeometryView::ArrayFlattened)
                .unwrap();
        assert_eq!(drill.features.len(), 1);
        assert_eq!(drill.features[0].kind, FeatureKind::Hole);
        assert_eq!(drill.features[0].bucket, FeatureBucket::Cutout);
        assert_eq!(drill.features[0].intent.domain, FeatureDomain::Drill);
        assert_eq!(drill.features[0].intent.role, FeatureRole::Hole);
        assert_eq!(drill.features[0].intent.operation, FeatureOperation::Drill);
        assert_eq!(drill.features[0].intent.plating, PlatingKind::NonPlated);

        let package = build_manufacturing_package(&parsed, GeometryView::ArrayFlattened).unwrap();
        let top = package
            .files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        let mask = package
            .files
            .iter()
            .find(|file| file.filename == "F_Mask.gts")
            .unwrap();
        let drill = package
            .files
            .iter()
            .find(|file| file.filename == "NPTH.drl")
            .unwrap();

        assert!(
            top.contents
                .contains("%TA.AperFunction,FiducialPad,Global*%")
        );
        assert!(
            mask.contents
                .contains("%TA.AperFunction,FiducialPad,Global*%")
        );
        assert!(drill.contents.contains("; #@! TF.FileFunction,NonPlated"));
        assert!(
            drill
                .contents
                .contains("; #@! TA.AperFunction,NonPlated,NPTH,ComponentDrill")
        );
        assert!(drill.contents.contains("X20.0Y20.0"));
        assert!(!top.contents.contains("%TA.AperFunction,Other,Drill*%"));
        assert!(!mask.contents.contains("%TA.AperFunction,Other,Drill*%"));
    }

    #[test]
    fn board_array_creation_adds_default_tooling_at_single_column_min_width() {
        let input = board_fixture_with_mask_bbox_mm(35.0, 40.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                board_margin_mm: board_margin(5.0, 0.0),
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        let step = array_step(&ipc);
        let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
        let mask_fiducials = fiducials_on_layer(&ipc, step, "F.Mask");
        let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);

        assert_eq!(top_fiducials.len(), 4);
        assert_eq!(mask_fiducials.len(), 4);
        assert_eq!(tooling_holes.len(), 4);
        assert!(
            top_fiducials
                .iter()
                .all(|fiducial| close(fiducial_diameter(fiducial), 1.0))
        );
        assert!(
            mask_fiducials
                .iter()
                .all(|fiducial| close(fiducial_diameter(fiducial), 2.0))
        );
        assert!(tooling_holes.iter().all(|hole| {
            close(hole.diameter, 2.0) && hole.plating_status == PlatingStatus::NonPlated
        }));
        assert_points_close(
            fiducial_points(&top_fiducials),
            vec![(27.5, 66.15), (42.5, 66.15), (32.5, 3.85), (37.5, 3.85)],
        );
        assert_points_close(
            hole_points(&tooling_holes),
            vec![(22.5, 67.5), (47.5, 67.5), (27.5, 2.5), (42.5, 2.5)],
        );
    }

    #[test]
    fn board_array_creation_adds_default_tooling_at_multi_column_min_width() {
        let input = board_fixture_with_mask_bbox_mm(20.0, 40.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: board_margin(5.0, 0.0),
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        let step = array_step(&ipc);
        let top_fiducials = fiducials_on_layer(&ipc, step, "TOP");
        let mask_fiducials = fiducials_on_layer(&ipc, step, "F.Mask");
        let tooling_holes = holes_on_layer(&ipc, step, TOOLING_HOLE_LAYER_BASE_NAME);

        assert_eq!(top_fiducials.len(), 4);
        assert_eq!(mask_fiducials.len(), 4);
        assert_eq!(tooling_holes.len(), 4);
        assert_points_close(
            fiducial_points(&top_fiducials),
            vec![(27.5, 66.15), (52.5, 66.15), (32.5, 3.85), (47.5, 3.85)],
        );
        assert_points_close(
            hole_points(&tooling_holes),
            vec![(22.5, 67.5), (57.5, 67.5), (27.5, 2.5), (52.5, 2.5)],
        );
    }

    #[test]
    fn board_array_creation_skips_default_tooling_when_board_width_is_too_small() {
        let input = board_fixture_with_mask_bbox_mm(19.0, 40.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: board_margin(5.0, 0.0),
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        let ecad = ipc.ecad().unwrap();
        assert!(
            ecad.cad_data
                .layers
                .iter()
                .all(|layer| ipc.resolve(layer.name) != TOOLING_HOLE_LAYER_BASE_NAME)
        );

        let step = array_step(&ipc);
        let fiducial_count = step
            .layer_features
            .iter()
            .flat_map(|layer_feature| &layer_feature.sets)
            .flat_map(|set| set.fiducials())
            .count();
        let hole_count = step
            .layer_features
            .iter()
            .flat_map(|layer_feature| &layer_feature.sets)
            .flat_map(|set| set.holes())
            .count();

        assert_eq!(fiducial_count, 0);
        assert_eq!(hole_count, 0);
    }

    #[test]
    fn board_array_creation_adds_board_cell_fiducials_on_top_bottom_margins() {
        let input = board_fixture_with_mask_bbox_mm(40.0, 30.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: BoardMarginMm {
                    top: 5.0,
                    right: 0.0,
                    bottom: 5.0,
                    left: 0.0,
                },
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        let cell = board_cell_step(&ipc);
        let top_fiducials = fiducials_on_layer(&ipc, cell, "TOP");
        let mask_fiducials = fiducials_on_layer(&ipc, cell, "F.Mask");

        assert_eq!(top_fiducials.len(), 4);
        assert_eq!(mask_fiducials.len(), 4);
        assert!(
            top_fiducials
                .iter()
                .all(|fiducial| fiducial.kind == IpcFiducialKind::Local)
        );
        assert!(
            top_fiducials
                .iter()
                .all(|fiducial| close(fiducial_diameter(fiducial), 1.0))
        );
        assert!(
            mask_fiducials
                .iter()
                .all(|fiducial| close(fiducial_diameter(fiducial), 2.0))
        );
        assert_points_close(
            fiducial_points(&top_fiducials),
            vec![(5.0, 38.0), (35.0, 38.0), (10.0, 2.0), (30.0, 2.0)],
        );

        let top =
            geometry::extract_layer_for_view(&ipc, "TOP", GeometryView::ArrayFlattened).unwrap();
        assert_eq!(
            top.features
                .iter()
                .filter(|feature| feature.fiducial_kind == FiducialKind::Local)
                .count(),
            8
        );

        let package = build_manufacturing_package(&ipc, GeometryView::ArrayFlattened).unwrap();
        let top = package
            .files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(
            top.contents
                .contains("%TA.AperFunction,FiducialPad,Local*%")
        );
    }

    #[test]
    fn board_array_creation_adds_board_cell_fiducials_on_left_right_margins() {
        let input = board_fixture_with_mask_bbox_mm(30.0, 40.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 2,
                board_margin_mm: BoardMarginMm {
                    top: 0.0,
                    right: 5.0,
                    bottom: 0.0,
                    left: 5.0,
                },
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        let top_fiducials = fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP");

        assert_eq!(top_fiducials.len(), 4);
        assert_points_close(
            fiducial_points(&top_fiducials),
            vec![(2.0, 35.0), (2.0, 5.0), (38.0, 30.0), (38.0, 10.0)],
        );
    }

    #[test]
    fn board_array_creation_skips_board_cell_fiducials_for_single_board_array() {
        let input = board_fixture_with_mask_bbox_mm(40.0, 30.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                board_margin_mm: BoardMarginMm {
                    top: 5.0,
                    right: 5.0,
                    bottom: 5.0,
                    left: 5.0,
                },
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP").is_empty());
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask").is_empty());
        assert_eq!(fiducials_on_layer(&ipc, array_step(&ipc), "TOP").len(), 4);
    }

    #[test]
    fn board_array_creation_skips_board_cell_fiducials_without_eligible_margin() {
        let input = board_fixture_with_mask_bbox_mm(40.0, 35.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: BoardMarginMm {
                    top: 4.99,
                    right: 0.0,
                    bottom: 4.99,
                    left: 0.0,
                },
                edge_rail_width_mm: 15.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP").is_empty());
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask").is_empty());
    }

    #[test]
    fn board_array_creation_skips_board_cell_fiducials_without_eligible_span() {
        let input = board_fixture_with_mask_bbox_mm(29.99, 25.0);
        let xml = create_board_array_xml(
            &input,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: BoardMarginMm {
                    top: 5.0,
                    right: 5.0,
                    bottom: 5.0,
                    left: 5.0,
                },
                edge_rail_width_mm: 20.0,
            },
        )
        .unwrap();

        let ipc = Ipc2581::parse(&xml).unwrap();
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "TOP").is_empty());
        assert!(fiducials_on_layer(&ipc, board_cell_step(&ipc), "F.Mask").is_empty());
    }

    #[test]
    fn writes_generated_board_array_values_in_cad_header_units() {
        let xml = create_board_array_xml(
            board_fixture_inch(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                board_margin_mm: board_margin(0.0, 0.0),
                edge_rail_width_mm: 25.4,
            },
        )
        .unwrap();

        assert!(xml.contains(r#"<PolyStepSegment x="0" y="3"/>"#));
        assert!(xml.contains(r#"<PolyStepSegment x="3" y="3"/>"#));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board_cell" x="1" y="1" nx="1" ny="1" dx="1" dy="1" angle="0.00" mirror="false"/>"#
        ));
        assert!(xml.contains(
            r#"<StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>"#
        ));
    }

    #[test]
    fn rejects_primary_panel_step() {
        let error = create_board_array_xml(
            panel_fixture(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                board_margin_mm: board_margin(0.0, 0.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("primary IPC-2581 step is already a board array")
        );
    }

    #[test]
    fn validates_simple_api_ranges() {
        let error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 11,
                rows: 1,
                board_margin_mm: board_margin(0.0, 0.0),
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
    fn rejects_small_clearance_and_edge_rail_width() {
        let horizontal_gap_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 1,
                board_margin_mm: board_margin(4.99, 0.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            horizontal_gap_error
                .to_string()
                .contains("horizontal board clearance must be 0 mm or at least 5 mm")
        );

        let vertical_gap_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 2,
                board_margin_mm: board_margin(0.0, 4.99),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            vertical_gap_error
                .to_string()
                .contains("vertical board clearance must be 0 mm or at least 5 mm")
        );

        let rail_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 1,
                board_margin_mm: board_margin(0.0, 0.0),
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
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            narrow_error
                .to_string()
                .contains("array width must be at least 70 mm; got 55 mm")
        );

        let short_error = create_board_array_xml(
            board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 4,
                rows: 2,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            short_error
                .to_string()
                .contains("array height must be at least 70 mm; got 40 mm")
        );

        let wide_error = create_board_array_xml(
            large_board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 6,
                rows: 1,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            wide_error
                .to_string()
                .contains("array width must be at most 260 mm; got 400 mm")
        );

        let tall_error = create_board_array_xml(
            large_board_fixture_mm(),
            &BoardArrayCreateOptions {
                columns: 1,
                rows: 6,
                board_margin_mm: board_margin(5.0, 5.0),
                edge_rail_width_mm: 5.0,
            },
        )
        .unwrap_err();
        assert!(
            tall_error
                .to_string()
                .contains("array height must be at most 260 mm; got 400 mm")
        );
    }

    fn board_margin(horizontal_gap_mm: f64, vertical_gap_mm: f64) -> BoardMarginMm {
        BoardMarginMm {
            top: vertical_gap_mm / 2.0,
            right: horizontal_gap_mm / 2.0,
            bottom: vertical_gap_mm / 2.0,
            left: horizontal_gap_mm / 2.0,
        }
    }

    fn assert_point_close(actual: Point, expected: Point) {
        assert!(
            (actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9,
            "expected {expected:?}, got {actual:?}"
        );
    }

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    fn array_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
        ipc.ecad()
            .unwrap()
            .cad_data
            .steps
            .iter()
            .find(|step| ipc.resolve(step.name) == "array")
            .unwrap()
    }

    fn board_cell_step(ipc: &Ipc2581) -> &ipc2581::types::ecad::Step {
        ipc.ecad()
            .unwrap()
            .cad_data
            .steps
            .iter()
            .find(|step| ipc.resolve(step.name) == "board_cell")
            .unwrap()
    }

    fn fiducials_on_layer<'a>(
        ipc: &'a Ipc2581,
        step: &'a ipc2581::types::ecad::Step,
        layer_name: &str,
    ) -> Vec<&'a Fiducial> {
        step.layer_features
            .iter()
            .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
            .flat_map(|layer_feature| &layer_feature.sets)
            .flat_map(|set| set.fiducials())
            .collect()
    }

    fn holes_on_layer<'a>(
        ipc: &'a Ipc2581,
        step: &'a ipc2581::types::ecad::Step,
        layer_name: &str,
    ) -> Vec<&'a Hole> {
        step.layer_features
            .iter()
            .filter(|layer_feature| ipc.resolve(layer_feature.layer_ref) == layer_name)
            .flat_map(|layer_feature| &layer_feature.sets)
            .flat_map(|set| set.holes())
            .collect()
    }

    fn fiducial_diameter(fiducial: &Fiducial) -> f64 {
        match &fiducial.shape {
            FiducialShape::Primitive(StandardPrimitive::Circle(circle)) => circle.shape.diameter,
            _ => panic!("expected round fiducial"),
        }
    }

    fn fiducial_points(fiducials: &[&Fiducial]) -> Vec<(f64, f64)> {
        fiducials
            .iter()
            .map(|fiducial| (fiducial.location.x, fiducial.location.y))
            .collect()
    }

    fn hole_points(holes: &[&Hole]) -> Vec<(f64, f64)> {
        holes.iter().map(|hole| (hole.x, hole.y)).collect()
    }

    fn assert_points_close(actual: Vec<(f64, f64)>, expected: Vec<(f64, f64)>) {
        let actual = sorted_points(actual);
        let expected = sorted_points(expected);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(&expected) {
            assert!(
                close(actual.0, expected.0) && close(actual.1, expected.1),
                "expected {expected:?}, got {actual:?}"
            );
        }
    }

    fn sorted_points(mut points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
        points.sort_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then_with(|| left.0.total_cmp(&right.0))
        });
        points
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

    fn rounded_corner_board_fixture_mm() -> &'static str {
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
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="4" y="10"/>
            <PolyStepCurve x="0" y="6" centerX="4" centerY="6" clockwise="false"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
    }

    fn board_fixture_with_mask_bbox_mm(width_mm: f64, height_mm: f64) -> String {
        format!(
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
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="{width_mm}" y="0"/>
            <PolyStepSegment x="{width_mm}" y="{height_mm}"/>
            <PolyStepSegment x="0" y="{height_mm}"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        )
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

    fn board_fixture_with_edge_cuts_layer_mm() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <LayerRef name="Edge.Cuts"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="Edge.Cuts" layerFunction="BOARD_OUTLINE" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="40"/>
            <PolyStepSegment x="0" y="40"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="1" startY="1" endX="5" endY="1">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="Edge.Cuts">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="0" startY="0" endX="40" endY="0">
                <LineDesc lineWidth="0.05" lineEnd="ROUND"/>
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
