//! Fabrication drawings: the sheets a bare-board fabricator builds a board
//! or a board array to, as a PDF.
//!
//! A drawing states what the design data cannot be asked at a glance: what
//! the board is made of and finished in, how large it is, which holes are
//! drilled where. Every view is ordinary artwork plotted at a stated scale,
//! so a sheet printed at full size measures true, and colour means a
//! material: copper is drawn in copper, a mask in its own ink.
//!
//! A drawing opens with the board's dimensioned outline beside its
//! specification, layer stack and notes; then its drill pattern and drill
//! table; then, for an array, the array as it is delivered; and closes with
//! every layer the fabricator images, as large as the sheet allows.

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
use pcb_ir::geom::{BBox, Point, Resolution};
use pcb_ir::import::ipc2581::ImportedDesign;
use pcb_ir::render::RenderOptions;

use self::blocks::{Block, Cell, Column, Figure, Material, Notes, Sample, ScoreSection, Table};
use self::data::{
    ArrayData, DrillTool, FabLayer, Ink, REQUIREMENTS, Source, copper_weight, drill_tools,
    fab_layers, layer_ink, mil, mm, mm_fine, stack_rows,
};
use self::pdf::{Align, Canvas, Dash, Document, DocumentInfo, Fonts, INK, Pen, TextStyle};
use self::sheet::{HEADING, SheetLabel, THIN, TitleBlock, draw_sheet};
use self::views::{COPPER, OutlineFeatures, Placement, Scale, Title, TitleSide, UNCOLOURED, View};
use crate::LayoutTarget;
use crate::accessors::{IpcAccessor, StackupLayerType};

pub use self::pdf::Typeface;
pub use self::sheet::SheetSize;

/// What a drawing's layer artwork is inked in.
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayerInk {
    /// The colour of what a layer is made of: copper in copper, a mask in
    /// its own ink under the legend printed on it.
    #[default]
    Material,
    /// A colour of its own for every copper layer, to tell them apart.
    Layer,
}

/// What a fabrication drawing is made of and what its title strip says.
#[derive(Debug, Clone)]
pub struct FabDrawingOptions {
    /// The board alone, or the array the file lays it out in.
    pub target: LayoutTarget,
    /// The sheet every page is laid out on; without one, the smallest of
    /// A4, A3 and A2 that shows what is delivered at half size or larger.
    pub sheet: Option<SheetSize>,
    pub ink: LayerInk,
    /// The fewest layer views a sheet is laid out to hold. One draws every
    /// layer as large as the sheet allows; views then share a sheet only
    /// where that costs them no scale.
    pub layers_per_sheet: usize,
    /// The design's name, where the board step's own is not wanted.
    pub title: Option<String>,
    pub revision: Option<String>,
    /// What names the data the drawing was made from: a file and its digest.
    pub source: Option<String>,
    /// The face everything is lettered in.
    pub typeface: Typeface,
}

impl Default for FabDrawingOptions {
    fn default() -> Self {
        Self {
            target: LayoutTarget::default(),
            sheet: Some(SheetSize::A4),
            ink: LayerInk::default(),
            layers_per_sheet: 1,
            title: None,
            revision: None,
            source: None,
            typeface: Typeface::default(),
        }
    }
}

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
    below: f64,
}

/// Draw the fabrication drawing of a design as a PDF.
pub fn fab_drawing(
    ipc: &Ipc2581,
    imported: &ImportedDesign,
    options: &FabDrawingOptions,
    resolution: Resolution,
) -> Result<Vec<u8>> {
    let source = Source {
        ipc,
        imported,
        accessor: IpcAccessor::new(ipc),
        resolution,
    };
    let (_, board) = views::outline(imported, ProfileSet::BoardOutlines);
    anyhow::ensure!(
        !board.is_empty(),
        "IPC-2581 design has no board profile to draw"
    );
    let array = match options.target {
        LayoutTarget::Board => None,
        LayoutTarget::BoardArray => ArrayData::of(&source)?,
    };
    let delivered = array.as_ref().map_or(board, |array| array.bounds);
    let fonts = Fonts::new(&options.typeface).context("cannot letter the drawing")?;
    let mut drawing = Drawing {
        source: &source,
        options,
        size: options.sheet.unwrap_or_else(|| fitting_sheet(delivered)),
        fonts: &fonts,
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
    let specification = drawing.specification(&printed, &tools, board, array.as_ref())?;
    let notes = Block::Notes(Notes {
        title: "NOTES".to_string(),
        notes: data::notes(array.as_ref(), &REQUIREMENTS),
    });
    let carried = drawing.board_sheet(&tools, specification, drawing.stack_table(), notes)?;
    drawing.drill_sheet(&tools, &symbols, carried)?;
    if let Some(array) = &array {
        let holes = tools.iter().map(|tool| tool.hits.len()).sum::<usize>();
        let mut blocks = vec![
            drawing.array_table(array, holes),
            drawing.array_legend(array),
        ];
        blocks.extend(drawing.tab_figure(array)?);
        blocks.extend(drawing.score_section(array));
        drawing.array_sheet(array, blocks)?;
    }
    drawing.layer_sheets(layers)?;
    drawing.finish(array.as_ref())
}

/// The smallest sheet that shows `delivered` at half size or larger beside
/// a column of tables: what a drawing adapts to when its sheet is not fixed.
fn fitting_sheet(delivered: BBox) -> SheetSize {
    let fits = |size: &SheetSize| {
        let body = size.body();
        let room = (
            body.width() - size.column_width() - 3.0 * PAD - 2.0 * VIEW_MARGIN - ORDINATES,
            body.height() - 2.0 * PAD - 2.0 * VIEW_MARGIN - ORDINATES - 20.0,
        );
        Scale::fit(delivered.width(), delivered.height(), room.0, room.1).factor() >= 0.5
    };
    let sizes = [SheetSize::A4, SheetSize::A3, SheetSize::A2];
    sizes.into_iter().find(fits).unwrap_or(SheetSize::A2)
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
    size: SheetSize,
    fonts: &'a Fonts,
    doc: Document,
    sheets: Vec<Sheet<'a>>,
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

    /// What a sheet has to draw in: its body, less a margin.
    fn area(&self) -> BBox {
        let body = self.size.body();
        BBox::new(
            Point::new(body.min.x + PAD, body.min.y + PAD),
            Point::new(body.max.x - PAD, body.max.y - PAD),
        )
    }

    /// Lay `blocks` down a column on the sheet's right, in order, as far as
    /// they fit. A block is never split: one the column has no room for is
    /// left over with every block after it.
    fn column(&self, blocks: Vec<Block>) -> Beside {
        let area = self.area();
        let width = self.size.column_width();
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
    fn table_sheets(&mut self, mut blocks: Vec<Block>) {
        let area = self.area();
        let width = self.size.column_width();
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
        board: BBox,
        array: Option<&ArrayData>,
    ) -> Result<Block> {
        let rows = data::specification(self.source, layers, tools, board, array)?
            .into_iter()
            .map(|row| {
                let chips = match row.chips.is_empty() {
                    true => Cell::Empty,
                    false => Cell::Chips(row.chips),
                };
                vec![Cell::text(row.label), chips, Cell::Bold(row.value)]
            })
            .collect();
        Ok(Block::Table(Table {
            title: "SPECIFICATION".to_string(),
            columns: vec![
                Column::new("", 29.0, Align::Left),
                Column::new("", 13.0, Align::Left),
                Column::new("", 63.0, Align::Left),
            ],
            rows,
            footer: None,
            header: false,
        }))
    }

    /// The drill table: a symbol, a count and a finished size for every
    /// tool, and what its holes are. `narrow` leaves out the size in mils,
    /// to fit the strip beside a drill pattern.
    fn drill_table(
        &mut self,
        tools: &[DrillTool],
        symbols: &[usize],
        narrow: bool,
    ) -> Result<Block> {
        fn wide<T>(narrow: bool, item: T) -> Option<T> {
            (!narrow).then_some(item)
        }
        let rows = tools
            .iter()
            .zip(symbols)
            .map(|(tool, &index)| {
                let symbol = views::symbol_view(index)?;
                let viewport = BBox::from_point(Point::ZERO).expand(views::SYMBOL_SIZE);
                let form = self
                    .doc
                    .plot(&symbol.artwork, &self.render_options(&symbol, viewport))?;
                // What the holes are, then what sets them apart from a round
                // hole through the board.
                let span = (!tool.is_through()).then(|| tool.span());
                let kind = [Some(tool.usage().to_string()), span, tool.shape()];
                let kind = kind.into_iter().flatten().collect::<Vec<_>>().join(" · ");
                let cells = [
                    Some(Cell::Symbol(form)),
                    Some(Cell::text(tool.hits.len().to_string())),
                    Some(Cell::Bold(mm_fine(tool.diameter))),
                    wide(narrow, Cell::text(mil(tool.diameter))),
                    Some(Cell::text(kind)),
                ];
                Ok(cells.into_iter().flatten().collect::<Vec<_>>())
            })
            .collect::<Result<Vec<_>>>()?;
        let total = tools.iter().map(|tool| tool.hits.len()).sum::<usize>();
        let footer = [
            Some(Cell::Empty),
            Some(Cell::Bold(total.to_string())),
            Some(Cell::Bold("TOTAL".to_string())),
            wide(narrow, Cell::Empty),
            Some(Cell::Empty),
        ];
        let columns = [
            Some(Column::new("SYM", 8.0, Align::Center)),
            Some(Column::new("QTY", 10.0, Align::Right)),
            Some(Column::new("Ø mm", 13.0, Align::Right)),
            wide(narrow, Column::new("mil", 11.0, Align::Right)),
            Some(Column::new(
                "TYPE",
                if narrow { 25.0 } else { 40.0 },
                Align::Left,
            )),
        ];
        Ok(Block::Table(Table {
            title: "DRILL TABLE, FINISHED SIZES".to_string(),
            columns: columns.into_iter().flatten().collect(),
            rows,
            footer: Some(footer.into_iter().flatten().collect()),
            header: true,
        }))
    }

    /// The colour each stackup row's section is drawn in.
    fn stack_table(&self) -> Option<Block> {
        /// Dielectrics under their hatching.
        const CORE: u32 = 0xccd5ae;
        const PREPREG: u32 = 0xe9edc9;
        const DIELECTRIC: u32 = 0xebe6dc;
        let stackup = self.source.accessor.stackup_details()?;
        let rows = stack_rows(&stackup);
        let copper_layers = rows
            .iter()
            .filter(|layer| layer.layer_type == StackupLayerType::Conductor)
            .count();
        let mut copper = 0;
        let rows = rows
            .into_iter()
            .map(|layer| {
                let optional = |value: Option<String>| value.map_or(Cell::Empty, Cell::Text);
                let thickness = optional(layer.thickness_mm.map(|mm| format!("{mm:.3}")));
                let material = layer.material.clone().unwrap_or_default();
                match layer.layer_type {
                    StackupLayerType::Conductor => {
                        copper += 1;
                        vec![
                            Cell::Section(Material::Copper, self.copper_ink(copper, copper_layers)),
                            Cell::Bold(format!("L{copper}")),
                            Cell::Bold(layer.name.clone()),
                            Cell::text("COPPER"),
                            thickness,
                            optional(layer.thickness_mm.map(copper_weight)),
                            Cell::Empty,
                        ]
                    }
                    StackupLayerType::Soldermask | StackupLayerType::Other => {
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
                            Cell::text(format!("{kind} {material}").trim_end()),
                            thickness,
                            Cell::Empty,
                            optional(layer.dielectric_constant.map(|dk| format!("{dk:.2}"))),
                        ]
                    }
                }
            })
            .collect::<Vec<_>>();
        let footer = stackup.overall_thickness_mm.map(|thickness| {
            let mut footer = vec![Cell::Empty; 7];
            footer[3] = Cell::Bold("FINISHED THICKNESS".to_string());
            footer[4] = Cell::Bold(format!("{thickness:.3}"));
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

    /// What the array adds to its boards: how it steps, what holds them and
    /// the tooling on its rails.
    fn array_table(&self, array: &ArrayData, board_holes: usize) -> Block {
        let mut rows = Vec::new();
        let mut row = |item: &str, value: String| {
            rows.push(vec![Cell::text(item), Cell::Bold(value)]);
        };
        row(
            "ARRAY SIZE",
            format!(
                "{} × {} mm",
                mm(array.bounds.width()),
                mm(array.bounds.height())
            ),
        );
        let boards = array.boards.len();
        let grid = array.grid.as_ref();
        let layout = grid.map(|grid| format!(" · {} ACROSS × {} UP", grid.columns, grid.rows));
        row("BOARDS", format!("{boards}{}", layout.unwrap_or_default()));
        let steps = [
            ("X", grid.and_then(|grid| grid.pitch_x)),
            ("Y", grid.and_then(|grid| grid.pitch_y)),
        ]
        .into_iter()
        .filter_map(|(axis, pitch)| Some(format!("{axis} {}", mm(pitch?.mm()))))
        .collect::<Vec<_>>();
        if !steps.is_empty() {
            row("BOARD STEP", steps.join(" · "));
        }
        row("SEPARATION", array.separation().to_string());
        let tab = array.tab();
        if let Some(tab) = &tab {
            row(
                "TABS",
                format!(
                    "{}X Ø{} NPTH PER TAB · PITCH {}",
                    tab.holes,
                    mm_fine(tab.diameter),
                    mm_fine(tab.pitch)
                ),
            );
        }
        if !array.scores.is_empty() {
            let thickness = self.source.accessor.stackup_details();
            let thickness = thickness.and_then(|stackup| stackup.overall_thickness_mm);
            let web = thickness.map(|thickness| {
                format!(
                    " · WEB {} ±{}",
                    mm(thickness * REQUIREMENTS.score_web),
                    mm(REQUIREMENTS.score_tolerance)
                )
            });
            row(
                "V-SCORE",
                format!(
                    "{} LINES · {}°{} · BOTH SIDES",
                    array.scores.len(),
                    REQUIREMENTS.score_angle,
                    web.unwrap_or_default()
                ),
            );
        }
        if let Some(grid) = grid {
            let rail = &grid.edge_rail;
            row(
                "RAILS",
                format!(
                    "TOP {} · RIGHT {} · BOTTOM {} · LEFT {}",
                    mm(rail.top.mm()),
                    mm(rail.right.mm()),
                    mm(rail.bottom.mm()),
                    mm(rail.left.mm())
                ),
            );
        }
        // Every hole of the array's own but the perforations of its tabs.
        let tooling = array
            .tools
            .iter()
            .filter(|tool| tab.is_none_or(|tab| tool.diameter != tab.diameter))
            .map(|tool| format!("{}X Ø{}", tool.hits.len(), mm_fine(tool.diameter)))
            .collect::<Vec<_>>();
        if !tooling.is_empty() {
            row("TOOLING HOLES", format!("{} NPTH", tooling.join(" · ")));
        }
        let fiducials = [(Side::Top, "TOP"), (Side::Bottom, "BOTTOM")]
            .into_iter()
            .filter_map(|(side, name)| {
                let on_side = array
                    .fiducials
                    .iter()
                    .filter(|fiducial| fiducial.side == side);
                let count = on_side.clone().count();
                let diameter = on_side.map(|fiducial| fiducial.diameter).next()?;
                Some(format!("{count} {name} Ø{}", mm(diameter)))
            })
            .collect::<Vec<_>>();
        if !fiducials.is_empty() {
            row("FIDUCIALS", fiducials.join(" · "));
        }
        let own = array
            .tools
            .iter()
            .map(|tool| tool.hits.len())
            .sum::<usize>();
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
    fn array_legend(&self, array: &ArrayData) -> Block {
        let has = |side: Side| array.fiducials.iter().any(|fiducial| fiducial.side == side);
        let rows = [
            (true, Sample::ArrayProfile, "ARRAY PROFILE"),
            (true, Sample::BoardProfile, "BOARD PROFILE"),
            (!array.scores.is_empty(), Sample::Score, "V-SCORE LINE"),
            (!array.removal.is_empty(), Sample::Routed, "ROUTED OUT"),
            (has(Side::Top), Sample::FiducialTop, "FIDUCIAL, TOP SIDE"),
            (
                has(Side::Bottom),
                Sample::FiducialBottom,
                "FIDUCIAL, BOTTOM SIDE",
            ),
        ];
        Block::Table(Table {
            title: "LEGEND".to_string(),
            columns: vec![
                Column::new("", 20.0, Align::Center),
                Column::new("", 85.0, Align::Left),
            ],
            rows: rows
                .into_iter()
                .filter(|(shown, ..)| *shown)
                .map(|(_, sample, meaning)| vec![Cell::Sample(sample), Cell::text(meaning)])
                .collect(),
            footer: None,
            header: false,
        })
    }

    /// One breakaway tab, drawn large enough to read its perforations.
    fn tab_figure(&mut self, array: &ArrayData) -> Result<Option<Block>> {
        let Some(tab) = array.tab() else {
            return Ok(None);
        };
        let room = (self.size.column_width() - 10.0, 46.0);
        let scale = Scale::fit(tab.bounds.width(), tab.bounds.height(), room.0, room.1);
        let view = views::array_view(self.source, array, scale.factor())?;
        let form = self
            .doc
            .plot(&view.artwork, &self.render_options(&view, tab.bounds))?;
        Ok(Some(Block::Figure(Figure {
            title: format!("DETAIL A · TAB · SCALE {scale}"),
            captions: vec![format!(
                "{}X Ø{} NPTH · {} CENTRE TO CENTRE",
                tab.holes,
                mm_fine(tab.diameter),
                mm_fine(tab.pitch)
            )],
            form,
            center: tab.bounds.center(),
            scale: scale.factor(),
            height: tab.bounds.height() * scale.factor(),
        })))
    }

    /// A V-score in section, where the array is scored.
    fn score_section(&self, array: &ArrayData) -> Option<Block> {
        if array.scores.is_empty() {
            return None;
        }
        let thickness = self
            .source
            .accessor
            .stackup_details()?
            .overall_thickness_mm?;
        let requirements = &REQUIREMENTS;
        Some(Block::ScoreSection(ScoreSection {
            title: "V-SCORE SECTION".to_string(),
            thickness: mm(thickness),
            web: format!(
                "{} ±{}",
                mm(thickness * requirements.score_web),
                mm(requirements.score_tolerance)
            ),
            angle: format!("{}°", requirements.score_angle),
        }))
    }

    /// Fit `bounds` of the design into `region` of a sheet, keeping `clear`
    /// around it for what is lettered there.
    fn place(&self, bounds: BBox, region: BBox, clear: Clear) -> (Scale, Placement) {
        let room = BBox::new(
            Point::new(
                region.min.x + VIEW_MARGIN + clear.left,
                region.min.y + VIEW_MARGIN + clear.below,
            ),
            Point::new(
                region.max.x - VIEW_MARGIN - clear.right,
                region.max.y - VIEW_MARGIN,
            ),
        );
        let scale = Scale::fit(bounds.width(), bounds.height(), room.width(), room.height());
        (scale, Placement::centered(bounds, room.center(), scale))
    }

    /// Fit `bounds` and `title` into `region`: the title under the view, or
    /// beside it where that draws the view at a larger scale.
    fn place_titled(
        &self,
        bounds: BBox,
        region: BBox,
        title: &Title,
    ) -> (Scale, Placement, TitleSide) {
        let below = Clear {
            below: TITLE_DROP + views::TITLE_HEIGHT,
            ..Clear::default()
        };
        let beside = Clear {
            right: TITLE_DROP + title.width_beside(self.fonts),
            ..Clear::default()
        };
        let (scale_below, placed_below) = self.place(bounds, region, below);
        let (scale_beside, placed_beside) = self.place(bounds, region, beside);
        if scale_beside.factor() <= scale_below.factor() {
            return (scale_below, placed_below, TitleSide::Below);
        }
        // A view with its title beside it stands at the left of its region,
        // so all the room it leaves is in one strip.
        let left = region.min.x + VIEW_MARGIN + bounds.width() * scale_beside.factor() / 2.0;
        let placed = Placement {
            at: Point::new(left, placed_beside.at.y),
            ..placed_beside
        };
        (scale_beside, placed, TitleSide::Beside)
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
            TitleSide::Below => Point::new(drawn.center().x, drawn.min.y - TITLE_DROP),
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
        let form = self
            .doc
            .plot(&view.artwork, &self.render_options(view, viewport))?;
        canvas.place(form, placement.anchor, placement.at, placement.scale);
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
        let (column, carried) = match column.left_over.is_empty() {
            true => (column, None),
            false => (self.column(vec![specification, notes]), stack),
        };
        let mut canvas = self.canvas();
        let (contours, bounds) = views::outline(self.source.imported, ProfileSet::BoardOutlines);

        // An axis whose outline has edges between its extents is dimensioned
        // by ordinates from the lower-left corner; a plain one by its length.
        let features = OutlineFeatures::of(&contours);
        let across = views::stations(features.edges_x.iter().copied(), bounds.min.x);
        let up = views::stations(features.edges_y.iter().copied(), bounds.min.y);
        let across = Some(across).filter(|stations| stations.len() > 2);
        let up = Some(up).filter(|stations| stations.len() > 2);
        let radii = features.radii_note();
        let title_height = views::TITLE_HEIGHT + radii.iter().len() as f64 * views::TITLE_LEADING;
        let lettered = |ordinates: bool, dimension: f64| match ordinates {
            true => ORDINATES,
            false => dimension,
        };
        let below = lettered(across.is_some(), DIMENSION_OFFSET + 2.0);
        let clear = Clear {
            left: lettered(up.is_some(), 0.0),
            right: match up {
                Some(_) => 0.0,
                None => DIMENSION_OFFSET + DIMENSION_VALUE,
            },
            below: below + TITLE_DROP + title_height,
        };
        let (scale, placement) = self.place(bounds, column.view, clear);
        let view = views::outline_view(self.source, tools, scale.factor())?;
        self.draw_view(&mut canvas, &view, bounds, placement)?;

        let drawn = placement.sheet_box(bounds);
        let datum = across.is_some() || up.is_some();
        match across {
            Some(across) => {
                let across = on_sheet(&placement, across, true);
                views::ordinates_horizontal(&mut canvas, &across, drawn.min.y, false);
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

    /// Draw a sheet's column and file the sheet.
    fn finish_sheet(
        &mut self,
        mut canvas: Canvas<'a>,
        column: Beside,
        content: &str,
        scale: Scale,
    ) {
        let width = self.size.column_width();
        for (block, at) in &column.placed {
            block.draw(&mut canvas, *at, width);
        }
        self.sheets.push(Sheet {
            canvas,
            content: content.to_string(),
            scale: scale.to_string(),
        });
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
        let (_, bounds) = views::outline(self.source.imported, ProfileSet::BoardOutlines);
        let title = |scale: Scale| Title {
            name: "DRILL PATTERN".to_string(),
            details: vec![
                format!("SCALE {scale}"),
                "VIEWED FROM TOP".to_string(),
                "SYMBOLS PER DRILL TABLE".to_string(),
            ],
        };
        let sample = title(Scale::fit(1.0, 1.0, 1.0, 1.0));
        let table = self.drill_table(tools, symbols, false)?;
        let tables = std::iter::once(table)
            .chain(carried.clone())
            .collect::<Vec<_>>();
        let beside = self.column(tables.clone());
        let alone = self.column(Vec::new());
        let scale_beside = self.place_titled(bounds, beside.view, &sample).0;
        let alone_placed = self.place_titled(bounds, alone.view, &sample);
        let mut canvas = self.canvas();
        if alone_placed.0.factor() <= scale_beside.factor() {
            let placed = self.place_titled(bounds, beside.view, &sample);
            let view = views::drill_view(self.source, tools, symbols, placed.0.factor())?;
            self.draw_titled(&mut canvas, &view, bounds, placed, &title(placed.0))?;
            let left_over = beside.left_over.clone();
            self.finish_sheet(canvas, beside, "DRILL", placed.0);
            self.table_sheets(left_over);
            return Ok(());
        }
        // Alone on its sheet, the pattern leaves the table a place of its
        // own beside it: the strip above a title lettered at its right, or
        // the corner under it, clear of a title lettered below.
        let (scale, placement, side) = alone_placed;
        let area = self.area();
        let narrow = self.drill_table(tools, symbols, true)?;
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
            TitleSide::Below => {
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
                let fits = area.min.y + height <= under && left >= title_right + TITLE_DROP;
                match fits {
                    true => (raised, Some(Point::new(left, area.min.y + height))),
                    false => (placement, None),
                }
            }
        };
        let view = views::drill_view(self.source, tools, symbols, scale.factor())?;
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
        let (scale, placement) = self.place(bounds, column.view, clear);
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
        views::ordinates_horizontal(&mut canvas, &across, drawn.min.y, false);
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
        let title = Title {
            name: "BOARD ARRAY".to_string(),
            details: vec![
                format!("SCALE {scale}"),
                "VIEWED FROM TOP".to_string(),
                "ORDINATES TO BOARD EDGES AND SCORES".to_string(),
            ],
        };
        let top = Point::new(drawn.center().x, drawn.min.y - ORDINATES - TITLE_DROP);
        title.draw(&mut canvas, top, TitleSide::Below, scale);
        let left_over = column.left_over.clone();
        self.finish_sheet(canvas, column, "ARRAY", scale);
        self.table_sheets(left_over);
        Ok(())
    }

    /// The ink of copper layer `number` of `count`.
    fn copper_ink(&self, number: usize, count: usize) -> u32 {
        /// Inner layers, told apart in turn.
        const INNER: [u32; 6] = [0x1b7a3d, 0x0b7285, 0x7a3fa0, 0x8a5a00, 0x9c2f6b, 0x4a5d23];
        match self.options.ink {
            LayerInk::Material => COPPER,
            LayerInk::Layer if number == 1 => 0xb3261e,
            LayerInk::Layer if number == count => 0x1d3fb0,
            LayerInk::Layer => INNER[(number - 2) % INNER.len()],
        }
    }

    /// Every fabrication layer of the board as a view, with whether it has
    /// any artwork.
    ///
    /// Copper is drawn on paper. A mask is drawn as the board filled in its
    /// ink with its openings left as paper; a legend in its ink over the
    /// board in its side's mask, since white ink does not show on paper.
    fn layer_views(&self) -> Result<Vec<(FabLayer, View, bool)>> {
        let source = self.source;
        let layers = fab_layers(source.imported);
        let copper_layers = layers
            .iter()
            .filter(|layer| layer.role == LayerRole::Copper)
            .count();
        let (silhouette, _) = views::outline(source.imported, ProfileSet::BoardOutlines);
        // How far apart two colours read, by how bright each is.
        let brightness = |color: u32| {
            let [_, red, green, blue] = color.to_be_bytes().map(f64::from);
            0.2126 * red + 0.7152 * green + 0.0722 * blue
        };
        layers
            .into_iter()
            .map(|layer| {
                let mask = layer_ink(source, LayerRole::Soldermask, layer.side);
                let board = mask.and_then(|mask| mask.color).unwrap_or(UNCOLOURED);
                let material = self.options.ink == LayerInk::Material;
                let (ink, ground) = match layer.role {
                    LayerRole::Copper => {
                        let number = layer.number.unwrap_or(1);
                        (self.copper_ink(number, copper_layers), None)
                    }
                    LayerRole::Soldermask if material => (0xffffff, Some(board)),
                    LayerRole::Legend if material => {
                        let legend = layer_ink(source, LayerRole::Legend, layer.side);
                        // An ink the design does not colour, or one that
                        // would not show on its mask, is drawn to contrast.
                        let ink = legend
                            .and_then(|legend| legend.color)
                            .filter(|ink| (brightness(*ink) - brightness(board)).abs() > 40.0)
                            .unwrap_or(if brightness(board) > 128.0 {
                                INK
                            } else {
                                0xffffff
                            });
                        (ink, Some(board))
                    }
                    LayerRole::Soldermask => (0x6a1b7a, None),
                    _ => (0x8a6d00, None),
                };
                let ground = ground.map(|color| (color, silhouette.clone()));
                let (view, has_artwork) = views::layer_view(source, &layer, true, ink, ground)
                    .with_context(|| format!("failed to draw layer '{}'", layer.name))?;
                Ok((layer, view, has_artwork))
            })
            .collect()
    }

    /// What a layer view's title says of it besides its scale.
    fn layer_detail(&self, layer: &FabLayer) -> Option<String> {
        let source = self.source;
        let mask = layer_ink(source, LayerRole::Soldermask, layer.side).map(|mask| mask.name);
        match layer.role {
            LayerRole::Copper => {
                let stackup = source.accessor.stackup_details()?;
                let copper = stack_rows(&stackup)
                    .into_iter()
                    .filter(|row| row.layer_type == StackupLayerType::Conductor)
                    .nth(layer.number?.checked_sub(1)?)?;
                copper.thickness_mm.map(copper_weight)
            }
            LayerRole::Soldermask => Some(format!("MASK {} · OPENINGS WHITE", mask?)),
            LayerRole::Legend => {
                let legend = layer_ink(source, LayerRole::Legend, layer.side)?.name;
                Some(match mask {
                    Some(mask) => format!("LEGEND {legend} ON MASK {mask}"),
                    None => format!("LEGEND {legend}"),
                })
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

    /// The layer sheets: every layer with artwork, each as large as the
    /// sheet allows. Views share a sheet only where that costs them no
    /// scale.
    fn layer_sheets(&mut self, layers: Vec<(FabLayer, View, bool)>) -> Result<()> {
        /// The largest grid of views a sheet is divided into.
        const GRID: (usize, usize) = (6, 4);
        // A legend layer with nothing on it is not imaged at all.
        let layers = layers
            .into_iter()
            .filter(|(layer, _, has_artwork)| *has_artwork || layer.role != LayerRole::Legend)
            .collect::<Vec<_>>();
        if layers.is_empty() {
            return Ok(());
        }
        let (contours, bounds) = views::outline(self.source.imported, ProfileSet::BoardOutlines);
        let area = self.area();
        let widest = layers
            .iter()
            // Every layer's title, lettered before the scale is settled:
            // the scale a grid draws at depends on how wide the widest is.
            .map(|(layer, ..)| self.layer_title(layer, None))
            .max_by(|a, b| {
                a.width_beside(self.fonts)
                    .total_cmp(&b.width_beside(self.fonts))
            })
            .expect("there is a layer to draw");
        let cell = |columns: usize, rows: usize, column: usize, row: usize| {
            let (width, height) = (area.width() / columns as f64, area.height() / rows as f64);
            let min = Point::new(
                area.min.x + column as f64 * width,
                area.max.y - (row + 1) as f64 * height,
            );
            BBox::new(min, Point::new(min.x + width, min.y + height))
        };
        let cell_scale = |columns: usize, rows: usize| {
            self.place_titled(bounds, cell(columns, rows, 0, 0), &widest)
                .0
        };
        let wanted = self.options.layers_per_sheet.clamp(1, GRID.0 * GRID.1);
        let wanted = wanted.min(layers.len());
        let (columns, rows) = (1..=GRID.0)
            .flat_map(|columns| (1..=GRID.1).map(move |rows| (columns, rows)))
            .filter(|(columns, rows)| columns * rows >= wanted)
            .max_by(|a, b| {
                let (scale_a, scale_b) = (cell_scale(a.0, a.1), cell_scale(b.0, b.1));
                scale_a
                    .factor()
                    .total_cmp(&scale_b.factor())
                    // At one scale, the grid that takes the fewest sheets;
                    // then the one with the fewest empty cells.
                    .then(
                        (a.0 * a.1)
                            .min(layers.len())
                            .cmp(&(b.0 * b.1).min(layers.len())),
                    )
                    .then((b.0 * b.1).cmp(&(a.0 * a.1)))
            })
            .expect("a grid of the sheet's largest size always qualifies");
        let scale = cell_scale(columns, rows);

        let mut layers = layers.into_iter().peekable();
        while layers.peek().is_some() {
            let mut canvas = self.canvas();
            let mut names = Vec::new();
            for (index, (layer, view, _)) in layers.by_ref().take(columns * rows).enumerate() {
                let region = cell(columns, rows, index % columns, index / columns);
                let placed = self.place_titled(bounds, region, &widest);
                let view = view.outlined(contours.clone(), sheet::MEDIUM / scale.factor())?;
                let title = self.layer_title(&layer, Some(scale));
                self.draw_titled(&mut canvas, &view, bounds, placed, &title)?;
                names.push(match layer.number {
                    Some(number) => format!("L{number} {}", layer.name),
                    None => layer.name,
                });
            }
            let content = match names.as_slice() {
                [only] => only.clone(),
                [first, .., last] => format!("{first} TO {last}"),
                [] => "LAYERS".to_string(),
            };
            self.sheets.push(Sheet {
                canvas,
                content,
                scale: scale.to_string(),
            });
        }
        Ok(())
    }

    /// Frame every sheet, now that each knows how many there are, and write
    /// the document.
    fn finish(mut self, array: Option<&ArrayData>) -> Result<Vec<u8>> {
        let options = self.options;
        let name = options
            .title
            .clone()
            .unwrap_or_else(|| data::design_name(self.source));
        let subject = match array {
            None => "BARE BOARD",
            Some(_) => "BOARD ARRAY",
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
            size: self.size,
        };
        let (width, height) = self.size.dimensions();
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
