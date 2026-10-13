//! KiCad's newstroke font, the stroke font KiCad draws text with: every
//! glyph of `common/newstroke_font.cpp`, U+0020 to U+FFEE.

mod glyphs;

/// The size of one font grid unit relative to the text size.
const SCALE: f64 = 1.0 / 21.0;

/// A glyph scaled to a unit text size, as KiCad's `STROKE_FONT` loads it.
#[derive(Debug, Clone, Copy)]
pub struct Glyph {
    /// The distance to the next glyph.
    pub advance: f64,
    start_x: f64,
    /// Hershey pairs, each char offset from `'R'`, with `" R"` lifting the pen.
    strokes: &'static str,
}

impl Glyph {
    /// The polylines drawn with the pen down, as `(x, y)` with `y` down
    /// from the baseline.
    pub fn strokes(&self) -> impl Iterator<Item = impl Iterator<Item = (f64, f64)>> {
        self.strokes.split(" R").filter(|s| !s.is_empty()).map(|s| {
            s.as_bytes().as_chunks::<2>().0.iter().map(|p| {
                // KiCad's FONT_OFFSET of -8 moves the origin to the baseline.
                let y = (f64::from(p[1]) - f64::from(b'R') - 8.0) * SCALE;
                (grid(p[0]) - self.start_x, y)
            })
        })
    }
}

/// The glyph KiCad draws for `c`: `?` outside the font.
pub fn glyph(c: char) -> Glyph {
    let index = (c as usize).wrapping_sub(' ' as usize);
    let raw = glyphs::GLYPHS
        .get(index)
        .unwrap_or(&glyphs::GLYPHS['?' as usize - ' ' as usize]);
    let header = raw.as_bytes();
    let start_x = grid(header[0]);
    Glyph {
        advance: grid(header[1]) - start_x,
        start_x,
        strokes: &raw[2..],
    }
}

/// A Hershey coordinate char, scaled.
fn grid(c: u8) -> f64 {
    (f64::from(c) - f64::from(b'R')) * SCALE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_glyphs() {
        // "I[MUWU RK[RFY[": the crossbar, then both legs through the apex.
        let a = glyph('A');
        let units = |v: f64| (v * 21.0).round() as i32;
        assert_eq!(units(a.advance), 18);
        let strokes: Vec<Vec<_>> = a
            .strokes()
            .map(|s| s.map(|(x, y)| (units(x), units(y))).collect())
            .collect();
        assert_eq!(
            strokes,
            [vec![(4, -5), (14, -5)], vec![(2, 1), (9, -20), (16, 1)]]
        );
        assert_eq!(glyph(' ').strokes().count(), 0);
        assert_eq!(glyph('\u{ffef}').strokes, glyph('?').strokes);
        assert_ne!(glyph('\u{ffee}').strokes, glyph('?').strokes);
    }
}
