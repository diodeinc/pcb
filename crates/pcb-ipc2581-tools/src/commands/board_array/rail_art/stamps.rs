//! The pieces a rail carries: the maker's tablet, the title block with its
//! Data Matrix, the sheet station, the fabrication coupons and the marks
//! between them.

use std::f64::consts::{SQRT_2, TAU};

use anyhow::{Context, Result};
use datamatrix::{DataMatrix, SymbolSize};

use super::shape::{Fill, Layer, Stamp, Text, chip, pen, rect_ring, row, silk_text, stack};

/// Nothing sits closer than this to the array edge, or further from it than
/// the band.
pub(super) const EDGE_MM: f64 = 0.8;
pub(super) const BAND_MM: f64 = 5.0;
/// The height of a piece that fills the band.
const FULL_MM: f64 = BAND_MM - EDGE_MM;
const RULE_MM: f64 = 0.15;
const CAPTION_MM: f64 = 1.0;
const VALUE_MM: f64 = 1.5;
/// The largest rectangular symbol the band holds at a module legend ink
/// resolves.
const DATA_MATRIX: SymbolSize = SymbolSize::Rect12x36;
const DATA_MATRIX_MODULE_MM: f64 = 0.3;
/// A printed symbol with one module of quiet zone around it, along the rail
/// and across it.
const DATA_MATRIX_MM: (f64, f64) = (38.0 * DATA_MATRIX_MODULE_MM, 14.0 * DATA_MATRIX_MODULE_MM);
const COUPON_MM: f64 = 2.4;
const COUPON_GAP_MM: f64 = 2.2;
/// Mask left between two openings of one letter is closed when narrower than
/// twice this: JLCPCB holds no web under 0.1 mm.
const MASK_WEB_CLOSE_MM: f64 = 0.06;

/// The Diode mark as round-pen strokes in a `LOGO_BOX`, y down.
const LOGO: [[f64; 4]; 8] = [
    [8.585, 51.02, 26.26, 51.02],
    [28.993, 23.834, 28.993, 78.126],
    [72.406, 24.105, 72.406, 77.937],
    [29.018, 23.881, 67.021, 50.776],
    [29.013, 78.032, 67.232, 51.612],
    [72.679, 51.02, 90.353, 51.02],
    [62.883, 18.046, 71.696, 23.582],
    [72.437, 77.914, 81.205, 83.422],
];
/// Left, top, width and height of the mark's ink, and its pen.
const LOGO_BOX: [f64; 4] = [4.8, 14.2, 89.4, 73.0];
const LOGO_PEN: f64 = 7.6;

/// What one side of the array says about itself.
#[derive(Debug, Clone)]
pub(super) struct Legend {
    /// The title block's cells in reading order: a caption and its value.
    pub fields: Vec<(&'static str, String)>,
    /// The words of the Data Matrix, the one to keep longest first.
    pub scan: Vec<String>,
    /// The A-series sheet the array fills, when it fills one.
    pub sheet: Option<&'static str>,
    /// The array's outline, as `74×105`.
    pub outline: String,
    pub boards: u32,
    pub top: bool,
    pub copper_layers: usize,
}

/// The maker's tablet: one piece of copper under two mask treatments. The
/// mark is mask left standing on plated copper; the name is cut through the
/// mask beside it.
pub(super) fn brand(fill: Fill) -> Result<Stamp> {
    let (h, pad, inset) = (FULL_MM, 0.75, 0.85);
    let mark_h = h - 2.0 * pad;
    let scale = mark_h / LOGO_BOX[3];
    let cell = LOGO_BOX[2] * scale + 2.0 * inset;
    let name = wordmark(fill)?;
    let mut stamp = Stamp::new(cell + 0.9 + name.w + 1.0, h);
    let tablet = fill.rounded_rect((0.0, 0.0, stamp.w, h), 0.7)?;
    let mark = LOGO
        .map(|[x0, y0, x1, y1]| {
            let at = |x: f64, y: f64| {
                (
                    inset + (x - LOGO_BOX[0]) * scale,
                    pad + mark_h - (y - LOGO_BOX[1]) * scale,
                )
            };
            [at(x0, y0), at(x1, y1)]
        })
        .to_vec();
    // The tablet's rounded end, squared off where the name begins.
    let opening = tablet.intersection(&fill.rounded_rect((-1.0, -1.0, cell, h + 1.0), 0.0)?)?;
    stamp.area(
        Layer::Mask,
        opening.difference(&fill.strokes(&mark, LOGO_PEN * scale)?)?,
    );
    stamp.area(Layer::Copper, tablet);
    stamp.place(&name, cell + 0.9, (h - name.h) / 2.0);
    Ok(stamp)
}

/// The company name as mask openings, justified on two lines.
fn wordmark(fill: Fill) -> Result<Stamp> {
    let cut = |text: Text| -> Result<Stamp> {
        let mut stamp = Stamp::new(text.w, text.cap);
        stamp.area(
            Layer::Mask,
            fill.strokes(&text.lines, text.pen)?
                .disk_close(MASK_WEB_CLOSE_MM)?,
        );
        Ok(stamp)
    };
    let (upper, lower) = (1.45, 1.05);
    let computers = Text::bold("COMPUTERS", lower, 0.06);
    let track = (computers.w - Text::bold("DIODE", upper, 0.0).w) / 4.0;
    Ok(stack(
        &[cut(Text::bold("DIODE", upper, track))?, cut(computers)?],
        3.0 - upper - lower,
    ))
}

/// The Data Matrix docked to a framed row of title-block cells holding
/// `fields`, each a small caption over its value. The symbol encodes as many
/// of the legend's words as it holds.
pub(super) fn title_block(
    fill: Fill,
    legend: &Legend,
    fields: &[(&'static str, String)],
) -> Result<Stamp> {
    let symbol = scan_symbol(&legend.scan).context("no label fits the Data Matrix")?;
    let (h, pad_u, half) = (FULL_MM, 0.75, RULE_MM / 2.0);
    let pad_v = (h - CAPTION_MM - VALUE_MM) / 3.0;
    let mut cells = Stamp::new(0.0, h);
    for (caption, value) in fields {
        let caption = Text::new(caption, CAPTION_MM, pen(CAPTION_MM, false), 0.05);
        let value = silk_text(value, VALUE_MM);
        let u = cells.w;
        if u > 0.0 {
            cells.line(Layer::Silk, (u, RULE_MM), (u, h - RULE_MM), RULE_MM);
        }
        cells.place(
            &caption.stamp(Layer::Silk),
            u + pad_u,
            h - pad_v - CAPTION_MM,
        );
        cells.place(&value, u + pad_u, pad_v);
        cells.w += caption.w.max(value.w) + 2.0 * pad_u;
    }
    if !fields.is_empty() {
        cells.frame(Layer::Silk, (half, half, cells.w - half, h - half), RULE_MM);
    }
    Ok(row(&[data_matrix(fill, &symbol)?, cells], 0.6))
}

/// The symbol for as many of `words` as it holds, an earlier word before a
/// later one: each is taken in turn when the message still fits with it.
/// `None` when none fits.
fn scan_symbol(words: &[String]) -> Option<DataMatrix> {
    let mut message = String::new();
    let mut symbol = None;
    for word in words {
        let longer = if message.is_empty() {
            word.clone()
        } else {
            format!("{message} {word}")
        };
        if let Ok(longer_symbol) = DataMatrix::encode(longer.as_bytes(), DATA_MATRIX) {
            (message, symbol) = (longer, Some(longer_symbol));
        }
    }
    symbol
}

/// An ECC 200 symbol in legend ink: its light modules and one module of
/// quiet zone printed, its dark modules left open.
fn data_matrix(fill: Fill, symbol: &DataMatrix) -> Result<Stamp> {
    let module = DATA_MATRIX_MODULE_MM;
    let bitmap = symbol.bitmap();
    let mut stamp = Stamp::new(DATA_MATRIX_MM.0, DATA_MATRIX_MM.1);
    let dark = bitmap
        .pixels()
        .map(|(column, row)| {
            let u = (column + 1) as f64 * module;
            let v = (bitmap.height() - row) as f64 * module;
            rect_ring((u, v, u + module, v + module))
        })
        .collect();
    stamp.area(
        Layer::Silk,
        fill.rounded_rect((0.0, 0.0, stamp.w, stamp.h), module * 0.6)?
            .difference(&fill.rings(dark)?)?,
    );
    // One background under every module: no balance copper shows through.
    stamp.area(
        Layer::Clear,
        fill.rounded_rect((0.0, 0.0, stamp.w, stamp.h), 0.0)?,
    );
    Ok(stamp)
}

/// The sheet station: the A-series nest with this sheet's share filled, then
/// the sheet's name and the side over the outline and the board count.
pub(super) fn sheet_station(fill: Fill, legend: &Legend) -> Result<Stamp> {
    let side = chip(
        fill,
        if legend.top { "TOP" } else { "BOTTOM" },
        VALUE_MM,
        (0.55, 0.35),
        0.35,
    )?;
    let name = match legend.sheet {
        Some(sheet) => row(&[silk_text(sheet, VALUE_MM), side], 0.8),
        None => side,
    };
    let detail = format!("{} {}-UP", legend.outline, legend.boards);
    let detail = Text::new(&detail, CAPTION_MM, pen(CAPTION_MM, false), 0.0).stamp(Layer::Silk);
    let gap = FULL_MM - name.h - CAPTION_MM;
    let words = stack(&[name, detail], gap);
    Ok(match legend.sheet.and_then(|sheet| nest(fill, sheet)) {
        Some(nest) => row(&[nest?, words], 0.9),
        None => words,
    })
}

/// An A4 halved down to A7, with `sheet`'s share of it filled.
/// `None` for a sheet outside the nest.
fn nest(fill: Fill, sheet: &str) -> Option<Result<Stamp>> {
    let (h, half) = (FULL_MM, RULE_MM / 2.0);
    let w = h / SQRT_2;
    let share = match sheet {
        "A4" => (0.0, 0.0, w, h),
        "A5" => (0.0, h / 2.0, w, h),
        "A6" => (0.0, 0.0, w / 2.0, h / 2.0),
        "A7" => (w / 2.0, 0.0, w, h / 4.0),
        _ => return None,
    };
    let mut stamp = Stamp::new(w, h);
    stamp.frame(Layer::Silk, (half, half, w - half, h - half), RULE_MM);
    for (from, to) in [
        ((RULE_MM, h / 2.0), (w - RULE_MM, h / 2.0)),
        ((w / 2.0, RULE_MM), (w / 2.0, h / 2.0)),
        ((w / 2.0, h / 4.0), (w - RULE_MM, h / 4.0)),
    ] {
        stamp.line(Layer::Silk, from, to, RULE_MM);
    }
    Some(fill.rounded_rect(share, 0.0).map(|share| {
        stamp.area(Layer::Silk, share);
        stamp
    }))
}

/// A metric reference graduation: two 10 mm blocks, numbered.
pub(super) fn graduation(fill: Fill) -> Result<Stamp> {
    let (tens, h, cap) = (2, 0.9, 1.1);
    let w = tens as f64 * 10.0;
    let mut stamp = Stamp::new(w, h + 0.4 + cap);
    stamp.frame(Layer::Silk, (0.0, 0.0, w, h), RULE_MM);
    for ten in (0..tens).step_by(2) {
        let u = ten as f64 * 10.0;
        stamp.area(Layer::Silk, fill.rounded_rect((u, 0.0, u + 10.0, h), 0.0)?);
    }
    for ten in 0..=tens {
        let numeral = Text::bold(&(ten * 10).to_string(), cap, -0.08);
        let u = (ten as f64 * 10.0 - numeral.w / 2.0).clamp(0.0, w - numeral.w);
        stamp.place(&numeral.stamp(Layer::Silk), u, h + 0.4);
    }
    Ok(stamp)
}

/// DIODE in Morse: one copper bar, its dots and dashes cut through the mask
/// over it.
pub(super) fn morse(fill: Fill) -> Result<Stamp> {
    const UNIT: f64 = 0.55;
    // Copper around the marks.
    const PAD: f64 = 0.45;
    // D, I, O, D, E; a dash is three units.
    const WORD: [&[f64]; 5] = [
        &[3.0, 1.0, 1.0],
        &[1.0, 1.0],
        &[3.0, 3.0, 3.0],
        &[3.0, 1.0, 1.0],
        &[1.0],
    ];
    let v = PAD + UNIT / 2.0;
    let mut marks = Vec::new();
    let mut u = PAD;
    for letter in WORD {
        for units in letter {
            let length = units * UNIT;
            marks.push([(u + UNIT / 2.0, v), (u + length - UNIT / 2.0, v)]);
            u += length + UNIT;
        }
        u += 2.0 * UNIT;
    }
    let (w, h) = (u - 3.0 * UNIT + PAD, UNIT + 2.0 * PAD);
    let mut stamp = Stamp::new(w, h);
    stamp.area(Layer::Copper, fill.rounded_rect((0.0, 0.0, w, h), h / 2.0)?);
    stamp.area(Layer::Mask, fill.strokes(&marks, UNIT)?);
    Ok(stamp)
}

/// Room for a second Data Matrix, marked later with a serial number: solid
/// copper under unbroken mask, with no balance pattern and no legend ink.
pub(super) fn serial_blank(fill: Fill) -> Result<Stamp> {
    let (w, h) = DATA_MATRIX_MM;
    let mut stamp = Stamp::new(w, h);
    stamp.area(Layer::Copper, fill.rounded_rect((0.0, 0.0, w, h), 0.0)?);
    Ok(stamp)
}

/// The fabrication coupons in one strip, each behind its caption.
pub(super) fn coupons(fill: Fill, legend: &Legend) -> Result<Stamp> {
    let mut coupons = vec![("REG", registration(fill)?), ("L/S", line_space(fill)?)];
    // Read from the top through every layer, so drawn once, on the top side.
    if legend.top {
        coupons.insert(0, ("LYR", layer_ladder(fill, legend.copper_layers)?));
    }
    let captioned = coupons
        .into_iter()
        .map(|(caption, coupon)| {
            let caption = chip(fill, caption, VALUE_MM, (0.4, 0.45), 0.2)?;
            Ok(row(&[caption, coupon], 0.6))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(row(&captioned, COUPON_GAP_MM))
}

/// Layer ladder: numeral `n` is copper on layer `n` alone, under one mask
/// window that no other copper crosses, so the stackup's order and count
/// read through the laminate.
fn layer_ladder(fill: Fill, layers: usize) -> Result<Stamp> {
    let h = COUPON_MM;
    // Two digits take a wider cell.
    let cell = if layers < 10 { 1.7 } else { 2.0 };
    let w = layers as f64 * cell + 0.6;
    let mut stamp = Stamp::new(w, h);
    stamp.area(
        Layer::Keepout,
        fill.rounded_rect((-0.4, -0.4, w + 0.4, h + 0.4), 0.0)?,
    );
    stamp.area(Layer::Mask, fill.rounded_rect((0.0, 0.0, w, h), 0.4)?);
    for layer in 0..layers {
        let numeral = (layer + 1).to_string();
        // Two digits are smaller and set apart: at the font's own spacing
        // "11" leaves 0.15 mm between its strokes, too close to the finest
        // space copper holds.
        let (cap, track) = if numeral.len() == 1 {
            (1.25, 0.0)
        } else {
            (1.0, 0.08)
        };
        let numeral = Text::bold(&numeral, cap, track);
        stamp.place(
            &numeral.stamp(Layer::Stack(layer)),
            0.3 + layer as f64 * cell + (cell - numeral.w) / 2.0,
            (h - cap) / 2.0,
        );
    }
    Ok(stamp)
}

/// Registration target: a copper bullseye in a round mask window, with
/// legend ticks on its axis. Mask and legend offsets show against the copper.
fn registration(fill: Fill) -> Result<Stamp> {
    let h = COUPON_MM;
    let w = h + 2.2;
    let centre = (w / 2.0, h / 2.0);
    let mut stamp = Stamp::new(w, h);
    stamp.area(Layer::Mask, fill.disc(centre, h / 2.0)?);
    let ring = h / 2.0 - 0.45;
    stamp.area(
        Layer::Copper,
        fill.disc(centre, ring + 0.1)?
            .difference(&fill.disc(centre, ring - 0.1)?)?
            .union(&fill.disc(centre, 0.28)?)?,
    );
    stamp.line(Layer::Silk, (0.09, centre.1), (0.66, centre.1), 0.18);
    stamp.line(
        Layer::Silk,
        (w - 0.66, centre.1),
        (w - 0.09, centre.1),
        0.18,
    );
    Ok(stamp)
}

/// Line and space comb: five lines at each width, spaced by their width,
/// from 0.25 mm down to just above the 5 mil the standard PDK allows, so the
/// array still passes the DFM check it is held to.
fn line_space(fill: Fill) -> Result<Stamp> {
    let h = COUPON_MM;
    let mut rings = Vec::new();
    let mut u = 0.5;
    for width in [0.25, 0.2, 0.15, 0.13] {
        for line in 0..5 {
            let u = u + line as f64 * 2.0 * width;
            rings.push(rect_ring((u, 0.35, u + width, h - 0.35)));
        }
        u += 9.0 * width + 0.55;
    }
    let w = u - 0.05;
    let mut stamp = Stamp::new(w, h);
    stamp.area(Layer::Mask, fill.rounded_rect((0.0, 0.0, w, h), 0.4)?);
    stamp.area(Layer::Copper, fill.rings(rings)?);
    Ok(stamp)
}

/// A legend ring around a global fiducial, a different one for each, so the
/// four are told apart: none, solid, dashed, dotted.
pub(super) fn fiducial_ring(fill: Fill, index: usize) -> Result<Stamp> {
    const RADIUS: f64 = 1.75;
    let mut stamp = Stamp::new(0.0, 0.0);
    let at = |turn: f64| {
        let (sin, cos) = (turn * TAU).sin_cos();
        (RADIUS * cos, RADIUS * sin)
    };
    match index {
        1 => {
            for step in 0..48 {
                stamp.line(
                    Layer::Silk,
                    at(step as f64 / 48.0),
                    at((step + 1) as f64 / 48.0),
                    0.2,
                );
            }
        }
        2 => {
            for dash in 0..8 {
                for step in 0..5 {
                    let turn = |step: usize| (dash as f64 + 0.1 + 0.5 * step as f64 / 5.0) / 8.0;
                    stamp.line(Layer::Silk, at(turn(step)), at(turn(step + 1)), 0.2);
                }
            }
        }
        3 => {
            for dot in 0..14 {
                stamp.area(Layer::Silk, fill.disc(at(dot as f64 / 14.0), 0.16)?);
            }
        }
        _ => {}
    }
    Ok(stamp)
}

/// A row or column label: a letter or a number open in a printed chip.
pub(super) fn grid_label(fill: Fill, label: &str) -> Result<Stamp> {
    chip(fill, label, 1.6, (0.45, 0.35), 0.3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word that does not fit is passed over, not the end of the message:
    /// two 16-character labels overflow together, and the hash still fits
    /// beside the first.
    #[test]
    fn the_data_matrix_keeps_every_word_that_fits() {
        let read = |words: &[&str]| {
            let words = words.iter().map(|word| word.to_string());
            let bitmap = scan_symbol(&words.collect::<Vec<_>>())?.bitmap();
            let mut modules = vec![false; bitmap.width() * bitmap.height()];
            for (column, row) in bitmap.pixels() {
                modules[row * bitmap.width() + column] = true;
            }
            assert_eq!((bitmap.width(), bitmap.height()), (36, 12));
            String::from_utf8(DataMatrix::decode(&modules, bitmap.width()).unwrap()).ok()
        };
        assert_eq!(
            read(&["PCB-ETMK3", "DO-RVX0Y", "FABCDE"]).as_deref(),
            Some("PCB-ETMK3 DO-RVX0Y FABCDE")
        );
        assert_eq!(
            read(&["PCB-PRODUCTION-A", "DO-PRODUCTION-42", "FABCDE"]).as_deref(),
            Some("PCB-PRODUCTION-A FABCDE")
        );
        assert_eq!(read(&["A".repeat(64).as_str()]), None);
    }
}
