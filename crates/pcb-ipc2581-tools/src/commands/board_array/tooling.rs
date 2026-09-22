//! Edge-rail tooling holes and fiducials.

use super::*;

type FiducialPoints = [(f64, f64); 4];

/// An opposite pair of rails, or of board-cell margins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RailPair {
    TopBottom,
    LeftRight,
}

/// Pick the rail pair for board array tooling.
///
/// Eligibility is checked per pair: the board span along the rail direction
/// must satisfy the single- or multi-board minimum. Prefer the shorter pair of
/// rails (left/right for landscape arrays), then fall back to the other
/// eligible pair.
pub(super) fn board_array_tooling_rails(grid: &ArrayGrid) -> Option<RailPair> {
    let eligible = |&rails: &RailPair| {
        let (span_count, board_span) = match rails {
            RailPair::TopBottom => (grid.columns, grid.board_width_mm),
            RailPair::LeftRight => (grid.rows, grid.board_height_mm),
        };
        // Each side's pair sits over its own outer board, and the array is
        // scored along both edges of that board: the deepest fiducial has to
        // stop its mask opening short of the far one.
        let min_span = if span_count == 1 {
            SINGLE_BOARD_TOOLING_MIN_SPAN_MM
        } else {
            SECONDARY_FIDUCIAL_SPAN_INSET_MM + FIDUCIAL_MASK_OPENING_DIAMETER_MM / 2.0
        };
        board_span + EPSILON >= min_span
    };
    if grid.array_width_mm > grid.array_height_mm {
        [RailPair::LeftRight, RailPair::TopBottom]
    } else {
        [RailPair::TopBottom, RailPair::LeftRight]
    }
    .iter()
    .copied()
    .find(eligible)
}

/// Place board array tooling on the rail pair chosen by
/// [`board_array_tooling_rails`].
///
/// The generated board array uses a rectangular profile with the lower-left
/// array corner at (0, 0). Fiducials and tooling holes live in the outer 5 mm
/// rail band even when the configured edge rail is wider. They are placed over
/// board columns for top/bottom rails and over board rows for left/right rails,
/// so removing side rails and gaps keeps the rail tooling attached to board
/// material.
///
/// Span rules:
/// - one board in the tooling axis requires at least 28 mm board span: 12 mm
///   deepest fiducial inset from each side plus 4 mm center spacing;
/// - multiple boards in the tooling axis require at least 13 mm board span:
///   each side's pair sits over a different outer board, and the 12 mm deepest
///   fiducial keeps its 1 mm mask opening off the score line along that
///   board's far edge;
/// - primary rail centers use 2.5 mm tooling, 8 mm top-fiducial, and 9 mm
///   bottom-fiducial span insets;
/// - secondary rail centers use 6.5 mm tooling, 12 mm top-fiducial, and 11 mm
///   bottom-fiducial span insets.
///
/// Rail-depth rules:
/// - tooling hole centers are 2.5 mm from the array edge;
/// - fiducial centers are 3.85 mm from the array edge.
pub(super) fn add_board_array_tooling(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    tooling_hole_layer_name: &str,
    grid: &ArrayGrid,
) -> Result<()> {
    let Some(rails) = board_array_tooling_rails(grid) else {
        return Ok(());
    };
    let points = |rail_depth_mm, primary_span_inset_mm, secondary_span_inset_mm| {
        board_array_tooling_points(
            grid,
            rails,
            rail_depth_mm,
            primary_span_inset_mm,
            secondary_span_inset_mm,
        )
    };

    add_two_sided_fiducials(
        generated_geometry,
        ipc,
        ecad,
        GeneratedFeatureScope::Array,
        IpcFiducialKind::Global,
        points(
            FIDUCIAL_EDGE_OFFSET_MM,
            PRIMARY_FIDUCIAL_SPAN_INSET_MM,
            SECONDARY_FIDUCIAL_SPAN_INSET_MM,
        ),
        points(
            FIDUCIAL_EDGE_OFFSET_MM,
            BOTTOM_PRIMARY_FIDUCIAL_SPAN_INSET_MM,
            BOTTOM_SECONDARY_FIDUCIAL_SPAN_INSET_MM,
        ),
    )
    .context("cannot add global board-array fiducials")?;
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        tooling_hole_layer_name,
        round_nonplated_hole_features(
            points(
                TOOLING_HOLE_EDGE_OFFSET_MM,
                PRIMARY_TOOLING_HOLE_SPAN_INSET_MM,
                SECONDARY_TOOLING_HOLE_SPAN_INSET_MM,
            ),
            TOOLING_HOLE_DIAMETER_MM,
        ),
    );
    Ok(())
}

pub(super) fn add_board_array_corner_tooling(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    tooling_hole_layer_name: &str,
    grid: &ArrayGrid,
) {
    let inset = ARRAY_CORNER_TOOLING_HOLE_INSET_MM;
    let (width, height) = (grid.array_width_mm, grid.array_height_mm);
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        tooling_hole_layer_name,
        round_nonplated_hole_features(
            [
                (inset, inset),
                (width - inset, inset),
                (width - inset, height - inset),
                (inset, height - inset),
            ],
            CORNER_TOOLING_HOLE_DIAMETER_MM,
        ),
    );
}

pub(super) fn add_board_cell_fiducials(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    grid: &ArrayGrid,
    board_margin: BoardMarginMm,
) -> Result<()> {
    let Some(fiducials) = board_cell_fiducials(grid, board_margin) else {
        return Ok(());
    };

    add_two_sided_fiducials(
        generated_geometry,
        ipc,
        ecad,
        GeneratedFeatureScope::BoardCell,
        IpcFiducialKind::Local,
        fiducials,
        fiducials,
    )
    .context("cannot add local board-cell fiducials")
}

fn add_two_sided_fiducials(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    scope: GeneratedFeatureScope,
    kind: IpcFiducialKind,
    top_points: FiducialPoints,
    bottom_points: FiducialPoints,
) -> Result<()> {
    let layers = crate::layers::two_sided_surface_layers(ecad)?;
    for (layer, diameter_mm, points) in [
        (layers.top_copper, FIDUCIAL_COPPER_DIAMETER_MM, top_points),
        (
            layers.top_soldermask,
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
            top_points,
        ),
        (
            layers.bottom_copper,
            FIDUCIAL_COPPER_DIAMETER_MM,
            bottom_points,
        ),
        (
            layers.bottom_soldermask,
            FIDUCIAL_MASK_OPENING_DIAMETER_MM,
            bottom_points,
        ),
    ] {
        generated_geometry.add_layer_feature(
            scope,
            ipc.resolve(layer),
            round_fiducial_features(kind, points, diameter_mm),
        );
    }
    Ok(())
}

fn board_array_tooling_points(
    grid: &ArrayGrid,
    rails: RailPair,
    rail_depth_mm: f64,
    primary_span_inset_mm: f64,
    secondary_span_inset_mm: f64,
) -> FiducialPoints {
    match rails {
        RailPair::TopBottom => {
            let left_edge = grid.margin_x_mm;
            let right_edge = grid.margin_x_mm
                + (grid.columns - 1) as f64 * grid.pitch_x_mm
                + grid.board_width_mm;
            let top_y = grid.array_height_mm - rail_depth_mm;
            let bottom_y = rail_depth_mm;

            [
                (left_edge + primary_span_inset_mm, top_y),
                (right_edge - primary_span_inset_mm, top_y),
                (left_edge + secondary_span_inset_mm, bottom_y),
                (right_edge - secondary_span_inset_mm, bottom_y),
            ]
        }
        RailPair::LeftRight => {
            let bottom_edge = grid.margin_y_mm;
            let top_edge =
                grid.margin_y_mm + (grid.rows - 1) as f64 * grid.pitch_y_mm + grid.board_height_mm;
            let left_x = rail_depth_mm;
            let right_x = grid.array_width_mm - rail_depth_mm;

            [
                (left_x, top_edge - primary_span_inset_mm),
                (left_x, bottom_edge + primary_span_inset_mm),
                (right_x, top_edge - secondary_span_inset_mm),
                (right_x, bottom_edge + secondary_span_inset_mm),
            ]
        }
    }
}

/// Place four board fiducials in each board cell's margin.
///
/// Eligibility is checked per orientation: top/bottom needs enough horizontal
/// board span and top/bottom margins; left/right needs enough vertical board
/// span and left/right margins. Prefer the board's longer dimension, then fall
/// back to the other eligible orientation. Offsets along the board span are
/// measured from the board bbox; offsets into the margin are measured from the
/// board-cell outer edge. The primary side is top/left and uses a 3 mm span
/// inset; the opposite side uses 7 mm.
fn board_cell_fiducials(grid: &ArrayGrid, margin: BoardMarginMm) -> Option<FiducialPoints> {
    let (width, height) = (grid.board_width_mm, grid.board_height_mm);
    let eligible = |&margins: &RailPair| {
        let (span, near, far) = match margins {
            RailPair::TopBottom => (width, margin.top, margin.bottom),
            RailPair::LeftRight => (height, margin.left, margin.right),
        };
        span + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_SPAN_MM
            && near + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM
            && far + EPSILON >= MIN_BOARD_CELL_FIDUCIAL_MARGIN_MM
    };
    let margins = if width >= height {
        [RailPair::TopBottom, RailPair::LeftRight]
    } else {
        [RailPair::LeftRight, RailPair::TopBottom]
    }
    .iter()
    .copied()
    .find(eligible)?;

    let board_left = margin.left;
    let board_right = margin.left + width;
    let board_bottom = margin.bottom;
    let board_top = margin.bottom + height;
    let cell_right = board_right + margin.right;
    let cell_top = board_top + margin.top;
    let inset = BOARD_CELL_FIDUCIAL_MARGIN_INSET_MM;
    let (primary, secondary) = (
        PRIMARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
        SECONDARY_BOARD_CELL_FIDUCIAL_SPAN_INSET_MM,
    );

    Some(match margins {
        RailPair::TopBottom => [
            (board_left + primary, cell_top - inset),
            (board_right - primary, cell_top - inset),
            (board_left + secondary, inset),
            (board_right - secondary, inset),
        ],
        RailPair::LeftRight => [
            (inset, board_top - primary),
            (inset, board_bottom + primary),
            (cell_right - inset, board_top - secondary),
            (cell_right - inset, board_bottom + secondary),
        ],
    })
}
