use std::collections::BTreeSet;

use anyhow::{Result, bail};

use crate::{CONNECTION_GRID_MM, GEOMETRY_EPS_MM, Paper, Point, field_autoplace::Bounds};

const DEFAULT_TITLE_BLOCK_WIDTH_MM: f64 = 110.0;
const DEFAULT_TITLE_BLOCK_HEIGHT_MM: f64 = 34.0;
const PACKING_MARGIN_CELLS: i32 = 5;
const PACKING_CLEARANCE_CELLS: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridPoint {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

impl GridPoint {
    pub(crate) fn to_point(self) -> Point {
        Point::new(
            self.x as f64 * CONNECTION_GRID_MM,
            self.y as f64 * CONNECTION_GRID_MM,
        )
    }

    pub(crate) fn translated(self, offset: Self) -> Self {
        Self {
            x: self.x + offset.x,
            y: self.y + offset.y,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridRect {
    pub(crate) min_x: i32,
    pub(crate) min_y: i32,
    pub(crate) max_x: i32,
    pub(crate) max_y: i32,
}

impl GridRect {
    pub(crate) fn from_bounds(bounds: Bounds) -> Self {
        let min_x = grid_floor(bounds.min_x);
        let min_y = grid_floor(bounds.min_y);
        Self {
            min_x,
            min_y,
            max_x: grid_ceil(bounds.max_x).max(min_x + 1),
            max_y: grid_ceil(bounds.max_y).max(min_y + 1),
        }
    }

    pub(crate) fn translated(self, point: GridPoint) -> Self {
        Self {
            min_x: self.min_x + point.x,
            min_y: self.min_y + point.y,
            max_x: self.max_x + point.x,
            max_y: self.max_y + point.y,
        }
    }

    pub(crate) fn expanded(self, amount: i32) -> Self {
        Self {
            min_x: self.min_x - amount,
            min_y: self.min_y - amount,
            max_x: self.max_x + amount,
            max_y: self.max_y + amount,
        }
    }

    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    pub(crate) fn width(self) -> i32 {
        self.max_x - self.min_x
    }

    pub(crate) fn height(self) -> i32 {
        self.max_y - self.min_y
    }

    pub(crate) fn area(self) -> i64 {
        i64::from(self.width()) * i64::from(self.height())
    }

    /// Twice the longer side: a square beats a strip of equal area.
    fn compactness(self) -> i64 {
        2 * i64::from(self.width().max(self.height()))
    }
}

pub(crate) struct GridPacker {
    usable: GridRect,
    width: usize,
    occupied: Vec<bool>,
    occupied_bounds: Option<GridRect>,
    placed_cluster: Option<GridRect>,
    placement_anchors: Vec<GridPoint>,
}

impl GridPacker {
    pub(crate) fn for_page(paper: &Paper) -> Result<Self> {
        let (width_mm, height_mm) = paper_dimensions(paper)?;
        let usable = GridRect {
            min_x: PACKING_MARGIN_CELLS,
            min_y: PACKING_MARGIN_CELLS,
            max_x: (grid_floor(width_mm) - PACKING_MARGIN_CELLS).max(PACKING_MARGIN_CELLS + 1),
            max_y: (grid_floor(height_mm) - PACKING_MARGIN_CELLS).max(PACKING_MARGIN_CELLS + 1),
        };
        let width = usable.width() as usize;
        let height = usable.height() as usize;
        let mut packer = Self {
            usable,
            width,
            occupied: vec![false; width * height],
            occupied_bounds: None,
            placed_cluster: None,
            placement_anchors: Vec::new(),
        };
        packer.occupy(GridRect::from_bounds(
            Bounds::from_points([
                Point::new(
                    width_mm - DEFAULT_TITLE_BLOCK_WIDTH_MM,
                    height_mm - DEFAULT_TITLE_BLOCK_HEIGHT_MM,
                ),
                Point::new(width_mm, height_mm),
            ])
            .expect("title block has two corners"),
        ));
        Ok(packer)
    }

    pub(crate) fn occupy(&mut self, rect: GridRect) {
        let rect = rect.expanded(PACKING_CLEARANCE_CELLS);
        self.occupied_bounds = Some(
            self.occupied_bounds
                .map_or(rect, |bounds| bounds.union(rect)),
        );
        let min_x = rect.min_x.max(self.usable.min_x);
        let min_y = rect.min_y.max(self.usable.min_y);
        let max_x = rect.max_x.min(self.usable.max_x);
        let max_y = rect.max_y.min(self.usable.max_y);
        for y in min_y..max_y {
            for x in min_x..max_x {
                let index = self.index(x, y);
                self.occupied[index] = true;
            }
        }
    }

    pub(crate) fn occupy_anchored(&mut self, rect: GridRect, anchor: Point) {
        self.record_placement(
            rect,
            Some(GridPoint {
                x: (anchor.x / CONNECTION_GRID_MM).round() as i32,
                y: (anchor.y / CONNECTION_GRID_MM).round() as i32,
            }),
        );
    }

    pub(crate) fn usable_bounds(&self) -> GridRect {
        self.usable
    }

    pub(crate) fn can_place_without_overlap(&self, relative: GridRect) -> bool {
        let min_anchor_x = self.usable.min_x - relative.min_x;
        let min_anchor_y = self.usable.min_y - relative.min_y;
        let max_anchor_x = self.usable.max_x - relative.max_x;
        let max_anchor_y = self.usable.max_y - relative.max_y;
        if max_anchor_x < min_anchor_x || max_anchor_y < min_anchor_y {
            return false;
        }
        let occupied = self.occupancy_prefix();
        (min_anchor_y..=max_anchor_y).any(|y| {
            (min_anchor_x..=max_anchor_x).any(|x| {
                self.occupied_cells(relative.translated(GridPoint { x, y }), &occupied) == 0
            })
        })
    }

    pub(crate) fn place(&mut self, relative: GridRect) -> GridPoint {
        self.place_internal(relative, false)
    }

    /// Place a component block, preferring rows or columns through existing
    /// component symbol anchors before falling back to the unrestricted grid.
    pub(crate) fn place_anchored(&mut self, relative: GridRect) -> GridPoint {
        self.place_internal(relative, true)
    }

    fn place_internal(&mut self, relative: GridRect, align_anchor: bool) -> GridPoint {
        let min_anchor_x = self.usable.min_x - relative.min_x;
        let min_anchor_y = self.usable.min_y - relative.min_y;
        let max_anchor_x = self.usable.max_x - relative.max_x;
        let max_anchor_y = self.usable.max_y - relative.max_y;
        if max_anchor_x < min_anchor_x || max_anchor_y < min_anchor_y {
            return self.place_outside(relative, align_anchor);
        }
        let occupied = self.occupancy_prefix();
        let aligned_x = self
            .placement_anchors
            .iter()
            .map(|anchor| anchor.x)
            .collect::<BTreeSet<_>>();
        let aligned_y = self
            .placement_anchors
            .iter()
            .map(|anchor| anchor.y)
            .collect::<BTreeSet<_>>();
        let mut best_aligned = None;
        let mut best_fallback = None;
        for y in min_anchor_y..=max_anchor_y {
            for x in min_anchor_x..=max_anchor_x {
                let anchor = GridPoint { x, y };
                let candidate = relative.translated(anchor);
                let overlap = self.occupied_cells(candidate, &occupied);
                let cluster = self
                    .placed_cluster
                    .map_or(candidate, |placed| placed.union(candidate));
                let cluster_score = self.placed_cluster.map_or(0, |_| cluster.compactness());
                let rank = (
                    overlap,
                    cluster_score,
                    cluster.area(),
                    self.distance_from_center_squared(cluster),
                    y,
                    x,
                );
                if align_anchor
                    && overlap == 0
                    && (aligned_x.contains(&x) || aligned_y.contains(&y))
                    && best_aligned.is_none_or(|(best_rank, _)| rank < best_rank)
                {
                    best_aligned = Some((rank, anchor));
                }
                if best_fallback.is_none_or(|(best_rank, _)| rank < best_rank) {
                    best_fallback = Some((rank, anchor));
                }
            }
        }
        let (rank, anchor) = best_aligned
            .or(best_fallback)
            .expect("a non-empty anchor range has a candidate");
        if rank.0 > 0 {
            return self.place_outside(relative, align_anchor);
        }
        self.record_placement(relative.translated(anchor), align_anchor.then_some(anchor));
        anchor
    }

    fn record_placement(&mut self, rect: GridRect, anchor: Option<GridPoint>) {
        self.occupy(rect);
        self.placed_cluster = Some(
            self.placed_cluster
                .map_or(rect, |cluster| cluster.union(rect)),
        );
        self.placement_anchors.extend(anchor);
    }

    fn place_outside(&mut self, relative: GridRect, align_anchor: bool) -> GridPoint {
        // Include off-page items, which are not represented in the occupancy
        // bitmap, so repeated applies never stack overflow on existing work.
        let right = self.occupied_bounds.map_or(self.usable.max_x, |bounds| {
            bounds.max_x.max(self.usable.max_x)
        });
        let anchor = GridPoint {
            x: right + PACKING_MARGIN_CELLS - relative.min_x,
            y: self.usable.min_y - relative.min_y,
        };
        self.record_placement(relative.translated(anchor), align_anchor.then_some(anchor));
        anchor
    }

    fn distance_from_center_squared(&self, rect: GridRect) -> i64 {
        let dx = i64::from(rect.min_x) + i64::from(rect.max_x)
            - i64::from(self.usable.min_x)
            - i64::from(self.usable.max_x);
        let dy = i64::from(rect.min_y) + i64::from(rect.max_y)
            - i64::from(self.usable.min_y)
            - i64::from(self.usable.max_y);
        dx * dx + dy * dy
    }

    fn occupancy_prefix(&self) -> Vec<u32> {
        let height = self.occupied.len() / self.width;
        let stride = self.width + 1;
        let mut prefix = vec![0; stride * (height + 1)];
        for y in 0..height {
            let mut row = 0;
            for x in 0..self.width {
                row += u32::from(self.occupied[y * self.width + x]);
                prefix[(y + 1) * stride + x + 1] = prefix[y * stride + x + 1] + row;
            }
        }
        prefix
    }

    fn occupied_cells(&self, rect: GridRect, prefix: &[u32]) -> u32 {
        let x0 = (rect.min_x - self.usable.min_x) as usize;
        let y0 = (rect.min_y - self.usable.min_y) as usize;
        let x1 = (rect.max_x - self.usable.min_x) as usize;
        let y1 = (rect.max_y - self.usable.min_y) as usize;
        let stride = self.width + 1;
        prefix[y1 * stride + x1] + prefix[y0 * stride + x0]
            - prefix[y0 * stride + x1]
            - prefix[y1 * stride + x0]
    }

    fn index(&self, x: i32, y: i32) -> usize {
        (y - self.usable.min_y) as usize * self.width + (x - self.usable.min_x) as usize
    }
}

pub(crate) fn point_rect(point: Point) -> GridRect {
    GridRect {
        min_x: grid_floor(point.x),
        min_y: grid_floor(point.y),
        max_x: grid_ceil(point.x).max(grid_floor(point.x) + 1),
        max_y: grid_ceil(point.y).max(grid_floor(point.y) + 1),
    }
}

fn grid_floor(value: f64) -> i32 {
    ((value + GEOMETRY_EPS_MM) / CONNECTION_GRID_MM).floor() as i32
}

fn grid_ceil(value: f64) -> i32 {
    ((value - GEOMETRY_EPS_MM) / CONNECTION_GRID_MM).ceil() as i32
}

fn paper_dimensions(paper: &Paper) -> Result<(f64, f64)> {
    let (mut width, mut height) = match paper {
        Paper::Custom {
            width_mm,
            height_mm,
        } => (*width_mm, *height_mm),
        Paper::Named { name, .. } => match name.as_str() {
            "A0" => (1189.0, 841.0),
            "A1" => (841.0, 594.0),
            "A2" => (594.0, 420.0),
            "A3" => (420.0, 297.0),
            "A4" => (297.0, 210.0),
            "A5" => (210.0, 148.0),
            "A" | "USLetter" => (279.4, 215.9),
            "B" | "USLedger" => (431.8, 279.4),
            "C" => (558.8, 431.8),
            "D" => (863.6, 558.8),
            "E" => (1117.6, 863.6),
            "USLegal" => (355.6, 215.9),
            "GERBER" => (812.8, 812.8),
            _ => bail!("unsupported KiCad paper size '{name}' for automatic placement"),
        },
    };
    if matches!(paper, Paper::Named { portrait: true, .. }) {
        std::mem::swap(&mut width, &mut height);
    }
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_block_prefers_the_page_center() {
        let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
        let relative = GridRect {
            min_x: -2,
            min_y: -2,
            max_x: 2,
            max_y: 2,
        };

        let placed = relative.translated(packer.place(relative));

        let center_dx = placed.min_x + placed.max_x - packer.usable.min_x - packer.usable.max_x;
        let center_dy = placed.min_y + placed.max_y - packer.usable.min_y - packer.usable.max_y;
        assert!(center_dx.abs() <= 1);
        assert!(center_dy.abs() <= 1);
    }

    #[test]
    fn overflow_avoids_existing_off_page_items_and_other_overflow() {
        let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
        packer.occupy(packer.usable_bounds());
        let existing = GridRect {
            min_x: 1000,
            min_y: 0,
            max_x: 1100,
            max_y: 100,
        };
        packer.occupy(existing);
        let relative = GridRect {
            min_x: -2,
            min_y: -3,
            max_x: 2,
            max_y: 3,
        };

        let first = relative.translated(packer.place_anchored(relative));
        let second = relative.translated(packer.place(relative));

        assert!(first.min_x > existing.max_x + PACKING_CLEARANCE_CELLS);
        assert!(second.min_x > first.max_x + PACKING_CLEARANCE_CELLS);
        assert_eq!(first.min_y, packer.usable.min_y);
    }

    #[test]
    fn later_blocks_form_a_compact_cluster() {
        let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
        let relative = GridRect {
            min_x: 0,
            min_y: 0,
            max_x: 4,
            max_y: 4,
        };

        for _ in 0..4 {
            packer.place(relative);
        }

        let cluster = packer.placed_cluster.unwrap();
        assert!(cluster.width() <= 12);
        assert!(cluster.height() <= 12);
    }

    #[test]
    fn asymmetric_envelopes_align_on_symbol_anchors() {
        let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
        let symbol_with_decoration_above = GridRect {
            min_x: -2,
            min_y: -8,
            max_x: 2,
            max_y: 2,
        };
        let symbol_with_decoration_below = GridRect {
            min_x: -2,
            min_y: -2,
            max_x: 2,
            max_y: 8,
        };

        let first = packer.place_anchored(symbol_with_decoration_above);
        let second = packer.place_anchored(symbol_with_decoration_below);

        assert_eq!(first.y, second.y);
    }

    #[test]
    fn only_anchored_blocks_align_with_existing_symbol_anchors() {
        let existing_anchor = GridPoint { x: 100, y: 80 };
        let existing = GridRect {
            min_x: 96,
            min_y: 76,
            max_x: 104,
            max_y: 84,
        };
        let new_block = GridRect {
            min_x: -2,
            min_y: -2,
            max_x: 2,
            max_y: 2,
        };
        let packer_with_existing = || {
            let mut packer = GridPacker::for_page(&Paper::default()).unwrap();
            packer.occupy_anchored(existing, existing_anchor.to_point());
            packer
        };

        let anchored = packer_with_existing().place_anchored(new_block);
        assert!(anchored.x == existing_anchor.x || anchored.y == existing_anchor.y);
        assert!(anchored.x < 130 && anchored.y < 110);

        let unanchored = packer_with_existing().place(new_block);
        assert_ne!(unanchored.x, existing_anchor.x);
        assert_ne!(unanchored.y, existing_anchor.y);
    }
}
