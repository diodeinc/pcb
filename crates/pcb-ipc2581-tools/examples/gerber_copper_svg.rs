//! Render one Gerber layer's composed final image to a filled SVG.
//!
//! Development harness for visually inspecting exported copper: the Gerber is
//! parsed and composed exactly as `pcbc ipc2581 dfm` sees it, so the picture
//! matches manufacturing output rather than any IPC-2581 view.
//!
//! Rings paint back-to-front by containment depth — copper for boundaries,
//! background for holes — so no path element grows past what common SVG
//! rasterizers accept, even for planes perforated by tens of thousands of
//! voids.

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_ir::geom::region::rings_from_contours;
use pcb_ir::geom::{ContourSet, FillRule};

const COPPER: &str = "#d87822";
const BACKGROUND: &str = "#ffffff";

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

    // A containing ring always has the larger unsigned area, so painting in
    // descending area order layers islands over holes over boundaries.
    let mut rings = region
        .rings
        .iter()
        .map(|ring| (signed_ring_area(ring), ring))
        .collect::<Vec<_>>();
    rings.sort_by(|(left, _), (right, _)| right.abs().total_cmp(&left.abs()));

    // Same-color runs split at a fixed size so no attribute outgrows the
    // 10 MB limit libxml2-based rasterizers enforce.
    const MAX_PATH_DATA_BYTES: usize = 2_000_000;
    let mut body = String::new();
    let mut fill = "";
    let mut path_data = String::new();
    for (area, ring) in rings {
        let ring_fill = if area >= 0.0 { COPPER } else { BACKGROUND };
        if (ring_fill != fill || path_data.len() > MAX_PATH_DATA_BYTES) && !path_data.is_empty() {
            writeln!(body, "    <path d='{path_data}' fill='{fill}'/>")?;
            path_data.clear();
        }
        fill = ring_fill;
        for (index, [x, y]) in ring.iter().enumerate() {
            let command = if index == 0 { 'M' } else { 'L' };
            write!(path_data, "{command}{x:.6} {y:.6} ")?;
        }
        path_data.push_str("Z ");
    }
    if !path_data.is_empty() {
        writeln!(body, "    <path d='{path_data}' fill='{fill}'/>")?;
    }

    let pad = 1.0;
    let min_x = region.bbox.min.x - pad;
    let width = region.bbox.width() + 2.0 * pad;
    let min_y = -(region.bbox.max.y + pad);
    let height = region.bbox.height() + 2.0 * pad;
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='{min_x:.3} {min_y:.3} {width:.3} {height:.3}'>\n  \
         <title>{}</title>\n  \
         <g transform='scale(1 -1)'>\n{body}  </g>\n</svg>\n",
        input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("gerber"),
    );
    std::fs::write(&output, svg)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok(())
}

fn signed_ring_area(ring: &[[f64; 2]]) -> f64 {
    ring.iter()
        .zip(ring.iter().cycle().skip(1))
        .map(|([x0, y0], [x1, y1])| x0 * y1 - x1 * y0)
        .sum::<f64>()
        / 2.0
}
