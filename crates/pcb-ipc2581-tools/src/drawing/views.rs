//! The views a drawing places: each ordinary artwork in the design's own
//! millimetres, plotted once and drawn on a sheet at a stated scale, and
//! the dimensions lettered around it.

use anyhow::Result;
use ipc2581::Symbol;
use pcb_ir::dialects::artwork::{self, Aperture, ApertureShape, Geometry, Object, PaintStage};
use pcb_ir::dialects::ipc::{FeatureSpan, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    Affine2, BBox, ContourBuf, FillRule, LineCap, LinePattern, Paint, PathCmd, Point, Polarity,
    StrokeStyle,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use pcb_ir::render::LayerStyle;

use super::data::{
    ArrayData, DrillTool, FabLayer, Hit, Source, copper_order, micrometres, mm, mm_fine,
    span_layers,
};
use super::pdf::{Align, Canvas, Fonts, INK, Pen, TextStyle, Weight};
use super::sheet::{BODY, HAIR, HEADING, HEAVY, LABEL, MEDIUM, THIN};
use super::symbols::symbol;
use crate::geometry::render::layer_objects;
use crate::geometry::step_artwork::root_step;
use crate::layers::layer_role;

type ViewArtwork = artwork::Document<(), Option<Symbol>>;
type ViewObject = Object<Option<Symbol>>;

/// Size of a drill symbol on the sheet, and the smallest one is drawn
/// where holes crowd.
pub const SYMBOL_SIZE: f64 = 1.5;
const SMALLEST_SYMBOL: f64 = 0.7;

const fn ink(color: u32) -> LayerStyle {
    LayerStyle {
        color,
        opacity: 1.0,
    }
}

const BLACK: LayerStyle = ink(INK);
/// The grey of material an array has routed away.
pub const ROUTED: u32 = 0xd4d4d4;

/// The ratio a view is drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    drawn: u32,
    actual: u32,
}

impl Scale {
    /// The scales a drawing uses, largest first: those of ISO 5455.
    const STEPS: [Scale; 10] = [
        Scale::up(50),
        Scale::up(20),
        Scale::up(10),
        Scale::up(5),
        Scale::up(2),
        Scale::up(1),
        Scale::down(2),
        Scale::down(5),
        Scale::down(10),
        Scale::down(20),
    ];
    /// Full size.
    pub const FULL: Scale = Scale::up(1);

    const fn up(drawn: u32) -> Self {
        Self { drawn, actual: 1 }
    }

    const fn down(actual: u32) -> Self {
        Self { drawn: 1, actual }
    }

    pub fn factor(self) -> f64 {
        f64::from(self.drawn) / f64::from(self.actual)
    }

    /// The largest scale at which `width` by `height` fits the room.
    pub fn fit(width: f64, height: f64, room_width: f64, room_height: f64) -> Self {
        let fits = |scale: &Scale| {
            width * scale.factor() <= room_width && height * scale.factor() <= room_height
        };
        let smallest = Self::STEPS[Self::STEPS.len() - 1];
        Self::STEPS.into_iter().find(fits).unwrap_or(smallest)
    }
}

impl std::fmt::Display for Scale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.drawn, self.actual)
    }
}

/// Where a view sits on its sheet.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub scale: f64,
    /// A point of the artwork and where on the sheet it lands.
    pub anchor: Point,
    pub at: Point,
}

impl Placement {
    /// Centre `bounds` of the artwork on `center` of the sheet.
    pub fn centered(bounds: BBox, center: Point, scale: Scale) -> Self {
        Self {
            scale: scale.factor(),
            anchor: bounds.center(),
            at: center,
        }
    }

    pub fn sheet(&self, point: Point) -> Point {
        self.at + (point - self.anchor) * self.scale
    }

    pub fn sheet_box(&self, bounds: BBox) -> BBox {
        BBox::new(self.sheet(bounds.min), self.sheet(bounds.max))
    }
}

/// A view's artwork and the ink of each of its layers.
pub struct View {
    pub artwork: ViewArtwork,
    pub styles: Vec<LayerStyle>,
}

impl View {
    fn new() -> Self {
        Self {
            artwork: ViewArtwork::new(),
            styles: Vec::new(),
        }
    }

    /// Lay the outline of what carries the view over it, `width` wide, and
    /// settle the artwork.
    pub fn outlined(mut self, contours: Vec<ContourBuf>, width: f64) -> Result<Self> {
        let profile = self.layer("Profile", LayerRole::Profile, BLACK);
        self.stroke(profile, contours, StrokeStyle::round(width));
        self.finish()
    }

    fn layer(&mut self, name: &str, role: LayerRole, style: LayerStyle) -> u32 {
        self.styles.push(style);
        self.artwork
            .push_layer(artwork::Layer::new(name, role, Side::None))
    }

    fn stroke(&mut self, layer: u32, contours: Vec<ContourBuf>, stroke: StrokeStyle) {
        if contours.is_empty() {
            return;
        }
        let path = self.artwork.push_path(Paint::Stroke(stroke), contours);
        self.artwork.push_object(
            layer,
            ViewObject::new(Polarity::Dark, Geometry::Stroke { path }),
        );
    }

    fn fill(&mut self, layer: u32, contours: Vec<ContourBuf>) {
        if contours.is_empty() {
            return;
        }
        let rule = FillRule::NonZero;
        let path = self.artwork.push_path(Paint::Fill { rule }, contours);
        self.artwork.push_object(
            layer,
            ViewObject::new(Polarity::Dark, Geometry::Region { path }),
        );
    }

    fn flash(&mut self, layer: u32, aperture: u32, at: Point) {
        let transform = Affine2::translation(at);
        self.artwork.push_object(
            layer,
            ViewObject::new(
                Polarity::Dark,
                Geometry::Flash {
                    aperture,
                    transform,
                },
            ),
        );
    }

    fn finish(mut self) -> Result<Self> {
        crate::geometry::step_artwork::finish_step_graph_artwork(&mut self.artwork)?;
        Ok(self)
    }
}

/// The profile contours `set` selects and the bounds of their outer edges.
pub fn outline(imported: &ImportedDesign, set: ProfileSet) -> (Vec<ContourBuf>, BBox) {
    let geometry = &imported.geometry;
    let mut bounds = BBox::empty();
    let mut contours = Vec::new();
    for occurrence in profile_occurrences_for(geometry, set) {
        let outer = occurrence.profile.outer_path;
        bounds = bounds.union(geometry.transformed_path_bbox(outer, occurrence.transform));
        let cutouts = occurrence.profile.cutouts.slice(&geometry.profile_cutouts);
        for path in std::iter::once(outer).chain(cutouts.iter().map(|cutout| cutout.path)) {
            contours.extend(geometry.transformed_path_contours(path, occurrence.transform));
        }
    }
    (contours, bounds)
}

/// What an outline says beyond its extents: where its straight edges run,
/// and what its corners are rounded to.
#[derive(Debug, Default, PartialEq)]
pub struct OutlineFeatures {
    /// X of every vertical edge and Y of every horizontal one.
    pub edges_x: Vec<f64>,
    pub edges_y: Vec<f64>,
    /// Corner radii with how many corners have each, the commonest first.
    radii: Vec<(f64, usize)>,
}

impl OutlineFeatures {
    /// Edges shorter than this are detail the data states, not the drawing.
    const SHORTEST_EDGE: f64 = 1.0;
    /// How far off its axis an edge may run, as a share of its length.
    const OFF_AXIS: f64 = 2e-3;

    /// The features of an outline given as its outer contour followed by
    /// its cutouts. Edges and corners are the outer contour's.
    pub fn of(contours: &[ContourBuf]) -> Self {
        let mut features = Self::default();
        let mut radii = std::collections::BTreeMap::<i64, usize>::new();
        let segments = contours.iter().enumerate().flat_map(|(index, contour)| {
            let outer = index == 0;
            contour.segments().map(move |segment| (outer, segment))
        });
        for (outer, segment) in segments {
            match segment {
                // A cutout's edges are its own shape, which the data states;
                // an ordinate to one would point at nothing on the outline.
                pcb_ir::geom::Segment::Line { start, end } if outer => {
                    let run = end - start;
                    let length = run.length();
                    if length < Self::SHORTEST_EDGE {
                        continue;
                    }
                    // An edge a layout tool left a few micrometres off its
                    // axis is still that edge.
                    let middle = (start + end) * 0.5;
                    if run.x.abs() <= Self::OFF_AXIS * length {
                        features.edges_x.push(middle.x);
                    } else if run.y.abs() <= Self::OFF_AXIS * length {
                        features.edges_y.push(middle.y);
                    }
                }
                pcb_ir::geom::Segment::Line { .. } => {}
                // A corner turns the outline by no more than a right angle;
                // a longer arc is a feature of its own.
                pcb_ir::geom::Segment::Arc(arc) => {
                    if outer && arc.sweep_radians() <= 100.0_f64.to_radians() {
                        *radii.entry(micrometres(arc.radius())).or_default() += 1;
                    }
                }
                pcb_ir::geom::Segment::Ellipse(_) => {}
            }
        }
        features.radii = radii
            .into_iter()
            .map(|(radius, count)| (radius as f64 / 1000.0, count))
            .collect();
        features
            .radii
            .sort_by(|a, b| b.1.cmp(&a.1).then(a.0.total_cmp(&b.0)));
        features
    }

    /// The corner radii as a drawing letters them, where the outline has
    /// few enough to say.
    pub fn radii(&self) -> Option<String> {
        if self.radii.is_empty() || self.radii.len() > 3 {
            return None;
        }
        let radii = self
            .radii
            .iter()
            .map(|(radius, count)| format!("{count}X R{}", mm_fine(*radius)))
            .collect::<Vec<_>>();
        Some(radii.join(", "))
    }
}

fn segment(from: Point, to: Point) -> ContourBuf {
    ContourBuf::new(vec![PathCmd::move_to(from), PathCmd::line_to(to)])
}

/// The edge of a slot `diameter` wide along the centre line `start..end`.
fn slot_outline(start: Point, end: Point, diameter: f64) -> ContourBuf {
    let run = end - start;
    let along = run / run.length();
    let across = Point::new(-along.y, along.x) * (diameter / 2.0);
    ContourBuf::new(vec![
        PathCmd::move_to(start + across),
        PathCmd::line_to(end + across),
        PathCmd::arc_to(end - across, end, true),
        PathCmd::line_to(start - across),
        PathCmd::arc_to(start + across, start, true),
        PathCmd::close(),
    ])
}

/// How a view marks its holes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HoleMarks<'a> {
    /// Each hole with its tool's symbol, at a size that reads on the sheet,
    /// and its edge where that stands clear of the symbol. Around a smaller
    /// hole an edge would read as part of the symbol.
    Symbols(&'a [usize]),
    /// Each hole's edge alone.
    Edges,
}

/// Draw every tool's holes into `view`.
fn draw_tools(view: &mut View, tools: &[DrillTool], marks: HoleMarks<'_>, scale: f64) {
    let edge = match marks {
        HoleMarks::Symbols(_) => HAIR / scale,
        HoleMarks::Edges => THIN / scale,
    };
    let holes = view.layer("Holes", LayerRole::Drill, BLACK);
    for tool in tools {
        let clear = marks == HoleMarks::Edges || tool.diameter * scale >= 2.0 * SYMBOL_SIZE;
        let ring = clear.then(|| {
            view.artwork.push_aperture(Aperture {
                shape: ApertureShape::Circle {
                    diameter: tool.diameter,
                },
                hole_diameter: (tool.diameter - 2.0 * edge).max(0.0),
            })
        });
        for hit in &tool.hits {
            let outline = match (hit, ring) {
                (Hit::Hole(at), Some(ring)) => {
                    view.flash(holes, ring, *at);
                    continue;
                }
                (Hit::Hole(_), None) => continue,
                (Hit::Slot { start, end }, _) => vec![slot_outline(*start, *end, tool.diameter)],
                (Hit::Routed(contours), _) => contours.clone(),
            };
            view.stroke(holes, outline, StrokeStyle::round(edge));
        }
    }
    let HoleMarks::Symbols(symbols) = marks else {
        return;
    };
    let marks = view.layer("Symbols", LayerRole::Drill, BLACK);
    let sizes = symbol_sizes(tools, scale);
    for ((tool, &index), size) in tools.iter().zip(symbols).zip(sizes) {
        let mark = view
            .artwork
            .push_aperture(symbol_aperture(index, size / scale));
        for hit in &tool.hits {
            view.flash(marks, mark, hit.center());
        }
    }
}

/// The size on the sheet of each tool's symbol: the size the table shows
/// it at, or smaller where the tool's holes stand closer to their
/// neighbours than that, so a field of them reads as holes and not as a
/// hatch.
fn symbol_sizes(tools: &[DrillTool], scale: f64) -> Vec<f64> {
    /// How much of the way to its neighbour a symbol takes.
    const FILL: f64 = 0.85;
    // No hole further from its neighbours than this needs a smaller symbol.
    let reach = SYMBOL_SIZE / FILL / scale;
    let mut holes = tools
        .iter()
        .enumerate()
        .flat_map(|(tool, drill)| drill.hits.iter().map(move |hit| (hit.center(), tool)))
        .collect::<Vec<_>>();
    holes.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
    // Each hole's distance to its nearest neighbour of any tool, by tool.
    let mut nearest = vec![Vec::new(); tools.len()];
    for (index, (at, tool)) in holes.iter().enumerate() {
        let within = |other: &&(Point, usize)| (other.0.x - at.x).abs() <= reach;
        let before = holes[..index].iter().rev().take_while(within);
        let after = holes[index + 1..].iter().take_while(within);
        let distance = before
            .chain(after)
            .map(|(other, _)| at.distance_to(*other))
            .fold(reach, f64::min);
        nearest[*tool].push(distance);
    }
    nearest
        .into_iter()
        .map(|mut distances| {
            // The crowding of a tool's closest tenth: a few tight pairs do
            // not shrink a symbol that stands clear everywhere else.
            distances.sort_by(f64::total_cmp);
            let crowded = distances
                .get(distances.len() / 10)
                .copied()
                .unwrap_or(reach);
            (FILL * crowded * scale).clamp(SMALLEST_SYMBOL, SYMBOL_SIZE)
        })
        .collect()
}

/// A drill symbol as an aperture `size` across.
fn symbol_aperture(index: usize, size: f64) -> Aperture {
    Aperture::solid(ApertureShape::Contour {
        outline: symbol(index, size),
        fill_rule: FillRule::NonZero,
    })
}

/// A symbol on its own, to set in a table.
pub fn symbol_view(index: usize) -> Result<View> {
    let mut view = View::new();
    let layer = view.layer("Symbol", LayerRole::Drill, BLACK);
    let mark = view
        .artwork
        .push_aperture(symbol_aperture(index, SYMBOL_SIZE));
    view.flash(layer, mark, Point::ZERO);
    view.finish()
}

/// A board's outline with the holes of `tools`, marked as `marks` says.
pub fn board_view(
    outline: &[ContourBuf],
    tools: &[DrillTool],
    marks: HoleMarks<'_>,
    scale: f64,
) -> Result<View> {
    let mut view = View::new();
    let profile = view.layer("Profile", LayerRole::Profile, BLACK);
    view.stroke(
        profile,
        outline.to_vec(),
        StrokeStyle::round(MEDIUM / scale),
    );
    draw_tools(&mut view, tools, marks, scale);
    view.finish()
}

/// Lay an array's outline over `view` in a heavy line, and its boards' in
/// a thin one, so the two are never taken for each other.
fn array_outlines(view: &mut View, source: &Source<'_>, array: &ArrayData, scale: f64) {
    let (boards, _) = outline(source.imported, ProfileSet::FabricationOutlines);
    let profile = view.layer("Profile", LayerRole::Profile, BLACK);
    view.stroke(profile, boards, StrokeStyle::round(THIN / scale));
    for contours in &array.outlines {
        view.stroke(profile, contours.clone(), StrokeStyle::round(HEAVY / scale));
    }
}

/// An array as it is fabricated: its outline and its boards', what is
/// routed out of it, where it is scored, and the holes of its own.
pub fn array_view(source: &Source<'_>, array: &ArrayData, scale: f64) -> Result<View> {
    let mut view = View::new();

    let routed = view.layer("Routed", LayerRole::Other, ink(ROUTED));
    view.fill(routed, array.removal.clone());
    let routed_edge = view.layer("Routed edge", LayerRole::Other, BLACK);
    view.stroke(
        routed_edge,
        array.removal.clone(),
        StrokeStyle::round(THIN / scale),
    );
    array_outlines(&mut view, source, array, scale);

    // A score runs edge to edge; drawn a little past both, it reads as a
    // line of cut rather than an edge.
    let scores = view.layer("Scores", LayerRole::Other, BLACK);
    let overrun = 4.0 / scale;
    for line in &array.scores {
        let run = line.end - line.start;
        let along = run / run.length() * overrun;
        view.stroke(
            scores,
            vec![segment(line.start - along, line.end + along)],
            StrokeStyle {
                pattern: LinePattern::Center,
                ..StrokeStyle::new(THIN / scale, LineCap::Butt)
            },
        );
    }

    draw_tools(&mut view, &array.tools, HoleMarks::Edges, scale);
    view.finish()
}

/// An array with nothing drawn but its outline, its boards and what is
/// routed out of it: what a sheet draws its tooling holes and fiducials
/// over.
pub fn tooling_view(source: &Source<'_>, array: &ArrayData, scale: f64) -> Result<View> {
    let mut view = View::new();
    let routed = view.layer("Routed", LayerRole::Other, ink(ROUTED));
    view.fill(routed, array.removal.clone());
    array_outlines(&mut view, source, array, scale);
    view.finish()
}

/// What an array carries for whoever handles it, each drawn with a mark of
/// its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// A hole in the border: a circle on a centre cross.
    Tooling,
    /// A fiducial of the array's own: a dot in a ring.
    ArrayFiducial,
    /// A fiducial beside a board: a dot.
    BoardFiducial,
}

/// Sizes of the marks on the sheet: the radius of a tooling hole drawn no
/// smaller than reads and the reach of its cross, and the radii of a
/// fiducial's dot and of the ring round one of the array's own.
const TOOLING_RADIUS: f64 = 0.7;
const TOOLING_REACH: f64 = 1.5;
const FIDUCIAL_DOT: f64 = 0.4;
const FIDUCIAL_RING: f64 = 0.9;

impl Mark {
    /// How far the mark reaches from its centre: where a leader to it ends
    /// and a tag beside it starts.
    pub fn reach(self) -> f64 {
        match self {
            Self::Tooling => TOOLING_REACH,
            Self::ArrayFiducial => FIDUCIAL_RING,
            Self::BoardFiducial => FIDUCIAL_DOT,
        }
    }

    /// Draw the mark on the sheet at `at`. A tooling hole is drawn at its
    /// own size where that reads: `hole` is its radius on the sheet.
    pub fn draw(self, canvas: &mut Canvas<'_>, at: Point, hole: f64) {
        // A ring's line lies inside its radius, as the edge of a hole does.
        let ring = |canvas: &mut Canvas<'_>, radius: f64| {
            canvas.circle(at, radius - THIN / 2.0, Pen::solid(THIN));
        };
        match self {
            Self::Tooling => {
                let radius = hole.max(TOOLING_RADIUS);
                ring(canvas, radius);
                let reach = radius + TOOLING_REACH - TOOLING_RADIUS;
                for arm in [Point::new(reach, 0.0), Point::new(0.0, reach)] {
                    canvas.line(at - arm, at + arm, Pen::solid(HAIR));
                }
            }
            Self::ArrayFiducial => {
                canvas.fill_circle(at, FIDUCIAL_DOT, INK);
                ring(canvas, FIDUCIAL_RING);
            }
            Self::BoardFiducial => canvas.fill_circle(at, FIDUCIAL_DOT, INK),
        }
    }
}

/// One fabrication layer of the board as it images, still to be
/// [`View::outlined`]: its artwork in `color`, a copper layer with every hole
/// that passes through it left open. Returns the view and whether the layer
/// has artwork of its own.
pub fn layer_view(source: &Source<'_>, layer: &FabLayer, color: u32) -> Result<(View, bool)> {
    let imported = source.imported;
    let root = root_step(imported, true)?;
    let mut view = View::new();
    let (staged, has_content) = layer_objects(
        imported,
        layer.id,
        root,
        &mut view.artwork,
        source.resolution,
    )?;
    let mut objects = staged.into_iter().flatten().collect::<Vec<_>>();

    // A hole images only on its own layer, so the drill layers that reach
    // a copper layer are laid over it. A mask or a legend shows what is
    // printed: a hole under an opening is in the opening, and one the mask
    // covers is covered.
    if let Some(this) = layer.number.filter(|_| layer.role == LayerRole::Copper) {
        let copper = copper_order(imported);
        for (index, drill) in imported.layer_definitions.iter().enumerate() {
            if layer_role(drill.layer_function) != LayerRole::Drill {
                continue;
            }
            let span = drill
                .span
                .map_or(FeatureSpan::ThroughBoard, |span| FeatureSpan::FromTo {
                    from: span.from_layer,
                    to: span.to_layer,
                });
            let (from, to) = span_layers(imported, &copper, span);
            if !(from..=to).contains(&this) {
                continue;
            }
            let (holes, _) = layer_objects(
                imported,
                LayerId(index as u32),
                root,
                &mut view.artwork,
                source.resolution,
            )?;
            objects.extend(holes.into_iter().flatten().map(|mut hole| {
                hole.order.stage = PaintStage::FinalCutout;
                hole
            }));
        }
    }

    // Drawn in the copper role whatever it is: what cuts the layer, a hole
    // laid over it or a rout slot in its span, then removes material and
    // never images as artwork, even where the layer paints nothing.
    let artwork = view.layer(&layer.name, LayerRole::Copper, ink(color));
    for object in objects {
        view.artwork.push_object(artwork, object);
    }
    Ok((view, has_content))
}

const ARROW_LENGTH: f64 = 2.2;
const ARROW_HALF_WIDTH: f64 = 0.42;
/// How far an extension line starts from what it measures and runs past
/// its dimension line.
const EXTENSION_GAP: f64 = 1.0;
const EXTENSION_OVERRUN: f64 = 1.5;
const DIMENSION_TEXT_GAP: f64 = 0.9;

fn arrow(canvas: &mut Canvas<'_>, tip: Point, toward: Point) {
    let run = toward - tip;
    let along = run / run.length();
    let across = Point::new(-along.y, along.x) * ARROW_HALF_WIDTH;
    let base = tip + along * ARROW_LENGTH;
    canvas.fill_polygon(&[tip, base + across, base - across], INK);
}

/// Dimension the horizontal extent `from..to`, measured off features at
/// height `edge`, on a line at height `line`.
pub fn dimension_horizontal(
    canvas: &mut Canvas<'_>,
    from: f64,
    to: f64,
    edge: f64,
    line: f64,
    text: &str,
) {
    let pen = Pen::solid(THIN);
    let away = (line - edge).signum();
    for x in [from, to] {
        canvas.line(
            Point::new(x, edge + away * EXTENSION_GAP),
            Point::new(x, line + away * EXTENSION_OVERRUN),
            pen,
        );
    }
    let (left, right) = (Point::new(from, line), Point::new(to, line));
    canvas.line(left, right, pen);
    arrow(canvas, left, right);
    arrow(canvas, right, left);
    canvas.text(
        Point::new((from + to) / 2.0, line + DIMENSION_TEXT_GAP),
        text,
        TextStyle::new(BODY).align(Align::Center),
    );
}

/// Dimension the vertical extent `from..to`, measured off features at
/// `edge`, on a line at `line`.
pub fn dimension_vertical(
    canvas: &mut Canvas<'_>,
    from: f64,
    to: f64,
    edge: f64,
    line: f64,
    text: &str,
) {
    let pen = Pen::solid(THIN);
    let away = (line - edge).signum();
    for y in [from, to] {
        canvas.line(
            Point::new(edge + away * EXTENSION_GAP, y),
            Point::new(line + away * EXTENSION_OVERRUN, y),
            pen,
        );
    }
    let (bottom, top) = (Point::new(line, from), Point::new(line, to));
    canvas.line(bottom, top, pen);
    arrow(canvas, bottom, top);
    arrow(canvas, top, bottom);
    // Lettered to read from the bottom of the sheet, as every value is.
    canvas.text(
        Point::new(
            line + away * DIMENSION_TEXT_GAP * 1.6,
            (from + to) / 2.0 - BODY / 2.0,
        ),
        text,
        TextStyle::new(BODY).align(if away > 0.0 {
            Align::Left
        } else {
            Align::Right
        }),
    );
}

/// Closest two ordinate labels may sit.
const ORDINATE_PITCH: f64 = 3.4;
/// How far an ordinate's leader runs before its label, the jog included.
const ORDINATE_LEADER: f64 = 7.0;

/// Spread label positions at least `pitch` apart, keeping their order.
/// Labels too close to sit on their stations form a run at the pitch,
/// centred on the stations it labels where the datum leaves it room.
fn spread(stations: &[f64], pitch: f64) -> Vec<f64> {
    // Runs as (first station, count).
    let mut runs = Vec::<(usize, usize)>::new();
    // No run starts before the first station: the chain of the other axis
    // letters there, and the two would cross.
    let start = |(first, count): (usize, usize)| {
        let mean = stations[first..first + count].iter().sum::<f64>() / count as f64;
        (mean - (count - 1) as f64 * pitch / 2.0).max(stations[0])
    };
    for index in 0..stations.len() {
        runs.push((index, 1));
        while let [.., previous, last] = runs[..] {
            let previous_end = start(previous) + previous.1 as f64 * pitch;
            if previous_end <= start(last) + 1e-9 {
                break;
            }
            runs.truncate(runs.len() - 2);
            runs.push((previous.0, previous.1 + last.1));
        }
    }
    runs.into_iter()
        .flat_map(|run| (0..run.1).map(move |label| start(run) + label as f64 * pitch))
        .collect()
}

/// Ordinate dimensions along the bottom of a view: each station's distance
/// from the datum, on a leader run out from `edge` and jogged clear of its
/// neighbours.
pub fn ordinates_horizontal(canvas: &mut Canvas<'_>, stations: &[(f64, String)], edge: f64) {
    let pen = Pen::solid(THIN);
    let stood = stations.iter().map(|(x, _)| *x).collect::<Vec<_>>();
    let labels = spread(&stood, ORDINATE_PITCH);
    for ((x, text), label) in stations.iter().zip(labels) {
        let top = edge - EXTENSION_GAP;
        canvas.polyline(
            &[
                Point::new(*x, top),
                Point::new(*x, top - 2.0),
                Point::new(label, top - ORDINATE_LEADER + 1.5),
                Point::new(label, top - ORDINATE_LEADER),
            ],
            false,
            pen,
        );
        canvas.text(
            Point::new(label + BODY / 2.0, top - ORDINATE_LEADER - 0.8),
            text,
            TextStyle::new(BODY).align(Align::Right).vertical(),
        );
    }
}

/// Ordinate dimensions down the left of a view.
pub fn ordinates_vertical(canvas: &mut Canvas<'_>, stations: &[(f64, String)], edge: f64) {
    let pen = Pen::solid(THIN);
    let stood = stations.iter().map(|(y, _)| *y).collect::<Vec<_>>();
    let labels = spread(&stood, ORDINATE_PITCH);
    for ((y, text), label) in stations.iter().zip(labels) {
        let right = edge - EXTENSION_GAP;
        canvas.polyline(
            &[
                Point::new(right, *y),
                Point::new(right - 2.0, *y),
                Point::new(right - ORDINATE_LEADER + 1.5, label),
                Point::new(right - ORDINATE_LEADER, label),
            ],
            false,
            pen,
        );
        canvas.text(
            Point::new(right - ORDINATE_LEADER - 0.8, label - BODY / 2.0),
            text,
            TextStyle::new(BODY).align(Align::Right),
        );
    }
}

/// Room tags take outside a view: their leaders and the tags themselves.
pub const TAGS: f64 = 10.0;

/// Tag marks of a view from outside it: each mark's tag beyond the edge of
/// `view` it stands nearest, on a leader from the mark, the tags of an edge
/// spread so none stands on another. A mark is where it is on the sheet,
/// how far it reaches, and its tag.
pub fn tags(canvas: &mut Canvas<'_>, view: BBox, marks: &[(Point, f64, String)]) {
    /// How far apart tags sit along an edge they are lettered across, and
    /// down one they are stacked beside.
    const ACROSS: f64 = 4.8;
    const DOWN: f64 = 3.0;
    /// How far past the edge a leader bends and ends.
    const BEND: f64 = 2.5;
    const END: f64 = 5.0;
    let pen = Pen::solid(HAIR);
    let style = TextStyle::new(LABEL).bold();
    // Top, bottom, left, right: the edge's place, whether tags run along X,
    // and which way is out.
    let edges = [
        (view.max.y, true, 1.0),
        (view.min.y, true, -1.0),
        (view.min.x, false, -1.0),
        (view.max.x, false, 1.0),
    ];
    let nearest = |at: Point| {
        let distances = [
            view.max.y - at.y,
            at.y - view.min.y,
            at.x - view.min.x,
            view.max.x - at.x,
        ];
        let nearest = (0..4).min_by(|a, b| distances[*a].total_cmp(&distances[*b]));
        nearest.expect("a view has four edges")
    };
    for (edge, &(place, along_x, out)) in edges.iter().enumerate() {
        let mut marks = marks
            .iter()
            .filter(|(at, ..)| nearest(*at) == edge)
            .collect::<Vec<_>>();
        let along = |at: Point| if along_x { at.x } else { at.y };
        marks.sort_by(|a, b| along(a.0).total_cmp(&along(b.0)));
        let stood = marks.iter().map(|(at, ..)| along(*at)).collect::<Vec<_>>();
        if stood.is_empty() {
            continue;
        }
        // Spread about their own middle, so a cluster in a corner fans both
        // ways rather than trailing off one.
        let pitch = if along_x { ACROSS } else { DOWN };
        let labels = spread(&stood, pitch);
        let shift = (stood.iter().sum::<f64>() - labels.iter().sum::<f64>()) / stood.len() as f64;
        for ((at, reach, tag), label) in marks.into_iter().zip(labels) {
            let label = label + shift;
            let point = |along: f64, across: f64| {
                if along_x {
                    Point::new(along, across)
                } else {
                    Point::new(across, along)
                }
            };
            let (bend, end) = (
                point(label, place + out * BEND),
                point(label, place + out * END),
            );
            let run = bend - *at;
            let from = *at + run / run.length().max(1e-9) * *reach;
            canvas.polyline(&[from, bend, end], false, pen);
            let (at, align) = match (along_x, out > 0.0) {
                (true, true) => (end + Point::new(0.0, 0.7), Align::Center),
                (true, false) => (end - Point::new(0.0, 0.7 + LABEL), Align::Center),
                (false, true) => (end + Point::new(0.7, -LABEL / 2.0), Align::Left),
                (false, false) => (end - Point::new(0.7, LABEL / 2.0), Align::Right),
            };
            canvas.text(at, tag, style.align(align));
        }
    }
}

/// The datum ordinates are measured from: a target on the corner.
pub fn datum(canvas: &mut Canvas<'_>, at: Point) {
    let pen = Pen::solid(THIN);
    canvas.circle(at, 1.6, pen);
    canvas.line(
        Point::new(at.x - 2.6, at.y),
        Point::new(at.x + 2.6, at.y),
        pen,
    );
    canvas.line(
        Point::new(at.x, at.y - 2.6),
        Point::new(at.x, at.y + 2.6),
        pen,
    );
}

/// Height of a view's title with one line of detail and its scale bar, and
/// of each further line of detail.
pub const TITLE_HEIGHT: f64 = 13.0;
pub const TITLE_LEADING: f64 = 4.0;

/// Where a view's title is lettered: under it with its details on a line,
/// under it with a detail to a line where the line would outrun the view's
/// room, or to its right where the sheet has more width to spare than
/// height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSide {
    Below,
    Stacked,
    Beside,
}

/// The room a title takes however it is lettered; over the views of a
/// sheet, the most any of their titles takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TitleRoom {
    /// Width with the details on one line, and with a detail to a line.
    pub below: f64,
    pub beside: f64,
    /// How many details there are.
    lines: usize,
}

impl TitleRoom {
    pub fn most(self, other: Self) -> Self {
        Self {
            below: self.below.max(other.below),
            beside: self.beside.max(other.beside),
            lines: self.lines.max(other.lines),
        }
    }

    /// Height lettered a detail to a line.
    pub fn height(self) -> f64 {
        stacked_height(self.lines)
    }
}

/// Height of a title lettered a detail to a line, `details` of them.
fn stacked_height(details: usize) -> f64 {
    TITLE_HEIGHT + details.saturating_sub(1) as f64 * TITLE_LEADING
}

/// A view's title: what it shows, and what is said of it.
#[derive(Debug, Clone)]
pub struct Title {
    pub name: String,
    /// The scale first, then whatever else, each short enough to stand on
    /// a line of its own.
    pub details: Vec<String>,
}

impl Title {
    /// The longest a scale bar is drawn under a view, and beside one.
    const BAR_BELOW: f64 = 50.0;
    const BAR_BESIDE: f64 = 30.0;
    /// Room a scale bar's labels take at its ends.
    const BAR_LABELS: (f64, f64) = (3.0, 9.0);

    /// Width the title takes lettered under a view, its details on a line.
    pub fn width_below(&self, fonts: &Fonts) -> f64 {
        let name = fonts.width(Weight::Bold, HEADING, &self.name);
        let details = fonts.width(Weight::Regular, BODY, &self.details.join(" · "));
        let bar = Self::BAR_LABELS.0 + Self::BAR_BELOW + Self::BAR_LABELS.1;
        name.max(details).max(bar)
    }

    /// Height the title takes lettered beside a view, a detail to a line.
    pub fn height_beside(&self) -> f64 {
        stacked_height(self.details.len())
    }

    pub fn room(&self, fonts: &Fonts) -> TitleRoom {
        TitleRoom {
            below: self.width_below(fonts),
            beside: self.width_beside(fonts),
            lines: self.details.len(),
        }
    }

    /// Width the title takes lettered beside a view, a detail to a line.
    fn width_beside(&self, fonts: &Fonts) -> f64 {
        let name = fonts.width(Weight::Bold, HEADING, &self.name);
        let details = self
            .details
            .iter()
            .map(|detail| fonts.width(Weight::Regular, BODY, detail));
        let bar = Self::BAR_LABELS.0 + Self::BAR_BESIDE + Self::BAR_LABELS.1;
        details.fold(name.max(bar), f64::max)
    }

    /// Letter the title down from `top`: centred there under a view with
    /// its details on one line or a detail to a line, or from there
    /// rightwards beside one with a detail to a line. Under the details a
    /// bar measures true at the view's scale however the sheet is printed.
    pub fn draw(&self, canvas: &mut Canvas<'_>, top: Point, side: TitleSide, scale: Scale) {
        let (align, details) = match side {
            TitleSide::Below => (Align::Center, vec![self.details.join(" · ")]),
            TitleSide::Stacked => (Align::Center, self.details.clone()),
            TitleSide::Beside => (Align::Left, self.details.clone()),
        };
        let baseline = top.y - HEADING;
        let name = TextStyle::new(HEADING).bold().align(align);
        let width = canvas.text(Point::new(top.x, baseline), &self.name, name);
        let rule = baseline - 1.3;
        let left = match side {
            TitleSide::Below | TitleSide::Stacked => top.x - width / 2.0,
            TitleSide::Beside => top.x,
        };
        canvas.line(
            Point::new(left, rule),
            Point::new(left + width, rule),
            Pen::solid(MEDIUM),
        );
        let mut y = rule;
        for detail in &details {
            y -= TITLE_LEADING;
            canvas.text(
                Point::new(top.x, y + 0.6),
                detail,
                TextStyle::new(BODY).align(align),
            );
        }
        let (longest, bar_left) = match side {
            TitleSide::Below => (Self::BAR_BELOW, None),
            TitleSide::Stacked => (Self::BAR_BESIDE, None),
            TitleSide::Beside => (Self::BAR_BESIDE, Some(top.x + Self::BAR_LABELS.0)),
        };
        scale_bar(canvas, top.x, bar_left, y - 3.4, scale, longest);
    }
}

/// A bar a round number of millimetres long at `scale`, drawn no longer
/// than `longest` with a tick at each end and at its middle: centred on
/// `center`, or from `left`.
fn scale_bar(
    canvas: &mut Canvas<'_>,
    center: f64,
    left: Option<f64>,
    y: f64,
    scale: Scale,
    longest: f64,
) {
    const LENGTHS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];
    let length = LENGTHS
        .into_iter()
        .rev()
        .find(|length| length * scale.factor() <= longest)
        .unwrap_or(1.0);
    let drawn = length * scale.factor();
    let pen = Pen::solid(THIN);
    let left = left.unwrap_or(center - drawn / 2.0);
    let right = left + drawn;
    canvas.line(Point::new(left, y), Point::new(right, y), pen);
    for x in [left, (left + right) / 2.0, right] {
        canvas.line(Point::new(x, y), Point::new(x, y + 1.2), pen);
    }
    let label = TextStyle::new(LABEL);
    canvas.text(Point::new(left - 1.2, y), "0", label.align(Align::Right));
    canvas.text(
        Point::new(right + 1.2, y),
        &format!("{length:.0} mm"),
        label,
    );
}

/// The distinct stations of an ordinate chain, each lettered with its
/// distance from `datum`. Stations closer than a drawing can show apart
/// are one station.
pub fn stations(values: impl IntoIterator<Item = f64>, datum: f64) -> Vec<(f64, String)> {
    /// Edges nearer than this are one edge drawn twice, not two edges.
    const COINCIDENT: f64 = 0.05;
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    // The ends of the chain are the extents: a station that all but stands
    // on one is that end, not a second station beside it.
    if let (Some(&low), Some(&high)) = (values.first(), values.last()) {
        values.retain(|value| {
            let interior = *value - low >= COINCIDENT && high - *value >= COINCIDENT;
            interior || *value == low || *value == high
        });
    }
    values.dedup_by(|next, kept| (*next - *kept).abs() < COINCIDENT);
    // A first station on the datum is the datum: the chain reads from it,
    // not from a corner a few micrometres away.
    let datum = match values.first() {
        Some(first) if (*first - datum).abs() < COINCIDENT => *first,
        _ => datum,
    };
    values
        .into_iter()
        .map(|value| (value, mm(value - datum)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_step_down_to_fit_the_room() {
        assert_eq!(Scale::fit(100.0, 80.0, 240.0, 250.0), Scale::up(2));
        assert_eq!(Scale::fit(100.0, 80.0, 500.0, 400.0), Scale::up(5));
        assert_eq!(Scale::fit(297.0, 210.0, 290.0, 400.0), Scale::down(2));
        assert_eq!(Scale::fit(5000.0, 10.0, 100.0, 100.0), Scale::down(20));
        assert_eq!(Scale::down(5).to_string(), "1:5");
    }

    #[test]
    fn ordinate_labels_keep_their_pitch_and_stay_over_their_stations() {
        let spread = |stations: &[f64]| spread(stations, ORDINATE_PITCH);
        let labels = spread(&[0.0, 1.0, 2.0, 50.0]);
        for pair in labels.windows(2) {
            assert!(pair[1] - pair[0] >= ORDINATE_PITCH - 1e-9, "{labels:?}");
        }
        // A tight group at the datum runs away from it, where the other
        // axis letters nothing; one elsewhere straddles its stations.
        assert_eq!(labels[0], 0.0, "{labels:?}");
        let labels = spread(&[0.0, 30.0, 31.0, 32.0]);
        assert!(labels[1] < 30.0 && labels[3] > 32.0, "{labels:?}");
        assert_eq!(spread(&[0.0, 10.0, 20.0]), [0.0, 10.0, 20.0]);
    }

    #[test]
    fn an_outline_states_its_straight_edges_and_corner_radii() {
        // A 20 x 10 board with one corner rounded to 2 mm and a 4 x 3 notch
        // in its top edge.
        let outline = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(0.0, 0.0)),
            PathCmd::line_to(Point::new(18.0, 0.0)),
            PathCmd::arc_to(Point::new(20.0, 2.0), Point::new(18.0, 2.0), false),
            PathCmd::line_to(Point::new(20.0, 10.0)),
            PathCmd::line_to(Point::new(12.0, 10.0)),
            PathCmd::line_to(Point::new(12.0, 7.0)),
            PathCmd::line_to(Point::new(8.0, 7.0)),
            PathCmd::line_to(Point::new(8.0, 10.0)),
            PathCmd::line_to(Point::new(0.0, 10.0)),
            PathCmd::close(),
        ]);
        let features = OutlineFeatures::of(&[outline]);
        let texts = |values: &[f64]| {
            stations(values.iter().copied(), 0.0)
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&features.edges_x), ["0.00", "8.00", "12.00", "20.00"]);
        // Edges a drawing cannot show apart are one station, and one that
        // all but stands on an end of the chain is that end.
        assert_eq!(texts(&[0.0, 4.982, 5.0, 10.0]), ["0.00", "4.98", "10.00"]);
        assert_eq!(texts(&[0.0, 9.982, 10.0, 10.021]), ["0.00", "10.02"]);
        assert_eq!(texts(&features.edges_y), ["0.00", "7.00", "10.00"]);
        assert_eq!(features.radii, [(2.0, 1)]);
        assert_eq!(features.radii().unwrap(), "1X R2.00");
    }

    #[test]
    fn a_slot_outline_is_its_centre_line_swept_by_the_tool() {
        let outline = slot_outline(Point::new(0.0, 0.0), Point::new(4.0, 0.0), 1.0);
        assert_eq!(
            outline.bbox,
            BBox::new(Point::new(-0.5, -0.5), Point::new(4.5, 0.5))
        );
    }
}
