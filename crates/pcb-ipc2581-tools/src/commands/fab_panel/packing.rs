use std::collections::{BTreeMap, HashMap};
use std::fmt;

pub const SOFT_ITEM_LIMIT: usize = 16;
pub const MAX_ITEM_COUNT: usize = 32;

const SOFT_LIMIT_SEARCH_WORK: usize = 100_000_000;
const EXTENDED_SEARCH_WORK: usize = 100_000_000;
const SPLITS_PER_STATE_LIMIT: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub item_index: usize,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub rotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    Empty,
    InvalidBin,
    InvalidItem { item_index: usize },
    TooManyItems { count: usize, max: usize },
    NoLayout,
    SearchLimitExceeded,
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "at least one assembly panel is required"),
            Self::InvalidBin => write!(f, "the usable fabrication panel size must be positive"),
            Self::InvalidItem { item_index } => {
                write!(f, "assembly panel {} has an empty profile", item_index + 1)
            }
            Self::TooManyItems { count, max } => {
                write!(
                    f,
                    "at most {max} assembly panels are supported; got {count}"
                )
            }
            Self::NoLayout => write!(
                f,
                "no slicing layout fits the requested assembly panels in the fabrication panel"
            ),
            Self::SearchLimitExceeded => write!(
                f,
                "no layout was found before the slicing-layout search limit was reached"
            ),
        }
    }
}

impl std::error::Error for PackError {}

#[derive(Debug, Clone)]
struct Shape {
    size: Size,
    item_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    size: Size,
    node: usize,
}

#[derive(Debug, Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
enum PlanNode {
    Leaf {
        shape_index: usize,
        size: Size,
    },
    Join {
        axis: Axis,
        first: Candidate,
        second: Candidate,
        size: Size,
    },
}

struct Solver {
    bin: Size,
    gap: u32,
    shapes: Vec<Shape>,
    memo: HashMap<Vec<u8>, Vec<Candidate>>,
    nodes: Vec<PlanNode>,
    work: usize,
    work_limit: usize,
}

pub fn pack(items: &[Size], bin: Size, gap: u32) -> Result<Vec<Placement>, PackError> {
    if items.is_empty() {
        return Err(PackError::Empty);
    }
    if items.len() > MAX_ITEM_COUNT {
        return Err(PackError::TooManyItems {
            count: items.len(),
            max: MAX_ITEM_COUNT,
        });
    }
    if bin.width == 0 || bin.height == 0 {
        return Err(PackError::InvalidBin);
    }
    if let Some((item_index, _)) = items
        .iter()
        .enumerate()
        .find(|(_, size)| size.width == 0 || size.height == 0)
    {
        return Err(PackError::InvalidItem { item_index });
    }

    let shapes = group_shapes(items);
    let full_state = shapes
        .iter()
        .map(|shape| shape.item_indices.len() as u8)
        .collect::<Vec<_>>();
    let item_area = items.iter().map(|size| size.area()).sum::<u64>();
    if item_area > bin.area() {
        return Err(PackError::NoLayout);
    }

    let mut solver = Solver {
        bin,
        gap,
        shapes,
        memo: HashMap::new(),
        nodes: Vec::new(),
        work: 0,
        work_limit: if items.len() <= SOFT_ITEM_LIMIT {
            SOFT_LIMIT_SEARCH_WORK
        } else {
            EXTENDED_SEARCH_WORK
        },
    };
    let frontier = solver.solve(&full_state)?;
    let best = frontier
        .into_iter()
        .min_by_key(|candidate| {
            (
                candidate.size.area(),
                candidate.size.width + candidate.size.height,
                candidate.size.width,
                candidate.size.height,
            )
        })
        .ok_or(PackError::NoLayout)?;

    let origin_x = (bin.width - best.size.width) / 2;
    let origin_y = (bin.height - best.size.height) / 2;
    let mut next_item_in_shape = vec![0; solver.shapes.len()];
    let mut placements = Vec::with_capacity(items.len());
    solver.place(
        best,
        origin_x,
        origin_y,
        items,
        &mut next_item_in_shape,
        &mut placements,
    );
    placements.sort_by_key(|placement| placement.item_index);
    Ok(placements)
}

fn group_shapes(items: &[Size]) -> Vec<Shape> {
    let mut groups = BTreeMap::<(u32, u32), Vec<usize>>::new();
    for (item_index, item) in items.iter().enumerate() {
        let key = if item.width <= item.height {
            (item.width, item.height)
        } else {
            (item.height, item.width)
        };
        groups.entry(key).or_default().push(item_index);
    }
    groups
        .into_iter()
        .map(|((width, height), item_indices)| Shape {
            size: Size { width, height },
            item_indices,
        })
        .collect()
}

impl Solver {
    fn solve(&mut self, state: &[u8]) -> Result<Vec<Candidate>, PackError> {
        if let Some(frontier) = self.memo.get(state) {
            return Ok(frontier.clone());
        }

        let item_count = state.iter().map(|count| usize::from(*count)).sum::<usize>();
        let frontier = if item_count == 1 {
            self.leaf_frontier(state)
        } else {
            self.join_frontier(state)?
        };
        self.memo.insert(state.to_vec(), frontier.clone());
        Ok(frontier)
    }

    fn leaf_frontier(&mut self, state: &[u8]) -> Vec<Candidate> {
        let shape_index = state
            .iter()
            .position(|count| *count == 1)
            .expect("single-item state has one shape");
        let size = self.shapes[shape_index].size;
        let mut frontier = Vec::with_capacity(2);
        self.add_leaf_candidate(&mut frontier, shape_index, size);
        if size.width != size.height {
            self.add_leaf_candidate(
                &mut frontier,
                shape_index,
                Size {
                    width: size.height,
                    height: size.width,
                },
            );
        }
        frontier
    }

    fn add_leaf_candidate(
        &mut self,
        frontier: &mut Vec<Candidate>,
        shape_index: usize,
        size: Size,
    ) {
        if size.width > self.bin.width || size.height > self.bin.height {
            return;
        }
        let node = self.nodes.len();
        self.nodes.push(PlanNode::Leaf { shape_index, size });
        insert_pareto(frontier, Candidate { size, node });
    }

    fn join_frontier(&mut self, state: &[u8]) -> Result<Vec<Candidate>, PackError> {
        let mut splits = Vec::new();
        let mut left = vec![0; state.len()];
        self.collect_splits(state, 0, &mut left, &mut splits)?;
        let total_count = state.iter().map(|count| usize::from(*count)).sum::<usize>();
        splits.sort_by(|first, second| {
            let first_count = first.iter().map(|count| usize::from(*count)).sum::<usize>();
            let second_count = second
                .iter()
                .map(|count| usize::from(*count))
                .sum::<usize>();
            total_count
                .abs_diff(first_count * 2)
                .cmp(&total_count.abs_diff(second_count * 2))
                .then_with(|| first.cmp(second))
        });

        let mut frontier = Vec::new();
        for left_state in splits {
            let right_state = state
                .iter()
                .zip(&left_state)
                .map(|(total, left)| total - left)
                .collect::<Vec<_>>();
            let left_frontier = self.solve(&left_state)?;
            if left_frontier.is_empty() {
                continue;
            }
            let right_frontier = self.solve(&right_state)?;
            if right_frontier.is_empty() {
                continue;
            }
            for first in &left_frontier {
                for second in &right_frontier {
                    self.tick()?;
                    self.add_join_candidate(&mut frontier, Axis::Horizontal, *first, *second);
                    self.add_join_candidate(&mut frontier, Axis::Vertical, *first, *second);
                }
            }
        }
        Ok(frontier)
    }

    fn collect_splits(
        &mut self,
        state: &[u8],
        index: usize,
        left: &mut [u8],
        splits: &mut Vec<Vec<u8>>,
    ) -> Result<(), PackError> {
        if index == state.len() {
            self.tick()?;
            if left.iter().all(|count| *count == 0) || left == state {
                return Ok(());
            }
            let right = state
                .iter()
                .zip(left.iter())
                .map(|(total, left)| total - left)
                .collect::<Vec<_>>();
            if &*left <= right.as_slice() {
                if splits.len() == SPLITS_PER_STATE_LIMIT {
                    return Err(PackError::SearchLimitExceeded);
                }
                splits.push(left.to_vec());
            }
            return Ok(());
        }

        for count in 0..=state[index] {
            left[index] = count;
            self.collect_splits(state, index + 1, left, splits)?;
        }
        Ok(())
    }

    fn add_join_candidate(
        &mut self,
        frontier: &mut Vec<Candidate>,
        axis: Axis,
        first: Candidate,
        second: Candidate,
    ) {
        let size = match axis {
            Axis::Horizontal => {
                let Some(width) = first
                    .size
                    .width
                    .checked_add(self.gap)
                    .and_then(|width| width.checked_add(second.size.width))
                else {
                    return;
                };
                Size {
                    width,
                    height: first.size.height.max(second.size.height),
                }
            }
            Axis::Vertical => {
                let Some(height) = first
                    .size
                    .height
                    .checked_add(self.gap)
                    .and_then(|height| height.checked_add(second.size.height))
                else {
                    return;
                };
                Size {
                    width: first.size.width.max(second.size.width),
                    height,
                }
            }
        };
        if size.width > self.bin.width || size.height > self.bin.height {
            return;
        }
        if frontier
            .iter()
            .any(|candidate| dominates(candidate.size, size))
        {
            return;
        }
        frontier.retain(|candidate| !dominates(size, candidate.size));
        let node = self.nodes.len();
        self.nodes.push(PlanNode::Join {
            axis,
            first,
            second,
            size,
        });
        frontier.push(Candidate { size, node });
    }

    fn tick(&mut self) -> Result<(), PackError> {
        self.work += 1;
        if self.work > self.work_limit {
            Err(PackError::SearchLimitExceeded)
        } else {
            Ok(())
        }
    }

    fn place(
        &self,
        candidate: Candidate,
        x: u32,
        y: u32,
        items: &[Size],
        next_item_in_shape: &mut [usize],
        placements: &mut Vec<Placement>,
    ) {
        match self.nodes[candidate.node] {
            PlanNode::Leaf { shape_index, size } => {
                let item_slot = next_item_in_shape[shape_index];
                let item_index = self.shapes[shape_index].item_indices[item_slot];
                next_item_in_shape[shape_index] += 1;
                let original = items[item_index];
                placements.push(Placement {
                    item_index,
                    x,
                    y,
                    width: size.width,
                    height: size.height,
                    rotated: size.width != original.width || size.height != original.height,
                });
            }
            PlanNode::Join {
                axis,
                first,
                second,
                size,
            } => match axis {
                Axis::Horizontal => {
                    self.place(
                        first,
                        x,
                        y + (size.height - first.size.height) / 2,
                        items,
                        next_item_in_shape,
                        placements,
                    );
                    self.place(
                        second,
                        x + first.size.width + self.gap,
                        y + (size.height - second.size.height) / 2,
                        items,
                        next_item_in_shape,
                        placements,
                    );
                }
                Axis::Vertical => {
                    self.place(
                        first,
                        x + (size.width - first.size.width) / 2,
                        y,
                        items,
                        next_item_in_shape,
                        placements,
                    );
                    self.place(
                        second,
                        x + (size.width - second.size.width) / 2,
                        y + first.size.height + self.gap,
                        items,
                        next_item_in_shape,
                        placements,
                    );
                }
            },
        }
    }
}

fn dominates(first: Size, second: Size) -> bool {
    first.width <= second.width && first.height <= second.height
}

fn insert_pareto(frontier: &mut Vec<Candidate>, candidate: Candidate) {
    if frontier
        .iter()
        .any(|existing| dominates(existing.size, candidate.size))
    {
        return;
    }
    frontier.retain(|existing| !dominates(candidate.size, existing.size));
    frontier.push(candidate);
}

#[cfg(test)]
mod tests {
    use super::*;

    const USABLE_FAB_PANEL: Size = Size {
        width: 447_200,
        height: 599_600,
    };
    const GAP: u32 = 5_000;

    fn assert_valid(items: &[Size], placements: &[Placement]) {
        assert_eq!(items.len(), placements.len());
        for placement in placements {
            assert!(placement.x + placement.width <= USABLE_FAB_PANEL.width);
            assert!(placement.y + placement.height <= USABLE_FAB_PANEL.height);
            let original = items[placement.item_index];
            assert!(
                (placement.width == original.width && placement.height == original.height)
                    || (placement.width == original.height && placement.height == original.width)
            );
        }
        for (index, first) in placements.iter().enumerate() {
            for second in &placements[index + 1..] {
                let separated = first.x + first.width + GAP <= second.x
                    || second.x + second.width + GAP <= first.x
                    || first.y + first.height + GAP <= second.y
                    || second.y + second.height + GAP <= first.y;
                assert!(separated, "{first:?} overlaps {second:?}");
            }
        }

        let min_x = placements.iter().map(|item| item.x).min().unwrap();
        let max_x = placements
            .iter()
            .map(|item| item.x + item.width)
            .max()
            .unwrap();
        let min_y = placements.iter().map(|item| item.y).min().unwrap();
        let max_y = placements
            .iter()
            .map(|item| item.y + item.height)
            .max()
            .unwrap();
        assert!((min_x as i64 - (USABLE_FAB_PANEL.width - max_x) as i64).abs() <= 1);
        assert!((min_y as i64 - (USABLE_FAB_PANEL.height - max_y) as i64).abs() <= 1);
    }

    #[test]
    fn packs_four_a4_panels() {
        let items = vec![
            Size {
                width: 210_000,
                height: 297_000,
            };
            4
        ];
        let placements = pack(&items, USABLE_FAB_PANEL, GAP).unwrap();
        assert_valid(&items, &placements);
    }

    #[test]
    fn packs_heterogeneous_a_series_panels() {
        let items = vec![
            Size {
                width: 210_000,
                height: 297_000,
            },
            Size {
                width: 148_000,
                height: 210_000,
            },
            Size {
                width: 105_000,
                height: 148_000,
            },
            Size {
                width: 74_000,
                height: 105_000,
            },
            Size {
                width: 74_000,
                height: 105_000,
            },
        ];
        let placements = pack(&items, USABLE_FAB_PANEL, GAP).unwrap();
        assert_valid(&items, &placements);
    }

    #[test]
    fn repeated_shapes_scale_to_the_hard_limit() {
        let items = vec![
            Size {
                width: 50_000,
                height: 50_000,
            };
            MAX_ITEM_COUNT
        ];
        let placements = pack(&items, USABLE_FAB_PANEL, GAP).unwrap();
        assert_valid(&items, &placements);
    }

    #[test]
    fn packs_heterogeneous_shapes_above_the_soft_limit() {
        let items = [
            (
                Size {
                    width: 40_000,
                    height: 60_000,
                },
                5,
            ),
            (
                Size {
                    width: 50_000,
                    height: 70_000,
                },
                4,
            ),
            (
                Size {
                    width: 60_000,
                    height: 80_000,
                },
                4,
            ),
            (
                Size {
                    width: 70_000,
                    height: 90_000,
                },
                4,
            ),
        ]
        .into_iter()
        .flat_map(|(size, count)| std::iter::repeat_n(size, count))
        .collect::<Vec<_>>();

        assert!(items.len() > SOFT_ITEM_LIMIT);
        assert!(EXTENDED_SEARCH_WORK >= SOFT_LIMIT_SEARCH_WORK);
        let placements = pack(&items, USABLE_FAB_PANEL, GAP).unwrap();
        assert_valid(&items, &placements);
    }

    #[test]
    fn rejects_more_than_the_hard_limit() {
        let items = vec![
            Size {
                width: 1,
                height: 1,
            };
            MAX_ITEM_COUNT + 1
        ];
        assert_eq!(
            pack(&items, USABLE_FAB_PANEL, GAP),
            Err(PackError::TooManyItems {
                count: MAX_ITEM_COUNT + 1,
                max: MAX_ITEM_COUNT,
            })
        );
    }

    #[test]
    fn returns_no_layout_when_panels_do_not_fit() {
        let items = vec![
            Size {
                width: USABLE_FAB_PANEL.width,
                height: USABLE_FAB_PANEL.height,
            };
            2
        ];
        assert_eq!(
            pack(&items, USABLE_FAB_PANEL, GAP),
            Err(PackError::NoLayout)
        );
    }

    #[test]
    fn packing_is_deterministic() {
        let items = vec![
            Size {
                width: 100_000,
                height: 200_000,
            },
            Size {
                width: 120_000,
                height: 80_000,
            },
            Size {
                width: 100_000,
                height: 200_000,
            },
        ];
        assert_eq!(
            pack(&items, USABLE_FAB_PANEL, GAP).unwrap(),
            pack(&items, USABLE_FAB_PANEL, GAP).unwrap()
        );
    }
}
