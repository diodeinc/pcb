//! What a sheet sets beside its views: tables and numbered notes, each a
//! block of a known height that draws down from its top-left corner.

use pcb_ir::geom::{BBox, Point};
use pcb_ir::render::pdf::PdfForm;

use super::data::Chip;
use super::pdf::{Align, Canvas, Dash, Fonts, INK, Pen, TextStyle, Weight};
use super::sheet::{BODY, HAIR, HEADING, LABEL, MEDIUM, THICK, THIN};

/// Height of a block's heading, down to what it heads.
const HEADING_HEIGHT: f64 = 4.6;
const ROW_HEIGHT: f64 = 4.0;
const HEADER_ROW_HEIGHT: f64 = 4.4;
/// Space between a cell's edge and its text.
const CELL_PAD: f64 = 1.2;
const NOTE_LEADING: f64 = 3.7;
const NOTE_GAP: f64 = 1.5;
const NOTE_INDENT: f64 = 6.0;
/// A colour chip.
const CHIP_WIDTH: f64 = 5.0;
const CHIP_HEIGHT: f64 = 2.8;

/// What a stackup row is made of, as its section is hatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Material {
    Copper,
    Core,
    Prepreg,
    Dielectric,
    Mask,
}

/// How a view draws something, shown beside what it is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    ArrayProfile,
    BoardProfile,
    Score,
    Routed,
    FiducialTop,
    FiducialBottom,
}

#[derive(Debug, Clone)]
pub enum Cell {
    Text(String),
    Bold(String),
    /// A drill symbol, drawn at the size it has on a plot.
    Symbol(PdfForm),
    /// A slice of the board's section, in the colour of what it is made of.
    Section(Material, u32),
    /// Colour chips: one, or one for each side of the board.
    Chips(Vec<Chip>),
    Sample(Sample),
    Empty,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub title: &'static str,
    /// Share of the table's width.
    pub width: f64,
    pub align: Align,
}

impl Column {
    pub const fn new(title: &'static str, width: f64, align: Align) -> Self {
        Self {
            title,
            width,
            align,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Table {
    pub title: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    /// A closing row set off by a heavier rule: totals.
    pub footer: Option<Vec<Cell>>,
    /// Whether the columns are titled. A table of what and how much reads
    /// without.
    pub header: bool,
}

#[derive(Debug, Clone)]
pub struct Notes {
    pub title: String,
    pub notes: Vec<String>,
}

/// A view small enough to sit among the tables: a detail of a larger one.
#[derive(Debug, Clone)]
pub struct Figure {
    pub title: String,
    /// What the figure shows, lettered under it.
    pub captions: Vec<String>,
    pub form: PdfForm,
    /// The point of the artwork drawn at the figure's centre.
    pub center: Point,
    pub scale: f64,
    /// Height the artwork takes on the sheet.
    pub height: f64,
}

/// A V-score in section: both cuts, the web they leave and their angle.
#[derive(Debug, Clone)]
pub struct ScoreSection {
    pub title: String,
    pub thickness: String,
    pub web: String,
    pub angle: String,
}

#[derive(Debug, Clone)]
pub enum Block {
    Table(Table),
    Notes(Notes),
    Figure(Figure),
    ScoreSection(ScoreSection),
}

const CAPTION_LEADING: f64 = 3.7;
/// Height of the slab a score section draws, and the room around it for
/// its dimensions.
const SECTION_SLAB: f64 = 15.0;
const SECTION_HEIGHT: f64 = SECTION_SLAB + 16.0;

impl Block {
    pub fn height(&self, fonts: &Fonts, width: f64) -> f64 {
        match self {
            Self::Table(table) => HEADING_HEIGHT + table.height(),
            Self::Notes(notes) => {
                let lines = notes.lines(fonts, width);
                let text = lines.iter().map(|note| note.len()).sum::<usize>() as f64;
                HEADING_HEIGHT + text * NOTE_LEADING + lines.len() as f64 * NOTE_GAP
            }
            Self::Figure(figure) => {
                let captions = figure.captions.len() as f64 * CAPTION_LEADING;
                HEADING_HEIGHT + 2.0 + figure.height + 2.0 + captions
            }
            Self::ScoreSection(_) => HEADING_HEIGHT + SECTION_HEIGHT + CAPTION_LEADING,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::Table(table) => &table.title,
            Self::Notes(notes) => &notes.title,
            Self::Figure(figure) => &figure.title,
            Self::ScoreSection(section) => &section.title,
        }
    }

    /// Draw the block `width` wide with its top-left corner at `at`.
    pub fn draw(&self, canvas: &mut Canvas<'_>, at: Point, width: f64) {
        let title = self.title();
        canvas.text(
            Point::new(at.x, at.y - HEADING),
            title,
            TextStyle::new(HEADING).bold(),
        );
        let top = Point::new(at.x, at.y - HEADING_HEIGHT);
        match self {
            Self::Table(table) => table.draw(canvas, top, width),
            Self::Notes(notes) => notes.draw(canvas, top, width),
            Self::Figure(figure) => figure.draw(canvas, top, width),
            Self::ScoreSection(section) => section.draw(canvas, top, width),
        }
    }
}

impl Figure {
    fn draw(&self, canvas: &mut Canvas<'_>, at: Point, width: f64) {
        canvas.line(at, Point::new(at.x + width, at.y), Pen::solid(MEDIUM));
        let center = Point::new(at.x + width / 2.0, at.y - 2.0 - self.height / 2.0);
        canvas.place(self.form, self.center, center, self.scale);
        let mut y = at.y - 2.0 - self.height - 2.0;
        for caption in &self.captions {
            y -= CAPTION_LEADING;
            canvas.text(
                Point::new(center.x, y + 0.9),
                caption,
                TextStyle::new(BODY).align(Align::Center),
            );
        }
    }
}

impl ScoreSection {
    /// The board on edge, cut from both faces, with the cuts' angle, the
    /// web between them and the board's thickness lettered around it.
    fn draw(&self, canvas: &mut Canvas<'_>, at: Point, width: f64) {
        canvas.line(at, Point::new(at.x + width, at.y), Pen::solid(MEDIUM));
        let thin = Pen::solid(THIN);
        let (slab_width, height) = (64.0, SECTION_SLAB);
        let middle = at.x + width / 2.0;
        let (left, right) = (middle - slab_width / 2.0, middle + slab_width / 2.0);
        let top = at.y - 11.0;
        let bottom = top - height;
        // The web is drawn a third of the slab; the cuts open at the angle
        // the note states.
        let depth = height / 3.0;
        let half = depth * 15.0_f64.to_radians().tan();
        canvas.fill_polygon(
            &[
                Point::new(left, bottom),
                Point::new(middle - half, bottom),
                Point::new(middle, bottom + depth),
                Point::new(middle + half, bottom),
                Point::new(right, bottom),
                Point::new(right, top),
                Point::new(middle + half, top),
                Point::new(middle, top - depth),
                Point::new(middle - half, top),
                Point::new(left, top),
            ],
            0xe4e4e4,
        );
        let face = |canvas: &mut Canvas<'_>, y: f64, tip: f64| {
            canvas.polyline(
                &[
                    Point::new(left, y),
                    Point::new(middle - half, y),
                    Point::new(middle, tip),
                    Point::new(middle + half, y),
                    Point::new(right, y),
                ],
                false,
                Pen::solid(THICK),
            );
        };
        face(canvas, top, top - depth);
        face(canvas, bottom, bottom + depth);
        // The slab runs on past both ends.
        for x in [left, right] {
            let jog = 1.2;
            canvas.polyline(
                &[
                    Point::new(x, top + 1.5),
                    Point::new(x, top - height * 0.4),
                    Point::new(x - jog, top - height * 0.45),
                    Point::new(x + jog, top - height * 0.55),
                    Point::new(x, top - height * 0.6),
                    Point::new(x, bottom - 1.5),
                ],
                false,
                thin,
            );
        }

        let label = TextStyle::new(BODY);
        let cap = label.size;
        // The angle, between the cut's faces carried up past the surface.
        let reach = 6.0;
        let spread = (depth + reach) * 15.0_f64.to_radians().tan();
        for side in [-1.0, 1.0] {
            canvas.line(
                Point::new(middle + side * half, top),
                Point::new(middle + side * spread, top + reach),
                thin,
            );
        }
        canvas.text(
            Point::new(middle + spread + 1.5, top + reach - cap),
            &self.angle,
            label,
        );
        // The web, between the tips of the cuts.
        let (tip_top, tip_bottom) = (top - depth, bottom + depth);
        let web_x = middle + 14.0;
        for y in [tip_top, tip_bottom] {
            canvas.line(
                Point::new(middle + 0.8, y),
                Point::new(web_x + 1.5, y),
                thin,
            );
        }
        canvas.line(
            Point::new(web_x, tip_top + 4.0),
            Point::new(web_x, tip_bottom - 4.0),
            thin,
        );
        arrow(canvas, Point::new(web_x, tip_top), 1.0);
        arrow(canvas, Point::new(web_x, tip_bottom), -1.0);
        canvas.text(
            Point::new(right + 4.0, (tip_top + tip_bottom) / 2.0 - cap / 2.0),
            &self.web,
            label,
        );
        canvas.line(
            Point::new(web_x, (tip_top + tip_bottom) / 2.0),
            Point::new(right + 3.0, (tip_top + tip_bottom) / 2.0),
            Pen::solid(HAIR),
        );
        // The thickness, down the left end.
        let thickness_x = left - 6.0;
        for y in [top, bottom] {
            canvas.line(
                Point::new(left - 1.5, y),
                Point::new(thickness_x - 1.5, y),
                thin,
            );
        }
        canvas.line(
            Point::new(thickness_x, top),
            Point::new(thickness_x, bottom),
            thin,
        );
        arrow(canvas, Point::new(thickness_x, top), -1.0);
        arrow(canvas, Point::new(thickness_x, bottom), 1.0);
        canvas.text(
            Point::new(thickness_x - 2.0, (top + bottom) / 2.0 - cap / 2.0),
            &self.thickness,
            label.align(Align::Right),
        );
        canvas.text(
            Point::new(middle, at.y - SECTION_HEIGHT - CAPTION_LEADING + 0.9),
            "NOT TO SCALE  ·  BOTH SIDES, CUTS ALIGNED",
            label.align(Align::Center),
        );
    }
}

/// A vertical arrowhead with its tip at `tip`, its tail `toward` up (+1)
/// or down (-1).
fn arrow(canvas: &mut Canvas<'_>, tip: Point, toward: f64) {
    let (length, half) = (2.4, 0.4);
    canvas.fill_polygon(
        &[
            tip,
            Point::new(tip.x - half, tip.y + toward * length),
            Point::new(tip.x + half, tip.y + toward * length),
        ],
        INK,
    );
}

impl Notes {
    fn lines(&self, fonts: &Fonts, width: f64) -> Vec<Vec<String>> {
        self.notes
            .iter()
            .map(|note| fonts.wrap(Weight::Regular, BODY, note, width - NOTE_INDENT))
            .collect()
    }

    fn draw(&self, canvas: &mut Canvas<'_>, at: Point, width: f64) {
        canvas.line(at, Point::new(at.x + width, at.y), Pen::solid(MEDIUM));
        let mut y = at.y - NOTE_GAP;
        for (index, lines) in self.lines(canvas.fonts, width).iter().enumerate() {
            for (line_index, line) in lines.iter().enumerate() {
                y -= NOTE_LEADING;
                if line_index == 0 {
                    canvas.text(
                        Point::new(at.x + NOTE_INDENT - 1.5, y + 0.9),
                        &format!("{}.", index + 1),
                        TextStyle::new(BODY).align(Align::Right),
                    );
                }
                canvas.text(
                    Point::new(at.x + NOTE_INDENT, y + 0.9),
                    line,
                    TextStyle::new(BODY),
                );
            }
            y -= NOTE_GAP;
        }
    }
}

impl Table {
    fn height(&self) -> f64 {
        let rows = self.rows.len() + usize::from(self.footer.is_some());
        let header = if self.header { HEADER_ROW_HEIGHT } else { 0.0 };
        header + ROW_HEIGHT * rows as f64
    }

    fn draw(&self, canvas: &mut Canvas<'_>, at: Point, width: f64) {
        let total = self.columns.iter().map(|column| column.width).sum::<f64>();
        let mut left = at.x;
        let columns = self
            .columns
            .iter()
            .map(|column| {
                let cell = (left, width * column.width / total);
                left += cell.1;
                cell
            })
            .collect::<Vec<_>>();
        let bounds = BBox::new(
            Point::new(at.x, at.y - self.height()),
            Point::new(at.x + width, at.y),
        );
        let rule = |canvas: &mut Canvas<'_>, y: f64, pen: Pen| {
            canvas.line(
                Point::new(bounds.min.x, y),
                Point::new(bounds.max.x, y),
                pen,
            );
        };
        let mut y = at.y;
        if self.header {
            // Column titles over a rule.
            for (column, &(x, column_width)) in self.columns.iter().zip(&columns) {
                let baseline = at.y - (HEADER_ROW_HEIGHT + LABEL) / 2.0;
                let style = TextStyle::new(LABEL).bold().align(column.align);
                canvas.text(
                    Point::new(anchor(x, column_width, column.align), baseline),
                    column.title,
                    style,
                );
            }
            y -= HEADER_ROW_HEIGHT;
            rule(canvas, y, Pen::solid(THIN));
        }
        let footer = self.footer.iter().map(|row| (row, true));
        for (index, (row, is_footer)) in self
            .rows
            .iter()
            .map(|row| (row, false))
            .chain(footer)
            .enumerate()
        {
            if is_footer {
                rule(canvas, y, Pen::solid(THIN));
            } else if index > 0 {
                rule(canvas, y, Pen::solid(HAIR).color(0x808080));
            }
            let cell_box = |x: f64, width: f64| {
                BBox::new(Point::new(x, y - ROW_HEIGHT), Point::new(x + width, y))
            };
            for ((cell, column), &(x, column_width)) in row.iter().zip(&self.columns).zip(&columns)
            {
                draw_cell(canvas, cell, cell_box(x, column_width), column.align);
            }
            y -= ROW_HEIGHT;
        }
        canvas.rect(bounds, Pen::solid(MEDIUM));
    }
}

/// Where text aligned `align` in a cell is anchored.
fn anchor(x: f64, width: f64, align: Align) -> f64 {
    match align {
        Align::Left => x + CELL_PAD,
        Align::Center => x + width / 2.0,
        Align::Right => x + width - CELL_PAD,
    }
}

fn draw_cell(canvas: &mut Canvas<'_>, cell: &Cell, bounds: BBox, align: Align) {
    let center = bounds.center();
    match cell {
        Cell::Empty => {}
        Cell::Text(text) | Cell::Bold(text) => {
            let style = TextStyle::new(BODY).align(align);
            let style = match cell {
                Cell::Bold(_) => style.bold(),
                _ => style,
            };
            // Text too long for its cell is set smaller rather than run
            // into the next.
            let room = bounds.width() - 2.0 * CELL_PAD;
            let natural = canvas.fonts.width(style.weight, style.size, text);
            let size = style.size * (room / natural).min(1.0);
            let at = Point::new(
                anchor(bounds.min.x, bounds.width(), align),
                center.y - size / 2.0,
            );
            canvas.text(at, text, TextStyle { size, ..style });
        }
        Cell::Symbol(symbol) => canvas.place(*symbol, Point::ZERO, center, 1.0),
        Cell::Section(material, color) => draw_section(canvas, *material, *color, bounds),
        Cell::Chips(chips) => draw_chips(canvas, chips, bounds),
        Cell::Sample(sample) => draw_sample(canvas, *sample, bounds),
    }
}

/// A sample of a view's line or mark, as the view draws it.
fn draw_sample(canvas: &mut Canvas<'_>, sample: Sample, cell: BBox) {
    let center = cell.center();
    let (left, right) = (
        Point::new(cell.min.x + 1.5, center.y),
        Point::new(cell.max.x - 1.5, center.y),
    );
    match sample {
        Sample::ArrayProfile => canvas.line(left, right, Pen::solid(THICK)),
        Sample::BoardProfile => canvas.line(left, right, Pen::solid(MEDIUM)),
        Sample::Score => canvas.line(left, right, Pen::solid(THIN).dash(Dash::Chain)),
        Sample::Routed => {
            let swatch = BBox::new(
                Point::new(left.x, cell.min.y + 0.9),
                Point::new(right.x, cell.max.y - 0.9),
            );
            canvas.fill_rect(swatch, 0xd4d4d4);
            canvas.rect(swatch, Pen::solid(THIN));
        }
        Sample::FiducialTop => {
            canvas.fill_circle(center, 0.75, INK);
            canvas.circle(center, 1.5, Pen::solid(HAIR));
        }
        Sample::FiducialBottom => {
            canvas.circle(center, 0.75, Pen::solid(HAIR));
            canvas.circle(center, 1.5, Pen::solid(HAIR));
        }
    }
}

/// Colour chips side by side from the cell's left edge: a filled box for a
/// colour, a struck-out one for a side that has none, and a queried one for
/// a colour the design does not state. Every box is outlined, so white ink
/// still reads on white paper.
fn draw_chips(canvas: &mut Canvas<'_>, chips: &[Chip], cell: BBox) {
    let center = cell.center().y;
    let pen = Pen::solid(HAIR);
    for (index, chip) in chips.iter().enumerate() {
        let left = cell.min.x + CELL_PAD + index as f64 * (CHIP_WIDTH + 0.8);
        let chip_box = BBox::new(
            Point::new(left, center - CHIP_HEIGHT / 2.0),
            Point::new(left + CHIP_WIDTH, center + CHIP_HEIGHT / 2.0),
        );
        match chip {
            Chip::Color(color) => canvas.fill_rect(chip_box, *color),
            Chip::Absent => canvas.line(chip_box.min, chip_box.max, pen),
            Chip::Unstated => {
                let at = Point::new(chip_box.center().x, center - LABEL / 2.0);
                canvas.text(at, "?", TextStyle::new(LABEL).bold().align(Align::Center));
            }
        }
        canvas.rect(chip_box, pen);
    }
}

/// One layer of the board's section, in the colour of what it is made of:
/// copper and mask as solid bars, dielectrics as a tint under the hatching
/// their kinds are told apart by.
fn draw_section(canvas: &mut Canvas<'_>, material: Material, color: u32, cell: BBox) {
    let inset = |x: f64, y: f64| {
        BBox::new(
            Point::new(cell.min.x + x, cell.min.y + y),
            Point::new(cell.max.x - x, cell.max.y - y),
        )
    };
    let pen = Pen::solid(HAIR);
    match material {
        Material::Copper | Material::Mask => {
            let bar = inset(
                1.0,
                if material == Material::Copper {
                    1.2
                } else {
                    1.6
                },
            );
            canvas.fill_rect(bar, color);
            canvas.rect(bar, pen);
        }
        Material::Core | Material::Prepreg | Material::Dielectric => {
            let slab = inset(1.0, 0.0);
            canvas.fill_rect(slab, color);
            let mut hatch = |rising: bool| {
                for (from, to) in hatch_lines(slab, 1.1, rising) {
                    canvas.line(from, to, pen);
                }
            };
            match material {
                Material::Core => {
                    hatch(true);
                    hatch(false);
                }
                Material::Prepreg => hatch(true),
                _ => {}
            }
            canvas.line(slab.min, Point::new(slab.min.x, slab.max.y), pen);
            canvas.line(Point::new(slab.max.x, slab.min.y), slab.max, pen);
        }
    }
}

/// Diagonal hatch lines `spacing` apart across `rect`, clipped to it.
fn hatch_lines(rect: BBox, spacing: f64, rising: bool) -> Vec<(Point, Point)> {
    // A rising line is y = x + c; a falling one mirrors it left to right.
    let (width, height) = (rect.width(), rect.height());
    let step = spacing * std::f64::consts::SQRT_2;
    let count = ((width + height) / step).ceil() as i32;
    (1..count)
        .filter_map(|line| {
            let offset = f64::from(line) * step - width;
            let (from, to) = (
                offset.max(0.0) - offset,
                (offset + width).min(height) - offset,
            );
            (to > from).then(|| {
                let x = |along: f64| {
                    if rising {
                        rect.min.x + along
                    } else {
                        rect.max.x - along
                    }
                };
                (
                    Point::new(x(from), rect.min.y + from + offset),
                    Point::new(x(to), rect.min.y + to + offset),
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hatch_lines_stay_inside_their_cell() {
        let cell = BBox::new(Point::new(10.0, 20.0), Point::new(22.0, 24.4));
        for rising in [true, false] {
            let lines = hatch_lines(cell, 1.1, rising);
            assert!(lines.len() > 5);
            for (from, to) in lines {
                for point in [from, to] {
                    assert!(cell.expand(1e-9).contains_point(point), "{point:?}");
                }
                let run = to - from;
                assert!((run.x.abs() - run.y.abs()).abs() < 1e-9, "45 degrees");
            }
        }
    }
}
