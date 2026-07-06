//! Edge-rail tooling holes and fiducials.

use super::*;

pub(super) struct BoardArrayToolingSpec {
    pub(super) orientation: BoardArrayToolingOrientation,
    pub(super) columns: u32,
    pub(super) rows: u32,
    pub(super) board_width_mm: f64,
    pub(super) board_height_mm: f64,
    pub(super) margin_x_mm: f64,
    pub(super) margin_y_mm: f64,
    pub(super) pitch_x_mm: f64,
    pub(super) pitch_y_mm: f64,
    pub(super) array_width_mm: f64,
    pub(super) array_height_mm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardArrayToolingOrientation {
    TopBottom,
    LeftRight,
}

pub(super) fn board_array_tooling_orientation(
    array_width_mm: f64,
    array_height_mm: f64,
) -> BoardArrayToolingOrientation {
    if array_width_mm > array_height_mm {
        BoardArrayToolingOrientation::LeftRight
    } else {
        BoardArrayToolingOrientation::TopBottom
    }
}

pub(super) fn add_board_array_tooling(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    ipc: &Ipc2581,
    ecad: &ipc2581::types::Ecad,
    used_layer_names: &mut HashSet<String>,
    spec: BoardArrayToolingSpec,
) {
    let (span_count, board_span) = match spec.orientation {
        BoardArrayToolingOrientation::TopBottom => (spec.columns, spec.board_width_mm),
        BoardArrayToolingOrientation::LeftRight => (spec.rows, spec.board_height_mm),
    };
    let min_span = if span_count == 1 {
        SINGLE_BOARD_TOOLING_MIN_SPAN_MM
    } else {
        MULTI_BOARD_TOOLING_MIN_SPAN_MM
    };
    if board_span + EPSILON < min_span {
        return;
    }

    let top_copper_layer_name =
        ensure_top_copper_layer_name(generated_geometry, ipc, ecad, used_layer_names);
    let top_soldermask_layer_name =
        ensure_top_soldermask_layer_name(generated_geometry, ipc, ecad, used_layer_names);
    let tooling_hole_layer_name =
        ensure_tooling_hole_layer_name(generated_geometry, used_layer_names);

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

pub(super) fn add_board_array_corner_tooling(
    generated_geometry: &mut BoardArrayGeneratedGeometry,
    used_layer_names: &mut HashSet<String>,
    array_width_mm: f64,
    array_height_mm: f64,
) {
    let tooling_hole_layer_name =
        ensure_tooling_hole_layer_name(generated_geometry, used_layer_names);
    generated_geometry.add_layer_feature(
        GeneratedFeatureScope::Array,
        tooling_hole_layer_name,
        Polarity::Positive,
        round_nonplated_hole_features(
            board_array_corner_tooling_holes(array_width_mm, array_height_mm),
            CORNER_TOOLING_HOLE_DIAMETER_MM,
        ),
    );
}

pub(super) struct BoardCellFiducialSpec {
    pub(super) board_width_mm: f64,
    pub(super) board_height_mm: f64,
    pub(super) board_margin: BoardMarginMm,
}

pub(super) fn add_board_cell_fiducials(
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

/// Place board array tooling on the shorter pair of array rails.
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
/// - multiple boards in the tooling axis require at least 12 mm board span,
///   because each side's pair sits over a different outer board;
/// - primary rail centers use 2.5 mm tooling and 8 mm fiducial span insets;
/// - secondary rail centers use 6.5 mm tooling and 12 mm fiducial span insets.
///
/// Rail-depth rules:
/// - tooling hole centers are 2.5 mm from the array edge;
/// - fiducial centers are 3.85 mm from the array edge.
pub(super) fn board_array_tooling_fiducials(spec: &BoardArrayToolingSpec) -> [(f64, f64); 4] {
    board_array_tooling_points(
        spec,
        FIDUCIAL_EDGE_OFFSET_MM,
        PRIMARY_FIDUCIAL_SPAN_INSET_MM,
        SECONDARY_FIDUCIAL_SPAN_INSET_MM,
    )
}

pub(super) fn board_array_tooling_holes(spec: &BoardArrayToolingSpec) -> [(f64, f64); 4] {
    board_array_tooling_points(
        spec,
        TOOLING_HOLE_EDGE_OFFSET_MM,
        PRIMARY_TOOLING_HOLE_SPAN_INSET_MM,
        SECONDARY_TOOLING_HOLE_SPAN_INSET_MM,
    )
}

pub(super) fn board_array_tooling_points(
    spec: &BoardArrayToolingSpec,
    rail_depth_mm: f64,
    primary_span_inset_mm: f64,
    secondary_span_inset_mm: f64,
) -> [(f64, f64); 4] {
    match spec.orientation {
        BoardArrayToolingOrientation::TopBottom => {
            let left_edge = spec.margin_x_mm;
            let right_edge = spec.margin_x_mm
                + (spec.columns - 1) as f64 * spec.pitch_x_mm
                + spec.board_width_mm;
            let top_y = spec.array_height_mm - rail_depth_mm;
            let bottom_y = rail_depth_mm;

            [
                (left_edge + primary_span_inset_mm, top_y),
                (right_edge - primary_span_inset_mm, top_y),
                (left_edge + secondary_span_inset_mm, bottom_y),
                (right_edge - secondary_span_inset_mm, bottom_y),
            ]
        }
        BoardArrayToolingOrientation::LeftRight => {
            let bottom_edge = spec.margin_y_mm;
            let top_edge =
                spec.margin_y_mm + (spec.rows - 1) as f64 * spec.pitch_y_mm + spec.board_height_mm;
            let left_x = rail_depth_mm;
            let right_x = spec.array_width_mm - rail_depth_mm;

            [
                (left_x, top_edge - primary_span_inset_mm),
                (left_x, bottom_edge + primary_span_inset_mm),
                (right_x, top_edge - secondary_span_inset_mm),
                (right_x, bottom_edge + secondary_span_inset_mm),
            ]
        }
    }
}

pub(super) fn board_array_corner_tooling_holes(
    array_width_mm: f64,
    array_height_mm: f64,
) -> [(f64, f64); 4] {
    let inset = ARRAY_CORNER_TOOLING_HOLE_INSET_MM;
    [
        (inset, inset),
        (array_width_mm - inset, inset),
        (array_width_mm - inset, array_height_mm - inset),
        (inset, array_height_mm - inset),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoardCellFiducialOrientation {
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
/// board-cell outer edge. The primary side is top/left and uses a 3 mm span
/// inset; the opposite side uses 7 mm.
pub(super) fn board_cell_fiducials(spec: &BoardCellFiducialSpec) -> Option<[(f64, f64); 4]> {
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

pub(super) fn board_cell_fiducial_orientation(
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
