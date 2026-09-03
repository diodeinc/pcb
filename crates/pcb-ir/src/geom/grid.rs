//! Uniform-grid bucketing for spatial candidate lookup.
//!
//! Geometry checks ask "which items lie in these cells" tens of thousands
//! of times against one static item set. The buckets are stored flat and
//! compressed: every occupied cell's ids live in one arena, column by
//! column and row by row, and only occupied cells are listed, so a region
//! spanning a whole panel with a few hundred edges costs a few hundred
//! entries. A run of cells down one column is a single contiguous slice,
//! found by two binary searches, so a query walks its cells without hashing
//! or allocating.

use crate::geom::bbox::BBox;

#[derive(Debug, Clone)]
pub(crate) struct CellGrid {
    pitch: f64,
    /// Cell coordinate of the first column.
    first_column: i64,
    /// Offsets into `rows` and `cell_starts` per column; one longer than
    /// the column count.
    columns: Vec<u32>,
    /// The row of each occupied cell, ascending within its column.
    rows: Vec<i64>,
    /// Offsets into `ids` per occupied cell; one longer than the cell count.
    cell_starts: Vec<u32>,
    ids: Vec<u32>,
}

impl CellGrid {
    /// Bucket every `(id, cell)` registration over `bounds`. Registrations
    /// outside the bounds' columns are dropped; a cell lists its ids in
    /// registration order.
    pub fn new(
        pitch: f64,
        bounds: BBox,
        registrations: impl Iterator<Item = (u32, (i64, i64))>,
    ) -> Self {
        let cell = |value: f64| (value / pitch).floor() as i64;
        let (first_column, column_count) = if bounds.is_empty() {
            (0, 0)
        } else {
            let first = cell(bounds.min.x);
            (first, (cell(bounds.max.x) - first + 1) as usize)
        };
        let mut pairs = registrations
            .filter(|&(_, (column, _))| {
                (first_column..first_column + column_count as i64).contains(&column)
            })
            .collect::<Vec<_>>();
        // A stable sort keeps each cell's ids in registration order.
        pairs.sort_by_key(|&(_, cell)| cell);

        let mut columns = vec![0u32; column_count + 1];
        let mut rows = Vec::new();
        let mut cell_starts = Vec::new();
        let mut ids = Vec::with_capacity(pairs.len());
        let mut previous = None;
        for &(id, (column, row)) in &pairs {
            if previous != Some((column, row)) {
                columns[(column - first_column) as usize + 1] += 1;
                rows.push(row);
                cell_starts.push(ids.len() as u32);
                previous = Some((column, row));
            }
            ids.push(id);
        }
        cell_starts.push(ids.len() as u32);
        for column in 0..column_count {
            columns[column + 1] += columns[column];
        }
        Self {
            pitch,
            first_column,
            columns,
            rows,
            cell_starts,
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
        let column = column - self.first_column;
        if column < 0 || column as usize + 1 >= self.columns.len() {
            return &[];
        }
        let (first, last) = (
            self.columns[column as usize] as usize,
            self.columns[column as usize + 1] as usize,
        );
        let rows = &self.rows[first..last];
        let low = first + rows.partition_point(|&row| row < min_row);
        let high = first + rows.partition_point(|&row| row <= max_row);
        &self.ids[self.cell_starts[low] as usize..self.cell_starts[high] as usize]
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
                (4, (2, 7)),
            ]
            .into_iter(),
        );

        assert_eq!(grid.column(0, 0, 0), &[7, 3]);
        assert_eq!(grid.column(0, 0, 1), &[7, 3, 1]);
        assert_eq!(grid.column(0, -5, 5), &[2, 7, 3, 1]);
        assert_eq!(grid.column(0, 2, 5), &[]);
        assert_eq!(grid.column(1, -5, 5), &[]);
        assert_eq!(grid.column(2, 1, 1), &[5]);
        assert_eq!(grid.column(2, 1, 9), &[5, 4]);
        assert_eq!(grid.column(4, 0, 0), &[]);
        assert_eq!(grid.column(-1, 0, 0), &[]);
        assert_eq!(
            grid.rectangle(bounds(-1.0, -1.0, 9.0, 9.0))
                .collect::<Vec<_>>(),
            vec![2, 7, 3, 1, 5, 4]
        );
    }

    #[test]
    fn empty_bounds_hold_nothing() {
        let grid = CellGrid::new(1.0, BBox::empty(), [(1, (0, 0))].into_iter());
        assert_eq!(grid.column(0, 0, 0), &[]);
        assert_eq!(grid.rectangle(bounds(0.0, 0.0, 1.0, 1.0)).count(), 0);
    }
}
