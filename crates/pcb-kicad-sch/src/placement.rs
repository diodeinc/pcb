use anyhow::{Result, bail};

use crate::{CONNECTION_GRID_MM, GEOMETRY_EPS_MM, Paper, Point, field_autoplace::Bounds};

const DEFAULT_TITLE_BLOCK_WIDTH_MM: f64 = 110.0;
const DEFAULT_TITLE_BLOCK_HEIGHT_MM: f64 = 34.0;
const PACKING_MARGIN_CELLS: i32 = 5;
const PACKING_CLEARANCE_CELLS: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GridPoint {
    pub x: i32,
    pub y: i32,
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
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
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

    fn compactness(self) -> i64 {
        let width = i64::from(self.width());
        let height = i64::from(self.height());
        width + height + (width - height).abs()
    }
}

pub(crate) struct GridPacker {
    usable: GridRect,
    width: usize,
    occupied: Vec<bool>,
    placed_cluster: Option<GridRect>,
}

impl GridPacker {
    pub(crate) fn for_page(paper: &Paper) -> Result<Self> {
        let (width_mm, height_mm) = paper_dimensions(paper)?;
        let usable = GridRect {
            min_x: PACKING_MARGIN_CELLS,
            min_y: PACKING_MARGIN_CELLS,
            max_x: grid_floor(width_mm) - PACKING_MARGIN_CELLS,
            max_y: grid_floor(height_mm) - PACKING_MARGIN_CELLS,
        };
        if usable.max_x <= usable.min_x || usable.max_y <= usable.min_y {
            bail!("schematic page is too small for automatic placement");
        }
        let width = usable.width() as usize;
        let height = usable.height() as usize;
        let mut packer = Self {
            usable,
            width,
            occupied: vec![false; width * height],
            placed_cluster: None,
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

    /// Place one generated block. The first block prefers the page center;
    /// later blocks prefer the smallest near-square cluster, matching the PCB
    /// layout packer's behavior instead of independently orbiting the center.
    pub(crate) fn place(&mut self, relative: GridRect) -> GridPoint {
        let min_anchor_x = self.usable.min_x - relative.min_x;
        let min_anchor_y = self.usable.min_y - relative.min_y;
        let max_anchor_x = self.usable.max_x - relative.max_x;
        let max_anchor_y = self.usable.max_y - relative.max_y;
        if max_anchor_x < min_anchor_x || max_anchor_y < min_anchor_y {
            let anchor = self.centered_anchor(relative);
            self.record_placement(relative.translated(anchor));
            return anchor;
        }
        let occupied = self.occupancy_prefix();
        let mut best = None;
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
                if best.is_none_or(|(best_rank, _)| rank < best_rank) {
                    best = Some((rank, anchor));
                }
            }
        }
        let anchor = best.expect("a non-empty anchor range has a candidate").1;
        self.record_placement(relative.translated(anchor));
        anchor
    }

    fn record_placement(&mut self, rect: GridRect) {
        self.occupy(rect);
        self.placed_cluster = Some(
            self.placed_cluster
                .map_or(rect, |cluster| cluster.union(rect)),
        );
    }

    fn centered_anchor(&self, relative: GridRect) -> GridPoint {
        let x2 = i64::from(self.usable.min_x) + i64::from(self.usable.max_x)
            - i64::from(relative.min_x)
            - i64::from(relative.max_x);
        let y2 = i64::from(self.usable.min_y) + i64::from(self.usable.max_y)
            - i64::from(relative.min_y)
            - i64::from(relative.max_y);
        GridPoint {
            x: x2.div_euclid(2) as i32,
            y: y2.div_euclid(2) as i32,
        }
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
}
