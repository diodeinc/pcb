//! Uniform-grid bucketing for spatial candidate lookup.
//!
//! Geometry checks ask "which items lie in these cells" tens of thousands
//! of times against one static item set. The buckets are stored flat and
//! compressed: every cell's ids live in one arena, column by column and row
//! by row, addressed by an offset table. A cell is two array reads, and a
//! run of cells down one column is a single contiguous slice, so a query
//! walks its cells without hashing or allocating.

use crate::geom::bbox::BBox;

#[derive(Debug, Clone)]
pub(crate) struct CellGrid {
    pitch: f64,
    /// Cell coordinates of the first column and row.
    origin: (i64, i64),
    columns: i64,
    rows: i64,
    /// Column-major cell offsets into `ids`; one longer than the cell count.
    starts: Vec<u32>,
    ids: Vec<u32>,
}

/// The offset table is dense, so cells per axis are bounded and the pitch
/// coarsens beyond that instead of the table growing without limit.
const MAX_CELLS_PER_AXIS: f64 = 2048.0;

impl CellGrid {
    /// The pitch a grid over `bounds` uses for a requested pitch.
    pub fn pitch(requested: f64, bounds: BBox) -> f64 {
        if bounds.is_empty() {
            return requested;
        }
        requested.max(bounds.width().max(bounds.height()) / MAX_CELLS_PER_AXIS)
    }

    /// Bucket every `(id, cell)` registration over `bounds`. Registrations
    /// outside the bounds are dropped; a cell lists its ids in registration
    /// order.
    pub fn new(
        pitch: f64,
        bounds: BBox,
        registrations: impl Iterator<Item = (u32, (i64, i64))>,
    ) -> Self {
        let cell = |value: f64| (value / pitch).floor() as i64;
        let (origin, columns, rows) = if bounds.is_empty() {
            ((0, 0), 0, 0)
        } else {
            let origin = (cell(bounds.min.x), cell(bounds.min.y));
            (
                origin,
                cell(bounds.max.x) - origin.0 + 1,
                cell(bounds.max.y) - origin.1 + 1,
            )
        };
        let cells = (columns * rows) as usize;
        let index = |(column, row): (i64, i64)| {
            let (column, row) = (column - origin.0, row - origin.1);
            ((0..columns).contains(&column) && (0..rows).contains(&row))
                .then(|| (column * rows + row) as u32)
        };
        let pairs = registrations
            .filter_map(|(id, cell)| index(cell).map(|cell| (cell, id)))
            .collect::<Vec<_>>();
        // Counting sort by cell keeps each cell's ids in registration order.
        let mut starts = vec![0u32; cells + 1];
        for &(cell, _) in &pairs {
            starts[cell as usize + 1] += 1;
        }
        for cell in 0..cells {
            starts[cell + 1] += starts[cell];
        }
        let mut cursor = starts.clone();
        let mut ids = vec![0u32; pairs.len()];
        for &(cell, id) in &pairs {
            ids[cursor[cell as usize] as usize] = id;
            cursor[cell as usize] += 1;
        }
        Self {
            pitch,
            origin,
            columns,
            rows,
            starts,
            ids,
        }
    }

    /// Every cell of a grid of `pitch` that `bounds` meets, column by column
    /// and row by row.
    pub fn cells_of(bounds: BBox, pitch: f64) -> impl Iterator<Item = (i64, i64)> {
        let cell = move |value: f64| (value / pitch).floor() as i64;
        let (min_column, max_column) = (cell(bounds.min.x), cell(bounds.max.x));
        let (min_row, max_row) = (cell(bounds.min.y), cell(bounds.max.y));
        (min_column..=max_column)
            .flat_map(move |column| (min_row..=max_row).map(move |row| (column, row)))
    }

    pub fn pitch_mm(&self) -> f64 {
        self.pitch
    }

    pub fn cell_of(&self, value: f64) -> i64 {
        (value / self.pitch).floor() as i64
    }

    /// The ids of the cells of one column from `min_row` through `max_row`,
    /// row by row. Cells outside the grid are empty.
    pub fn column(&self, column: i64, min_row: i64, max_row: i64) -> &[u32] {
        let column = column - self.origin.0;
        let min_row = (min_row - self.origin.1).max(0);
        let max_row = (max_row - self.origin.1).min(self.rows - 1);
        if !(0..self.columns).contains(&column) || min_row > max_row {
            return &[];
        }
        let first = (column * self.rows + min_row) as usize;
        let last = (column * self.rows + max_row) as usize;
        &self.ids[self.starts[first] as usize..self.starts[last + 1] as usize]
    }

    /// The ids of every cell meeting `bounds`, column by column, then row
    /// by row. An id in several cells appears once per cell.
    pub fn rectangle(&self, bounds: BBox) -> impl Iterator<Item = u32> + '_ {
        let (min_column, max_column) = (self.cell_of(bounds.min.x), self.cell_of(bounds.max.x));
        let (min_row, max_row) = (self.cell_of(bounds.min.y), self.cell_of(bounds.max.y));
        (min_column..=max_column)
            .flat_map(move |column| self.column(column, min_row, max_row).iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::point::Point;

    fn bounds(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> BBox {
        BBox::new(Point::new(min_x, min_y), Point::new(max_x, max_y))
    }

    #[test]
    fn cells_keep_registration_order_and_ignore_outsiders() {
        let grid = CellGrid::new(
            1.0,
            bounds(0.0, 0.0, 2.5, 1.5),
            [
                (7, (0, 0)),
                (3, (0, 0)),
                (5, (2, 1)),
                (9, (4, 0)),
                (1, (0, 1)),
                (2, (0, -1)),
            ]
            .into_iter(),
        );

        assert_eq!(grid.column(0, 0, 0), &[7, 3]);
        assert_eq!(grid.column(0, 0, 1), &[7, 3, 1]);
        assert_eq!(grid.column(0, -5, 5), &[7, 3, 1]);
        assert_eq!(grid.column(2, 1, 1), &[5]);
        assert_eq!(grid.column(4, 0, 0), &[]);
        assert_eq!(grid.column(-1, 0, 0), &[]);
        assert_eq!(
            grid.rectangle(bounds(-1.0, -1.0, 9.0, 9.0))
                .collect::<Vec<_>>(),
            vec![7, 3, 1, 5]
        );
    }

    #[test]
    fn empty_bounds_hold_nothing() {
        let grid = CellGrid::new(1.0, BBox::empty(), [(1, (0, 0))].into_iter());
        assert_eq!(grid.column(0, 0, 0), &[]);
        assert_eq!(grid.rectangle(bounds(0.0, 0.0, 1.0, 1.0)).count(), 0);
    }

    #[test]
    fn pitch_coarsens_only_for_extreme_extents() {
        assert_eq!(CellGrid::pitch(0.5, bounds(0.0, 0.0, 300.0, 200.0)), 0.5);
        assert_eq!(CellGrid::pitch(0.5, bounds(0.0, 0.0, 4096.0, 10.0)), 2.0);
        assert_eq!(CellGrid::pitch(0.5, BBox::empty()), 0.5);
    }
}
