//! KiCad's stroke font laid out the way KiCad lays it out: the glyphs of
//! `newstroke.rs`, KiCad's line spacing, justification, italic tilt and
//! markup, so silkscreen text lands where the plot puts it.

use std::sync::OnceLock;

use crate::geom::{Vec2, rotate_kicad};
use crate::newstroke::GLYPHS;

/// Glyph coordinates are stored on a 21-unit grid.
const STROKE_SCALE: f64 = 1.0 / 21.0;
/// Glyph rows are stored shifted so most shapes have non-negative rows.
const FONT_OFFSET: f64 = -8.0;
const ITALIC_TILT: f64 = 1.0 / 8.0;
const INTERLINE_PITCH: f64 = 1.68;
/// KiCad's stroke font shrinks the interline to match legacy spacing.
const LEGACY_INTERLINE: f64 = 0.9583;
const OVERBAR_HEIGHT: f64 = 1.23;
const SUPER_SUB_SIZE: f64 = 0.8;
const SUPER_HEIGHT_OFFSET: f64 = 0.35;
const SUB_HEIGHT_OFFSET: f64 = 0.15;

/// A decoded glyph: polylines in a unit box, and the advance width.
struct Glyph {
    strokes: Vec<Vec<Vec2>>,
    advance: f64,
}

fn glyphs() -> &'static [Glyph] {
    static DECODED: OnceLock<Vec<Glyph>> = OnceLock::new();
    DECODED.get_or_init(|| GLYPHS.iter().map(|g| decode(g.as_bytes())).collect())
}

fn decode(g: &[u8]) -> Glyph {
    let coordinate = |c: u8| (c as f64 - b'R' as f64) * STROKE_SCALE;
    let mut strokes: Vec<Vec<Vec2>> = Vec::new();
    let mut advance = 0.0;
    let mut start_x = 0.0;
    let mut current: Vec<Vec2> = Vec::new();
    for (i, pair) in g.as_chunks::<2>().0.iter().enumerate() {
        if i == 0 {
            start_x = coordinate(pair[0]);
            advance = coordinate(pair[1]) - start_x;
        } else if pair == b" R" {
            if !current.is_empty() {
                strokes.push(std::mem::take(&mut current));
            }
        } else {
            current.push(Vec2::new(
                coordinate(pair[0]) - start_x,
                (pair[1] as f64 - b'R' as f64 + FONT_OFFSET) * STROKE_SCALE,
            ));
        }
    }
    if !current.is_empty() {
        strokes.push(current);
    }
    Glyph { strokes, advance }
}

fn glyph_of(c: char) -> &'static Glyph {
    let table = glyphs();
    let index = (c as usize).wrapping_sub(' ' as usize);
    table
        .get(index)
        .unwrap_or_else(|| &table['?' as usize - ' ' as usize])
}

/// How a text is drawn, as KiCad stores it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TextStyle {
    /// Glyph width and height in mm.
    pub(crate) size: Vec2,
    /// Explicit stroke width; zero for KiCad's automatic width.
    pub(crate) thickness: f64,
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    /// -1 left, 0 centre, 1 right.
    pub(crate) halign: i8,
    /// -1 top, 0 centre, 1 bottom.
    pub(crate) valign: i8,
    pub(crate) mirror: bool,
    /// Rotation in degrees, KiCad's sense.
    pub(crate) angle: f64,
    /// The outline font's face name; the stroke font when absent.
    pub(crate) face: Option<String>,
}

impl TextStyle {
    /// The pen width KiCad draws with: the stated thickness, or a share of
    /// the glyph width (a larger one when bold), never more than a quarter
    /// of the smaller size.
    pub(crate) fn pen_width(&self) -> f64 {
        let width = if self.thickness > 1e-6 {
            self.thickness
        } else if self.bold {
            self.size.x / 5.0
        } else {
            self.size.x / 8.0
        };
        width.min(self.size.x.min(self.size.y) * 0.25)
    }
}

/// The strokes of `text` anchored at `at`, in board coordinates (y down),
/// one polyline per pen stroke. Lines are split on newlines and placed
/// with KiCad's interline and justification rules.
pub(crate) fn strokes(text: &str, at: Vec2, style: &TextStyle) -> Vec<Vec<Vec2>> {
    let size = style.size;
    let pen = style.pen_width();
    let lines = split_lines(text);
    let interline = size.y * INTERLINE_PITCH * LEGACY_INTERLINE;
    let extents: Vec<f64> = lines.iter().map(|l| line_extent(l, size)).collect();
    let height = size.y * 1.17 + interline * (lines.len() as f64 - 1.0);
    // Fudge factors KiCad keeps to match its 6.0 positioning.
    let offset = Vec2::new(pen / 1.52, size.y - pen * 0.052);
    let offset_y = match style.valign {
        -1 => offset.y,
        0 => offset.y - height / 2.0,
        _ => offset.y - height,
    };
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let offset_x = match style.halign {
            -1 => offset.x,
            0 => -extents[i] / 2.0,
            _ => -(extents[i] + offset.x),
        };
        let start = at + Vec2::new(offset_x, offset_y + interline * i as f64);
        let mut cursor = start;
        for run in markup(line) {
            cursor = draw_run(&run, cursor, at, style, &mut out);
        }
    }
    out
}

/// `text` broken into lines no wider than `column`, as KiCad wraps a
/// text box: a word is a run of markup or a space-delimited token, a
/// word that would overflow the column starts a new line, and the
/// spaces before it are dropped, as are the spaces ending a line.
pub(crate) fn wrap(
    text: &str,
    column: f64,
    style: &TextStyle,
    measure: impl Fn(&str, Vec2) -> f64,
) -> String {
    let size = style.size;
    let limit = column - style.pen_width();
    let space = measure(" ", size);
    let lines = split_lines(text);
    let mut out = String::new();
    for (k, line) in lines.iter().enumerate() {
        let mut words: Vec<(String, f64)> = Vec::new();
        for run in markup(line) {
            let pieces: Vec<(String, f64)> = if run.script != 0 || run.overbar {
                let escape = match run.script {
                    1 => '^',
                    -1 => '_',
                    _ => '~',
                };
                let run_size = if run.script == 0 {
                    size
                } else {
                    size * SUPER_SUB_SIZE
                };
                vec![(
                    format!("{escape}{{{}}}", run.text),
                    measure(&run.text, run_size),
                )]
            } else {
                run.text
                    .split_inclusive(' ')
                    .map(|token| {
                        let bare = token.trim_end();
                        let measured = if bare.is_empty() { token } else { bare };
                        (token.to_owned(), measure(measured, size))
                    })
                    .collect()
            };
            for (word, width) in pieces {
                match words.last_mut() {
                    Some(last) if !last.0.ends_with(' ') => {
                        last.0.push_str(&word);
                        last.1 += width;
                    }
                    _ => words.push((word, width)),
                }
            }
        }
        let mut bury = false;
        let mut line_width = 0.0;
        let mut pending = 0usize;
        for (word, width) in words {
            if pending > 0 && line_width + pending as f64 * space + width > limit {
                out.push('\n');
                line_width = 0.0;
                pending = 0;
                bury = true;
            }
            if word == " " {
                pending += 1;
                continue;
            }
            if bury {
                bury = false;
            } else {
                out.extend(std::iter::repeat_n(' ', pending));
                line_width += pending as f64 * space;
            }
            match word.strip_suffix(' ') {
                Some(bare) => {
                    out.push_str(bare);
                    pending = 1;
                }
                None => {
                    out.push_str(&word);
                    pending = 0;
                }
            }
            line_width += width;
        }
        if k + 1 < lines.len() {
            out.push('\n');
        }
    }
    out
}

/// The lines of a text as KiCad splits them: a trailing newline adds
/// no line.
pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.len() > 1 && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Width of one line as KiCad measures it for justification: the sum
/// of the glyph advances, trailing inter-character space included.
pub(crate) fn line_extent(line: &str, size: Vec2) -> f64 {
    let mut cursor = 0.0;
    for run in markup(line) {
        let run_size = if run.script == 0 {
            size
        } else {
            size * SUPER_SUB_SIZE
        };
        cursor += advance_of(&run.text, run_size);
    }
    cursor
}

/// Width of `text` in the stroke font at `size`, advances summed.
pub(crate) fn advance_of(text: &str, size: Vec2) -> f64 {
    let space = glyph_of(' ').advance;
    let mut x = 0.0;
    let mut count = 0usize;
    for c in text.chars() {
        if c == '\t' {
            count = (count / 4 + 1) * 4 - 1;
            let mut next = size.x * count as f64 + size.x * space;
            while next <= x {
                count += 4;
                next += size.x * 4.0;
            }
            x = next;
        } else if c == ' ' {
            x += size.x * space;
        } else {
            x += size.x * glyph_of(c).advance;
        }
        count += 1;
    }
    x
}

/// A stretch of text with one style: plain, superscript, subscript, with
/// or without an overbar.
pub(crate) struct Run {
    pub(crate) text: String,
    /// 0 plain, 1 superscript, -1 subscript.
    pub(crate) script: i8,
    pub(crate) overbar: bool,
}

/// Split KiCad markup: `~{x}` draws an overbar, `^{x}` a superscript and
/// `_{x}` a subscript. Anything else is literal.
pub(crate) fn markup(line: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '~' | '^' | '_') && chars.get(i + 1) == Some(&'{') {
            let mut depth = 0;
            let mut end = None;
            for (j, &d) in chars.iter().enumerate().skip(i + 1) {
                match d {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                if !plain.is_empty() {
                    runs.push(Run {
                        text: std::mem::take(&mut plain),
                        script: 0,
                        overbar: false,
                    });
                }
                runs.push(Run {
                    text: chars[i + 2..end].iter().collect(),
                    script: match c {
                        '^' => 1,
                        '_' => -1,
                        _ => 0,
                    },
                    overbar: c == '~',
                });
                i = end + 1;
                continue;
            }
        }
        plain.push(c);
        i += 1;
    }
    if !plain.is_empty() {
        runs.push(Run {
            text: plain,
            script: 0,
            overbar: false,
        });
    }
    runs
}

/// Draw one run from `cursor`; returns the cursor after it. `origin` is
/// the text anchor, about which the whole text is mirrored and rotated.
fn draw_run(
    run: &Run,
    cursor: Vec2,
    origin: Vec2,
    style: &TextStyle,
    out: &mut Vec<Vec<Vec2>>,
) -> Vec2 {
    let mut size = style.size;
    let mut pen = cursor;
    match run.script {
        1 => {
            size *= SUPER_SUB_SIZE;
            pen.y -= size.y * SUPER_HEIGHT_OFFSET;
        }
        -1 => {
            size *= SUPER_SUB_SIZE;
            pen.y += size.y * SUB_HEIGHT_OFFSET;
        }
        _ => {}
    }
    let tilt = if style.italic { ITALIC_TILT } else { 0.0 };
    let place = |p: Vec2| -> Vec2 {
        let mut p = p;
        if style.mirror {
            p.x = origin.x - (p.x - origin.x);
        }
        if style.angle != 0.0 {
            p = origin + rotate_kicad(p - origin, style.angle);
        }
        p
    };
    let space = glyph_of(' ').advance;
    let start_x = pen.x;
    let mut count = 0usize;
    for c in run.text.chars() {
        if c == '\t' {
            count = (count / 4 + 1) * 4 - 1;
            let mut next = start_x + style.size.x * count as f64 + style.size.x * space;
            while next <= pen.x {
                count += 4;
                next += style.size.x * 4.0;
            }
            pen.x = next;
        } else if c == ' ' {
            pen.x += size.x * space;
        } else {
            let glyph = glyph_of(c);
            for stroke in &glyph.strokes {
                out.push(
                    stroke
                        .iter()
                        .map(|p| {
                            let scaled =
                                Vec2::new(p.x * size.x - p.y * size.y * tilt, p.y * size.y);
                            place(scaled + pen)
                        })
                        .collect(),
                );
            }
            pen.x += size.x * glyph.advance;
        }
        count += 1;
    }
    let next = Vec2::new(pen.x, cursor.y);
    if run.overbar {
        let trim = style.size.x * 0.1;
        let y = cursor.y - style.size.y * OVERBAR_HEIGHT;
        out.push(vec![
            place(Vec2::new(cursor.x + trim, y)),
            place(Vec2::new(next.x - trim, y)),
        ]);
    }
    next
}
