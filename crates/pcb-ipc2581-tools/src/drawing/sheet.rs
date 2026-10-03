//! The drawing sheet: its size, its frame, and the title strip along its
//! bottom edge.

use pcb_ir::geom::{BBox, Point};

use super::pdf::{Align, Canvas, Pen, TextStyle};

/// Lines of a sheet, by what they draw.
pub const THICK: f64 = 0.5;
pub const MEDIUM: f64 = 0.35;
pub const THIN: f64 = 0.18;
pub const HAIR: f64 = 0.1;

/// Lettering heights, as the height of the capitals: labels, body text,
/// headings and the drawing's title.
pub const LABEL: f64 = 1.4;
pub const BODY: f64 = 1.8;
pub const HEADING: f64 = 2.5;
pub const TITLE: f64 = 3.5;

/// A sheet a drawing is laid out on, in landscape.
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetSize {
    #[default]
    A4,
    A3,
    A2,
    A1,
    /// ANSI B, 17 × 11 in.
    #[cfg_attr(feature = "cli", value(name = "ansi-b"))]
    AnsiB,
    /// ANSI C, 22 × 17 in.
    #[cfg_attr(feature = "cli", value(name = "ansi-c"))]
    AnsiC,
    /// ANSI D, 34 × 22 in.
    #[cfg_attr(feature = "cli", value(name = "ansi-d"))]
    AnsiD,
}

impl SheetSize {
    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "A4",
            Self::A3 => "A3",
            Self::A2 => "A2",
            Self::A1 => "A1",
            Self::AnsiB => "B",
            Self::AnsiC => "C",
            Self::AnsiD => "D",
        }
    }

    /// Width and height in millimetres.
    pub fn dimensions(self) -> (f64, f64) {
        match self {
            Self::A4 => (297.0, 210.0),
            Self::A3 => (420.0, 297.0),
            Self::A2 => (594.0, 420.0),
            Self::A1 => (841.0, 594.0),
            Self::AnsiB => (431.8, 279.4),
            Self::AnsiC => (558.8, 431.8),
            Self::AnsiD => (863.6, 558.8),
        }
    }

    /// The area inside the frame.
    pub fn frame(self) -> BBox {
        let (width, height) = self.dimensions();
        BBox::new(
            Point::new(MARGIN, MARGIN),
            Point::new(width - MARGIN, height - MARGIN),
        )
    }

    /// The title strip, along the bottom of the frame.
    pub fn title_strip(self) -> BBox {
        let frame = self.frame();
        BBox::new(
            frame.min,
            Point::new(frame.max.x, frame.min.y + TITLE_STRIP_HEIGHT),
        )
    }

    /// What is left of the frame to draw in.
    pub fn body(self) -> BBox {
        let frame = self.frame();
        BBox::new(
            Point::new(frame.min.x, frame.min.y + TITLE_STRIP_HEIGHT),
            frame.max,
        )
    }

    /// Width of the column of tables a sheet sets beside its view: a good
    /// third of the sheet, between what a table needs and what it can use.
    pub fn column_width(self) -> f64 {
        (self.frame().width() * 0.38).clamp(105.0, 170.0)
    }
}

/// Margin from the sheet's edge to the frame.
const MARGIN: f64 = 10.0;
/// Two rows of fields.
pub const TITLE_STRIP_HEIGHT: f64 = 12.0;

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
    pub size: SheetSize,
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

/// Draw the frame and the title strip: the design's name on the left, the
/// sheet's number on the right, and two rows of fields between them, each a
/// small label over its value.
pub fn draw_sheet(canvas: &mut Canvas<'_>, title: &TitleBlock, label: &SheetLabel) {
    /// Room between a field's edge and its lettering.
    const INSET: f64 = 1.2;
    let strip = title.size.title_strip();
    let rule = Pen::solid(THIN);
    let middle = strip.center().y;
    // A value too long for its field is lettered smaller rather than run
    // into the next.
    let fit = |canvas: &Canvas<'_>, text: &str, style: TextStyle, room: f64| {
        let natural = canvas.fonts.width(style.weight, style.size, text);
        TextStyle {
            size: style.size * (room / natural).min(1.0),
            ..style
        }
    };
    let field = |canvas: &mut Canvas<'_>, cell: BBox, name: &str, text: &str, style: TextStyle| {
        canvas.text(
            Point::new(cell.min.x + INSET, cell.max.y - 0.9 - LABEL),
            name,
            TextStyle::new(LABEL),
        );
        let style = fit(canvas, text, style, cell.width() - 2.0 * INSET);
        let at = match style.align {
            Align::Right => cell.max.x - INSET,
            _ => cell.min.x + INSET,
        };
        canvas.text(Point::new(at, cell.min.y + 1.1), text, style);
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
    let sheet = format!("{} · mm", title.size.name());
    let rows = [
        vec![
            ("DOCUMENT", title.document.as_str(), 0.36),
            ("CONTENT", label.content.as_str(), 0.25),
            ("SCALE", label.scale.as_str(), 0.1),
            ("SHEET SIZE · UNITS", sheet.as_str(), 0.14),
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
    canvas.rect(title.size.frame(), Pen::solid(THICK));
}
