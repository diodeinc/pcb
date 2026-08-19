//! Read a *real* assembly panel — the IPC-2581 output of
//! `pcbc ipc2581 board-array create` — into the interposer's frame.
//!
//! The generator's output shape is fixed: an `array` step whose profile is
//! the sheet, one `StepRepeat` of `board_cell` carrying the grid, a
//! `board_cell` step placing the board's own step at a constant offset,
//! NPTH `Hole` features on the array's drill layer, and `GlobalFiducial`
//! features on the outer copper layers. This is a deliberately narrow
//! text-level reader of that shape, not a general IPC-2581 parser.
//!
//! Coordinates: IPC-2581 is Y-up while the interposer works in the KiCad
//! layout frame (Y-down); KiCad's IPC export maps `(x, y)` → `(x, −y)`.
//! Everything returned here is already converted to the Y-down sheet frame.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::instantiate::Placement;
use crate::types::{BoardId, PanelSpec};

/// A real panel: sheet size, per-board placements (the position of the
/// board's KiCad-bbox origin, ready for [`crate::instantiate::instantiate`]),
/// and the panel's own tooling and fiducials.
pub struct ArrayPanel {
    pub sheet_w: f64,
    pub sheet_h: f64,
    pub places: Vec<Placement>,
    pub panel: PanelSpec,
}

fn attr(tag: &str, name: &str) -> Option<f64> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    tag[start..end].parse().ok()
}

fn step_block<'a>(xml: &'a str, name: &str) -> Result<&'a str> {
    let open = format!("<Step name=\"{name}\"");
    let start = xml
        .find(&open)
        .with_context(|| format!("panel XML has no step {name:?}"))?;
    let rest = &xml[start + open.len()..];
    let end = rest.find("<Step name=\"").unwrap_or(rest.len());
    Ok(&rest[..end])
}

fn profile_bbox(step: &str) -> Result<(f64, f64, f64, f64)> {
    let start = step.find("<Profile>").context("step has no profile")?;
    let end = step[start..]
        .find("</Profile>")
        .context("unterminated profile")?;
    let profile = &step[start..start + end];
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for tag in profile.split('<') {
        if let (Some(x), Some(y)) = (attr(tag, "x"), attr(tag, "y")) {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 {
        bail!("empty profile");
    }
    Ok((x0, y0, x1, y1))
}

/// All tags named `tag` inside `block`, as raw text.
fn tags<'a>(block: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i..];
        let end = after.find('>').unwrap_or(after.len());
        out.push(&after[..end]);
        rest = &after[end..];
    }
    out
}

/// Parse a generated panel. `board_min` is the board's KiCad-frame
/// Edge.Cuts bbox minimum corner — the same datum `localize` subtracts.
pub fn parse_panel(path: &Path, board_min: [f64; 2]) -> Result<ArrayPanel> {
    let xml =
        std::fs::read_to_string(path).with_context(|| format!("read panel {}", path.display()))?;

    let array = step_block(&xml, "array")?;
    let (px0, py0, px1, py1) = profile_bbox(array)?;
    if px0.abs() > 1e-6 || py0.abs() > 1e-6 {
        bail!("array profile does not start at the origin");
    }
    let (sheet_w, sheet_h) = (px1, py1);

    // The board grid.
    let grid = tags(array, "StepRepeat")
        .into_iter()
        .find(|t| t.contains("board_cell"))
        .context("array step has no board_cell repeat")?;
    let (cx, cy) = (
        attr(grid, "x").context("grid x")?,
        attr(grid, "y").context("grid y")?,
    );
    let (nx, ny) = (
        attr(grid, "nx").context("grid nx")? as u32,
        attr(grid, "ny").context("grid ny")? as u32,
    );
    let (dx, dy) = (
        attr(grid, "dx").context("grid dx")?,
        attr(grid, "dy").context("grid dy")?,
    );

    // The board's placement inside a cell.
    let cell = step_block(&xml, "board_cell")?;
    let layout = tags(cell, "StepRepeat")
        .into_iter()
        .next()
        .context("board_cell has no layout repeat")?;
    let (offx, offy) = (
        attr(layout, "x").context("layout x")?,
        attr(layout, "y").context("layout y")?,
    );
    if attr(layout, "angle").unwrap_or(0.0).abs() > 1e-6 || layout.contains("mirror=\"true\"") {
        bail!("rotated or mirrored board cells are not handled yet");
    }

    // KiCad-frame TP (x, y) lands on the Y-up panel at
    // (x + offx + cell_x, −y + offy + cell_y); flipping to the Y-down
    // sheet frame gives the placement of the board's bbox-min datum.
    let mut places = Vec::new();
    for j in 0..ny {
        for i in 0..nx {
            let cell_x = cx + i as f64 * dx;
            let cell_y = cy + j as f64 * dy;
            places.push(Placement {
                board: BoardId((j * nx + i) as u32),
                origin: [
                    board_min[0] + offx + cell_x,
                    sheet_h + board_min[1] - offy - cell_y,
                ],
            });
        }
    }

    // The panel's own tooling holes and two-sided global fiducials.
    let mut panel = PanelSpec::default();
    for h in tags(array, "Hole") {
        if let (Some(x), Some(y), Some(d)) = (attr(h, "x"), attr(h, "y"), attr(h, "diameter")) {
            panel.holes.push(([x, sheet_h - y], d));
        }
    }
    // Fiducials, per copper-layer block.
    let mut rest = array;
    while let Some(i) = rest.find("<LayerFeature layerRef=\"") {
        let after = &rest[i + 24..];
        let name_end = after.find('"').unwrap_or(0);
        let lname = &after[..name_end];
        let body_end = after.find("</LayerFeature>").unwrap_or(after.len());
        let body = &after[..body_end];
        if lname == "F.Cu" || lname == "B.Cu" {
            for gf in body.split("<GlobalFiducial>").skip(1) {
                if let Some(loc) = tags(gf, "Location").first() {
                    if let (Some(x), Some(y)) = (attr(loc, "x"), attr(loc, "y")) {
                        let p = [x, sheet_h - y];
                        // The panel's top face mates the interposer's top.
                        if lname == "F.Cu" {
                            panel.fids_top.push(p);
                        } else {
                            panel.fids_bottom.push(p);
                        }
                    }
                }
            }
        }
        rest = &after[body_end..];
    }
    if panel.holes.is_empty() {
        bail!("panel has no tooling holes");
    }

    Ok(ArrayPanel {
        sheet_w,
        sheet_h,
        places,
        panel,
    })
}
