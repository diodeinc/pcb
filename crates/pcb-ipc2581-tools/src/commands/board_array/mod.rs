use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;

use super::board_array_auto::{
    AutoBoardArrayPlan, AutoSheetSize, TargetSizeMm, auto_board_array_plan,
    auto_board_array_plan_for_sheet,
};
use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;
use crate::utils::format::fmt_num;
use anyhow::{Context, Result, bail};
use ipc2581::types::{
    Units,
    ecad::{
        Fiducial, FiducialKind as IpcFiducialKind, FiducialShape, Hole, LayerFunction, Line,
        PlatingStatus, Polarity, SetFeature, Side, StepType,
    },
    primitives::{
        Circle, LineEnd, LineProperty, Point as IpcPoint, PolyStep, PolyStepCurve, PolyStepSegment,
        Polygon, StandardPrimitive, Styled,
    },
    transform::Location,
};
use pcb_ir::{
    dialects::ipc::{LayoutStepKind, root_step},
    geom::{BBox, Point},
};

const EPSILON: f64 = 1e-9;
const MIN_BOARD_ARRAY_DIMENSION_MM: f64 = 70.0;
const MAX_BOARD_ARRAY_DIMENSION_MM: f64 = 297.0;
const MAX_VCUT_LINES_PER_AXIS: usize = 25;
const MIN_VCUT_CLEARANCE_MM: f64 = 5.0;
const MIN_EDGE_RAIL_WIDTH_MM: f64 = 5.0;
const MAX_MANUAL_EDGE_RAIL_WIDTH_MM: f64 = 30.0;
const VCUT_LAYER_BASE_NAME: &str = "V-Score";
const VCUT_SPEC_BASE_NAME: &str = "Board_Array_VCut";
const VCUT_MARKER_STROKE_MM: f64 = 0.10;
const VCUT_CALLOUT_ARROW_LENGTH_MM: f64 = 2.5;
const VCUT_CALLOUT_ARROW_CLEARANCE_MM: f64 = 0.8;
const VCUT_CALLOUT_ARROW_HEAD_MM: f64 = 0.45;
const VCUT_CALLOUT_TEXT_HEIGHT_MM: f64 = 1.2;
const VCUT_CALLOUT_TEXT_STROKE_MM: f64 = 0.12;
const VCUT_CALLOUT_TEXT_GAP_MM: f64 = 0.45;
// KiCad's built-in "KiCad Font" stroke glyph coordinates use Hershey/newstroke units.
const KICAD_STROKE_FONT_SCALE: f64 = 1.0 / 21.0;
const KICAD_STROKE_FONT_OFFSET: i32 = -8;
const KICAD_VCUT_LABEL_GLYPHS: [&str; 5] = [
    "I[KFR[YF",
    "E_JSZS",
    "F[WYVZS[Q[NZLXKVJRJOKKLINGQFSFVGWH",
    "G]LFLWMYNZP[T[VZWYXWXF",
    "JZLFXF RR[RF",
];
const TOP_COPPER_LAYER_BASE_NAME: &str = "F.Cu";
const TOP_SOLDERMASK_LAYER_BASE_NAME: &str = "F.Mask";
const TOOLING_HOLE_LAYER_BASE_NAME: &str = "Board_Array_Drill";
const GENERATED_HOLE_NAME_PREFIX: &str = "array_tooling_hole";
const FIDUCIAL_COPPER_DIAMETER_MM: f64 = 1.0;
const FIDUCIAL_MASK_OPENING_DIAMETER_MM: f64 = 2.0;
const TOOLING_HOLE_DIAMETER_MM: f64 = 2.0;
const CORNER_TOOLING_HOLE_DIAMETER_MM: f64 = 2.1;
const TOOLING_HOLE_EDGE_OFFSET_MM: f64 = 2.5;
const FIDUCIAL_EDGE_OFFSET_MM: f64 = 3.85;
const ARRAY_CORNER_RADIUS_MM: f64 = 3.0;
const ARRAY_CORNER_TOOLING_HOLE_INSET_MM: f64 = 3.0;
const PRIMARY_TOOLING_HOLE_SPAN_INSET_MM: f64 = 2.5;
const PRIMARY_FIDUCIAL_SPAN_INSET_MM: f64 = 8.0;
const SECONDARY_TOOLING_HOLE_SPAN_INSET_MM: f64 = 6.5;
const SECONDARY_FIDUCIAL_SPAN_INSET_MM: f64 = 12.0;
const SINGLE_BOARD_TOOLING_MIN_SPAN_MM: f64 = 28.0;
const MULTI_BOARD_TOOLING_MIN_SPAN_MM: f64 = 12.0;
const MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM: f64 = 5.0;
const MIN_BOARD_CELL_FIDUCIAL_SPAN_MM: f64 = 17.0;
const BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM: f64 = 2.0;
const PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = 3.0;
const SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = 7.0;

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
    MmMin {
        field: &'static str,
        value: f64,
        min: f64,
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
            Self::MmMin { field, value, min } => write!(
                f,
                "{field} must be at least {} mm; got {} mm",
                fmt_num(*min),
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
    pub edge_rail_mm: BoardMarginMm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardArrayValidationMode {
    Manual,
    Auto,
    AutoMinimumPanel,
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
        Self::from_css_shorthand_named("board margin", values)
    }

    pub fn from_css_shorthand_named(name: &'static str, values: &[f64]) -> Result<Self> {
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
            _ => bail!("{name} expects 1 to 4 values"),
        }
    }

    fn horizontal_gap(self) -> f64 {
        self.left + self.right
    }

    fn vertical_gap(self) -> f64 {
        self.top + self.bottom
    }

    fn board_margin_sides(self) -> [(&'static str, f64); 4] {
        [
            ("board margin top", self.top),
            ("board margin right", self.right),
            ("board margin bottom", self.bottom),
            ("board margin left", self.left),
        ]
    }

    fn edge_rail_sides(self) -> [(&'static str, f64); 4] {
        [
            ("edge rail top", self.top),
            ("edge rail right", self.right),
            ("edge rail bottom", self.bottom),
            ("edge rail left", self.left),
        ]
    }
}

#[derive(Debug, Clone)]
struct BoardArraySpec {
    array_name: String,
    board_cell_name: String,
    board_name: String,
    vcut_spec_name: String,
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
    board_margin_mm: BoardMarginMm,
    edge_rail_mm: BoardMarginMm,
    panelization: BoardArrayPanelizationMetadata,
    generated_geometry: BoardArrayGeneratedGeometry,
    units: Units,
}

#[derive(Debug, Clone, Copy)]
struct BoardArrayPanelizationMetadata {
    mode: BoardArrayPanelizationMode,
    sheet: Option<AutoSheetSize>,
    sheet_target_mm: Option<TargetSizeMm>,
}

impl BoardArrayPanelizationMetadata {
    fn for_auto_plan(mode: BoardArrayPanelizationMode, plan: AutoBoardArrayPlan) -> Self {
        Self {
            mode,
            sheet: Some(plan.sheet),
            sheet_target_mm: Some(plan.target),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardArrayPanelizationMode {
    Manual,
    Auto,
    AutoSheet,
    AutoMinimumPanel,
}

impl BoardArrayPanelizationMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
            Self::AutoSheet => "auto_sheet",
            Self::AutoMinimumPanel => "auto_minimum_panel",
        }
    }
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
        self.add_layer_feature_with_spec_refs(scope, layer_name, polarity, Vec::new(), features);
    }

    fn add_layer_feature_with_spec_refs(
        &mut self,
        scope: GeneratedFeatureScope,
        layer_name: impl Into<String>,
        polarity: Polarity,
        spec_refs: Vec<String>,
        features: Vec<SetFeature>,
    ) {
        self.layer_features.push(GeneratedLayerFeature {
            scope,
            layer_name: layer_name.into(),
            polarity,
            spec_refs,
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
    spec_refs: Vec<String>,
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
    write_board_array_output(output, &updated_xml)?;
    Ok(())
}

pub fn execute_auto(input: &Path, output: &Path, sheet: Option<AutoSheetSize>) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    let updated_xml = create_auto_board_array_xml_with_sheet(&content, sheet)?;
    write_board_array_output(output, &updated_xml)?;
    Ok(())
}

fn write_board_array_output(output: &Path, content: &str) -> Result<()> {
    if output.as_os_str() == "-" {
        io::stdout().lock().write_all(content.as_bytes())?;
        eprintln!("✓ Created IPC-2581 board array on stdout");
    } else {
        file_utils::save_ipc_file(output, content)?;
        eprintln!("✓ Created IPC-2581 board array at {}", output.display());
    }

    Ok(())
}

fn create_board_array_xml(xml: &str, options: &BoardArrayCreateOptions) -> Result<String> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let spec = build_board_array_spec(
        &ipc,
        options,
        BoardArrayValidationMode::Manual,
        BoardArrayPanelizationMetadata {
            mode: BoardArrayPanelizationMode::Manual,
            sheet: None,
            sheet_target_mm: None,
        },
    )?;
    write_board_array_xml(xml, &spec)
}

#[cfg(test)]
fn create_auto_board_array_xml(xml: &str) -> Result<String> {
    create_auto_board_array_xml_with_sheet(xml, None)
}

fn create_auto_board_array_xml_with_sheet(
    xml: &str,
    sheet: Option<AutoSheetSize>,
) -> Result<String> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let (options, validation_mode, panelization) = auto_board_array_options(&ipc, sheet)?;
    let spec = build_board_array_spec(&ipc, &options, validation_mode, panelization)?;
    write_board_array_xml(xml, &spec)
}

fn auto_board_array_options(
    ipc: &Ipc2581,
    sheet: Option<AutoSheetSize>,
) -> Result<(
    BoardArrayCreateOptions,
    BoardArrayValidationMode,
    BoardArrayPanelizationMetadata,
)> {
    let board = primary_board_layout(ipc)?;
    let board_margin = auto_board_margin(ipc, board.bbox)?;
    let board_width = board.bbox.width();
    let board_height = board.bbox.height();

    let plan = match sheet {
        Some(sheet) => Some((
            auto_board_array_plan_for_sheet(board_width, board_height, board_margin, sheet)?,
            BoardArrayPanelizationMode::AutoSheet,
        )),
        None => auto_board_array_plan(board_width, board_height, board_margin)
            .ok()
            .map(|plan| (plan, BoardArrayPanelizationMode::Auto)),
    };

    // A board too large for any sheet still gets a minimum single-board panel.
    Ok(match plan {
        Some((plan, mode)) => (
            options_for_auto_plan(plan),
            BoardArrayValidationMode::Auto,
            BoardArrayPanelizationMetadata::for_auto_plan(mode, plan),
        ),
        None => (
            minimum_single_board_auto_options(board_margin),
            BoardArrayValidationMode::AutoMinimumPanel,
            BoardArrayPanelizationMetadata {
                mode: BoardArrayPanelizationMode::AutoMinimumPanel,
                sheet: None,
                sheet_target_mm: None,
            },
        ),
    })
}

fn options_for_auto_plan(plan: AutoBoardArrayPlan) -> BoardArrayCreateOptions {
    BoardArrayCreateOptions {
        columns: plan.columns,
        rows: plan.rows,
        board_margin_mm: plan.board_margin_mm,
        edge_rail_mm: plan.edge_rail_mm,
    }
}

fn minimum_single_board_auto_options(board_margin: BoardMarginMm) -> BoardArrayCreateOptions {
    BoardArrayCreateOptions {
        columns: 1,
        rows: 1,
        board_margin_mm: board_margin,
        edge_rail_mm: BoardMarginMm::all(MIN_EDGE_RAIL_WIDTH_MM),
    }
}

fn auto_board_margin(ipc: &Ipc2581, board_bbox: BBox) -> Result<BoardMarginMm> {
    let safe_bbox = board_bbox.union(board_courtyard_bbox(ipc)?);
    Ok(BoardMarginMm {
        top: (safe_bbox.max.y - board_bbox.max.y).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        right: (safe_bbox.max.x - board_bbox.max.x).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        bottom: (board_bbox.min.y - safe_bbox.min.y).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        left: (board_bbox.min.x - safe_bbox.min.x).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
    })
}

fn board_courtyard_bbox(ipc: &Ipc2581) -> Result<BBox> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut bbox = BBox::empty();

    for layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Courtyard)
    {
        let layer_name = ipc.resolve(layer.name);
        let doc =
            geometry::extract_layer_for_view(ipc, layer_name, pcb_ir::dialects::ipc::View::Board)
                .with_context(|| {
                format!("failed to extract IPC-2581 courtyard layer '{layer_name}'")
            })?;
        for feature in doc
            .features
            .iter()
            .filter(|feature| feature.source_layer_ref == Some(layer.name))
        {
            bbox = bbox.union(feature.bbox);
        }
    }

    Ok(bbox)
}

fn write_board_array_xml(xml: &str, spec: &BoardArraySpec) -> Result<String> {
    let generated_spec_xml = write_generated_specs_xml(spec);
    let generated_layer_xml = write_generated_layers_xml(&spec.generated_geometry);
    let generated_steps_xml = write_generated_steps_xml(spec)?;

    // One parse serves both the board-array patch and the history append;
    // all edits splice in a single pass.
    let doc = ipc2581::edit::Doc::parse(xml)?;
    let mut edits = board_array_edits(
        &doc,
        spec,
        &generated_spec_xml,
        generated_layer_xml.as_deref(),
        &generated_steps_xml,
    )?;
    edits.extend(crate::utils::history::file_revision_edits(
        &doc,
        "Created board array",
    )?);
    let xml = ipc2581::edit::apply(xml, edits)?;
    let xml = crate::utils::format::reformat_xml(&xml)?;

    Ipc2581::parse(&xml).context("Generated IPC-2581 board array XML did not parse")?;
    Ok(xml)
}

#[derive(Debug, Clone, Copy)]
struct PrimaryBoardLayout {
    source_step_ref: ipc2581::Symbol,
    bbox: pcb_ir::geom::BBox,
}

fn primary_board_layout(ipc: &Ipc2581) -> Result<PrimaryBoardLayout> {
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

    Ok(PrimaryBoardLayout {
        source_step_ref: root.source_step_ref,
        bbox: root.bbox,
    })
}

fn build_board_array_spec(
    ipc: &Ipc2581,
    options: &BoardArrayCreateOptions,
    validation_mode: BoardArrayValidationMode,
    panelization: BoardArrayPanelizationMetadata,
) -> Result<BoardArraySpec> {
    validate_options(options, validation_mode)?;

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

    let root = primary_board_layout(ipc)?;
    let board_width = root.bbox.width();
    let board_height = root.bbox.height();

    let columns = options.columns;
    let rows = options.rows;
    let board_margin = options.board_margin_mm;
    let edge_rail = options.edge_rail_mm;
    let margin_x = edge_rail.left + board_margin.left;
    let margin_y = edge_rail.bottom + board_margin.bottom;
    let pitch_x = board_width + board_margin.horizontal_gap();
    let pitch_y = board_height + board_margin.vertical_gap();
    let array_width = columns as f64 * board_width
        + columns as f64 * board_margin.horizontal_gap()
        + edge_rail.left
        + edge_rail.right;
    let array_height = rows as f64 * board_height
        + rows as f64 * board_margin.vertical_gap()
        + edge_rail.bottom
        + edge_rail.top;
    validate_array_dimensions(array_width, array_height, validation_mode)?;
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
    let existing_spec_names = ecad
        .cad_header
        .specs
        .keys()
        .map(|name| ipc.resolve(*name).to_string())
        .collect::<HashSet<_>>();
    let vcut_spec_name = unique_name(&existing_spec_names, VCUT_SPEC_BASE_NAME);
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
        vcut_spec_name.clone(),
        array_width,
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
    add_board_array_corner_tooling(
        &mut generated_geometry,
        &mut used_layer_names,
        array_width,
        array_height,
    );
    add_board_array_tooling(
        &mut generated_geometry,
        ipc,
        ecad,
        &mut used_layer_names,
        BoardArrayToolingSpec {
            orientation: board_array_tooling_orientation(array_width, array_height),
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
        },
    );
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
    let board_outline_layer_names = board_outline_layer_names(ipc, ecad);
    let content_step_refs = content_step_refs(ipc, &array_name, &board_cell_name, &board_name);
    let content_layer_refs =
        content_layer_refs(ipc, &generated_geometry, &board_outline_layer_names);

    Ok(BoardArraySpec {
        array_name,
        board_cell_name,
        board_name,
        vcut_spec_name,
        board_outline_layer_names,
        content_step_refs,
        content_layer_refs,
        columns,
        rows,
        array_repeat_x_mm: edge_rail.left,
        array_repeat_y_mm: edge_rail.bottom,
        board_repeat_x_mm: board_repeat_x,
        board_repeat_y_mm: board_repeat_y,
        pitch_x_mm: pitch_x,
        pitch_y_mm: pitch_y,
        array_width_mm: array_width,
        array_height_mm: array_height,
        board_margin_mm: board_margin,
        edge_rail_mm: edge_rail,
        panelization,
        generated_geometry,
        units: ecad.cad_header.units,
    })
}

fn validate_options(
    options: &BoardArrayCreateOptions,
    validation_mode: BoardArrayValidationMode,
) -> Result<()> {
    validate_u32_range("columns", options.columns, 1, 10)?;
    validate_u32_range("rows", options.rows, 1, 10)?;
    for (field, value) in options.board_margin_mm.board_margin_sides() {
        validate_mm_min(field, value, 0.0)?;
    }
    for (field, value) in options.edge_rail_mm.edge_rail_sides() {
        match validation_mode {
            BoardArrayValidationMode::Manual => validate_mm_range(
                field,
                value,
                MIN_EDGE_RAIL_WIDTH_MM,
                MAX_MANUAL_EDGE_RAIL_WIDTH_MM,
            )?,
            BoardArrayValidationMode::Auto | BoardArrayValidationMode::AutoMinimumPanel => {
                validate_mm_min(field, value, MIN_EDGE_RAIL_WIDTH_MM)?
            }
        }
    }
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

fn validate_mm_min(field: &'static str, value: f64, min: f64) -> Result<()> {
    if value.is_finite() && value + EPSILON >= min {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::MmMin { field, value, min }.into())
    }
}

fn validate_zero_or_min_mm(field: &'static str, value: f64, min: f64) -> Result<()> {
    if value.abs() <= EPSILON || value + EPSILON >= min {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::ZeroOrMinMm { field, value, min }.into())
    }
}

fn validate_array_dimensions(
    width_mm: f64,
    height_mm: f64,
    validation_mode: BoardArrayValidationMode,
) -> Result<()> {
    validate_array_dimension("width", width_mm, validation_mode)?;
    validate_array_dimension("height", height_mm, validation_mode)
}

fn validate_array_dimension(
    axis: &'static str,
    value: f64,
    validation_mode: BoardArrayValidationMode,
) -> Result<()> {
    if validation_mode == BoardArrayValidationMode::AutoMinimumPanel {
        return validate_minimum_panel_array_dimension(axis, value);
    }

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

fn validate_minimum_panel_array_dimension(axis: &'static str, value: f64) -> Result<()> {
    if value.is_finite() && value > EPSILON {
        Ok(())
    } else {
        Err(BoardArrayCreateValidationError::ArrayDimensionMin {
            axis,
            value,
            min: 0.0,
        }
        .into())
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

fn ensure_tooling_hole_layer_name(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
) -> String {
    if let Some(layer) = generated_geometry.layers.iter().find(|layer| {
        layer.layer_function == LayerFunction::Drill
            && layer.name.starts_with(TOOLING_HOLE_LAYER_BASE_NAME)
    }) {
        return layer.name.clone();
    }

    let layer_name = reserve_unique_name(used_layer_names, TOOLING_HOLE_LAYER_BASE_NAME);
    generated_geometry.add_layer(GeneratedLayer::new(
        layer_name.clone(),
        LayerFunction::Drill,
        Some(Side::All),
        Some(Polarity::Positive),
    ));
    layer_name
}

fn top_copper_layer_name(ipc: &Ipc2581, ecad: &ipc2581::types::Ecad) -> Option<String> {
    ecad.cad_data
        .layers
        .iter()
        .find(|layer| {
            layer.side == Some(Side::Top) && crate::layers::is_copper(layer.layer_function)
        })
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

fn reserve_unique_name(used_names: &mut HashSet<String>, base: &str) -> String {
    let name = unique_name(used_names, base);
    used_names.insert(name.clone());
    name
}

mod tooling;
mod vcut;
mod xml;

#[cfg(test)]
mod tests;

use tooling::*;
use vcut::*;
use xml::*;
