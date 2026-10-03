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
    ArrayData, DrillTool, FabLayer, Hit, HoleKind, Source, copper_order, mm, mm_fine, span_layers,
};
use super::pdf::{Align, Canvas, Fonts, INK, Pen, TextStyle, Weight};
use super::sheet::{BODY, HAIR, HEADING, LABEL, MEDIUM, THICK, THIN};
use super::symbols::symbol;
use crate::geometry::render::layer_objects;
use crate::geometry::step_artwork::root_step;
use crate::layers::layer_role;

pub type ViewArtwork = artwork::Document<(), Option<Symbol>>;
type ViewObject = Object<Option<Symbol>>;

/// Size of a drill symbol on the sheet.
pub const SYMBOL_SIZE: f64 = 1.5;

const fn ink(color: u32) -> LayerStyle {
    LayerStyle {
        color,
        opacity: 1.0,
    }
}

/// Copper, in every view that shows it.
pub const COPPER: u32 = 0xa0501c;
/// A board whose mask the design does not colour.
pub const UNCOLOURED: u32 = 0xbdbdbd;

const BLACK: LayerStyle = ink(INK);
/// Material an array has routed away.
const ROUTED: LayerStyle = ink(0xd4d4d4);

/// The ratio a view is drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    pub drawn: u32,
    pub actual: u32,
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

    fn fill(&mut self, layer: u32, contours: Vec<ContourBuf>, rule: FillRule) {
        if contours.is_empty() {
            return;
        }
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
    pub radii: Vec<(f64, usize)>,
}

impl OutlineFeatures {
    /// Edges shorter than this are detail the data states, not the drawing.
    const SHORTEST_EDGE: f64 = 1.0;

    /// The features of an outline given as its outer contour followed by
    /// its cutouts. Corners are the outer contour's: an arc of a cutout is
    /// that cutout's shape, which the data states.
    pub fn of(contours: &[ContourBuf]) -> Self {
        let mut features = Self::default();
        let mut radii = std::collections::BTreeMap::<i64, usize>::new();
        let segments = contours.iter().enumerate().flat_map(|(index, contour)| {
            let outer = index == 0;
            contour.segments().map(move |segment| (outer, segment))
        });
        for (outer, segment) in segments {
            match segment {
                pcb_ir::geom::Segment::Line { start, end } => {
                    let run = end - start;
                    if run.x.hypot(run.y) < Self::SHORTEST_EDGE {
                        continue;
                    }
                    if run.x.abs() < 1e-6 {
                        features.edges_x.push(start.x);
                    } else if run.y.abs() < 1e-6 {
                        features.edges_y.push(start.y);
                    }
                }
                // A corner turns the outline by no more than a right angle;
                // a longer arc is a feature of its own.
                pcb_ir::geom::Segment::Arc(arc) => {
                    if outer && arc.sweep_radians() <= 100.0_f64.to_radians() {
                        *radii
                            .entry((arc.radius() * 1000.0).round() as i64)
                            .or_default() += 1;
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
    pub fn radii_note(&self) -> Option<String> {
        if self.radii.is_empty() || self.radii.len() > 3 {
            return None;
        }
        let radii = self
            .radii
            .iter()
            .map(|(radius, count)| format!("{count}X R{}", mm_fine(*radius)))
            .collect::<Vec<_>>();
        Some(format!("CORNER RADII {}", radii.join(", ")))
    }
}

fn segment(from: Point, to: Point) -> ContourBuf {
    ContourBuf::new(vec![PathCmd::move_to(from), PathCmd::line_to(to)])
}

/// The edge of a slot `diameter` wide along the centre line `start..end`.
fn slot_outline(start: Point, end: Point, diameter: f64) -> ContourBuf {
    let run = end - start;
    let along = run / run.x.hypot(run.y);
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
    /// Each hole's edge alone: a detail, drawn large enough to show it.
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
    for (tool, &index) in tools.iter().zip(symbols) {
        let mark = view.artwork.push_aperture(symbol_aperture(index, scale));
        for hit in &tool.hits {
            view.flash(marks, mark, hit.center());
        }
    }
}

/// A drill symbol as an aperture that images at [`SYMBOL_SIZE`] on a sheet
/// drawn at `scale`.
pub fn symbol_aperture(index: usize, scale: f64) -> Aperture {
    Aperture::solid(ApertureShape::Contour {
        outline: symbol(index, SYMBOL_SIZE / scale),
        fill_rule: FillRule::NonZero,
    })
}

/// A symbol on its own, to set in a table.
pub fn symbol_view(index: usize) -> Result<View> {
    let mut view = View::new();
    let layer = view.layer("Symbol", LayerRole::Drill, BLACK);
    let mark = view.artwork.push_aperture(symbol_aperture(index, 1.0));
    view.flash(layer, mark, Point::ZERO);
    view.finish()
}

/// A board's outline with the holes that are part of its shape: every
/// hole but the vias, each by its edge.
pub fn outline_view(source: &Source<'_>, tools: &[DrillTool], scale: f64) -> Result<View> {
    let mut view = View::new();
    let (contours, _) = outline(source.imported, ProfileSet::BoardOutlines);
    let profile = view.layer("Profile", LayerRole::Profile, BLACK);
    view.stroke(profile, contours, StrokeStyle::round(MEDIUM / scale));
    let mechanical = tools
        .iter()
        .filter(|tool| tool.kind != HoleKind::Via)
        .cloned()
        .collect::<Vec<_>>();
    draw_tools(&mut view, &mechanical, HoleMarks::Edges, scale);
    view.finish()
}

/// A board's outline and its drill pattern.
pub fn drill_view(
    source: &Source<'_>,
    tools: &[DrillTool],
    symbols: &[usize],
    scale: f64,
) -> Result<View> {
    let mut view = View::new();
    let (contours, _) = outline(source.imported, ProfileSet::BoardOutlines);
    let profile = view.layer("Profile", LayerRole::Profile, BLACK);
    view.stroke(profile, contours, StrokeStyle::round(MEDIUM / scale));
    draw_tools(&mut view, tools, HoleMarks::Symbols(symbols), scale);
    view.finish()
}

/// An array as it is fabricated: its outline and its boards', what is
/// routed out of it, where it is scored, and the tooling it carries.
pub fn array_view(source: &Source<'_>, array: &ArrayData, scale: f64) -> Result<View> {
    let mut view = View::new();

    let routed = view.layer("Routed", LayerRole::Other, ROUTED);
    view.fill(routed, array.removal.clone(), FillRule::NonZero);
    let routed_edge = view.layer("Routed edge", LayerRole::Other, BLACK);
    view.stroke(
        routed_edge,
        array.removal.clone(),
        StrokeStyle::round(THIN / scale),
    );

    let (boards, _) = outline(source.imported, ProfileSet::FabricationOutlines);
    let profile = view.layer("Profile", LayerRole::Profile, BLACK);
    view.stroke(profile, boards, StrokeStyle::round(MEDIUM / scale));
    for contours in &array.outlines {
        view.stroke(profile, contours.clone(), StrokeStyle::round(THICK / scale));
    }

    // A score runs edge to edge; drawn a little past both, it reads as a
    // line of cut rather than an edge.
    let scores = view.layer("Scores", LayerRole::Other, BLACK);
    let overrun = 4.0 / scale;
    for line in &array.scores {
        let run = line.end - line.start;
        let along = run / run.x.hypot(run.y) * overrun;
        view.stroke(
            scores,
            vec![segment(line.start - along, line.end + along)],
            StrokeStyle {
                pattern: LinePattern::Center,
                ..StrokeStyle::new(THIN / scale, LineCap::Butt)
            },
        );
    }

    // A fiducial is a copper dot: the top side's solid, the bottom's in
    // outline.
    let fiducials = view.layer("Fiducials", LayerRole::Other, BLACK);
    for fiducial in &array.fiducials {
        let wall = THIN / scale;
        let dot = match fiducial.side {
            Side::Bottom => Aperture {
                shape: ApertureShape::Circle {
                    diameter: fiducial.diameter,
                },
                hole_diameter: (fiducial.diameter - 2.0 * wall).max(0.0),
            },
            _ => Aperture::circle(fiducial.diameter),
        };
        let dot = view.artwork.push_aperture(dot);
        view.flash(fiducials, dot, fiducial.at);
    }

    draw_tools(&mut view, &array.tools, HoleMarks::Edges, scale);
    view.finish()
}

/// One fabrication layer as it images, still to be [`View::outlined`]: its
/// artwork in `ink`, over the board it is printed on filled in `ground`
/// where the layer is seen against one, with every hole that passes through
/// it left open. Returns the view and whether the layer has artwork of its
/// own.
pub fn layer_view(
    source: &Source<'_>,
    layer: &FabLayer,
    board: bool,
    ink: u32,
    ground: Option<(u32, Vec<ContourBuf>)>,
) -> Result<(View, bool)> {
    let imported = source.imported;
    let root = root_step(imported, board)?;
    let mut view = View::new();
    if let Some((color, silhouette)) = ground {
        let ground = view.layer("Board", LayerRole::Other, self::ink(color));
        view.fill(ground, silhouette, FillRule::EvenOdd);
    }
    let (staged, has_content) = layer_objects(
        imported,
        layer.id,
        root,
        &mut view.artwork,
        source.resolution,
    )?;
    let mut objects = staged.into_iter().flatten().collect::<Vec<_>>();

    // A hole images only on its own layer, so the drill layers that reach
    // this one are laid over it: mask and legend lie on the outer copper.
    let copper = copper_order(imported);
    let this = match (layer.number, layer.side) {
        (Some(number), _) => number,
        (None, Side::Bottom) => copper.len().max(1),
        (None, _) => 1,
    };
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

    // Drawn as copper whatever it is, so its holes cut it even where it
    // paints nothing.
    let artwork = view.layer(&layer.name, LayerRole::Copper, self::ink(ink));
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
    let along = run / run.x.hypot(run.y);
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

/// Spread label positions at least [`ORDINATE_PITCH`] apart, keeping their
/// order. Labels too close to sit on their stations form a run at the pitch,
/// centred on the stations it labels.
fn spread(stations: &[f64]) -> Vec<f64> {
    // Runs as (first station, count).
    let mut runs = Vec::<(usize, usize)>::new();
    let start = |(first, count): (usize, usize)| {
        let mean = stations[first..first + count].iter().sum::<f64>() / count as f64;
        mean - (count - 1) as f64 * ORDINATE_PITCH / 2.0
    };
    for index in 0..stations.len() {
        runs.push((index, 1));
        while let [.., previous, last] = runs[..] {
            let previous_end = start(previous) + previous.1 as f64 * ORDINATE_PITCH;
            if previous_end <= start(last) + 1e-9 {
                break;
            }
            runs.truncate(runs.len() - 2);
            runs.push((previous.0, previous.1 + last.1));
        }
    }
    runs.into_iter()
        .flat_map(|run| (0..run.1).map(move |label| start(run) + label as f64 * ORDINATE_PITCH))
        .collect()
}

/// Ordinate dimensions along the bottom of a view, or along its top: each
/// station's distance from the datum, on a leader run out from `edge` and
/// jogged clear of its neighbours.
pub fn ordinates_horizontal(
    canvas: &mut Canvas<'_>,
    stations: &[(f64, String)],
    edge: f64,
    above: bool,
) {
    let pen = Pen::solid(THIN);
    let away = if above { 1.0 } else { -1.0 };
    let labels = spread(&stations.iter().map(|(x, _)| *x).collect::<Vec<_>>());
    for ((x, text), label) in stations.iter().zip(labels) {
        let from = edge + away * EXTENSION_GAP;
        let to = from + away * ORDINATE_LEADER;
        canvas.polyline(
            &[
                Point::new(*x, from),
                Point::new(*x, from + away * 2.0),
                Point::new(label, to - away * 1.5),
                Point::new(label, to),
            ],
            false,
            pen,
        );
        let align = if above { Align::Left } else { Align::Right };
        canvas.text(
            Point::new(label + BODY / 2.0, to + away * 0.8),
            text,
            TextStyle::new(BODY).align(align).vertical(),
        );
    }
}

/// Ordinate dimensions down the left of a view.
pub fn ordinates_vertical(canvas: &mut Canvas<'_>, stations: &[(f64, String)], edge: f64) {
    let pen = Pen::solid(THIN);
    let labels = spread(&stations.iter().map(|(y, _)| *y).collect::<Vec<_>>());
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

/// Where a view's title is lettered: under it, or to its right where the
/// sheet has more width to spare than height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSide {
    Below,
    Beside,
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
        TITLE_HEIGHT + self.details.len().saturating_sub(1) as f64 * TITLE_LEADING
    }

    /// Width the title takes lettered beside a view, a detail to a line.
    pub fn width_beside(&self, fonts: &Fonts) -> f64 {
        let name = fonts.width(Weight::Bold, HEADING, &self.name);
        let details = self
            .details
            .iter()
            .map(|detail| fonts.width(Weight::Regular, BODY, detail));
        let bar = Self::BAR_LABELS.0 + Self::BAR_BESIDE + Self::BAR_LABELS.1;
        details.fold(name.max(bar), f64::max)
    }

    /// Letter the title down from `top`: centred there under a view with
    /// its details on one line, or from there rightwards beside one with a
    /// detail to a line. Under the details a bar measures true at the
    /// view's scale however the sheet is printed.
    pub fn draw(&self, canvas: &mut Canvas<'_>, top: Point, side: TitleSide, scale: Scale) {
        let (align, details) = match side {
            TitleSide::Below => (Align::Center, vec![self.details.join(" · ")]),
            TitleSide::Beside => (Align::Left, self.details.clone()),
        };
        let baseline = top.y - HEADING;
        let name = TextStyle::new(HEADING).bold().align(align);
        let width = canvas.text(Point::new(top.x, baseline), &self.name, name);
        let rule = baseline - 1.3;
        let left = match side {
            TitleSide::Below => top.x - width / 2.0,
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
    values.dedup_by(|next, kept| (*next - *kept).abs() < COINCIDENT);
    values
        .into_iter()
        .map(|value| (value, mm(value - datum)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_fit_the_room_and_never_overflow_it() {
        assert_eq!(Scale::fit(100.0, 80.0, 240.0, 250.0), Scale::up(2));
        assert_eq!(Scale::fit(100.0, 80.0, 300.0, 250.0), Scale::up(2));
        assert_eq!(Scale::fit(100.0, 80.0, 500.0, 400.0), Scale::up(5));
        assert_eq!(Scale::fit(297.0, 210.0, 290.0, 400.0), Scale::down(2));
        assert_eq!(Scale::fit(5000.0, 10.0, 100.0, 100.0), Scale::down(20));
        assert_eq!(Scale::down(5).to_string(), "1:5");
    }

    #[test]
    fn ordinate_labels_keep_their_pitch_and_stay_over_their_stations() {
        let labels = spread(&[0.0, 1.0, 2.0, 50.0]);
        for pair in labels.windows(2) {
            assert!(pair[1] - pair[0] >= ORDINATE_PITCH - 1e-9, "{labels:?}");
        }
        // The tight group straddles its stations rather than trailing them.
        assert!(labels[0] < 0.0 && labels[2] > 2.0, "{labels:?}");
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
        // Edges a drawing cannot show apart are one station.
        assert_eq!(texts(&[0.0, 9.982, 10.0, 10.021]), ["0.00", "9.98"]);
        assert_eq!(texts(&features.edges_y), ["0.00", "7.00", "10.00"]);
        assert_eq!(features.radii, [(2.0, 1)]);
        assert_eq!(features.radii_note().unwrap(), "CORNER RADII 1X R2.00");
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
