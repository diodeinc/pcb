//! Rasterizes [`crate::geometry::BoardGeometry`] into a 2D obstacle grid.
//!
//! Scope note: this only rasterizes the top copper layer (`F.Cu`). Multi-layer
//! grids and via cost modeling are a follow-up PR once this model has landed
//! and been reviewed on its own — see the PR breakdown this crate came from.

use crate::geometry::{BoardGeometry, BoardOutline, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CellState {
    #[default]
    Free,
    /// Within `clearance_mm` of an obstacle. The router's cost model (added in
    /// a later PR) decides whether tight-clearance routing through here is
    /// ever allowed; for now this crate only reports the classification.
    Keepout,
    /// Inside a pad/via/footprint courtyard; routing must never pass through here.
    Obstacle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridConfig {
    /// Grid cell size in mm. Smaller pitch = finer routing resolution, more cells.
    pub pitch_mm: f64,
    /// Minimum copper-to-copper clearance in mm, rasterized as a `Keepout`
    /// halo around every obstacle.
    pub clearance_mm: f64,
    /// Extra margin (mm) added around the board outline / footprint bounding
    /// box before rasterizing, so components near the edge still get a full
    /// keepout halo.
    pub margin_mm: f64,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            pitch_mm: 0.2,
            clearance_mm: 0.2,
            margin_mm: 1.0,
        }
    }
}

/// A single-layer routing grid rasterized from board geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingGrid {
    pub config: GridConfig,
    /// Board-space position (mm) of grid cell (0, 0)'s corner.
    pub origin: Point,
    pub cols: usize,
    pub rows: usize,
    cells: Vec<CellState>,
}

impl RoutingGrid {
    pub fn cell(&self, col: usize, row: usize) -> CellState {
        self.cells[row * self.cols + col]
    }

    fn raise(&mut self, col: usize, row: usize, state: CellState) {
        let idx = row * self.cols + col;
        if state > self.cells[idx] {
            self.cells[idx] = state;
        }
    }

    fn world_to_cell(&self, p: Point) -> (isize, isize) {
        (
            ((p.x - self.origin.x) / self.config.pitch_mm).floor() as isize,
            ((p.y - self.origin.y) / self.config.pitch_mm).floor() as isize,
        )
    }

    /// Renders the grid as a compact ASCII map for tests/debugging:
    /// `.` = free, `o` = keepout, `X` = obstacle.
    pub fn to_ascii(&self) -> String {
        let mut out = String::with_capacity((self.cols + 1) * self.rows);
        for row in 0..self.rows {
            for col in 0..self.cols {
                out.push(match self.cell(col, row) {
                    CellState::Free => '.',
                    CellState::Keepout => 'o',
                    CellState::Obstacle => 'X',
                });
            }
            out.push('\n');
        }
        out
    }
}

/// Build a routing grid covering `board`'s outline (or, if the board has no
/// `Edge.Cuts` geometry, a bounding box over all placed footprints), rasterizing
/// every `F.Cu` pad as an `Obstacle` region with a `Keepout` halo of
/// `config.clearance_mm` around it.
pub fn build_grid(board: &BoardGeometry, config: GridConfig) -> RoutingGrid {
    let bounds = board_bounds(board, config.margin_mm);
    let cols = (bounds.width() / config.pitch_mm).ceil().max(1.0) as usize;
    let rows = (bounds.height() / config.pitch_mm).ceil().max(1.0) as usize;

    let mut grid = RoutingGrid {
        config,
        origin: bounds.min,
        cols,
        rows,
        cells: vec![CellState::Free; cols * rows],
    };

    for footprint in &board.footprints {
        for pad in &footprint.pads {
            if !pad.is_on_layer("F.Cu") {
                continue;
            }
            rasterize_circle(
                &mut grid,
                pad.position,
                pad.bounding_radius(),
                CellState::Obstacle,
            );
            rasterize_circle(
                &mut grid,
                pad.position,
                pad.bounding_radius() + config.clearance_mm,
                CellState::Keepout,
            );
        }
    }

    grid
}

fn board_bounds(board: &BoardGeometry, margin_mm: f64) -> BoardOutline {
    if let Some(outline) = board.outline {
        return BoardOutline {
            min: Point::new(outline.min.x - margin_mm, outline.min.y - margin_mm),
            max: Point::new(outline.max.x + margin_mm, outline.max.y + margin_mm),
        };
    }

    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut found_any_pad = false;
    for footprint in &board.footprints {
        for pad in &footprint.pads {
            let r = pad.bounding_radius();
            min.x = min.x.min(pad.position.x - r);
            min.y = min.y.min(pad.position.y - r);
            max.x = max.x.max(pad.position.x + r);
            max.y = max.y.max(pad.position.y + r);
            found_any_pad = true;
        }
    }

    if !found_any_pad {
        // No `Edge.Cuts` outline and no pads to fall back on (an empty board,
        // or one with footprints that have no pads at all). Returning a tiny
        // well-defined 1x1-cell area here is deliberate, not an accident of
        // infinity arithmetic falling through `.max(1.0)` downstream — this
        // case is real: `lib/std/test/layout/test_BoardConfig/layout.kicad_pcb`
        // in this repo hits it.
        return BoardOutline {
            min: Point::new(0.0, 0.0),
            max: Point::new(margin_mm.max(0.1), margin_mm.max(0.1)),
        };
    }

    BoardOutline {
        min: Point::new(min.x - margin_mm, min.y - margin_mm),
        max: Point::new(max.x + margin_mm, max.y + margin_mm),
    }
}

/// Mark every grid cell whose *center* falls within `radius` of `center` with
/// at least `state` (a cell already marked `Obstacle` is never downgraded to
/// `Keepout`, since `CellState`'s derived `Ord` ranks `Obstacle` highest).
fn rasterize_circle(grid: &mut RoutingGrid, center: Point, radius: f64, state: CellState) {
    let pitch = grid.config.pitch_mm;
    let (min_col, min_row) = grid.world_to_cell(Point::new(center.x - radius, center.y - radius));
    let (max_col, max_row) = grid.world_to_cell(Point::new(center.x + radius, center.y + radius));

    let row_lo = min_row.max(0);
    let row_hi = max_row.min(grid.rows as isize - 1);
    let col_lo = min_col.max(0);
    let col_hi = max_col.min(grid.cols as isize - 1);

    for row in row_lo..=row_hi {
        for col in col_lo..=col_hi {
            let cell_center = Point::new(
                grid.origin.x + (col as f64 + 0.5) * pitch,
                grid.origin.y + (row as f64 + 0.5) * pitch,
            );
            let dx = cell_center.x - center.x;
            let dy = cell_center.y - center.y;
            if dx * dx + dy * dy <= radius * radius {
                grid.raise(col as usize, row as usize, state);
            }
        }
    }
}
