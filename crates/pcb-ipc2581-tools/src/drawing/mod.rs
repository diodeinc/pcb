//! Fabrication drawings: the sheets a bare-board fabricator builds a board
//! or a board array to, as a PDF.
//!
//! A drawing states what the design data cannot be asked at a glance: what
//! the board is made of and finished in, how large it is, which holes are
//! drilled where. Every view is ordinary artwork plotted at a stated scale,
//! so a sheet printed at full size measures true.
//!
//! A drawing is A4 sheets: the board's dimensioned outline beside its
//! specification, layer stack and notes; then its drill pattern and drill
//! table; then, for an array, the array as it is delivered, and its tooling
//! holes and fiducials on a sheet for each side that carries fiducials; and
//! last every layer the fabricator images, two to a sheet, each in a colour
//! of its own.

mod blocks;
mod data;
mod pdf;
mod sheet;
mod symbols;
#[cfg(test)]
mod tests;
mod views;

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, ContourBuf, Point, Resolution};
use pcb_ir::import::ipc2581::ImportedDesign;
use pcb_ir::render::RenderOptions;

use self::blocks::{Block, Cell, Column, Figure, Material, Notes, Sample, ScoreSection, Table};
use self::data::{
    ArrayData, DrillTool, FabLayer, Fiducial, HoleKind, Ink, SCORE_ANGLE, Source, copper_weight,
    drill_tools, fab_layers, hole_count, layer_ink, mm, mm_fine, score_web, size_mm, stack_rows,
};
use self::pdf::{Align, Canvas, Dash, Document, DocumentInfo, Fonts, Pen, TextStyle};
use self::sheet::{HEADING, SheetLabel, THIN, TitleBlock, draw_sheet};
use self::views::{
    HoleMarks, Mark, OutlineFeatures, Placement, Scale, Title, TitleRoom, TitleSide, View,
};
use crate::LayoutTarget;
use crate::accessors::StackupLayerType;

pub use self::pdf::Typeface;

/// What a fabrication drawing draws and what its title strip says.
#[derive(Debug, Clone, Default)]
pub struct FabDrawingOptions {
    /// The board alone, or the array the file lays it out in.
    pub target: LayoutTarget,
    /// The design's name, where the board step's own is not wanted.
    pub title: Option<String>,
    pub revision: Option<String>,
    /// What names the data the drawing was made from: a file and its digest.
    pub source: Option<String>,
    /// The face everything is lettered in.
    pub typeface: Typeface,
}

/// How many layer views a sheet holds.
const LAYERS_PER_SHEET: usize = 2;
/// The ink a mask's openings and a legend are drawn in, and a mask the
/// design does not colour.
const MASK_INK: u32 = 0x6a1b7a;
const LEGEND_INK: u32 = 0x8a6d00;
const UNCOLOURED: u32 = 0xbdbdbd;

/// Room kept between the frame and what a sheet draws, and between blocks.
const PAD: f64 = 4.0;
/// Room a view keeps around its artwork.
const VIEW_MARGIN: f64 = 3.0;
/// How far a dimension line stands off what it measures, and the room the
/// value beside a vertical one takes.
const DIMENSION_OFFSET: f64 = 7.0;
const DIMENSION_VALUE: f64 = 13.0;
/// Room ordinates take beside a view: their leaders and the longest label.
const ORDINATES: f64 = 19.0;
/// Room between a view, with what is lettered around it, and its title.
const TITLE_DROP: f64 = 5.0;

/// Room a view keeps clear on each side, beyond the margin every view has.
#[derive(Debug, Clone, Copy, Default)]
struct Clear {
    left: f64,
    right: f64,
    above: f64,
    below: f64,
}

/// Draw the fabrication drawing of a design as a PDF.
pub fn fab_drawing(
    ipc: &Ipc2581,
    imported: &ImportedDesign,
    options: &FabDrawingOptions,
    resolution: Resolution,
) -> Result<Vec<u8>> {
    let source = Source::new(ipc, imported, resolution);
    let (outline, bounds) = views::outline(imported, ProfileSet::BoardOutlines);
    anyhow::ensure!(
        !bounds.is_empty(),
        "IPC-2581 design has no board profile to draw"
    );
    let array = match options.target {
        LayoutTarget::Board => None,
        LayoutTarget::BoardArray => ArrayData::of(&source)?,
    };
    let fonts = Fonts::new(&options.typeface).context("cannot letter the drawing")?;
    let mut drawing = Drawing {
        source: &source,
        options,
        fonts: &fonts,
        outline,
        bounds,
        array: array.is_some(),
        doc: Document::new(),
        sheets: Vec::new(),
    };

    // Layer views first: the specification says which sides are printed.
    let layers = drawing.layer_views()?;
    let printed = layers
        .iter()
        .map(|(layer, _, has_artwork)| (layer.clone(), *has_artwork))
        .collect::<Vec<_>>();
    let tools = drill_tools(imported, ArtworkScope::Board)?;
    let symbols = assign_symbols(&tools);

    // The board, its drill pattern, then the array it is delivered in.
    let specification = drawing.specification(&printed, &tools, array.as_ref());
    let notes = Block::Notes(Notes {
        title: "NOTES".to_string(),
        notes: data::notes(&source, array.as_ref()),
    });
    let carried = drawing.board_sheet(&tools, specification, drawing.stack_table(), notes)?;
    // A board with no holes has no drill sheet, only what the first sheet
    // had no room for.
    if tools.is_empty() {
        drawing.table_sheets(carried.into_iter().collect());
    } else {
        drawing.drill_sheet(&tools, &symbols, carried)?;
    }
    if let Some(array) = &array {
        let holes = hole_count(&tools);
        let mut blocks = vec![drawing.array_table(array, holes), drawing.array_key(array)];
        blocks.extend(drawing.tab_figure(array)?);
        blocks.extend(drawing.score_section(array));
        drawing.array_sheet(array, blocks)?;
        drawing.tooling_sheets(array)?;
    }
    drawing.layer_sheets(layers)?;
    drawing.finish()
}

/// Ordinate stations of the design as they fall on a sheet: along X, or
/// along Y.
fn on_sheet(placement: &Placement, stations: Vec<(f64, String)>, x: bool) -> Vec<(f64, String)> {
    stations
        .into_iter()
        .map(|(station, text)| {
            let at = placement.sheet(Point::new(station, station));
            (if x { at.x } else { at.y }, text)
        })
        .collect()
}

/// Give each tool a symbol: the tools with the most holes take the lightest
/// symbols.
fn assign_symbols(tools: &[DrillTool]) -> Vec<usize> {
    let mut order = (0..tools.len()).collect::<Vec<_>>();
    order.sort_by_key(|&tool| std::cmp::Reverse(tools[tool].hits.len()));
    let mut symbols = vec![0; tools.len()];
    for (rank, tool) in order.into_iter().enumerate() {
        symbols[tool] = rank;
    }
    symbols
}

/// A sheet as it is drawn, before the drawing knows how many it has.
struct Sheet<'a> {
    canvas: Canvas<'a>,
    /// What the sheet shows.
    content: String,
    scale: String,
}

struct Drawing<'a> {
    source: &'a Source<'a>,
    options: &'a FabDrawingOptions,
    fonts: &'a Fonts,
    /// The board's outline with its cutouts, and the bounds of its outer
    /// edge.
    outline: Vec<ContourBuf>,
    bounds: BBox,
    /// Whether the boards are delivered as an array.
    array: bool,
    doc: Document,
    sheets: Vec<Sheet<'a>>,
}

/// The ink of copper layer `number` of `count`: the outer layers in red and
/// blue, the inner ones told apart in turn.
fn copper_ink(number: usize, count: usize) -> u32 {
    // None of them a colour a mask, a legend or their chips are shown in.
    const INNER: [u32; 6] = [0x0f766e, 0xc2410c, 0x475569, 0xbe185d, 0x65a30d, 0x0891b2];
    match number {
        1 => 0xb3261e,
        number if number == count => 0x1d3fb0,
        number => INNER[(number - 2) % INNER.len()],
    }
}

/// A key to a view: a sample of each line or mark beside what it means.
fn key(rows: Vec<(Sample, String)>) -> Block {
    Block::Table(Table {
        title: "KEY".to_string(),
        columns: vec![
            Column::new("", 16.0, Align::Center),
            Column::new("", 89.0, Align::Left),
        ],
        rows: rows
            .into_iter()
            .map(|(sample, meaning)| vec![Cell::Sample(sample), Cell::Text(meaning)])
            .collect(),
        footer: None,
        header: false,
    })
}

/// A tooling hole or a fiducial of the array's own, as its sheet marks,
/// tags and lists it.
struct Tagged {
    tag: String,
    feature: String,
    mark: Mark,
    at: Point,
    diameter: f64,
}

/// The drill table's columns, TYPE taking `kind` as its share of the width.
fn drill_columns(kind: f64) -> Vec<Column> {
    vec![
        Column::new("SYM", 8.0, Align::Center),
        Column::new("QTY", 10.0, Align::Right),
        Column::new("DIA mm", 13.0, Align::Right),
        Column::new("TYPE", kind, Align::Left),
    ]
}

/// A column of blocks down a sheet's right side, and the room left of it.
struct Beside {
    /// Each block with its top-left corner.
    placed: Vec<(Block, Point)>,
    /// Blocks the column has no room for.
    left_over: Vec<Block>,
    /// What is left of the sheet for its view.
    view: BBox,
}

/// What a sheet has to draw in: its body, less a margin.
fn area() -> BBox {
    let body = sheet::body();
    BBox::new(
        Point::new(body.min.x + PAD, body.min.y + PAD),
        Point::new(body.max.x - PAD, body.max.y - PAD),
    )
}

/// Fit `bounds` of the design into `region` of a sheet, keeping `clear`
/// around it for what is lettered there.
fn place(bounds: BBox, region: BBox, clear: Clear) -> (Scale, Placement) {
    let room = BBox::new(
        Point::new(
            region.min.x + VIEW_MARGIN + clear.left,
            region.min.y + VIEW_MARGIN + clear.below,
        ),
        Point::new(
            region.max.x - VIEW_MARGIN - clear.right,
            region.max.y - VIEW_MARGIN - clear.above,
        ),
    );
    let scale = Scale::fit(bounds.width(), bounds.height(), room.width(), room.height());
    (scale, Placement::centered(bounds, room.center(), scale))
}

/// Fit `bounds` and a title taking `title` room into `region`: the title
/// under the view, or beside it where that draws the view at a larger
/// scale.
fn place_titled(bounds: BBox, region: BBox, title: TitleRoom) -> (Scale, Placement, TitleSide) {
    // Under a view, details too long for one line of the region stand a line
    // each.
    let (under, height) = if title.below > region.width() - 2.0 * VIEW_MARGIN {
        (TitleSide::Stacked, title.height())
    } else {
        (TitleSide::Below, views::TITLE_HEIGHT)
    };
    let below = Clear {
        below: TITLE_DROP + height,
        ..Clear::default()
    };
    let beside = Clear {
        right: TITLE_DROP + title.beside,
        ..Clear::default()
    };
    let (scale_below, placed_below) = place(bounds, region, below);
    let (scale_beside, placed_beside) = place(bounds, region, beside);
    if scale_beside.factor() <= scale_below.factor() {
        return (scale_below, placed_below, under);
    }
    // A view with its title beside it stands at the left of its region, so
    // all the room it leaves is in one strip.
    let left = region.min.x + VIEW_MARGIN + bounds.width() * scale_beside.factor() / 2.0;
    let placed = Placement {
        at: Point::new(left, placed_beside.at.y),
        ..placed_beside
    };
    (scale_beside, placed, TitleSide::Beside)
}

impl<'a> Drawing<'a> {
    fn canvas(&self) -> Canvas<'a> {
        Canvas::new(self.fonts)
    }

    fn render_options(&self, view: &View, viewport: BBox) -> RenderOptions {
        RenderOptions::default()
            .with_accuracy(self.source.resolution.accuracy)
            .with_styles(view.styles.clone())
            .with_viewport(viewport)
    }

    /// Lay `blocks` down a column on the sheet's right, in order, as far as
    /// they fit. A block is never split: one the column has no room for is
    /// left over with every block after it.
    fn column(&self, blocks: Vec<Block>) -> Beside {
        let area = area();
        let width = sheet::COLUMN_WIDTH;
        let left = area.max.x - width;
        let mut placed = Vec::new();
        let mut left_over = Vec::new();
        let mut y = area.max.y;
        for block in blocks {
            let height = block.height(self.fonts, width);
            if !left_over.is_empty() || y - height < area.min.y {
                left_over.push(block);
                continue;
            }
            placed.push((block, Point::new(left, y)));
            y -= height + PAD;
        }
        let right = if placed.is_empty() {
            area.max.x
        } else {
            left - PAD
        };
        Beside {
            placed,
            left_over,
            view: BBox::new(area.min, Point::new(right, area.max.y)),
        }
    }

    /// Sheets of nothing but blocks, for those a view's sheet had no room
    /// for: columns across the sheet, left to right.
    fn table_sheets(&mut self, blocks: Vec<Block>) {
        let area = area();
        let width = sheet::COLUMN_WIDTH;
        // A table taller than a column continues in the next.
        let mut blocks = blocks
            .into_iter()
            .flat_map(|block| block.split(area.height()))
            .collect::<Vec<_>>();
        let count = ((area.width() + PAD) / (width + PAD)).floor().max(1.0) as usize;
        // Columns spread evenly across the sheet.
        let pitch = (area.width() - width) / (count - 1).max(1) as f64;
        while !blocks.is_empty() {
            let mut canvas = self.canvas();
            let mut remaining = blocks.into_iter().peekable();
            let mut titles = Vec::new();
            for column in 0..count {
                let x = area.min.x + column as f64 * pitch;
                let mut y = area.max.y;
                while let Some(block) = remaining.peek() {
                    let height = block.height(self.fonts, width);
                    // A block taller than a whole column is drawn anyway
                    // rather than never.
                    if y - height < area.min.y && y < area.max.y {
                        break;
                    }
                    block.draw(&mut canvas, Point::new(x, y), width);
                    titles.push(block.title().to_string());
                    remaining.next();
                    y -= height + PAD;
                }
            }
            blocks = remaining.collect();
            self.sheets.push(Sheet {
                canvas,
                content: titles.join(", "),
                scale: "NONE".to_string(),
            });
        }
    }

    /// What a fabricator quotes and builds to, with a colour chip for every
    /// ink and finish.
    fn specification(
        &self,
        layers: &[(FabLayer, bool)],
        tools: &[DrillTool],
        array: Option<&ArrayData>,
    ) -> Block {
        let rows = data::specification(self.source, layers, tools, self.bounds, array)
            .into_iter()
            .map(|row| {
                let chip = row.chip.map_or(Cell::Empty, Cell::Chip);
                vec![Cell::text(row.label), chip, Cell::Bold(row.value)]
            })
            .collect();
        Block::Table(Table {
            title: "SPECIFICATION".to_string(),
            columns: vec![
                Column::new("", 29.0, Align::Left),
                Column::new("", 8.0, Align::Left),
                Column::new("", 68.0, Align::Left),
            ],
            rows,
            footer: None,
            header: false,
        })
    }

    /// The drill table: a symbol, a count and a finished size for every
    /// tool, and what its holes are.
    fn drill_table(&mut self, tools: &[DrillTool], symbols: &[usize]) -> Result<Table> {
        let rows = tools
            .iter()
            .zip(symbols)
            .map(|(tool, &index)| {
                let symbol = views::symbol_view(index)?;
                let viewport = BBox::from_point(Point::ZERO).expand(views::SYMBOL_SIZE);
                let plot = self
                    .doc
                    .plot(&symbol.artwork, &self.render_options(&symbol, viewport))?;
                // What the holes are, then what sets them apart from a round
                // hole through the board.
                let kind = [Some(tool.usage().to_string()), tool.span(), tool.shape()];
                let kind = kind.into_iter().flatten().collect::<Vec<_>>().join(" · ");
                Ok(vec![
                    Cell::Symbol(plot),
                    Cell::text(tool.hits.len().to_string()),
                    Cell::Bold(mm_fine(tool.diameter)),
                    Cell::text(kind),
                ])
            })
            .collect::<Result<Vec<_>>>()?;
        // An array's drawing lists the holes of one board; the array sheet
        // totals them.
        let footer = vec![
            Cell::Empty,
            Cell::Bold(hole_count(tools).to_string()),
            Cell::Bold("TOTAL".to_string()),
            if self.array {
                Cell::text("ONE BOARD")
            } else {
                Cell::Empty
            },
        ];
        Ok(Table {
            title: "DRILL TABLE, FINISHED SIZES".to_string(),
            columns: drill_columns(51.0),
            rows,
            footer: Some(footer),
            header: true,
        })
    }

    /// The layer stack: a section, number, name, material and thickness for
    /// every stackup row.
    fn stack_table(&self) -> Option<Block> {
        /// Dielectrics under their hatching.
        const CORE: u32 = 0xccd5ae;
        const PREPREG: u32 = 0xe9edc9;
        const DIELECTRIC: u32 = 0xebe6dc;
        let stackup = self.source.stackup.as_ref()?;
        let stack = stack_rows(stackup);
        let copper_layers = stack
            .iter()
            .filter(|layer| layer.layer_type == StackupLayerType::Conductor)
            .count();
        let sum = stack
            .iter()
            .filter_map(|layer| layer.thickness_mm)
            .sum::<f64>();
        let mut copper = 0;
        let rows = stack
            .into_iter()
            .map(|layer| {
                let optional = |value: Option<String>| value.map_or(Cell::Empty, Cell::Text);
                // A thickness of nothing is one the stackup does not state.
                let stated = layer.thickness_mm.filter(|thickness| *thickness > 0.0);
                let thickness = optional(stated.map(|mm| format!("{mm:.3}")));
                let material = layer.material.clone().unwrap_or_default().to_uppercase();
                match layer.layer_type {
                    StackupLayerType::Conductor => {
                        copper += 1;
                        vec![
                            Cell::Section(Material::Copper, copper_ink(copper, copper_layers)),
                            Cell::Bold(format!("L{copper}")),
                            Cell::Bold(layer.name.clone()),
                            Cell::text("COPPER"),
                            thickness,
                            optional(stated.map(copper_weight)),
                            Cell::Empty,
                        ]
                    }
                    StackupLayerType::Soldermask => {
                        // The mask over the first copper is the top's.
                        let side = if copper == 0 { Side::Top } else { Side::Bottom };
                        let ink = layer_ink(self.source, LayerRole::Soldermask, side);
                        let ink = ink.unwrap_or_else(|| Ink::of(None));
                        vec![
                            Cell::Section(Material::Mask, ink.color.unwrap_or(UNCOLOURED)),
                            Cell::Empty,
                            Cell::text(&layer.name),
                            Cell::text(format!("MASK, {}", ink.name)),
                            thickness,
                            Cell::Empty,
                            Cell::Empty,
                        ]
                    }
                    StackupLayerType::Other => unreachable!("stack_rows lists no such row"),
                    dielectric => {
                        let (section, tint, kind) = match dielectric {
                            StackupLayerType::DielectricCore => (Material::Core, CORE, "CORE"),
                            StackupLayerType::DielectricPrepreg => {
                                (Material::Prepreg, PREPREG, "PREPREG")
                            }
                            _ => (Material::Dielectric, DIELECTRIC, "DIELECTRIC"),
                        };
                        vec![
                            Cell::Section(section, tint),
                            Cell::Empty,
                            Cell::Empty,
                            // A material named for its kind says both.
                            Cell::text(if material.contains(kind) {
                                material
                            } else {
                                format!("{kind} {material}").trim_end().to_string()
                            }),
                            thickness,
                            Cell::Empty,
                            optional(layer.dielectric_constant.map(|dk| format!("{dk:.2}"))),
                        ]
                    }
                }
            })
            .collect::<Vec<_>>();
        // The stated thickness where the layers add up to it; where they do
        // not, the table totals what it lists and the specification states
        // the other.
        let footer = stackup.overall_thickness_mm.map(|thickness| {
            let (what, total) = if (sum - thickness).abs() < 0.002 {
                ("FINISHED THICKNESS", thickness)
            } else {
                ("SUM OF LAYERS", sum)
            };
            let mut footer = vec![Cell::Empty; 7];
            footer[3] = Cell::Bold(what.to_string());
            footer[4] = Cell::Bold(format!("{total:.3}"));
            footer
        });
        Some(Block::Table(Table {
            title: "LAYER STACK".to_string(),
            columns: vec![
                Column::new("", 11.0, Align::Center),
                Column::new("NO.", 7.0, Align::Left),
                Column::new("LAYER", 21.0, Align::Left),
                Column::new("MATERIAL", 30.0, Align::Left),
                Column::new("mm", 12.0, Align::Right),
                Column::new("COPPER", 13.0, Align::Right),
                Column::new("Dk", 9.0, Align::Right),
            ],
            rows,
            footer,
            header: true,
        }))
    }

    /// What the array adds to its boards: how it steps and what holds them.
    fn array_table(&self, array: &ArrayData, board_holes: usize) -> Block {
        let mut rows = Vec::new();
        let mut row = |item: &str, value: String| {
            rows.push(vec![Cell::text(item), Cell::Bold(value)]);
        };
        row("ARRAY SIZE", size_mm(array.bounds));
        let boards = array.boards.len();
        let grid = array.grid.as_ref();
        let layout = grid.map(|grid| format!(" · {} ACROSS × {} UP", grid.columns, grid.rows));
        row("BOARDS", format!("{boards}{}", layout.unwrap_or_default()));
        let steps = [
            ("X", grid.and_then(|grid| grid.pitch_x)),
            ("Y", grid.and_then(|grid| grid.pitch_y)),
        ]
        .into_iter()
        .filter_map(|(axis, pitch)| Some(format!("{axis} {}", mm_fine(pitch?.mm()))))
        .collect::<Vec<_>>();
        if !steps.is_empty() {
            row("BOARD STEP", steps.join(" · "));
        }
        row("SEPARATION", array.separation().to_string());
        if !array.scores.is_empty() {
            let web = self.source.thickness().and_then(score_web);
            let web = web.map(|web| format!(" · WEB {}", mm(web)));
            row(
                "V-SCORE",
                format!(
                    "{} LINES · {SCORE_ANGLE}°{} · BOTH SIDES",
                    array.scores.len(),
                    web.unwrap_or_default()
                ),
            );
        }
        // The border as the ordinates measure it: from the array's edge to
        // the nearest board's.
        let held = array
            .boards
            .iter()
            .copied()
            .fold(BBox::empty(), BBox::union);
        if !held.is_empty() {
            let bounds = array.bounds;
            let sides = [
                ("TOP", bounds.max.y - held.max.y),
                ("RIGHT", bounds.max.x - held.max.x),
                ("BOTTOM", held.min.y - bounds.min.y),
                ("LEFT", held.min.x - bounds.min.x),
            ]
            .map(|(side, width)| (side, mm(width)));
            let border = if sides.iter().all(|(_, width)| *width == sides[0].1) {
                format!("{} ALL ROUND", sides[0].1)
            } else {
                let sides = sides.iter().map(|(side, width)| format!("{side} {width}"));
                sides.collect::<Vec<_>>().join(" · ")
            };
            row("BORDER TO BOARDS", border);
        }
        let corners = array
            .outlines
            .first()
            .map(|outline| OutlineFeatures::of(outline));
        if let Some(radii) = corners.and_then(|corners| corners.radii()) {
            row("CORNER RADII", radii);
        }
        let own = hole_count(&array.tools);
        row(
            "HOLES",
            format!(
                "{} TOTAL · {} IN BOARDS · {own} IN ARRAY",
                boards * board_holes + own,
                boards * board_holes
            ),
        );
        Block::Table(Table {
            title: "BOARD ARRAY".to_string(),
            columns: vec![
                Column::new("", 29.0, Align::Left),
                Column::new("", 76.0, Align::Left),
            ],
            rows,
            footer: None,
            header: false,
        })
    }

    /// How the array's view draws what it shows.
    fn array_key(&self, array: &ArrayData) -> Block {
        let rows = [
            (true, Sample::ArrayProfile, "ARRAY PROFILE"),
            (true, Sample::BoardProfile, "BOARD PROFILE"),
            (!array.scores.is_empty(), Sample::Score, "V-SCORE LINE"),
            (!array.removal.is_empty(), Sample::Routed, "ROUTED OUT"),
        ];
        let rows = rows
            .into_iter()
            .filter(|(shown, ..)| *shown)
            .map(|(_, sample, meaning)| (sample, meaning.to_string()));
        key(rows.collect())
    }

    /// The array's tooling sheets: the array with every tooling hole and
    /// fiducial marked and tagged, beside a table that places each. A side
    /// that carries fiducials has a sheet of its own, so marks a millimetre
    /// apart on opposite sides are never drawn over each other; an array
    /// with fiducials on neither has one sheet for its holes, and one with
    /// neither holes nor fiducials has none.
    fn tooling_sheets(&mut self, array: &ArrayData) -> Result<()> {
        let on = |side: Side| array.fiducials.iter().any(|fiducial| fiducial.side == side);
        let sides = [Side::Top, Side::Bottom]
            .into_iter()
            .filter(|side| on(*side));
        let sides = sides.map(Some).collect::<Vec<_>>();
        if sides.is_empty() && array.tooling().is_empty() {
            return Ok(());
        }
        let sides = if sides.is_empty() { vec![None] } else { sides };
        // Tags count on from one side's sheet to the next.
        let (mut own_tagged, mut board_tagged) = (0, 0);
        for side in sides {
            let tagged = self.tooling_sheet(array, side, (own_tagged, board_tagged))?;
            own_tagged += tagged.0;
            board_tagged += tagged.1;
        }
        Ok(())
    }

    /// One tooling sheet: the tooling holes, and the fiducials of `side`.
    /// The array's own are placed from its datum. A board's are placed from
    /// the lower-left corner of the board's extents, as every board has
    /// them, and tagged in a detail of one board. Tags are numbered on from
    /// `tagged`, the array's own and the boards'; returns how many of each
    /// this sheet tags.
    fn tooling_sheet(
        &mut self,
        array: &ArrayData,
        side: Option<Side>,
        tagged: (usize, usize),
    ) -> Result<(usize, usize)> {
        let datum = array.bounds.min;
        let tooling = array.tooling();
        let on_side = |fiducial: &&Fiducial| Some(fiducial.side) == side;
        let fiducials = array.fiducials.iter().filter(on_side);
        let (cells, own): (Vec<&Fiducial>, Vec<&Fiducial>) =
            fiducials.partition(|fiducial| fiducial.board.is_some());
        let per_board = side.and_then(|side| array.board_fiducials(side));
        let per_board = per_board.unwrap_or_default();

        // What the array's view marks and tags: its tooling holes, then its
        // own fiducials.
        let holes = tooling.iter().enumerate().map(|(index, (at, tool))| {
            let feature = ["TOOLING HOLE".to_string(), tool.usage().to_string()];
            let feature = feature.into_iter().chain(tool.span()).collect::<Vec<_>>();
            Tagged {
                tag: format!("T{}", index + 1),
                feature: feature.join(" "),
                mark: Mark::Tooling,
                at: *at,
                diameter: tool.diameter,
            }
        });
        let own_fiducials = own.iter().enumerate().map(|(index, fiducial)| Tagged {
            tag: format!("F{}", tagged.0 + index + 1),
            feature: "ARRAY FIDUCIAL".to_string(),
            mark: Mark::ArrayFiducial,
            at: fiducial.at,
            diameter: fiducial.diameter,
        });
        let features = holes.chain(own_fiducials).collect::<Vec<_>>();
        let board_tags = (1..=per_board.len()).map(|index| format!("B{}", tagged.1 + index));
        let board_tags = board_tags.collect::<Vec<_>>();

        let row = |tag: &str, feature: &str, diameter: f64, at: Point| {
            vec![
                Cell::Bold(tag.to_string()),
                Cell::text(feature),
                Cell::text(mm_fine(diameter)),
                Cell::text(mm(at.x)),
                Cell::text(mm(at.y)),
            ]
        };
        let array_rows = features
            .iter()
            .map(|item| row(&item.tag, &item.feature, item.diameter, item.at - datum));
        let board_rows = board_tags
            .iter()
            .zip(&per_board)
            .map(|(tag, fiducial)| row(tag, "BOARD FIDUCIAL", fiducial.diameter, fiducial.at));
        let table = Block::Table(Table {
            title: "TOOLING HOLES AND FIDUCIALS".to_string(),
            columns: vec![
                Column::new("TAG", 9.0, Align::Left),
                Column::new("FEATURE", 44.0, Align::Left),
                Column::new("DIA mm", 14.0, Align::Right),
                Column::new("X mm", 19.0, Align::Right),
                Column::new("Y mm", 19.0, Align::Right),
            ],
            rows: array_rows.chain(board_rows).collect(),
            footer: None,
            header: true,
        });
        let board_fiducials = if per_board.is_empty() {
            format!("BOARD FIDUCIAL · {} · PER DATA", cells.len())
        } else {
            "B · BOARD FIDUCIAL · X Y FROM BOARD DATUM, EACH BOARD".to_string()
        };
        let marked = [
            (
                !tooling.is_empty(),
                Sample::Mark(Mark::Tooling),
                "T · TOOLING HOLE · X Y FROM ARRAY DATUM".to_string(),
            ),
            (
                !own.is_empty(),
                Sample::Mark(Mark::ArrayFiducial),
                "F · ARRAY FIDUCIAL · X Y FROM ARRAY DATUM".to_string(),
            ),
            (
                !cells.is_empty(),
                Sample::Mark(Mark::BoardFiducial),
                board_fiducials,
            ),
        ];
        let marked = marked.into_iter().filter(|(shown, ..)| *shown);
        let key = key(marked
            .map(|(_, sample, meaning)| (sample, meaning))
            .collect());
        // The key and the detail stay beside the view; a table too long to
        // join them follows on a sheet of its own.
        let detail = self.board_detail(array, &per_board, &board_tags)?;
        let blocks = [Some(key), detail, Some(table)];
        let column = self.column(blocks.into_iter().flatten().collect());

        let mut canvas = self.canvas();
        let bounds = array.bounds;
        let clear = Clear {
            left: views::TAGS,
            right: views::TAGS,
            above: views::TAGS,
            below: views::TAGS + TITLE_DROP + views::TITLE_HEIGHT,
        };
        let (scale, placement) = place(bounds, column.view, clear);
        let view = views::tooling_view(self.source, array, scale.factor())?;
        self.draw_view(&mut canvas, &view, bounds, placement)?;
        for item in &features {
            let hole = item.diameter / 2.0 * placement.scale;
            item.mark.draw(&mut canvas, placement.sheet(item.at), hole);
        }
        for fiducial in &cells {
            Mark::BoardFiducial.draw(&mut canvas, placement.sheet(fiducial.at), 0.0);
        }
        views::datum(&mut canvas, placement.sheet(datum));
        let drawn = placement.sheet_box(bounds);
        let tags = features
            .into_iter()
            .map(|item| (placement.sheet(item.at), item.mark.reach(), item.tag))
            .collect::<Vec<_>>();
        views::tags(&mut canvas, drawn, &tags);
        let name = match side {
            Some(Side::Bottom) => "ARRAY TOOLING · BOTTOM SIDE",
            Some(_) => "ARRAY TOOLING · TOP SIDE",
            None => "ARRAY TOOLING",
        };
        let title = Title {
            name: name.to_string(),
            details: vec![format!("SCALE {scale}"), "VIEWED FROM TOP".to_string()],
        };
        let top = Point::new(drawn.center().x, drawn.min.y - views::TAGS - TITLE_DROP);
        title.draw(&mut canvas, top, TitleSide::Below, scale);
        self.finish_sheet(canvas, column, &name.replace(" · ", ", "), scale);
        Ok((own.len(), per_board.len()))
    }

    /// One board in its cell, drawn large enough to tag the fiducials
    /// beside it: `fiducials` from the board's datum, each with its tag.
    fn board_detail(
        &mut self,
        array: &ArrayData,
        fiducials: &[Fiducial],
        tags: &[String],
    ) -> Result<Option<Block>> {
        let Some(board) = array.central_board().filter(|_| !fiducials.is_empty()) else {
            return Ok(None);
        };
        let board = array.boards[board];
        let placed = fiducials.iter().map(|fiducial| board.min + fiducial.at);
        let window = placed
            .clone()
            .fold(board, |window, at| window.union(BBox::from_point(at)))
            .expand(3.0);
        let (width, height) = (sheet::COLUMN_WIDTH - 24.0, 54.0);
        let scale = Scale::fit(window.width(), window.height(), width, height);
        let view = views::tooling_view(self.source, array, scale.factor())?;
        let plot = self
            .doc
            .plot(&view.artwork, &self.render_options(&view, window))?;
        Ok(Some(Block::Figure(Figure {
            title: format!("BOARD FIDUCIALS · EACH BOARD · SCALE {scale}"),
            caption: "BOARD DATUM AT THE LOWER LEFT OF THE BOARD'S EXTENTS".to_string(),
            plot,
            center: window.center(),
            scale: scale.factor(),
            height: window.height() * scale.factor(),
            fiducials: placed.zip(tags.iter().cloned()).collect(),
            board: Some(board),
        })))
    }

    /// One breakaway tab, drawn large enough to read its perforations.
    fn tab_figure(&mut self, array: &ArrayData) -> Result<Option<Block>> {
        let Some(tab) = array.tab() else {
            return Ok(None);
        };
        let (width, height) = (sheet::COLUMN_WIDTH - 10.0, 46.0);
        let scale = Scale::fit(tab.bounds.width(), tab.bounds.height(), width, height);
        let view = views::array_view(self.source, array, scale.factor())?;
        let plot = self
            .doc
            .plot(&view.artwork, &self.render_options(&view, tab.bounds))?;
        Ok(Some(Block::Figure(Figure {
            title: format!("DETAIL A · TAB · SCALE {scale}"),
            caption: format!(
                "{}X DIA {} NPTH · {} CENTRE TO CENTRE",
                tab.holes,
                mm_fine(tab.diameter),
                mm_fine(tab.pitch)
            ),
            plot,
            center: tab.bounds.center(),
            scale: scale.factor(),
            height: tab.bounds.height() * scale.factor(),
            fiducials: Vec::new(),
            board: None,
        })))
    }

    /// A V-score in section, where the array is scored.
    fn score_section(&self, array: &ArrayData) -> Option<Block> {
        if array.scores.is_empty() {
            return None;
        }
        let thickness = self.source.thickness()?;
        Some(Block::ScoreSection(ScoreSection {
            title: "V-SCORE SECTION".to_string(),
            thickness: mm(thickness),
            web: score_web(thickness).map(mm),
            angle: format!("{SCORE_ANGLE}°"),
        }))
    }

    /// Plot `view`, draw it where `placement` puts it, and letter `title`
    /// on the side of it that was kept clear.
    fn draw_titled(
        &mut self,
        canvas: &mut Canvas<'_>,
        view: &View,
        bounds: BBox,
        (scale, placement, side): (Scale, Placement, TitleSide),
        title: &Title,
    ) -> Result<()> {
        self.draw_view(canvas, view, bounds, placement)?;
        let drawn = placement.sheet_box(bounds);
        let top = match side {
            TitleSide::Below | TitleSide::Stacked => {
                Point::new(drawn.center().x, drawn.min.y - TITLE_DROP)
            }
            // Beside the view's foot, where a drawing's eye lands last.
            TitleSide::Beside => Point::new(
                drawn.max.x + TITLE_DROP,
                drawn.min.y + title.height_beside(),
            ),
        };
        title.draw(canvas, top, side, scale);
        Ok(())
    }

    /// Plot `view` and draw it where `placement` puts it.
    fn draw_view(
        &mut self,
        canvas: &mut Canvas<'_>,
        view: &View,
        bounds: BBox,
        placement: Placement,
    ) -> Result<()> {
        // Room for what overhangs the bounds: line weights, symbols on an
        // edge, a score drawn past the outline.
        let viewport = bounds.expand(5.0 / placement.scale);
        let plot = self
            .doc
            .plot(&view.artwork, &self.render_options(view, viewport))?;
        canvas.place(plot, placement.anchor, placement.at, placement.scale);
        Ok(())
    }

    /// The board's sheet: its outline, dimensioned, beside its
    /// specification, its layer stack and the notes. The specification and
    /// the notes never leave this sheet; a stack too tall to share it is
    /// returned to be set with the drill table.
    fn board_sheet(
        &mut self,
        tools: &[DrillTool],
        specification: Block,
        stack: Option<Block>,
        notes: Block,
    ) -> Result<Option<Block>> {
        let all = [
            Some(specification.clone()),
            stack.clone(),
            Some(notes.clone()),
        ];
        let column = self.column(all.into_iter().flatten().collect());
        let (column, carried) = if column.left_over.is_empty() {
            (column, None)
        } else {
            (self.column(vec![specification, notes]), stack)
        };
        let mut canvas = self.canvas();
        let bounds = self.bounds;

        // An axis whose outline has edges between its extents is dimensioned
        // by ordinates from the lower-left corner; a plain one by its length.
        let features = OutlineFeatures::of(&self.outline);
        // The extents are stations too: an outline whose ends are arcs
        // still reads from its datum to its overall size.
        let across = features.edges_x.iter().copied();
        let across = views::stations(across.chain([bounds.min.x, bounds.max.x]), bounds.min.x);
        let up = features.edges_y.iter().copied();
        let up = views::stations(up.chain([bounds.min.y, bounds.max.y]), bounds.min.y);
        let across = Some(across).filter(|stations| stations.len() > 2);
        let up = Some(up).filter(|stations| stations.len() > 2);
        let radii = features
            .radii()
            .map(|radii| format!("CORNER RADII {radii}"));
        let title_height = views::TITLE_HEIGHT + radii.iter().len() as f64 * views::TITLE_LEADING;
        let below = if across.is_some() {
            ORDINATES
        } else {
            DIMENSION_OFFSET + 2.0
        };
        let (left, right) = if up.is_some() {
            (ORDINATES, 0.0)
        } else {
            (0.0, DIMENSION_OFFSET + DIMENSION_VALUE)
        };
        let clear = Clear {
            left,
            right,
            below: below + TITLE_DROP + title_height,
            ..Clear::default()
        };
        let (scale, placement) = place(bounds, column.view, clear);
        // The holes that are part of the board's shape: every hole but the
        // vias, each by its edge.
        let mechanical = tools
            .iter()
            .filter(|tool| tool.kind != HoleKind::Via)
            .cloned()
            .collect::<Vec<_>>();
        let view = views::board_view(&self.outline, &mechanical, HoleMarks::Edges, scale.factor())?;
        self.draw_view(&mut canvas, &view, bounds, placement)?;

        let drawn = placement.sheet_box(bounds);
        let datum = across.is_some() || up.is_some();
        match across {
            Some(across) => {
                let across = on_sheet(&placement, across, true);
                views::ordinates_horizontal(&mut canvas, &across, drawn.min.y);
            }
            None => views::dimension_horizontal(
                &mut canvas,
                drawn.min.x,
                drawn.max.x,
                drawn.min.y,
                drawn.min.y - DIMENSION_OFFSET,
                &mm(bounds.width()),
            ),
        }
        match up {
            Some(up) => {
                let up = on_sheet(&placement, up, false);
                views::ordinates_vertical(&mut canvas, &up, drawn.min.x);
            }
            None => views::dimension_vertical(
                &mut canvas,
                drawn.min.y,
                drawn.max.y,
                drawn.max.x,
                drawn.max.x + DIMENSION_OFFSET,
                &mm(bounds.height()),
            ),
        }
        if datum {
            views::datum(&mut canvas, drawn.min);
        }
        let title = Title {
            name: "BOARD OUTLINE".to_string(),
            details: vec![format!("SCALE {scale} · VIEWED FROM TOP")],
        };
        let top = Point::new(drawn.center().x, drawn.min.y - below - TITLE_DROP);
        title.draw(&mut canvas, top, TitleSide::Below, scale);
        if let Some(radii) = radii {
            let at = Point::new(top.x, top.y - title_height + 1.0);
            canvas.text(at, &radii, TextStyle::new(sheet::BODY).align(Align::Center));
        }
        self.finish_sheet(canvas, column, "BOARD", scale);
        Ok(carried)
    }

    /// Draw a sheet's column and file the sheet, then sheets of the blocks
    /// its column had no room for.
    fn finish_sheet(
        &mut self,
        mut canvas: Canvas<'a>,
        column: Beside,
        content: &str,
        scale: Scale,
    ) {
        let width = sheet::COLUMN_WIDTH;
        for (block, at) in &column.placed {
            block.draw(&mut canvas, *at, width);
        }
        self.sheets.push(Sheet {
            canvas,
            content: content.to_string(),
            scale: scale.to_string(),
        });
        self.table_sheets(column.left_over);
    }

    /// The drill sheet: the board's outline with every hole marked by its
    /// tool's symbol, and the drill table.
    ///
    /// The pattern is drawn as large as the sheet allows. The table sits in
    /// a column beside it where that costs the pattern no scale; else in the
    /// strip the pattern leaves beside itself, above its title; and on a
    /// sheet of its own after it where it fits neither.
    fn drill_sheet(
        &mut self,
        tools: &[DrillTool],
        symbols: &[usize],
        carried: Option<Block>,
    ) -> Result<()> {
        /// The narrowest a drill table is set.
        const NARROWEST: f64 = 58.0;
        let bounds = self.bounds;
        let title = |scale: Scale| Title {
            name: "DRILL PATTERN".to_string(),
            details: vec![
                format!("SCALE {scale}"),
                "VIEWED FROM TOP".to_string(),
                "SYMBOLS PER DRILL TABLE".to_string(),
            ],
        };
        // Lettered before its scale is settled, to measure the room it takes.
        let sample = title(Scale::FULL);
        let table = self.drill_table(tools, symbols)?;
        let tables = std::iter::once(Block::Table(table.clone()))
            .chain(carried.clone())
            .collect::<Vec<_>>();
        let beside = self.column(tables.clone());
        let alone = self.column(Vec::new());
        let room = sample.room(self.fonts);
        let beside_placed = place_titled(bounds, beside.view, room);
        let alone_placed = place_titled(bounds, alone.view, room);
        let pattern = HoleMarks::Symbols(symbols);
        let mut canvas = self.canvas();
        if alone_placed.0.factor() <= beside_placed.0.factor() {
            let scale = beside_placed.0;
            let view = views::board_view(&self.outline, tools, pattern, scale.factor())?;
            self.draw_titled(&mut canvas, &view, bounds, beside_placed, &title(scale))?;
            // The sheet says what it holds besides the drill table.
            let carried = beside.placed.iter().skip(1).map(|(block, _)| block.title());
            let content = std::iter::once("DRILL").chain(carried).collect::<Vec<_>>();
            let content = content.join(", ");
            self.finish_sheet(canvas, beside, &content, scale);
            return Ok(());
        }
        // Alone on its sheet, the pattern leaves the table a place of its
        // own beside it: the strip above a title lettered at its right, or
        // the corner under it, clear of a title lettered below. Set there,
        // TYPE takes a smaller share of the table.
        let (scale, placement, side) = alone_placed;
        let area = area();
        let narrow = Block::Table(Table {
            columns: drill_columns(25.0),
            ..table
        });
        let height = narrow.height(self.fonts, NARROWEST);
        let drawn = placement.sheet_box(bounds);
        let (placement, corner) = match side {
            TitleSide::Beside => {
                let strip = BBox::new(
                    Point::new(
                        drawn.max.x + TITLE_DROP,
                        drawn.min.y + sample.height_beside() + TITLE_DROP,
                    ),
                    Point::new(area.max.x, drawn.max.y),
                );
                let fits = strip.width() >= NARROWEST && height <= strip.height();
                (
                    placement,
                    fits.then_some(Point::new(strip.min.x, strip.max.y)),
                )
            }
            TitleSide::Below | TitleSide::Stacked => {
                // Drawn at the top of the sheet, the pattern leaves the most
                // room under it.
                let raised = Placement {
                    at: Point::new(
                        placement.at.x,
                        area.max.y - VIEW_MARGIN - bounds.height() * scale.factor() / 2.0,
                    ),
                    ..placement
                };
                let under = raised.sheet_box(bounds).min.y - TITLE_DROP;
                let title_right = raised.at.x + sample.width_below(self.fonts) / 2.0;
                let left = area.max.x - NARROWEST;
                if area.min.y + height <= under && left >= title_right + TITLE_DROP {
                    (raised, Some(Point::new(left, area.min.y + height)))
                } else {
                    (placement, None)
                }
            }
        };
        let view = views::board_view(&self.outline, tools, pattern, scale.factor())?;
        let placed = (scale, placement, side);
        self.draw_titled(&mut canvas, &view, bounds, placed, &title(scale))?;
        let tables = match corner {
            Some(corner) => {
                narrow.draw(&mut canvas, corner, NARROWEST);
                carried.into_iter().collect()
            }
            None => tables,
        };
        self.finish_sheet(canvas, alone, "DRILL", scale);
        self.table_sheets(tables);
        Ok(())
    }

    /// The array's sheet: the array as it is delivered, with ordinates from
    /// its lower-left corner to every board edge and score, beside `blocks`.
    fn array_sheet(&mut self, array: &ArrayData, blocks: Vec<Block>) -> Result<()> {
        let column = self.column(blocks);
        let mut canvas = self.canvas();
        let bounds = array.bounds;
        let clear = Clear {
            left: ORDINATES,
            below: ORDINATES + TITLE_DROP + views::TITLE_HEIGHT,
            ..Clear::default()
        };
        let (scale, placement) = place(bounds, column.view, clear);
        let view = views::array_view(self.source, array, scale.factor())?;
        self.draw_view(&mut canvas, &view, bounds, placement)?;

        let drawn = placement.sheet_box(bounds);
        let edges = |low: fn(BBox) -> f64, high: fn(BBox) -> f64| {
            array
                .boards
                .iter()
                .chain([&bounds])
                .flat_map(move |board| [low(*board), high(*board)])
        };
        // A score is drawn where it cuts: a vertical one is a station in X.
        let scores = |vertical: bool| {
            array.scores.iter().filter_map(move |line| {
                let run = line.end - line.start;
                match (vertical, run.x.abs() < 1e-6, run.y.abs() < 1e-6) {
                    (true, true, _) => Some(line.start.x),
                    (false, _, true) => Some(line.start.y),
                    _ => None,
                }
            })
        };
        let across = views::stations(
            edges(|board| board.min.x, |board| board.max.x).chain(scores(true)),
            bounds.min.x,
        );
        let up = views::stations(
            edges(|board| board.min.y, |board| board.max.y).chain(scores(false)),
            bounds.min.y,
        );
        let across = on_sheet(&placement, across, true);
        views::ordinates_horizontal(&mut canvas, &across, drawn.min.y);
        views::ordinates_vertical(&mut canvas, &on_sheet(&placement, up, false), drawn.min.x);
        views::datum(&mut canvas, drawn.min);
        // The tab the detail shows, circled where it is.
        if let Some(tab) = array.tab() {
            let center = placement.sheet(tab.bounds.center());
            let radius = tab.bounds.width().max(tab.bounds.height()) / 2.0 * placement.scale;
            let radius = radius.max(4.0);
            canvas.circle(center, radius, Pen::solid(THIN).dash(Dash::Chain));
            let lean = std::f64::consts::FRAC_1_SQRT_2;
            let at = center + Point::new(lean, lean) * (radius + 1.0);
            canvas.text(at, "A", TextStyle::new(HEADING).bold());
        }
        let ordinates = if array.scores.is_empty() {
            "ORDINATES TO BOARD EDGES"
        } else {
            "ORDINATES TO BOARD EDGES AND SCORES"
        };
        let title = Title {
            name: "BOARD ARRAY".to_string(),
            details: vec![
                format!("SCALE {scale}"),
                "VIEWED FROM TOP".to_string(),
                ordinates.to_string(),
            ],
        };
        let top = Point::new(drawn.center().x, drawn.min.y - ORDINATES - TITLE_DROP);
        title.draw(&mut canvas, top, TitleSide::Below, scale);
        self.finish_sheet(canvas, column, "ARRAY", scale);
        Ok(())
    }

    /// Every fabrication layer of the board as a view in its own colour,
    /// with whether it has any artwork.
    fn layer_views(&self) -> Result<Vec<(FabLayer, View, bool)>> {
        let source = self.source;
        let layers = fab_layers(source.imported);
        let copper_layers = layers
            .iter()
            .filter(|layer| layer.role == LayerRole::Copper)
            .count();
        layers
            .into_iter()
            .map(|layer| {
                let ink = match layer.role {
                    LayerRole::Copper => copper_ink(layer.number.unwrap_or(1), copper_layers),
                    LayerRole::Soldermask => MASK_INK,
                    _ => LEGEND_INK,
                };
                let (view, has_artwork) = views::layer_view(source, &layer, ink)
                    .with_context(|| format!("failed to draw layer '{}'", layer.name))?;
                Ok((layer, view, has_artwork))
            })
            .collect()
    }

    /// What a layer view's title says of it besides its scale: the weight
    /// of a copper layer, and the ink a mask or a legend is printed in,
    /// which is not the colour the view draws it in.
    fn layer_detail(&self, layer: &FabLayer) -> Option<String> {
        let source = self.source;
        match layer.role {
            LayerRole::Copper => {
                let copper = stack_rows(source.stackup.as_ref()?)
                    .into_iter()
                    .filter(|row| row.layer_type == StackupLayerType::Conductor)
                    .nth(layer.number?.checked_sub(1)?)?;
                let thickness = copper.thickness_mm.filter(|thickness| *thickness > 0.0);
                thickness.map(copper_weight)
            }
            LayerRole::Soldermask => {
                let ink = layer_ink(source, LayerRole::Soldermask, layer.side)?;
                Some(format!("OPENINGS DRAWN · MASK {}", ink.name))
            }
            LayerRole::Legend => {
                let ink = layer_ink(source, LayerRole::Legend, layer.side)?;
                Some(format!("LEGEND {}", ink.name))
            }
            _ => None,
        }
    }

    /// A layer view's title, at `scale` or with room left to state one.
    fn layer_title(&self, layer: &FabLayer, scale: Option<Scale>) -> Title {
        let name = match layer.number {
            Some(number) => format!("L{number} · {}", layer.name),
            None => layer.name.clone(),
        };
        // Everything is viewed from the top, through the board, so what is
        // printed on the bottom reads mirrored.
        let viewed = match layer.side {
            Side::Bottom => "VIEWED FROM TOP, READS MIRRORED",
            _ => "VIEWED FROM TOP",
        };
        let scale = scale.map_or_else(
            || "SCALE 00:1".to_string(),
            |scale| format!("SCALE {scale}"),
        );
        let details = [
            Some(scale),
            Some(viewed.to_string()),
            self.layer_detail(layer),
        ];
        Title {
            name: format!("{name} · {}", layer.description()),
            details: details.into_iter().flatten().collect(),
        }
    }

    /// The layer sheets: every layer with artwork, two to a sheet, each as
    /// large as its half of the sheet allows.
    fn layer_sheets(&mut self, layers: Vec<(FabLayer, View, bool)>) -> Result<()> {
        // A legend layer with nothing on it is not imaged at all, nor is a
        // copper layer with neither artwork nor a weight.
        let layers = layers
            .into_iter()
            .filter(|(layer, _, has_artwork)| match layer.role {
                LayerRole::Legend => *has_artwork,
                LayerRole::Copper => *has_artwork || self.layer_detail(layer).is_some(),
                _ => true,
            })
            .collect::<Vec<_>>();
        if layers.is_empty() {
            return Ok(());
        }
        let bounds = self.bounds;
        let area = area();
        let widest = layers
            .iter()
            // Every layer's title, lettered before the scale is settled:
            // the scale a sheet draws at depends on how wide the widest is.
            .map(|(layer, ..)| self.layer_title(layer, None).room(self.fonts))
            .reduce(TitleRoom::most)
            .expect("there is a layer to draw");
        let cell = |(columns, rows): (usize, usize), index: usize| {
            let (width, height) = (area.width() / columns as f64, area.height() / rows as f64);
            let min = Point::new(
                area.min.x + (index % columns) as f64 * width,
                area.max.y - (index / columns + 1) as f64 * height,
            );
            BBox::new(min, Point::new(min.x + width, min.y + height))
        };
        let scale_of = |grid: (usize, usize)| place_titled(bounds, cell(grid, 0), widest).0;
        // Side by side, or one above the other where that draws larger.
        let per_sheet = LAYERS_PER_SHEET.min(layers.len());
        let (beside, above) = ((per_sheet, 1), (1, per_sheet));
        let (scale_beside, scale_above) = (scale_of(beside), scale_of(above));
        let (grid, scale) = if scale_above.factor() > scale_beside.factor() {
            (above, scale_above)
        } else {
            (beside, scale_beside)
        };

        let mut layers = layers.into_iter().peekable();
        while layers.peek().is_some() {
            let mut canvas = self.canvas();
            let mut names = Vec::new();
            for (index, (layer, view, _)) in layers.by_ref().take(per_sheet).enumerate() {
                let placed = place_titled(bounds, cell(grid, index), widest);
                let view = view.outlined(self.outline.clone(), sheet::MEDIUM / scale.factor())?;
                let title = self.layer_title(&layer, Some(scale));
                self.draw_titled(&mut canvas, &view, bounds, placed, &title)?;
                names.push(match layer.number {
                    Some(number) => format!("L{number} {}", layer.name),
                    None => layer.name,
                });
            }
            self.sheets.push(Sheet {
                canvas,
                content: names.join(", "),
                scale: scale.to_string(),
            });
        }
        Ok(())
    }

    /// Frame every sheet, now that each knows how many there are, and write
    /// the document.
    fn finish(mut self) -> Result<Vec<u8>> {
        let options = self.options;
        let name = options
            .title
            .clone()
            .unwrap_or_else(|| data::design_name(self.source));
        let subject = if self.array {
            "BOARD ARRAY"
        } else {
            "BARE BOARD"
        };
        let title = TitleBlock {
            title: name.clone(),
            document: format!("FABRICATION DRAWING · {subject}"),
            revision: options
                .revision
                .clone()
                .or_else(|| data::design_revision(self.source))
                .unwrap_or_default(),
            date: data::design_date(self.source).unwrap_or_default(),
            source: options
                .source
                .clone()
                .unwrap_or_else(|| "IPC-2581".to_string()),
            generator: format!("pcb {}", env!("CARGO_PKG_VERSION")),
        };
        let (width, height) = sheet::SIZE;
        let count = self.sheets.len();
        for (index, mut sheet) in self.sheets.into_iter().enumerate() {
            let label = SheetLabel {
                content: sheet.content,
                scale: sheet.scale,
                number: index + 1,
                count,
            };
            draw_sheet(&mut sheet.canvas, &title, &label);
            let bookmark = format!("{}  {}", label.number, label.content);
            self.doc.page(width, height, sheet.canvas, bookmark);
        }
        self.doc.finish(
            self.fonts,
            &DocumentInfo {
                title: format!("{name} fabrication drawing"),
                subject: subject.to_string(),
                creator: title.generator.clone(),
            },
        )
    }
}
