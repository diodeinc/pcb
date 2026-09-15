//! KiCad's outline fonts: TrueType glyphs shaped with HarfBuzz's rules
//! and laid out the way KiCad lays them out, as filled rings. A face
//! comes from the board's embedded fonts; any other face is Liberation
//! Sans, the metric-compatible stand-in for Arial, the face boards name
//! most, and what a Linux kicad-cli substitutes for it.

use rustybuzz::ttf_parser::{GlyphId, OutlineBuilder, fonts_in_collection, name_id};
use rustybuzz::{Face, UnicodeBuffer};

use crate::board::Board;
use crate::font::{TextStyle, markup, split_lines};
use crate::geom::{Vec2, rotate_kicad};

/// KiCad draws an outline glyph's em 1.4 times the text size.
const SIZE_COMPENSATION: f64 = 1.4;
/// Sub- and superscripts are shaped at KiCad's rounded 0.64 face size.
const SUPER_SUB_SIZE: f64 = 918.0 / 1433.6;
const SUB_OFFSET: f64 = -0.25;
const SUPER_OFFSET: f64 = 0.45;
const INTERLINE_PITCH: f64 = 1.68;
const OVERBAR_HEIGHT: f64 = 1.23;
/// KiCad's synthetic italic is a 12 degree shear.
const ITALIC_SLANT: f64 = 12.0;
/// Tabs stop every four space widths of 0.6 em.
const TAB_WIDTH: f64 = 4.0 * 0.6;
/// Curves are flattened within two of KiCad's 1433.6 units per em.
const FLATTEN_ERROR: f64 = 2.0 / 1433.6;

/// Liberation Sans in its four styles, indexed by bold then italic.
static LIBERATION: [[&[u8]; 2]; 2] = [
    [
        include_bytes!("../fonts/LiberationSans-Regular.ttf"),
        include_bytes!("../fonts/LiberationSans-Italic.ttf"),
    ],
    [
        include_bytes!("../fonts/LiberationSans-Bold.ttf"),
        include_bytes!("../fonts/LiberationSans-BoldItalic.ttf"),
    ],
];

/// One face of an embedded font file: the names it answers to, its
/// style, and where it is.
struct Font {
    names: Vec<String>,
    bold: bool,
    italic: bool,
    file: usize,
    index: u32,
}

/// The fonts a board's texts can be drawn with.
pub(crate) struct Fonts {
    files: Vec<Vec<u8>>,
    embedded: Vec<Font>,
}

/// A face picked for one text: the font and whether it is sheared into
/// an italic the file does not have.
pub(crate) struct Loaded<'a> {
    face: Face<'a>,
    slant: bool,
}

impl Fonts {
    /// The board's embedded fonts, decoded, every face of a collection
    /// on its own. A file that is not a font is noted and skipped.
    pub(crate) fn load(board: &Board, warnings: &mut Vec<String>) -> Self {
        let mut files = Vec::new();
        let mut embedded = Vec::new();
        for file in board.embedded.iter().filter(|f| f.font) {
            let data = match crate::decode_embedded(file.data) {
                Ok(data) => data,
                Err(err) => {
                    warnings.push(format!("embedded font {}: {err}", file.name));
                    continue;
                }
            };
            let count = fonts_in_collection(&data).unwrap_or(1);
            let faces: Vec<(u32, Face)> = (0..count)
                .filter_map(|index| Face::from_slice(&data, index).map(|f| (index, f)))
                .collect();
            if faces.is_empty() {
                warnings.push(format!("embedded font {}: not a readable font", file.name));
                continue;
            }
            for (index, face) in &faces {
                let names = face
                    .names()
                    .into_iter()
                    .filter(|n| {
                        matches!(
                            n.name_id,
                            name_id::FAMILY | name_id::FULL_NAME | name_id::TYPOGRAPHIC_FAMILY
                        )
                    })
                    .filter_map(|n| n.to_string())
                    .map(|n| n.to_lowercase())
                    .collect();
                embedded.push(Font {
                    names,
                    bold: face.is_bold(),
                    italic: face.is_italic(),
                    file: files.len(),
                    index: *index,
                });
            }
            files.push(data);
        }
        Self { files, embedded }
    }

    /// Every name an embedded face answers to, lowercased.
    pub(crate) fn names(&self) -> Vec<String> {
        self.embedded.iter().flat_map(|f| f.names.clone()).collect()
    }

    /// The font for a face name and style, as KiCad picks it: a name
    /// that says bold is bold, an embedded family is preferred in its
    /// closest style, and anything else is Liberation Sans in the style
    /// asked for.
    pub(crate) fn resolve(&self, name: &str, bold: bool, italic: bool) -> Loaded<'_> {
        let lower = name.to_lowercase();
        let bold = bold
            || ["bold", "heavy", "black", "thick", "dark"]
                .iter()
                .any(|w| lower.contains(w));
        let family: Vec<&Font> = self
            .embedded
            .iter()
            .filter(|f| f.names.contains(&lower))
            .collect();
        let closest = family
            .iter()
            .copied()
            .min_by_key(|f| usize::from(f.bold != bold) * 2 + usize::from(f.italic != italic));
        let (data, index, has_italic): (&[u8], u32, bool) = match closest {
            Some(font) => (&self.files[font.file], font.index, font.italic),
            None => (
                LIBERATION[usize::from(bold)][usize::from(italic)],
                0,
                italic,
            ),
        };
        Loaded {
            face: Face::from_slice(data, index).expect("font was readable when loaded"),
            slant: italic && !has_italic,
        }
    }
}

impl Loaded<'_> {
    /// The width of `text` at `size` and script, as KiCad measures it
    /// for wrapping and justification: the shaped advances, tabs
    /// included.
    pub(crate) fn advance(&self, text: &str, size: Vec2, script: i8) -> f64 {
        let mut sink = Vec::new();
        self.run(text, size, script, Vec2::ZERO, Vec2::ZERO, &mut sink)
    }

    /// The glyph rings of `text` anchored at `at`, in board coordinates,
    /// plus the overbar strokes, laid out with KiCad's line spacing,
    /// justification and markup.
    pub(crate) fn glyphs(
        &self,
        text: &str,
        at: Vec2,
        style: &TextStyle,
    ) -> (Vec<Vec<Vec2>>, Vec<Vec<Vec2>>) {
        let size = style.size;
        let lines = split_lines(text);
        let interline = size.y * INTERLINE_PITCH;
        let height = size.y * 1.17 + interline * (lines.len() as f64 - 1.0);
        let offset_y = match style.valign {
            -1 => size.y,
            0 => size.y - height / 2.0,
            _ => size.y - height,
        };
        let place = |p: Vec2| -> Vec2 {
            let mut p = p;
            if style.mirror {
                p.x = at.x - (p.x - at.x);
            }
            if style.angle != 0.0 {
                p = at + rotate_kicad(p - at, style.angle);
            }
            p
        };
        let mut rings = Vec::new();
        let mut strokes = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let runs = markup(line);
            // KiCad measures a line from the anchor's x with tab stops
            // counted from zero, and draws it from the justified start
            // with tab stops counted from the anchor.
            let mut measured = at.x;
            for run in &runs {
                let cursor = Vec2::new(measured, 0.0);
                measured += self.run(
                    &run.text,
                    size,
                    run.script,
                    cursor,
                    Vec2::ZERO,
                    &mut Vec::new(),
                );
            }
            let extent = measured - at.x;
            let offset_x = match style.halign {
                -1 => 0.0,
                0 => -extent / 2.0,
                _ => -extent,
            };
            let start = at + Vec2::new(offset_x, offset_y + interline * i as f64);
            let mut cursor = start;
            for run in &runs {
                let mut local = Vec::new();
                let end = cursor.x + self.run(&run.text, size, run.script, cursor, at, &mut local);
                rings.extend(
                    local
                        .into_iter()
                        .map(|r| r.into_iter().map(place).collect()),
                );
                if run.overbar {
                    let trim = size.x * 0.1;
                    let y = cursor.y - size.y * OVERBAR_HEIGHT;
                    strokes.push(vec![
                        place(Vec2::new(cursor.x + trim, y)),
                        place(Vec2::new(end - trim, y)),
                    ]);
                }
                cursor.x = end;
            }
        }
        (rings, strokes)
    }

    /// Shape one run of text from `cursor` and push its glyph rings, in
    /// unrotated board coordinates; returns the run's advance. `script`
    /// is 1 for a superscript and -1 for a subscript, which shrink the
    /// glyphs and shift them. Tabs stop at the next tab width from
    /// `origin`.
    fn run(
        &self,
        text: &str,
        size: Vec2,
        script: i8,
        cursor: Vec2,
        origin: Vec2,
        rings: &mut Vec<Vec<Vec2>>,
    ) -> f64 {
        let face = &self.face;
        let upem = f64::from(face.units_per_em());
        let scale = if script == 0 { 1.0 } else { SUPER_SUB_SIZE };
        let em = size * SIZE_COMPENSATION * scale;
        let shift = em.y
            * match script {
                1 => SUPER_OFFSET,
                -1 => SUB_OFFSET,
                _ => 0.0,
            };
        // KiCad's synthetic italic is FreeType's shear matrix, which
        // also narrows the glyph by the cosine of the slant.
        let (shear, narrow) = if self.slant {
            let (s, c) = ITALIC_SLANT.to_radians().sin_cos();
            (s, c)
        } else {
            (0.0, 1.0)
        };
        let tab = size.x * TAB_WIDTH;
        let mut x = cursor.x;
        for (k, piece) in text.split('\t').enumerate() {
            if k > 0 {
                // KiCad's intrusion is a C++ remainder: negative left of
                // the origin, where a justified line can start, so the
                // tab then jumps a whole extra stop.
                x += tab - (x - origin.x) % tab;
            }
            let mut buffer = UnicodeBuffer::new();
            buffer.push_str(piece);
            let shaped = rustybuzz::shape(face, &[], buffer);
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                // Font units, y up, to millimetres, y down, about the pen.
                let pen = Vec2::new(
                    x + f64::from(pos.x_offset) / upem * em.x,
                    cursor.y - f64::from(pos.y_offset) / upem * em.y - shift,
                );
                let to_board = |p: kurbo::Point| {
                    Vec2::new(
                        pen.x + (p.x * narrow + p.y * shear) / upem * em.x,
                        pen.y - p.y / upem * em.y,
                    )
                };
                let mut path = Outline::default();
                face.outline_glyph(GlyphId(info.glyph_id as u16), &mut path);
                let tolerance = FLATTEN_ERROR * upem;
                let mut ring: Vec<Vec2> = Vec::new();
                kurbo::flatten(path.0.iter(), tolerance, |el| match el {
                    kurbo::PathEl::MoveTo(p) => {
                        if ring.len() >= 3 {
                            rings.push(std::mem::take(&mut ring));
                        }
                        ring = vec![to_board(p)];
                    }
                    kurbo::PathEl::LineTo(p) => ring.push(to_board(p)),
                    kurbo::PathEl::ClosePath if ring.len() >= 3 => {
                        rings.push(std::mem::take(&mut ring));
                    }
                    _ => {}
                });
                if ring.len() >= 3 {
                    rings.push(ring);
                }
                x += f64::from(pos.x_advance) / upem * em.x;
            }
        }
        x - cursor.x
    }
}

/// A glyph outline collected as a kurbo path in font units.
#[derive(Default)]
struct Outline(kurbo::BezPath);

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0
            .quad_to((f64::from(x1), f64::from(y1)), (f64::from(x), f64::from(y)));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(x1), f64::from(y1)),
            (f64::from(x2), f64::from(y2)),
            (f64::from(x), f64::from(y)),
        );
    }

    fn close(&mut self) {
        self.0.close_path();
    }
}
