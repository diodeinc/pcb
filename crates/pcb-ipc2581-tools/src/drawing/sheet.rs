//! The drawing sheet: its size, its frame, and the title strip along its
//! bottom edge.

use pcb_ir::geom::{BBox, Point};

use super::pdf::{Align, Canvas, Pen, TextStyle};

/// Lines of a sheet, by what they draw.
pub const HEAVY: f64 = 0.7;
pub const THICK: f64 = 0.5;
pub const MEDIUM: f64 = 0.35;
pub const THIN: f64 = 0.18;
pub const HAIR: f64 = 0.1;

/// Lettering heights, as the height of the capitals: labels, body text,
/// headings and the drawing's title.
pub const LABEL: f64 = 1.4;
pub const BODY: f64 = 1.8;
pub const HEADING: f64 = 2.5;
const TITLE: f64 = 3.5;

/// The sheet every page is laid out on: A4, in landscape, and what its
/// title strip says of that.
pub const SIZE: (f64, f64) = (297.0, 210.0);
const SIZE_AND_UNITS: &str = "A4 · mm";

/// Margin from the sheet's edge to the frame, the outer half of which is
/// the band the zone letters and numbers stand in.
const MARGIN: f64 = 10.0;
const ZONE_BAND: f64 = 5.0;
/// The zones the frame divides into, in fields as near 50 mm as divide it
/// evenly: columns numbered from the left, rows lettered from the top.
const ZONE_COLUMNS: usize = 6;
const ZONE_ROWS: [&str; 4] = ["A", "B", "C", "D"];
/// Two rows of fields.
const TITLE_STRIP_HEIGHT: f64 = 12.0;
/// Width of the column of tables a sheet sets beside its view.
pub const COLUMN_WIDTH: f64 = 105.0;

/// The area inside the frame.
fn frame() -> BBox {
    BBox::new(
        Point::new(MARGIN, MARGIN),
        Point::new(SIZE.0 - MARGIN, SIZE.1 - MARGIN),
    )
}

/// The title strip, along the bottom of the frame.
fn title_strip() -> BBox {
    let frame = frame();
    BBox::new(
        frame.min,
        Point::new(frame.max.x, frame.min.y + TITLE_STRIP_HEIGHT),
    )
}

/// What is left of the frame to draw in.
pub fn body() -> BBox {
    let frame = frame();
    BBox::new(
        Point::new(frame.min.x, frame.min.y + TITLE_STRIP_HEIGHT),
        frame.max,
    )
}

/// What every sheet's title strip says.
#[derive(Debug, Clone)]
pub struct TitleBlock {
    /// The design drawn.
    pub title: String,
    /// What the document is, and of what: "FABRICATION DRAWING · BARE
    /// BOARD".
    pub document: String,
    /// Empty where the design states none.
    pub revision: String,
    pub date: String,
    /// The data the drawing was made from.
    pub source: String,
    pub generator: String,
}

/// What differs from sheet to sheet of one drawing.
#[derive(Debug, Clone)]
pub struct SheetLabel {
    /// What the sheet shows.
    pub content: String,
    /// The scale of its principal view.
    pub scale: String,
    pub number: usize,
    pub count: usize,
}

/// Draw the frame, its zone border and the title strip: the design's name
/// on the left, the sheet's number on the right, and two rows of fields
/// between them, each a small label over its value.
pub fn draw_sheet(canvas: &mut Canvas<'_>, title: &TitleBlock, label: &SheetLabel) {
    /// Room between a field's edge and its lettering.
    const INSET: f64 = 1.2;
    let strip = title_strip();
    let rule = Pen::solid(THIN);
    let middle = strip.center().y;
    let field = |canvas: &mut Canvas<'_>, cell: BBox, name: &str, text: &str, style: TextStyle| {
        canvas.text(
            Point::new(cell.min.x + INSET, cell.max.y - 0.9 - LABEL),
            name,
            TextStyle::new(LABEL),
        );
        let style = canvas.fonts.fitted(style, text, cell.width() - 2.0 * INSET);
        let at = match style.align {
            Align::Right => cell.max.x - INSET,
            _ => cell.min.x + INSET,
        };
        // Larger lettering stands higher, so its descenders clear the
        // frame.
        let baseline = cell.min.y + 1.1 + (style.size - BODY).max(0.0) * 0.6;
        canvas.text(Point::new(at, baseline), text, style);
    };
    let divider = |canvas: &mut Canvas<'_>, x: f64, top: f64, bottom: f64| {
        canvas.line(Point::new(x, top), Point::new(x, bottom), rule);
    };

    let name = BBox::new(
        strip.min,
        Point::new(strip.min.x + strip.width() * 0.29, strip.max.y),
    );
    let number = BBox::new(
        Point::new(strip.max.x - strip.width() * 0.08, strip.min.y),
        strip.max,
    );
    field(
        canvas,
        name,
        "TITLE",
        &title.title,
        TextStyle::new(TITLE).bold(),
    );
    field(
        canvas,
        number,
        "SHEET",
        &format!("{}/{}", label.number, label.count),
        TextStyle::new(TITLE).bold().align(Align::Right),
    );
    divider(canvas, name.max.x, strip.max.y, strip.min.y);
    divider(canvas, number.min.x, strip.max.y, strip.min.y);

    // Each row's fields with their share of the row.
    let value = TextStyle::new(BODY).bold();
    let rows = [
        vec![
            ("DOCUMENT", title.document.as_str(), 0.36),
            ("CONTENT", label.content.as_str(), 0.25),
            ("SCALE", label.scale.as_str(), 0.1),
            ("SHEET SIZE · UNITS", SIZE_AND_UNITS, 0.14),
            ("REV", title.revision.as_str(), 0.15),
        ],
        vec![
            ("SOURCE DATA", title.source.as_str(), 0.61),
            ("DATE", title.date.as_str(), 0.14),
            ("GENERATED BY", title.generator.as_str(), 0.25),
        ],
    ];
    let (left, right) = (name.max.x, number.min.x);
    canvas.line(Point::new(left, middle), Point::new(right, middle), rule);
    for (row, fields) in rows.iter().enumerate() {
        let (top, bottom) = match row {
            0 => (strip.max.y, middle),
            _ => (middle, strip.min.y),
        };
        let mut x = left;
        for (index, &(name, text, share)) in fields.iter().enumerate() {
            let width = (right - left) * share;
            if index > 0 {
                divider(canvas, x, top, bottom);
            }
            let cell = BBox::new(Point::new(x, bottom), Point::new(x + width, top));
            field(canvas, cell, name, text, value);
            x += width;
        }
    }
    canvas.line(
        Point::new(strip.min.x, strip.max.y),
        Point::new(strip.max.x, strip.max.y),
        Pen::solid(MEDIUM),
    );
    canvas.rect(frame(), Pen::solid(THICK));
    draw_zones(canvas);
}

/// The zone border around the frame: numbers along the top and bottom,
/// letters down both sides.
fn draw_zones(canvas: &mut Canvas<'_>) {
    let frame = frame();
    let border = frame.expand(ZONE_BAND);
    let pen = Pen::solid(THIN);
    canvas.rect(border, pen);
    let label = TextStyle::new(BODY).align(Align::Center);

    let width = frame.width() / ZONE_COLUMNS as f64;
    for column in 0..ZONE_COLUMNS {
        let left = frame.min.x + column as f64 * width;
        for (inner, outer) in [(frame.max.y, border.max.y), (frame.min.y, border.min.y)] {
            if column > 0 {
                canvas.line(Point::new(left, inner), Point::new(left, outer), pen);
            }
            let at = Point::new(left + width / 2.0, (inner + outer) / 2.0 - BODY / 2.0);
            canvas.text(at, &(column + 1).to_string(), label);
        }
    }
    let height = frame.height() / ZONE_ROWS.len() as f64;
    for (row, letter) in ZONE_ROWS.iter().enumerate() {
        let top = frame.max.y - row as f64 * height;
        for (inner, outer) in [(frame.min.x, border.min.x), (frame.max.x, border.max.x)] {
            if row > 0 {
                canvas.line(Point::new(inner, top), Point::new(outer, top), pen);
            }
            let at = Point::new((inner + outer) / 2.0, top - height / 2.0 - BODY / 2.0);
            canvas.text(at, letter, label);
        }
    }
}
