use pcb_route_native::{CellState, GridConfig, build_grid, parse_board};

const TWO_PAD_BOARD: &str = include_str!("fixtures/two_pad_board.kicad_pcb");

/// Coarse config so the ASCII snapshot is small enough to review by eye in a PR diff.
fn snapshot_config() -> GridConfig {
    GridConfig {
        pitch_mm: 1.0,
        clearance_mm: 0.5,
        margin_mm: 1.0,
    }
}

/// Fine config for the keepout-boundary test below: pitch must be small
/// relative to pad size (~0.42mm bounding radius) or grid-cell quantization
/// makes exact-radius assertions flaky.
fn fine_config() -> GridConfig {
    GridConfig {
        pitch_mm: 0.1,
        clearance_mm: 0.3,
        margin_mm: 1.0,
    }
}

#[test]
fn obstacle_grid_matches_snapshot() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    let grid = build_grid(&board, snapshot_config());
    insta::assert_snapshot!(grid.to_ascii());
}

#[test]
fn pad_keepout_radius_blocks_adjacent_cells_but_not_far_cells() {
    let board = parse_board(TWO_PAD_BOARD).expect("fixture should parse");
    let grid = build_grid(&board, fine_config());

    let cell_state_at = |x: f64, y: f64| {
        let col = ((x - grid.origin.x) / grid.config.pitch_mm).floor() as usize;
        let row = ((y - grid.origin.y) / grid.config.pitch_mm).floor() as usize;
        grid.cell(col, row)
    };

    // R1 pad 1 center: footprint at (5, 5), pad local (-0.51, 0), rot 0 -> (4.49, 5.0).
    assert_eq!(cell_state_at(4.49, 5.0), CellState::Obstacle);

    // 0.6mm from pad center: outside the ~0.42mm pad radius but inside the
    // 0.42 + 0.3 = ~0.72mm keepout radius.
    assert_eq!(cell_state_at(4.49 - 0.6, 5.0), CellState::Keepout);

    // Far corner of the board, outside any pad's obstacle + keepout radius.
    assert_eq!(cell_state_at(19.0, 14.0), CellState::Free);
}
