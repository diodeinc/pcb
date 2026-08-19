//! Assembly-panel tooling and fiducials for the interposer sheet.
//!
//! The interposer stacks under the assembly panel on the same tooling
//! pins, so it inherits the panel's tooling exactly — the same formulas
//! and constants as the board-array generator
//! (`pcb-ipc2581-tools/src/commands/board_array/tooling.rs`):
//!
//! - corner tooling: Ø2.1 NPTH, 3.0 mm inset from each array corner;
//! - rail tooling: Ø2.0 NPTH, centers 2.5 mm from the chosen rail-pair
//!   edge, span insets 2.5 mm (primary rail) / 6.5 mm (secondary) from
//!   the outer board edges;
//! - global fiducials: Ø1.0 copper with Ø2.0 mask opening on both faces,
//!   3.85 mm from the rail edge, span insets 8/12 mm (top) and 9/11 mm
//!   (bottom).
//!
//! On top of that, the folded A7 tile at the origin gets the A7 panel's
//! own corner tooling, so the fixture's A7-sized connector plate registers
//! on every sheet size. On the A7 sheet those four holes coincide exactly
//! with the sheet's corner tooling and dedupe to a single set — holes
//! either overlap perfectly or not at all.

use crate::instantiate::{Placement, Sheet};
use crate::pattern::mate_dims;
use crate::types::PanelSpec;

const CORNER_TOOLING_DIA_MM: f64 = 2.1;
const CORNER_TOOLING_INSET_MM: f64 = 3.0;
const RAIL_TOOLING_DIA_MM: f64 = 2.0;
const RAIL_TOOLING_EDGE_MM: f64 = 2.5;
const RAIL_TOOLING_SPAN_MM: [f64; 2] = [2.5, 6.5];
const FID_EDGE_MM: f64 = 3.85;
const FID_SPAN_TOP_MM: [f64; 2] = [8.0, 12.0];
const FID_SPAN_BOTTOM_MM: [f64; 2] = [9.0, 11.0];
const SINGLE_BOARD_MIN_SPAN_MM: f64 = 28.0;
const MULTI_BOARD_MIN_SPAN_MM: f64 = 12.0;
const EPS: f64 = 1e-6;

/// Corner tooling holes of a rectangle anchored at `origin`.
fn corner_holes(origin: [f64; 2], w: f64, h: f64) -> [[f64; 2]; 4] {
    let i = CORNER_TOOLING_INSET_MM;
    [
        [origin[0] + i, origin[1] + i],
        [origin[0] + w - i, origin[1] + i],
        [origin[0] + w - i, origin[1] + h - i],
        [origin[0] + i, origin[1] + h - i],
    ]
}

/// The board grid recovered from the packing: (count, margin, pitch) per axis.
fn axis(vals: impl Iterator<Item = f64>) -> (u32, f64, f64) {
    let mut xs: Vec<f64> = vals.collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup_by(|a, b| (*a - *b).abs() < EPS);
    let pitch = if xs.len() > 1 { xs[1] - xs[0] } else { 0.0 };
    (xs.len() as u32, xs.first().copied().unwrap_or(0.0), pitch)
}

/// Rail-pair points at `edge_depth` from the sheet edge, span-inset from
/// the outer board edges — the board-array placement formula verbatim.
struct Rails {
    columns: u32,
    rows: u32,
    bw: f64,
    bh: f64,
    margin: [f64; 2],
    pitch: [f64; 2],
    sheet: [f64; 2],
}

impl Rails {
    /// Prefer the shorter rail pair (the sheet's short edges), fall back to
    /// the other eligible pair; `None` when neither board span qualifies.
    fn orientation(&self) -> Option<bool> {
        let eligible = |count: u32, span: f64| {
            let min = if count == 1 {
                SINGLE_BOARD_MIN_SPAN_MM
            } else {
                MULTI_BOARD_MIN_SPAN_MM
            };
            span + EPS >= min
        };
        let top_bottom = eligible(self.columns, self.bw);
        let left_right = eligible(self.rows, self.bh);
        if self.sheet[0] > self.sheet[1] {
            if left_right {
                Some(false)
            } else {
                top_bottom.then_some(true)
            }
        } else if top_bottom {
            Some(true)
        } else {
            left_right.then_some(false)
        }
    }

    fn points(&self, top_bottom: bool, edge_depth: f64, span: [f64; 2]) -> [[f64; 2]; 4] {
        if top_bottom {
            let left = self.margin[0];
            let right = self.margin[0] + (self.columns - 1) as f64 * self.pitch[0] + self.bw;
            let top_y = self.sheet[1] - edge_depth;
            [
                [left + span[0], top_y],
                [right - span[0], top_y],
                [left + span[1], edge_depth],
                [right - span[1], edge_depth],
            ]
        } else {
            let bottom = self.margin[1];
            let top = self.margin[1] + (self.rows - 1) as f64 * self.pitch[1] + self.bh;
            let right_x = self.sheet[0] - edge_depth;
            [
                [edge_depth, top - span[0]],
                [edge_depth, bottom + span[0]],
                [right_x, top - span[1]],
                [right_x, bottom + span[1]],
            ]
        }
    }
}

/// Build the interposer sheet's panel features from its packing.
pub fn panel_spec(sheet: Sheet, places: &[Placement], bw: f64, bh: f64) -> PanelSpec {
    let mut holes: Vec<([f64; 2], f64)> = Vec::new();
    // Assembly-panel corner tooling at the sheet corners.
    for xy in corner_holes([0.0, 0.0], sheet.w, sheet.h) {
        holes.push((xy, CORNER_TOOLING_DIA_MM));
    }
    // The folded A7 tile's corner tooling (fixture connector plate).
    let (mw, mh) = mate_dims(sheet.w, sheet.h);
    for xy in corner_holes([0.0, 0.0], mw, mh) {
        holes.push((xy, CORNER_TOOLING_DIA_MM));
    }

    let rails = Rails {
        columns: axis(places.iter().map(|p| p.origin[0])).0,
        rows: axis(places.iter().map(|p| p.origin[1])).0,
        bw,
        bh,
        margin: [
            axis(places.iter().map(|p| p.origin[0])).1,
            axis(places.iter().map(|p| p.origin[1])).1,
        ],
        pitch: [
            axis(places.iter().map(|p| p.origin[0])).2,
            axis(places.iter().map(|p| p.origin[1])).2,
        ],
        sheet: [sheet.w, sheet.h],
    };
    let mut fids_top = Vec::new();
    let mut fids_bottom = Vec::new();
    if let Some(tb) = rails.orientation() {
        for xy in rails.points(tb, RAIL_TOOLING_EDGE_MM, RAIL_TOOLING_SPAN_MM) {
            holes.push((xy, RAIL_TOOLING_DIA_MM));
        }
        fids_top.extend(rails.points(tb, FID_EDGE_MM, FID_SPAN_TOP_MM));
        fids_bottom.extend(rails.points(tb, FID_EDGE_MM, FID_SPAN_BOTTOM_MM));
    }

    // Coincident holes (the A7 sheet's own corners) collapse to one.
    let mut dedup: Vec<([f64; 2], f64)> = Vec::new();
    for (xy, d) in holes {
        if !dedup
            .iter()
            .any(|(q, _)| (q[0] - xy[0]).abs() < EPS && (q[1] - xy[1]).abs() < EPS)
        {
            dedup.push((xy, d));
        }
    }
    PanelSpec {
        holes: dedup,
        fids_top,
        fids_bottom,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instantiate::{A5, A6, A7, pack};

    fn spec_for(sheet: Sheet) -> PanelSpec {
        let places = pack(sheet, 40.0, 30.0, 8);
        panel_spec(sheet, &places, 40.0, 30.0)
    }

    #[test]
    fn a7_sheet_and_tile_tooling_coincide() {
        let s = spec_for(A7);
        // 4 corner holes (sheet == A7 tile, perfect overlap) + 4 rail holes.
        let corner: Vec<_> = s.holes.iter().filter(|(_, d)| *d == 2.1).collect();
        assert_eq!(corner.len(), 4);
        assert_eq!(s.fids_top.len(), 4);
        assert_eq!(s.fids_bottom.len(), 4);
    }

    #[test]
    fn larger_sheets_add_distinct_a7_tile_holes() {
        for sheet in [A6, A5] {
            let s = spec_for(sheet);
            let corner: Vec<_> = s.holes.iter().filter(|(_, d)| *d == 2.1).collect();
            // 4 sheet corners + 4 A7-tile corners, minus exact overlaps.
            assert!(corner.len() > 4 && corner.len() <= 8, "{}", sheet.name);
            // No two holes closer than their diameters without being equal.
            for (i, (a, _)) in s.holes.iter().enumerate() {
                for (b, _) in s.holes.iter().skip(i + 1) {
                    let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                    assert!(
                        d > 4.0,
                        "{}: holes {a:?} and {b:?} nearly overlap",
                        sheet.name
                    );
                }
            }
        }
    }
}
