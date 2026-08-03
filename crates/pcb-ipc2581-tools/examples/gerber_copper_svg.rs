//! Render one Gerber layer's composed final image to a filled SVG.
//!
//! Development harness for visually inspecting exported copper: the Gerber is
//! parsed and composed exactly as `pcbc ipc2581 dfm` sees it, so the picture
//! matches manufacturing output rather than any IPC-2581 view.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_ir::geom::region::rings_from_contours;
use pcb_ir::geom::{ContourSet, FillRule};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: gerber_copper_svg <input.gbr> <output.svg>";
    let input = PathBuf::from(args.next().context(usage)?);
    let output = PathBuf::from(args.next().context(usage)?);

    let contents = std::fs::read_to_string(&input)
        .with_context(|| format!("failed to read {}", input.display()))?;
    let gerber = gerberx2::GerberX2::parse(&contents)?;
    let doc = gerberx2::geometry::extract_document(&gerber);
    let mask = pcb_ir::dialects::artwork::compose_to_mask(&doc);
    let mut rings = Vec::new();
    for layer in &mask.layers {
        for shape in mask.shapes(layer) {
            rings.extend(rings_from_contours(&mask.arena.path_contours(shape)));
        }
    }
    let region = ContourSet::new(rings, FillRule::NonZero, 1e-4);

    let pad = 1.0;
    let min_x = region.bbox.min.x - pad;
    let width = region.bbox.width() + 2.0 * pad;
    let min_y = -(region.bbox.max.y + pad);
    let height = region.bbox.height() + 2.0 * pad;
    let mut path_data = String::new();
    for ring in &region.rings {
        for (index, [x, y]) in ring.iter().enumerate() {
            let command = if index == 0 { 'M' } else { 'L' };
            write!(path_data, "{command}{x:.6} {y:.6} ")?;
        }
        path_data.push_str("Z ");
    }

    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{min_x:.3} {min_y:.3} {width:.3} {height:.3}'>\n  \
         <title>{}</title>\n  \
         <g transform='scale(1 -1)'>\n    \
         <path d='{path_data}' fill='#d87822' fill-opacity='0.9' fill-rule='nonzero'/>\n  \
         </g>\n</svg>\n",
        input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("gerber"),
    );
    std::fs::write(&output, svg)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(())
}
