//! The PDF a drawing is written to: pages in millimetres with the origin at
//! the bottom-left corner, text set in one typeface whose glyphs are
//! embedded as they are used, and artwork plots placed to scale.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use anyhow::Context as _;

use pcb_ir::geom::{BBox, Point};
use pcb_ir::render::{artwork_pdf_form, deflate};
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use subsetter::GlyphRemapper;
use ttf_parser::Face;

/// PDF points per millimetre.
const POINTS_PER_MM: f64 = 72.0 / 25.4;

/// The ink a drawing's linework and lettering are in.
pub const INK: u32 = 0x111111;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Bold,
}

/// A typeface to letter a drawing in: font files of its regular and bold
/// faces, with TrueType outlines.
#[derive(Debug, Clone)]
pub struct Typeface {
    pub regular: Cow<'static, [u8]>,
    pub bold: Cow<'static, [u8]>,
}

impl Default for Typeface {
    /// The face pcb bundles: Roboto Mono.
    fn default() -> Self {
        Self {
            regular: Cow::Borrowed(include_bytes!("../../fonts/RobotoMono-Regular.ttf")),
            bold: Cow::Borrowed(include_bytes!("../../fonts/RobotoMono-Bold.ttf")),
        }
    }
}

/// The faces a drawing is lettered in, and the glyphs it has used of each.
pub struct Fonts {
    faces: [FontFace; 2],
}

struct FontFace {
    /// The name the face gives itself, which its embedded subset keeps.
    name: String,
    data: Cow<'static, [u8]>,
    units_per_em: f64,
    cap_height: f64,
    /// Whether the face lets a document carry only the glyphs it uses. One
    /// that forbids subsetting is embedded whole.
    subset: bool,
    /// Each character's glyph, its advance in ems and the character the
    /// glyph is of, as they are asked for.
    glyphs: RefCell<HashMap<char, (u16, f64, char)>>,
    used: RefCell<UsedGlyphs>,
}

/// The glyphs a document draws, with the character each stands for. An
/// embedded subset renumbers them; a face embedded whole keeps its own
/// numbering.
struct UsedGlyphs {
    glyphs: GlyphRemapper,
    chars: BTreeMap<u16, char>,
}

impl Fonts {
    pub fn new(typeface: &Typeface) -> anyhow::Result<Self> {
        Ok(Self {
            faces: [
                FontFace::new(typeface.regular.clone()).context("regular face")?,
                FontFace::new(typeface.bold.clone()).context("bold face")?,
            ],
        })
    }

    fn face(&self, weight: Weight) -> &FontFace {
        &self.faces[weight as usize]
    }

    /// The em a face is set at for its capitals to stand `height` tall.
    /// Lettering is sized the way a drawing states it, by the height of its
    /// capitals, so a drawing keeps its proportions whatever face letters it.
    fn em(&self, weight: Weight, height: f64) -> f64 {
        let face = self.face(weight);
        height * face.units_per_em / face.cap_height
    }

    /// Width of `text` lettered `height` millimetres tall.
    pub fn width(&self, weight: Weight, height: f64, text: &str) -> f64 {
        let face = self.face(weight);
        text.chars().map(|c| face.glyph(c).1).sum::<f64>() * self.em(weight, height)
    }

    /// Break `text` into lines no wider than `width`, at spaces.
    pub fn wrap(&self, weight: Weight, size: f64, text: &str, width: f64) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for word in text.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && self.width(weight, size, &candidate) > width {
                lines.push(std::mem::replace(&mut line, word.to_string()));
            } else {
                line = candidate;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
        lines
    }

    /// `style` at the size `text` fits `room` in: text too long for its room
    /// is set smaller rather than run into what stands beside it.
    pub fn fitted(&self, style: TextStyle, text: &str, room: f64) -> TextStyle {
        let natural = self.width(style.weight, style.size, text);
        TextStyle {
            size: style.size * (room / natural).min(1.0),
            ..style
        }
    }

    /// `text` as the glyph codes of the embedded subset, in hexadecimal.
    fn encode(&self, weight: Weight, text: &str) -> String {
        let face = self.face(weight);
        let mut used = face.used.borrow_mut();
        let mut codes = String::with_capacity(text.len() * 4);
        for c in text.chars() {
            let (glyph, _, c) = face.glyph(c);
            let code = if face.subset {
                used.glyphs.remap(glyph)
            } else {
                glyph
            };
            used.chars.entry(code).or_insert(c);
            write!(codes, "{code:04X}").unwrap();
        }
        codes
    }
}

impl FontFace {
    fn new(data: Cow<'static, [u8]>) -> anyhow::Result<Self> {
        let face = Face::parse(&data, 0).context("not a font file")?;
        anyhow::ensure!(
            face.tables().glyf.is_some(),
            "the face has no TrueType outlines to embed"
        );
        anyhow::ensure!(
            face.is_outline_embedding_allowed()
                && face.permissions() != Some(ttf_parser::Permissions::Restricted),
            "the face does not permit embedding in documents"
        );
        let name = face
            .names()
            .into_iter()
            .filter(|name| name.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
            .find_map(|name| name.to_string())
            .unwrap_or_else(|| "Drawing".to_string());
        // A PDF name carries no spaces or delimiters.
        let name = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        // A face that states no cap height, or states nothing for one, is
        // sized by its ascender.
        let stated = face.capital_height().filter(|height| *height > 0);
        let (units_per_em, cap_height) = (
            f64::from(face.units_per_em()),
            f64::from(stated.unwrap_or(face.ascender())),
        );
        anyhow::ensure!(cap_height > 0.0, "font has no height to letter at");
        Ok(Self {
            name,
            units_per_em,
            cap_height,
            subset: face.is_subsetting_allowed(),
            glyphs: RefCell::new(HashMap::new()),
            used: RefCell::new(UsedGlyphs {
                glyphs: GlyphRemapper::new(),
                chars: BTreeMap::new(),
            }),
            data,
        })
    }

    fn parsed(&self) -> Face<'_> {
        Face::parse(&self.data, 0).expect("the face parsed when it was loaded")
    }

    /// The glyph for `c`, its advance in ems and the character it is of:
    /// the face's question mark stands for a character it lacks.
    fn glyph(&self, c: char) -> (u16, f64, char) {
        *self.glyphs.borrow_mut().entry(c).or_insert_with(|| {
            let face = self.parsed();
            let (glyph, c) = match face.glyph_index(c) {
                Some(glyph) => (glyph, c),
                None => (face.glyph_index('?').unwrap_or_default(), '?'),
            };
            let advance = f64::from(face.glyph_hor_advance(glyph).unwrap_or(0));
            (glyph.0, advance / self.units_per_em, c)
        })
    }

    /// Embed the glyphs used of this face as font `id`.
    fn write(&self, pdf: &mut Pdf, alloc: &mut Ref, id: Ref) -> anyhow::Result<()> {
        const SYSTEM_INFO: SystemInfo = SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"Identity"),
            supplement: 0,
        };
        let face = self.parsed();
        let used = self.used.borrow();
        // The font program the document carries, the glyphs it holds in the
        // order it numbers them, and the name it goes by.
        let (program, glyphs, name) = if self.subset {
            let subset = subsetter::subset(&self.data, 0, &used.glyphs)
                .map_err(|error| anyhow::anyhow!("cannot subset {}: {error}", self.name))?;
            let glyphs = used.glyphs.remapped_gids().collect::<Vec<_>>();
            // A subset is tagged by six letters; these say which glyphs it
            // has.
            let tag = glyphs
                .iter()
                .fold(0xcbf2_9ce4_8422_2325_u64, |hash, glyph| {
                    (hash ^ u64::from(*glyph)).wrapping_mul(0x0100_0000_01b3)
                });
            let tag = (0..6)
                .map(|letter| char::from(b'A' + (tag >> (letter * 8)) as u8 % 26))
                .collect::<String>();
            (Cow::Owned(subset), glyphs, format!("{tag}+{}", self.name))
        } else {
            let glyphs = (0..face.number_of_glyphs()).collect();
            (Cow::Borrowed(&*self.data), glyphs, self.name.clone())
        };
        let name = Name(name.as_bytes());
        let (descendant, descriptor, file, cmap) =
            (alloc.bump(), alloc.bump(), alloc.bump(), alloc.bump());
        let per_mille = |units: i16| (f64::from(units) * 1000.0 / self.units_per_em) as f32;

        pdf.type0_font(id)
            .base_font(name)
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(descendant)
            .to_unicode(cmap);
        let mut font = pdf.cid_font(descendant);
        font.subtype(CidFontType::Type2)
            .base_font(name)
            .system_info(SYSTEM_INFO)
            .font_descriptor(descriptor)
            .default_width(0.0)
            .cid_to_gid_map_predefined(Name(b"Identity"));
        let advance = |glyph: u16| {
            let advance = face.glyph_hor_advance(ttf_parser::GlyphId(glyph));
            (f64::from(advance.unwrap_or(0)) * 1000.0 / self.units_per_em) as f32
        };
        font.widths()
            .consecutive(0, glyphs.iter().copied().map(advance));
        font.finish();
        let bbox = face.global_bounding_box();
        let mut flags = FontFlags::NON_SYMBOLIC;
        if face.is_monospaced() {
            flags |= FontFlags::FIXED_PITCH;
        }
        // The stem width matters only to a viewer that cannot use the
        // embedded face; it grows with the face's weight.
        let stem_v = 50.0 + (f32::from(face.weight().to_number()) / 65.0).powi(2);
        pdf.font_descriptor(descriptor)
            .name(name)
            .flags(flags)
            .bbox(Rect::new(
                per_mille(bbox.x_min),
                per_mille(bbox.y_min),
                per_mille(bbox.x_max),
                per_mille(bbox.y_max),
            ))
            .italic_angle(0.0)
            .ascent(per_mille(face.ascender()))
            .descent(per_mille(face.descender()))
            .cap_height(per_mille(face.capital_height().unwrap_or(face.ascender())))
            .stem_v(stem_v)
            .font_file2(file);
        pdf.stream(file, &deflate(&program))
            .filter(Filter::FlateDecode)
            .pair(Name(b"Length1"), program.len() as i32);
        let mut unicode = UnicodeCmap::<u16>::new(Name(b"Custom"), SYSTEM_INFO);
        for (&code, &c) in &used.chars {
            unicode.pair(code, c);
        }
        pdf.cmap(cmap, &deflate(&unicode.finish()))
            .filter(Filter::FlateDecode);
        Ok(())
    }
}

/// How a line is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pen {
    width: f64,
    color: u32,
    dash: Dash,
}

impl Pen {
    pub const fn solid(width: f64) -> Self {
        Self {
            width,
            color: INK,
            dash: Dash::Solid,
        }
    }

    pub const fn color(self, color: u32) -> Self {
        Self { color, ..self }
    }

    pub const fn dash(self, dash: Dash) -> Self {
        Self { dash, ..self }
    }
}

/// Line types of a technical drawing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dash {
    Solid,
    /// Long dash, short dash: centre lines and lines of cut.
    Chain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// How a line of text is set. Its anchor is on the baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Height of the capitals, in millimetres.
    pub size: f64,
    pub weight: Weight,
    pub align: Align,
    /// Read from the right of the sheet, as a vertical dimension is.
    pub vertical: bool,
}

impl TextStyle {
    pub const fn new(size: f64) -> Self {
        Self {
            size,
            weight: Weight::Regular,
            align: Align::Left,
            vertical: false,
        }
    }

    pub const fn bold(self) -> Self {
        Self {
            weight: Weight::Bold,
            ..self
        }
    }

    pub const fn align(self, align: Align) -> Self {
        Self { align, ..self }
    }

    pub const fn vertical(self) -> Self {
        Self {
            vertical: true,
            ..self
        }
    }
}

/// Artwork plotted into a document, for its pages to place.
pub type Plot = Ref;

/// One page as it is drawn.
pub struct Canvas<'a> {
    pub fonts: &'a Fonts,
    ops: String,
    forms: BTreeMap<String, Ref>,
    pen: Option<Pen>,
    fill: Option<u32>,
}

fn number(value: f64) -> impl std::fmt::Display {
    // Four decimals of a millimetre, trimmed.
    let rounded = (value * 1e4).round() / 1e4;
    if rounded == 0.0 { 0.0 } else { rounded }
}

fn rgb(color: u32) -> [f64; 3] {
    let [_, red, green, blue] = color.to_be_bytes();
    [red, green, blue].map(|channel| (f64::from(channel) / 255.0 * 1e3).round() / 1e3)
}

impl<'a> Canvas<'a> {
    pub fn new(fonts: &'a Fonts) -> Self {
        Self {
            fonts,
            ops: String::new(),
            forms: BTreeMap::new(),
            pen: None,
            fill: None,
        }
    }

    fn set_pen(&mut self, pen: Pen) {
        if self.pen == Some(pen) {
            return;
        }
        let [red, green, blue] = rgb(pen.color);
        let width = pen.width;
        let dash = match pen.dash {
            Dash::Solid => "[] 0 d".to_string(),
            // The proportions artwork draws a centre line in, so a key's
            // sample is the line the view shows.
            Dash::Chain => format!(
                "[{} {} {} {}] 0 d",
                number(6.0 * width),
                number(2.0 * width),
                number(width),
                number(2.0 * width)
            ),
        };
        writeln!(
            self.ops,
            "{red} {green} {blue} RG {} w 0 J 0 j {dash}",
            number(width)
        )
        .unwrap();
        self.pen = Some(pen);
    }

    fn set_fill(&mut self, color: u32) {
        if self.fill == Some(color) {
            return;
        }
        let [red, green, blue] = rgb(color);
        writeln!(self.ops, "{red} {green} {blue} rg").unwrap();
        self.fill = Some(color);
    }

    fn path(&mut self, points: &[Point], closed: bool) {
        for (index, point) in points.iter().enumerate() {
            let op = if index == 0 { 'm' } else { 'l' };
            writeln!(self.ops, "{} {} {op}", number(point.x), number(point.y)).unwrap();
        }
        if closed {
            self.ops.push_str("h\n");
        }
    }

    /// A circle as four cubics.
    fn circle_path(&mut self, center: Point, radius: f64) {
        let handle = radius * 4.0 / 3.0 * (std::f64::consts::FRAC_PI_8).tan();
        let at = |x: f64, y: f64| format!("{} {}", number(center.x + x), number(center.y + y));
        writeln!(self.ops, "{} m", at(radius, 0.0)).unwrap();
        for [(c1x, c1y), (c2x, c2y), (x, y)] in [
            [(radius, handle), (handle, radius), (0.0, radius)],
            [(-handle, radius), (-radius, handle), (-radius, 0.0)],
            [(-radius, -handle), (-handle, -radius), (0.0, -radius)],
            [(handle, -radius), (radius, -handle), (radius, 0.0)],
        ] {
            writeln!(self.ops, "{} {} {} c", at(c1x, c1y), at(c2x, c2y), at(x, y)).unwrap();
        }
        self.ops.push_str("h\n");
    }

    pub fn line(&mut self, from: Point, to: Point, pen: Pen) {
        self.polyline(&[from, to], false, pen);
    }

    pub fn polyline(&mut self, points: &[Point], closed: bool, pen: Pen) {
        self.set_pen(pen);
        self.path(points, closed);
        self.ops.push_str("S\n");
    }

    pub fn rect(&mut self, rect: BBox, pen: Pen) {
        self.polyline(&corners(rect), true, pen);
    }

    pub fn fill_rect(&mut self, rect: BBox, color: u32) {
        self.fill_polygon(&corners(rect), color);
    }

    pub fn fill_polygon(&mut self, points: &[Point], color: u32) {
        self.set_fill(color);
        self.path(points, true);
        self.ops.push_str("f\n");
    }

    pub fn circle(&mut self, center: Point, radius: f64, pen: Pen) {
        self.set_pen(pen);
        self.circle_path(center, radius);
        self.ops.push_str("S\n");
    }

    pub fn fill_circle(&mut self, center: Point, radius: f64, color: u32) {
        self.set_fill(color);
        self.circle_path(center, radius);
        self.ops.push_str("f\n");
    }

    /// Set one line of text with its anchor at `at`; returns its width.
    pub fn text(&mut self, at: Point, text: &str, style: TextStyle) -> f64 {
        let width = self.fonts.width(style.weight, style.size, text);
        if text.is_empty() {
            return width;
        }
        let along = match style.align {
            Align::Left => 0.0,
            Align::Center => -width / 2.0,
            Align::Right => -width,
        };
        self.set_fill(INK);
        let font = style.weight as usize;
        let codes = self.fonts.encode(style.weight, text);
        let (matrix, x, y) = if style.vertical {
            ("0 1 -1 0", at.x, at.y + along)
        } else {
            ("1 0 0 1", at.x + along, at.y)
        };
        writeln!(
            self.ops,
            "BT /F{font} {} Tf {matrix} {} {} Tm <{codes}> Tj ET",
            number(self.fonts.em(style.weight, style.size)),
            number(x),
            number(y)
        )
        .unwrap();
        width
    }

    /// Place a plot so that the artwork point `anchor` lands on `at`, drawn
    /// at `scale`.
    pub fn place(&mut self, plot: Plot, anchor: Point, at: Point, scale: f64) {
        let name = format!("X{}", plot.get());
        writeln!(
            self.ops,
            "q {scale} 0 0 {scale} {} {} cm /{name} Do Q",
            number(at.x - scale * anchor.x),
            number(at.y - scale * anchor.y),
            scale = number(scale),
        )
        .unwrap();
        self.forms.insert(name, plot);
    }
}

fn corners(rect: BBox) -> [Point; 4] {
    [
        rect.min,
        Point::new(rect.max.x, rect.min.y),
        rect.max,
        Point::new(rect.min.x, rect.max.y),
    ]
}

/// What a PDF says about itself.
#[derive(Debug, Clone, Default)]
pub struct DocumentInfo {
    pub title: String,
    pub subject: String,
    pub creator: String,
}

pub struct Document {
    pdf: Pdf,
    alloc: Ref,
    catalog: Ref,
    tree: Ref,
    fonts: [Ref; 2],
    /// Every page with the name its bookmark carries.
    pages: Vec<(Ref, String)>,
}

impl Document {
    pub fn new() -> Self {
        let mut alloc = Ref::new(1);
        Self {
            pdf: Pdf::new(),
            catalog: alloc.bump(),
            tree: alloc.bump(),
            fonts: [alloc.bump(), alloc.bump()],
            alloc,
            pages: Vec::new(),
        }
    }

    /// Plot artwork into the document for pages to place.
    pub fn plot<LayerMeta, ObjectMeta>(
        &mut self,
        artwork: &pcb_ir::dialects::artwork::Document<LayerMeta, ObjectMeta>,
        options: &pcb_ir::render::RenderOptions,
    ) -> Result<Plot, pcb_ir::geom::AccuracyError> {
        artwork_pdf_form(&mut self.pdf, &mut self.alloc, artwork, options)
    }

    /// Append a page `width` by `height` millimetres, bookmarked as `name`.
    pub fn page(&mut self, width: f64, height: f64, canvas: Canvas<'_>, name: String) {
        let (page, content) = (self.alloc.bump(), self.alloc.bump());
        let ops = format!("{k} 0 0 {k} 0 0 cm\n{}", canvas.ops, k = POINTS_PER_MM);
        self.pdf
            .stream(content, &deflate(ops.as_bytes()))
            .filter(Filter::FlateDecode);
        let mut writer = self.pdf.page(page);
        writer
            .parent(self.tree)
            .media_box(Rect::new(
                0.0,
                0.0,
                (width * POINTS_PER_MM) as f32,
                (height * POINTS_PER_MM) as f32,
            ))
            .contents(content);
        let mut resources = writer.resources();
        let mut fonts = resources.fonts();
        for (index, font) in self.fonts.iter().enumerate() {
            fonts.pair(Name(format!("F{index}").as_bytes()), *font);
        }
        fonts.finish();
        let mut forms = resources.x_objects();
        for (name, id) in &canvas.forms {
            forms.pair(Name(name.as_bytes()), *id);
        }
        forms.finish();
        resources.finish();
        writer.finish();
        self.pages.push((page, name));
    }

    pub fn finish(mut self, fonts: &Fonts, info: &DocumentInfo) -> anyhow::Result<Vec<u8>> {
        for (face, id) in fonts.faces.iter().zip(self.fonts) {
            face.write(&mut self.pdf, &mut self.alloc, id)?;
        }
        self.pdf
            .pages(self.tree)
            .count(self.pages.len() as i32)
            .kids(self.pages.iter().map(|(page, _)| *page));
        // One bookmark a page, so a reader can go to a sheet by its name.
        let outline = self.alloc.bump();
        let items = self
            .pages
            .iter()
            .map(|_| self.alloc.bump())
            .collect::<Vec<_>>();
        for (index, ((page, name), item)) in self.pages.iter().zip(&items).enumerate() {
            let mut writer = self.pdf.outline_item(*item);
            writer.title(TextStr(name)).parent(outline);
            if let Some(previous) = index.checked_sub(1) {
                writer.prev(items[previous]);
            }
            if let Some(next) = items.get(index + 1) {
                writer.next(*next);
            }
            writer.dest().page(*page).fit();
        }
        let mut catalog = self.pdf.catalog(self.catalog);
        catalog.pages(self.tree);
        if let (Some(first), Some(last)) = (items.first(), items.last()) {
            catalog.outlines(outline);
            catalog.finish();
            self.pdf
                .outline(outline)
                .first(*first)
                .last(*last)
                .count(items.len() as i32);
        } else {
            catalog.finish();
        }
        let id = self.alloc.bump();
        self.pdf
            .document_info(id)
            .title(TextStr(&info.title))
            .subject(TextStr(&info.subject))
            .creator(TextStr(&info.creator));
        Ok(self.pdf.finish())
    }
}
