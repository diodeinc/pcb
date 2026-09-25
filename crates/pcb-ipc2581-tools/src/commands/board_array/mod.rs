use pcb_ir::geom::{ContourSet, FillRule, Resolution};
use std::collections::HashSet;
#[cfg(feature = "cli")]
use std::path::Path;

pub mod eligibility;
pub mod mouse_bite;
pub mod placement;

use super::PanelCreation;
use super::board_array_auto::{
    AutoSheetSize, TargetSizeMm, auto_board_array_plan, auto_board_array_plan_for_sheet,
};
use crate::generated::GeneratedLayerFeature;
use crate::geometry;
use crate::ipc2581::Ipc2581;
#[cfg(feature = "cli")]
use crate::utils::file as file_utils;
use crate::utils::format::fmt_num;
use anyhow::{Context, Result, bail, ensure};
use ipc2581::types::{
    Units,
    ecad::{
        Fiducial, FiducialKind as IpcFiducialKind, FiducialShape, Hole, LayerFunction,
        PlatingStatus, Polarity, SetFeature, Side, Stroke, StrokePath,
    },
    primitives::{
        Circle, Line, LineDesc, LineDescGroup, LineEnd, LineProperty, Point as IpcPoint, PolyStep,
        PolyStepCurve, PolyStepSegment, Polygon, StandardPrimitive, Styled,
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
const TOOLING_HOLE_LAYER_BASE_NAME: &str = "Board_Array_Drill";
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
const BOTTOM_PRIMARY_FIDUCIAL_SPAN_INSET_MM: f64 = 9.0;
const SECONDARY_TOOLING_HOLE_SPAN_INSET_MM: f64 = 6.5;
const BOTTOM_SECONDARY_FIDUCIAL_SPAN_INSET_MM: f64 = 11.0;
const SECONDARY_FIDUCIAL_SPAN_INSET_MM: f64 = 12.0;
const SINGLE_BOARD_TOOLING_MIN_SPAN_MM: f64 = 28.0;
const MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM: f64 = 5.0;
const MIN_BOARD_CELL_FIDUCIAL_SPAN_MM: f64 = 17.0;
const BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM: f64 = 2.0;
const PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = 3.0;
const SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM: f64 = 7.0;

// Board-cell fiducials sit at the outer edge of the margin a routed slot is
// cut from; the smallest margin that carries them keeps the slot clear of
// their mask openings.
const _: () = assert!(
    MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM - placement::PRESET.routing_gap_mm
        >= BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM + FIDUCIAL_MASK_OPENING_DIAMETER_MM / 2.0
);

#[derive(Debug, Clone)]
pub struct BoardArrayCreateOptions {
    pub columns: u32,
    pub rows: u32,
    pub board_margin_mm: BoardMarginMm,
    pub edge_rail_mm: BoardMarginMm,
}

/// How boards separate from the array: scored lines, or routed slots bridged
/// by perforated tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separation {
    VScore,
    MouseBite,
}

impl Separation {
    fn as_str(self) -> &'static str {
        match self {
            Self::VScore => "v-score",
            Self::MouseBite => "mouse-bite",
        }
    }
}

pub type BoardMarginMm = super::EdgeInsetsMm;

#[derive(Debug, Clone)]
struct BoardArraySpec {
    array_name: String,
    board_cell_name: String,
    board_name: String,
    vcut_spec_name: Option<String>,
    /// Routed material in the array profile; empty for scored arrays.
    profile_cutouts: Vec<Polygon>,
    separation: Separation,
    tabs_per_board: usize,
    /// What the panel falls short of, reported beside it.
    warnings: Vec<String>,
    board_outline_layer_names: Vec<String>,
    content_step_refs: Vec<String>,
    content_layer_refs: Vec<String>,
    grid: ArrayGrid,
    board_repeat_x_mm: f64,
    board_repeat_y_mm: f64,
    board_margin_mm: BoardMarginMm,
    edge_rail_mm: BoardMarginMm,
    panelization: BoardArrayPanelizationMetadata,
    generated_geometry: BoardArrayGeneratedGeometry,
    units: Units,
}

/// The array's grid of boards, in array coordinates.
#[derive(Debug, Clone, Copy)]
struct ArrayGrid {
    columns: u32,
    rows: u32,
    board_width_mm: f64,
    board_height_mm: f64,
    /// Lower-left corner of the first board.
    margin_x_mm: f64,
    margin_y_mm: f64,
    pitch_x_mm: f64,
    pitch_y_mm: f64,
    array_width_mm: f64,
    array_height_mm: f64,
}

#[derive(Debug, Clone, Copy)]
struct BoardArrayPanelizationMetadata {
    mode: BoardArrayPanelizationMode,
    /// The sheet an automatic array fills, with its orientation.
    sheet: Option<(AutoSheetSize, TargetSizeMm)>,
}

impl BoardArrayPanelizationMetadata {
    const MANUAL: Self = Self {
        mode: BoardArrayPanelizationMode::Manual,
        sheet: None,
    };
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
    layer_features: Vec<(GeneratedFeatureScope, GeneratedLayerFeature)>,
    user_entries: Vec<crate::copper_balance::BalanceVoidTemplate>,
}

impl BoardArrayGeneratedGeometry {
    /// One positive `Set` of `features` on a layer, returned so a caller can
    /// attach spec refs.
    fn add_layer_feature(
        &mut self,
        scope: GeneratedFeatureScope,
        layer_name: &str,
        features: Vec<SetFeature>,
    ) -> &mut GeneratedLayerFeature {
        self.layer_features.push((
            scope,
            GeneratedLayerFeature {
                layer_name: layer_name.to_string(),
                polarity: Polarity::Positive,
                copper_balance: None,
                spec_refs: Vec::new(),
                features,
                void_set: None,
            },
        ));
        &mut self.layer_features.last_mut().expect("just pushed").1
    }
}

/// A generated positive layer.
#[derive(Debug, Clone)]
struct GeneratedLayer {
    name: String,
    layer_function: LayerFunction,
    side: Side,
    /// The layers a drill layer's holes run between, from and to.
    span: Option<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeneratedFeatureScope {
    Array,
    BoardCell,
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &Path,
    output: &Path,
    options: &BoardArrayCreateOptions,
    balance_copper: bool,
    separation: Separation,
    resolution: Resolution,
) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    create_board_array(&content, options, balance_copper, separation, resolution)?
        .write(output, "board array")
}

#[cfg(feature = "cli")]
pub fn execute_auto(
    input: &Path,
    output: &Path,
    sheet: Option<AutoSheetSize>,
    balance_copper: bool,
    separation: Separation,
    resolution: Resolution,
) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    create_auto_board_array(&content, sheet, balance_copper, separation, resolution)?
        .write(output, "board array")
}

/// Create a manually configured board array and return its balance accounting.
pub fn create_board_array(
    xml: &str,
    options: &BoardArrayCreateOptions,
    balance_copper: bool,
    separation: Separation,
    resolution: Resolution,
) -> Result<PanelCreation> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let spec = build_board_array_spec(
        &ipc,
        primary_board_layout(&ipc)?,
        options,
        BoardArrayPanelizationMetadata::MANUAL,
        separation,
        resolution,
    )?;
    write_board_array_creation(xml, spec, balance_copper, resolution)
}

/// Create an automatically sized board array and return its balance accounting.
pub fn create_auto_board_array(
    xml: &str,
    sheet: Option<AutoSheetSize>,
    balance_copper: bool,
    separation: Separation,
    resolution: Resolution,
) -> Result<PanelCreation> {
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    let board = primary_board_layout(&ipc)?;
    let (options, panelization) = auto_board_array_options(&ipc, board, sheet, resolution)?;
    let spec = build_board_array_spec(&ipc, board, &options, panelization, separation, resolution)?;
    write_board_array_creation(xml, spec, balance_copper, resolution)
}

fn auto_board_array_options(
    ipc: &Ipc2581,
    board: PrimaryBoardLayout,
    sheet: Option<AutoSheetSize>,
    resolution: Resolution,
) -> Result<(BoardArrayCreateOptions, BoardArrayPanelizationMetadata)> {
    let board_margin = auto_board_margin(ipc, board.bbox, resolution)?;
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
            BoardArrayCreateOptions {
                columns: plan.columns,
                rows: plan.rows,
                board_margin_mm: plan.board_margin_mm,
                edge_rail_mm: plan.edge_rail_mm,
            },
            BoardArrayPanelizationMetadata {
                mode,
                sheet: Some((plan.sheet, plan.target)),
            },
        ),
        None => (
            minimum_auto_options(1, 1, board_margin),
            BoardArrayPanelizationMetadata {
                mode: BoardArrayPanelizationMode::AutoMinimumPanel,
                sheet: None,
            },
        ),
    })
}

/// The tightest array automatic panelization produces around boards with
/// `board_margin_mm`: the narrowest edge rails.
fn minimum_auto_options(
    columns: u32,
    rows: u32,
    board_margin_mm: BoardMarginMm,
) -> BoardArrayCreateOptions {
    BoardArrayCreateOptions {
        columns,
        rows,
        board_margin_mm,
        edge_rail_mm: BoardMarginMm::all(MIN_EDGE_RAIL_WIDTH_MM),
    }
}

/// The narrowest rail a tab of this array lands on once a slot of
/// `routing_gap_mm` is routed around every board: the strip between two
/// boards, or between an outer board and the array edge.
fn narrowest_rail_mm(options: &BoardArrayCreateOptions, routing_gap_mm: f64) -> f64 {
    let (margin, rail) = (options.board_margin_mm, options.edge_rail_mm);
    let outer = [
        rail.top + margin.top,
        rail.right + margin.right,
        rail.bottom + margin.bottom,
        rail.left + margin.left,
    ]
    .map(|width| width - routing_gap_mm);
    let between = [
        (options.columns > 1).then(|| margin.horizontal_sum()),
        (options.rows > 1).then(|| margin.vertical_sum()),
    ]
    .map(|gap| gap.map(|gap| gap - 2.0 * routing_gap_mm));
    outer
        .into_iter()
        .chain(between.into_iter().flatten())
        .fold(f64::INFINITY, f64::min)
}

fn auto_board_margin(
    ipc: &Ipc2581,
    board_bbox: BBox,
    resolution: Resolution,
) -> Result<BoardMarginMm> {
    let safe_bbox = board_bbox.union(board_courtyard_bbox(ipc, resolution)?);
    Ok(BoardMarginMm {
        top: (safe_bbox.max.y - board_bbox.max.y).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        right: (safe_bbox.max.x - board_bbox.max.x).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        bottom: (board_bbox.min.y - safe_bbox.min.y).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
        left: (board_bbox.min.x - safe_bbox.min.x).max(0.0) + MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM,
    })
}

fn board_courtyard_bbox(ipc: &Ipc2581, resolution: Resolution) -> Result<BBox> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    // One import serves every courtyard layer.
    let imported = pcb_ir::import::ipc2581::import_design(ipc, resolution)?;
    let mut bbox = BBox::empty();

    for layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Courtyard)
    {
        let layer_name = ipc.resolve(layer.name);
        let doc = imported
            .layer_id(layer_name)
            .with_context(|| format!("IPC-2581 layer '{layer_name}' was not found"))
            .and_then(|id| {
                imported.materialize_layer(id, pcb_ir::dialects::ipc::ArtworkScope::Board)
            })
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

/// The source `doc` indexes with the array spliced in: one parse serves the
/// board-array patch and the history append of every pass, and all edits of a
/// pass splice at once.
fn board_array_edited_xml(doc: &ipc2581::edit::Doc<'_>, spec: &BoardArraySpec) -> Result<String> {
    let mut edits = board_array_edits(doc, spec)?;
    edits.extend(crate::utils::history::file_revision_edits(
        doc,
        "Created board array",
    )?);
    Ok(doc.apply(edits)?)
}

fn finished_board_array_xml(doc: &ipc2581::edit::Doc<'_>, spec: &BoardArraySpec) -> Result<String> {
    let xml = board_array_edited_xml(doc, spec)?;
    let xml = crate::utils::format::reformat_xml(&xml)?;

    Ipc2581::parse(&xml).context("Generated IPC-2581 board array XML did not parse")?;
    Ok(xml)
}

fn write_board_array_creation(
    xml: &str,
    mut spec: BoardArraySpec,
    balance_copper: bool,
    resolution: Resolution,
) -> Result<PanelCreation> {
    let doc = ipc2581::edit::Doc::parse(xml)?;
    let mut copper_balance = None;
    if balance_copper {
        // The provisional array only feeds safe-region discovery; parsing it
        // below already validates it, so skip the cosmetic reformat pass.
        let provisional_xml = board_array_edited_xml(&doc, &spec)?;
        let provisional = Ipc2581::parse(&provisional_xml)
            .context("Failed to parse provisional IPC-2581 board array")?;
        let balance = balance::generate_automatic_board_array_copper_balance(
            &provisional,
            resolution.tolerance_mm,
        )?;
        copper_balance = Some(balance.report());
        let (templates, features) = balance::generated_features(balance);
        spec.generated_geometry.user_entries = templates;
        spec.generated_geometry.layer_features.extend(
            features
                .into_iter()
                .map(|feature| (GeneratedFeatureScope::Array, feature)),
        );
    }
    Ok(PanelCreation {
        xml: finished_board_array_xml(&doc, &spec)?,
        copper_balance,
        warnings: spec.warnings,
    })
}

#[derive(Debug, Clone, Copy)]
struct PrimaryBoardLayout {
    source_step_ref: ipc2581::Symbol,
    bbox: pcb_ir::geom::BBox,
}

fn primary_board_layout(ipc: &Ipc2581) -> Result<PrimaryBoardLayout> {
    let layout = geometry::extract_layout(ipc)?;
    let (_, root) = root_step(&layout).context("IPC-2581 board step has no layout root")?;
    match root.kind {
        LayoutStepKind::Board => {}
        LayoutStepKind::Panel => bail!(
            "primary IPC-2581 step is already a board array; board array create expects a board step"
        ),
        _ => bail!("primary IPC-2581 step is not a board step"),
    }
    if root.bbox.is_empty() {
        bail!(
            "board step '{}' has no Profile: the layout has no closed board outline on its \
             edge-cuts layer, so there is nothing to derive the board's shape from; draw the \
             outline and export again",
            ipc.resolve(root.source_step_ref)
        );
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
    root: PrimaryBoardLayout,
    options: &BoardArrayCreateOptions,
    panelization: BoardArrayPanelizationMetadata,
    separation: Separation,
    resolution: Resolution,
) -> Result<BoardArraySpec> {
    let mode = panelization.mode;
    validate_options(options, mode, separation)?;

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let board_width = root.bbox.width();
    let board_height = root.bbox.height();

    let columns = options.columns;
    let rows = options.rows;
    let board_margin = options.board_margin_mm;
    let edge_rail = options.edge_rail_mm;
    let pitch_x = board_width + board_margin.horizontal_sum();
    let pitch_y = board_height + board_margin.vertical_sum();
    let array_width = columns as f64 * board_width
        + columns as f64 * board_margin.horizontal_sum()
        + edge_rail.left
        + edge_rail.right;
    let array_height = rows as f64 * board_height
        + rows as f64 * board_margin.vertical_sum()
        + edge_rail.bottom
        + edge_rail.top;
    validate_array_dimensions(array_width, array_height, mode)?;
    let grid = ArrayGrid {
        columns,
        rows,
        board_width_mm: board_width,
        board_height_mm: board_height,
        margin_x_mm: edge_rail.left + board_margin.left,
        margin_y_mm: edge_rail.bottom + board_margin.bottom,
        pitch_x_mm: pitch_x,
        pitch_y_mm: pitch_y,
        array_width_mm: array_width,
        array_height_mm: array_height,
    };
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
    let mut used_layer_names = ecad
        .cad_data
        .layers
        .iter()
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect::<HashSet<_>>();
    let mut generated_geometry = BoardArrayGeneratedGeometry::default();
    let vcut_spec_name = (separation == Separation::VScore)
        .then(|| unique_name(&existing_spec_names, VCUT_SPEC_BASE_NAME));
    if let Some(vcut_spec_name) = &vcut_spec_name {
        add_vcut_lines(
            &mut generated_geometry,
            &mut used_layer_names,
            vcut_spec_name,
            &grid,
        );
    }
    let tooling_hole_layer =
        add_tooling_hole_layer(&mut generated_geometry, &mut used_layer_names, ipc, ecad);
    let (profile_cutouts, tabs_per_board, warnings) = match separation {
        Separation::VScore => (Vec::new(), 0, Vec::new()),
        Separation::MouseBite => {
            let preset = &placement::PRESET;
            let placement = placement::place(
                ipc,
                preset,
                narrowest_rail_mm(options, preset.routing_gap_mm),
                resolution,
            )?;
            let stock = array_stock(array_width, array_height, resolution)?;
            let offsets = (0..rows)
                .flat_map(|row| {
                    (0..columns).map(move |column| {
                        pcb_ir::geom::Point::new(
                            edge_rail.left + board_repeat_x + f64::from(column) * pitch_x,
                            edge_rail.bottom + board_repeat_y + f64::from(row) * pitch_y,
                        )
                    })
                })
                .collect::<Vec<_>>();
            let cell = BBox::new(
                root.bbox.min - Point::new(board_margin.left, board_margin.bottom),
                root.bbox.max + Point::new(board_margin.right, board_margin.top),
            );
            let tabs =
                mouse_bite::generate(&placement, cell, &stock, &offsets, preset, resolution)?;
            generated_geometry.add_layer_feature(
                GeneratedFeatureScope::Array,
                &tooling_hole_layer,
                round_nonplated_hole_features(
                    tabs.holes.iter().map(|hole| (hole.center.x, hole.center.y)),
                    pcb_ir::geom::mouse_bite::SparkFunShallow::HOLE_DIAMETER_MM,
                ),
            );
            (
                tabs.cutouts
                    .iter()
                    .map(mouse_bite::cutout_polygon)
                    .collect::<Result<Vec<_>>>()?,
                tabs.per_board,
                tabs.warning.into_iter().collect(),
            )
        }
    };
    add_board_array_corner_tooling(&mut generated_geometry, &tooling_hole_layer, &grid);
    if mode != BoardArrayPanelizationMode::Manual && board_array_tooling_rails(&grid).is_none() {
        bail!(
            "auto board array cannot fit rail fiducials and tooling holes on either rail pair; \
             panelize manually to skip rail tooling"
        );
    }
    add_board_array_tooling(
        &mut generated_geometry,
        ipc,
        ecad,
        &tooling_hole_layer,
        &grid,
    )?;
    add_board_cell_fiducials(&mut generated_geometry, ipc, ecad, &grid, board_margin)?;
    let board_outline_layer_names = board_outline_layer_names(ipc, ecad);
    let content_step_refs = content_step_refs(ipc, &array_name, &board_cell_name, &board_name);
    let content_layer_refs =
        content_layer_refs(ipc, &generated_geometry, &board_outline_layer_names);

    Ok(BoardArraySpec {
        array_name,
        board_cell_name,
        board_name,
        vcut_spec_name,
        profile_cutouts,
        separation,
        tabs_per_board,
        warnings,
        board_outline_layer_names,
        content_step_refs,
        content_layer_refs,
        grid,
        board_repeat_x_mm: board_repeat_x,
        board_repeat_y_mm: board_repeat_y,
        board_margin_mm: board_margin,
        edge_rail_mm: edge_rail,
        panelization,
        generated_geometry,
        units: ecad.cad_header.units,
    })
}

/// The array's stock: its rounded outline with the corner at the origin.
fn array_stock(width_mm: f64, height_mm: f64, resolution: Resolution) -> Result<ContourSet> {
    let outline = pcb_ir::geom::shapes::rounded_rect(
        width_mm,
        height_mm,
        ARRAY_CORNER_RADIUS_MM,
        pcb_ir::geom::shapes::ALL_CORNERS,
    )
    .context("invalid array dimensions")?;
    let centered = ContourSet::from_contours(&[outline], FillRule::EvenOdd, resolution.strict())?;
    Ok(pcb_ir::geom::attachment::transform_region(
        &centered,
        pcb_ir::geom::Affine2::translation(pcb_ir::geom::Point::new(
            width_mm / 2.0,
            height_mm / 2.0,
        )),
    )?)
}

fn validate_options(
    options: &BoardArrayCreateOptions,
    mode: BoardArrayPanelizationMode,
    separation: Separation,
) -> Result<()> {
    validate_u32_range("columns", options.columns, 1, 10)?;
    validate_u32_range("rows", options.rows, 1, 10)?;
    for (side, value) in options.board_margin_mm.sides() {
        validate_mm_min(&format!("board margin {side}"), value, 0.0)?;
    }
    for (side, value) in options.edge_rail_mm.sides() {
        let field = format!("edge rail {side}");
        match mode {
            BoardArrayPanelizationMode::Manual => ensure!(
                (MIN_EDGE_RAIL_WIDTH_MM..=MAX_MANUAL_EDGE_RAIL_WIDTH_MM).contains(&value),
                "{field} must be between {} and {} mm; got {} mm",
                fmt_num(MIN_EDGE_RAIL_WIDTH_MM),
                fmt_num(MAX_MANUAL_EDGE_RAIL_WIDTH_MM),
                fmt_num(value)
            ),
            // Automatic rails take up whatever the sheet leaves over.
            _ => validate_mm_min(&field, value, MIN_EDGE_RAIL_WIDTH_MM)?,
        }
    }
    match separation {
        Separation::VScore => {
            for (field, count, gap) in [
                (
                    "horizontal board clearance",
                    options.columns,
                    options.board_margin_mm.horizontal_sum(),
                ),
                (
                    "vertical board clearance",
                    options.rows,
                    options.board_margin_mm.vertical_sum(),
                ),
            ] {
                ensure!(
                    count == 1 || gap.abs() <= EPSILON || gap + EPSILON >= MIN_VCUT_CLEARANCE_MM,
                    "{field} must be 0 mm or at least {} mm; got {} mm",
                    fmt_num(MIN_VCUT_CLEARANCE_MM),
                    fmt_num(gap)
                );
            }
        }
        // The slot and the frame a tab lands on are cut from the board's own
        // margin, so a routed board never reaches a neighbour's material or
        // the tooling in the edge rails.
        Separation::MouseBite => {
            let (slot, landing) = (
                placement::PRESET.routing_gap_mm,
                placement::PRESET.frame_landing_mm,
            );
            for (side, value) in options.board_margin_mm.sides() {
                ensure!(
                    value + EPSILON >= slot + landing,
                    "board margin {side} must be at least {} mm for mouse-bite separation: it \
                     holds the {} mm routed slot and the {} mm of frame each tab lands on; got {} mm",
                    fmt_num(slot + landing),
                    fmt_num(slot),
                    fmt_num(landing),
                    fmt_num(value)
                );
            }
        }
    }
    Ok(())
}

fn validate_u32_range(field: &str, value: u32, min: u32, max: u32) -> Result<()> {
    ensure!(
        (min..=max).contains(&value),
        "{field} must be between {min} and {max}; got {value}"
    );
    Ok(())
}

fn validate_mm_min(field: &str, value: f64, min: f64) -> Result<()> {
    ensure!(
        value.is_finite() && value + EPSILON >= min,
        "{field} must be at least {} mm; got {} mm",
        fmt_num(min),
        fmt_num(value)
    );
    Ok(())
}

fn validate_array_dimensions(
    width_mm: f64,
    height_mm: f64,
    mode: BoardArrayPanelizationMode,
) -> Result<()> {
    for (field, value) in [("array width", width_mm), ("array height", height_mm)] {
        if mode == BoardArrayPanelizationMode::AutoMinimumPanel {
            ensure!(
                value.is_finite() && value > EPSILON,
                "{field} must be at least 0 mm; got {} mm",
                fmt_num(value)
            );
            continue;
        }
        validate_mm_min(field, value, MIN_BOARD_ARRAY_DIMENSION_MM)?;
        ensure!(
            value <= MAX_BOARD_ARRAY_DIMENSION_MM + EPSILON,
            "{field} must be at most {} mm; got {} mm",
            fmt_num(MAX_BOARD_ARRAY_DIMENSION_MM),
            fmt_num(value)
        );
    }
    Ok(())
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
    let mut seen = HashSet::new();
    let source = ipc
        .content()
        .step_refs
        .iter()
        .map(|name| ipc.resolve(*name));
    [array_name, board_cell_name]
        .into_iter()
        .chain(source)
        .chain([board_name])
        .filter(|name| seen.insert(*name))
        .map(str::to_string)
        .collect()
}

/// The source's layer refs less the removed layers, then every layer the
/// generated geometry declares or draws on.
fn content_layer_refs(
    ipc: &Ipc2581,
    generated_geometry: &BoardArrayGeneratedGeometry,
    removed_layer_names: &[String],
) -> Vec<String> {
    let mut seen = HashSet::new();
    let source = ipc
        .content()
        .layer_refs
        .iter()
        .map(|name| ipc.resolve(*name));
    let layers = generated_geometry.layers.iter().map(|layer| &*layer.name);
    let drawn = generated_geometry
        .layer_features
        .iter()
        .map(|(_, layer_feature)| &*layer_feature.layer_name);
    source
        .filter(|name| !removed_layer_names.iter().any(|removed| removed == name))
        .chain(layers)
        .chain(drawn)
        .filter(|name| seen.insert(*name))
        .map(str::to_string)
        .collect()
}

fn board_outline_layer_names(ipc: &Ipc2581, ecad: &ipc2581::types::Ecad) -> Vec<String> {
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::BoardOutline)
        .map(|layer| ipc.resolve(layer.name).to_string())
        .collect()
}

/// The drill layer every generated hole goes on. Its holes run through the
/// whole board, declared the way the source's own drill layers declare it:
/// as a span between the outer copper layers.
fn add_tooling_hole_layer(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
) -> String {
    let copper = crate::layers::copper_layers(ecad);
    let outer = |side| {
        copper
            .iter()
            .find(|layer| layer.side == side)
            .map(|layer| ipc.resolve(layer.name).to_string())
    };
    let name = reserve_unique_name(used_layer_names, TOOLING_HOLE_LAYER_BASE_NAME);
    generated_geometry.layers.push(GeneratedLayer {
        name: name.clone(),
        layer_function: LayerFunction::Drill,
        side: Side::All,
        span: outer(pcb_ir::dialects::Side::Top).zip(outer(pcb_ir::dialects::Side::Bottom)),
    });
    name
}

fn reserve_unique_name(used_names: &mut HashSet<String>, base: &str) -> String {
    let name = unique_name(used_names, base);
    used_names.insert(name.clone());
    name
}

pub mod balance;
mod tooling;
mod vcut;
pub(crate) mod xml;

#[cfg(test)]
mod tests;

use tooling::*;
use vcut::*;
use xml::*;
