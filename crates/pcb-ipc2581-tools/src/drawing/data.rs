//! What a fabrication drawing states, read from the design.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result};
use ipc2581::types::{LayerFunction, WhereMeasured};
use ipc2581::{Ipc2581, Symbol};
use pcb_ir::dialects::ipc::analysis::ProfileOccurrence;
use pcb_ir::dialects::ipc::lower::nc_linear_slot;
use pcb_ir::dialects::ipc::relief::VScoreLine;
use pcb_ir::dialects::ipc::{
    ArtworkScope, Feature, FeatureKind, FeatureSpan, FiducialKind, PlatingKind,
    ProfileOccurrenceRole, ProfileSet, SimpleShape, profile_occurrences_for,
};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, ContourBuf, Point, Resolution};
use pcb_ir::import::ipc2581::{GeometryDocument, ImportedDesign, LayerId};

use crate::accessors::{
    BoardArrayGridInfo, ColorInfo, IpcAccessor, StackupDetails, StackupLayerInfo, StackupLayerType,
};
use crate::layers::{ir_side, is_copper, layer_role};

/// A design as the drawing reads it.
pub struct Source<'a> {
    ipc: &'a Ipc2581,
    pub imported: &'a ImportedDesign,
    accessor: IpcAccessor<'a>,
    pub resolution: Resolution,
    /// The board's stackup, where the design has one.
    pub stackup: Option<StackupDetails>,
}

impl<'a> Source<'a> {
    pub fn new(ipc: &'a Ipc2581, imported: &'a ImportedDesign, resolution: Resolution) -> Self {
        let accessor = IpcAccessor::new(ipc);
        Self {
            ipc,
            imported,
            stackup: accessor.stackup_details(),
            accessor,
            resolution,
        }
    }

    /// The board's finished thickness, where its stackup states one.
    pub fn thickness(&self) -> Option<f64> {
        self.stackup.as_ref()?.overall_thickness_mm
    }
}

// What the drawing requires of a fabricator where the design data states
// nothing: the ordinary terms a rigid board is bought to.

/// IPC-6012 performance class.
const CLASS: u8 = 2;
/// Included angle of a V-score, in degrees.
pub const SCORE_ANGLE: f64 = 30.0;
/// Material a V-score leaves, as a share of the board's thickness, and the
/// thinnest web a score is asked to leave.
const SCORE_WEB: f64 = 1.0 / 3.0;
const THINNEST_WEB: f64 = 0.25;

/// The web a score leaves in a board `thickness` thick, where the board is
/// thick enough to be scored to a share of itself.
pub fn score_web(thickness: f64) -> Option<f64> {
    let web = thickness * SCORE_WEB;
    (web >= THINNEST_WEB).then_some(web)
}

/// A length in whole micrometres. Sizes compare by the micrometre: two that
/// differ by less are one size.
pub fn micrometres(length: f64) -> i64 {
    (length * 1000.0).round() as i64
}

/// A length as a drawing letters it: millimetres to two places.
pub fn mm(value: f64) -> String {
    // Rounded to the micrometre first, so two lengths the data states alike
    // letter alike, and before it is lettered, so nothing reads "-0.00".
    let micrometres = (value * 1000.0).round();
    format!("{:.2}", (micrometres / 10.0).round() / 100.0 + 0.0)
}

/// A length to the micrometre, lettered to two places where that says it
/// all: a tool's size must not round away what the data states.
pub fn mm_fine(value: f64) -> String {
    let text = format!("{:.3}", (value * 1000.0).round() / 1000.0 + 0.0);
    text.strip_suffix('0').map_or(text.clone(), str::to_string)
}

/// The extents of `bounds` as a drawing letters a size.
pub fn size_mm(bounds: BBox) -> String {
    format!("{} × {} mm", mm(bounds.width()), mm(bounds.height()))
}

/// The copper layers top to bottom: as the stackup orders them, or as they
/// are declared where the stackup does not say.
pub fn copper_order(imported: &ImportedDesign) -> Vec<Symbol> {
    let mut copper = imported
        .layer_definitions
        .iter()
        .filter(|layer| is_copper(layer.layer_function))
        .map(|layer| layer.name)
        .collect::<Vec<_>>();
    // Where the stackup does not place a layer its side does: the top
    // first and the bottom last.
    let side = |name: Symbol| {
        let mut layers = imported.layer_definitions.iter();
        match layers
            .find(|layer| layer.name == name)
            .map(|layer| ir_side(layer.side))
        {
            Some(Side::Top) => 0,
            Some(Side::Bottom) => 2,
            _ => 1,
        }
    };
    copper.sort_by_key(|name| side(*name));
    let Some(stackup) = imported.stackups.first() else {
        return copper;
    };
    // A sequence orders the stack only when every layer has its own.
    let mut stack = stackup.layers.iter().collect::<Vec<_>>();
    let numbers = stack
        .iter()
        .filter_map(|layer| layer.layer_number)
        .collect::<HashSet<_>>();
    if numbers.len() == stack.len() {
        stack.sort_by_key(|layer| layer.layer_number);
    }
    let place = |name: Symbol| {
        let name = imported.resolve(name);
        stack
            .iter()
            .position(|layer| imported.resolve(layer.layer_ref) == name)
    };
    // A stackup that leaves a copper layer out orders none of them.
    if copper.iter().all(|name| place(*name).is_some()) {
        copper.sort_by_key(|name| place(*name));
    }
    copper
}

/// What a hole is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HoleKind {
    Via,
    Plated,
    NonPlated,
}

/// Where a tool cuts.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Hole(Point),
    /// A slot's centre line.
    Slot {
        start: Point,
        end: Point,
    },
    /// Any other routed opening, by its edge.
    Routed(Vec<ContourBuf>),
}

impl Hit {
    pub fn center(&self) -> Point {
        match self {
            Self::Hole(at) => *at,
            Self::Slot { start, end } => (*start + *end) * 0.5,
            Self::Routed(contours) => {
                let bounds = contours.iter().map(|contour| contour.bbox);
                bounds.fold(BBox::empty(), BBox::union).center()
            }
        }
    }
}

/// One row of a drill table: every hole of one size, kind and span.
#[derive(Debug, Clone)]
pub struct DrillTool {
    /// Finished diameter; a slot's width.
    pub diameter: f64,
    /// A slot's overall length.
    pub slot_length: Option<f64>,
    pub kind: HoleKind,
    /// First and last copper layer the hole connects, counted from one.
    pub layers: (usize, usize),
    /// How many copper layers the board has.
    pub layer_count: usize,
    pub hits: Vec<Hit>,
}

impl DrillTool {
    fn is_through(&self) -> bool {
        self.layers == (1, self.layer_count.max(1))
    }

    /// A via from an outer layer that stops inside the board.
    fn is_blind(&self) -> bool {
        let outer = self.layers.0 == 1 || self.layers.1 == self.layer_count;
        self.kind == HoleKind::Via && !self.is_through() && outer
    }

    /// A via between inner layers.
    fn is_buried(&self) -> bool {
        self.kind == HoleKind::Via && !self.is_through() && !self.is_blind()
    }

    /// What the hole is, as a drill table calls it.
    pub fn usage(&self) -> &'static str {
        match self.kind {
            HoleKind::NonPlated => "NPTH",
            HoleKind::Plated => "PTH",
            HoleKind::Via if self.is_blind() => "BLIND VIA",
            HoleKind::Via if self.is_buried() => "BURIED VIA",
            HoleKind::Via => "VIA",
        }
    }

    /// The layers the hole connects, where it does not go through the board.
    pub fn span(&self) -> Option<String> {
        (!self.is_through()).then(|| format!("L{}-L{}", self.layers.0, self.layers.1))
    }

    /// The hole's shape, where it is not round.
    pub fn shape(&self) -> Option<String> {
        let length = self.slot_length?;
        let routed = self.hits.iter().any(|hit| matches!(hit, Hit::Routed(_)));
        Some(if routed {
            "ROUTED PER DATA".to_string()
        } else {
            format!("SLOT {} × {}", mm_fine(self.diameter), mm_fine(length))
        })
    }
}

/// How many holes `tools` drill between them.
pub fn hole_count(tools: &[DrillTool]) -> usize {
    tools.iter().map(|tool| tool.hits.len()).sum()
}

/// The copper layers a feature's span connects, counted from one. An end
/// the span leaves unnamed is the outer layer on its side, and a span that
/// names neither goes through.
pub fn span_layers(
    imported: &ImportedDesign,
    copper: &[Symbol],
    span: FeatureSpan,
) -> (usize, usize) {
    let number = |layer: Symbol| {
        let name = imported.resolve(layer);
        let mut copper = copper.iter();
        let place = copper.position(|copper| imported.resolve(*copper) == name);
        place.map(|index| index + 1)
    };
    let last = copper.len().max(1);
    let (from, to) = match span {
        FeatureSpan::ThroughBoard | FeatureSpan::Unknown => (None, None),
        FeatureSpan::Layer(layer) => (number(layer), number(layer)),
        FeatureSpan::FromTo { from, to } => (from.and_then(number), to.and_then(number)),
    };
    let (from, to) = (from.unwrap_or(1), to.unwrap_or(last));
    (from.min(to), from.max(to))
}

/// The drill table of what `scope` materializes, smallest tool first.
///
/// Every drilled and routed opening has a row: a drawing that left one out
/// would misstate the board. One that is neither round nor a straight slot
/// is listed by its extents and drawn by its edge.
pub fn drill_tools(imported: &ImportedDesign, scope: ArtworkScope) -> Result<Vec<DrillTool>> {
    let copper = copper_order(imported);
    let layer_count = copper.len().max(1);
    // Two holes that differ by less than a micrometre are drilled with one
    // tool.
    let mut tools = BTreeMap::new();
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        if !matches!(
            layer.layer_function,
            LayerFunction::Drill | LayerFunction::Rout
        ) {
            continue;
        }
        let name = imported.resolve(layer.name);
        let doc = imported
            .materialize_layer(LayerId(index as u32), scope)
            .with_context(|| format!("failed to extract IPC-2581 drill layer '{name}'"))?;
        let features = doc.layers.iter();
        for feature in features.flat_map(|layer| layer.features.slice(&doc.features)) {
            let Some((diameter, slot_length, hit)) = opening(&doc, feature) else {
                continue;
            };
            let kind = match feature.intent.plating {
                PlatingKind::Via | PlatingKind::ViaCapped => HoleKind::Via,
                PlatingKind::Plated => HoleKind::Plated,
                PlatingKind::NonPlated | PlatingKind::None | PlatingKind::Unknown => {
                    HoleKind::NonPlated
                }
            };
            let layers = span_layers(imported, &copper, feature.intent.span);
            let key = (
                micrometres(diameter),
                slot_length.map(micrometres),
                kind,
                layers,
                matches!(hit, Hit::Routed(_)),
            );
            tools
                .entry(key)
                .or_insert_with(|| DrillTool {
                    diameter,
                    slot_length,
                    kind,
                    layers,
                    layer_count,
                    hits: Vec::new(),
                })
                .hits
                .push(hit);
        }
    }
    Ok(tools.into_values().collect())
}

/// A drilled or routed feature as an opening: its width, its length where
/// it is not round, and where it is cut.
fn opening(doc: &GeometryDocument, feature: &Feature) -> Option<(f64, Option<f64>, Hit)> {
    match (feature.kind, feature.shape) {
        (FeatureKind::Hole, Some(SimpleShape::Circle { diameter })) if diameter > 0.0 => {
            Some((diameter, None, Hit::Hole(feature.center)))
        }
        (FeatureKind::Slot, Some(SimpleShape::Oval { .. })) => {
            let (diameter, start, end) = nc_linear_slot(feature)?;
            let length = start.distance_to(end);
            // An oval as wide as it is long, to what a table letters, is
            // one plunge.
            Some(if length >= 0.0005 {
                (diameter, Some(length + diameter), Hit::Slot { start, end })
            } else {
                (diameter, None, Hit::Hole(start))
            })
        }
        (FeatureKind::Hole | FeatureKind::Slot, _) => {
            let contours = doc.placed_feature_contours(feature);
            let bounds = contours.iter().map(|contour| contour.bbox);
            let bounds = bounds.fold(BBox::empty(), BBox::union);
            (!bounds.is_empty()).then(|| {
                let (short, long) = (
                    bounds.width().min(bounds.height()),
                    bounds.width().max(bounds.height()),
                );
                (short, Some(long), Hit::Routed(contours))
            })
        }
        _ => None,
    }
}

/// A layer the fabricator images, and what the drawing calls it.
#[derive(Debug, Clone)]
pub struct FabLayer {
    pub id: LayerId,
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    /// "L2" for the second copper layer.
    pub number: Option<usize>,
}

impl FabLayer {
    /// What the layer is, in the words of a drawing.
    pub fn description(&self) -> String {
        let side = match self.side {
            Side::Top => "TOP ",
            Side::Bottom => "BOTTOM ",
            Side::Inner => "INNER ",
            Side::None => "",
        };
        let what = match self.role {
            LayerRole::Copper => "COPPER",
            LayerRole::Soldermask => "SOLDER MASK",
            LayerRole::Legend => "LEGEND",
            _ => unreachable!("fab_layers lists no other layer"),
        };
        format!("{side}{what}")
    }
}

/// The copper, mask and legend layers in the order they are built up, top
/// to bottom. Paste is the assembler's, not the fabricator's.
pub fn fab_layers(imported: &ImportedDesign) -> Vec<FabLayer> {
    let copper = copper_order(imported);
    let mut layers = imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter_map(|(index, layer)| {
            let role = layer_role(layer.layer_function);
            let side = ir_side(layer.side);
            let number = copper
                .iter()
                .position(|copper| *copper == layer.name)
                .map(|index| index + 1);
            let rank = match (role, side) {
                (LayerRole::Legend, Side::Top) => (0, 0),
                (LayerRole::Soldermask, Side::Top) => (1, 0),
                (LayerRole::Copper, _) => (2, number?),
                (LayerRole::Soldermask, Side::Bottom) => (3, 0),
                (LayerRole::Legend, Side::Bottom) => (4, 0),
                _ => return None,
            };
            let layer = FabLayer {
                id: LayerId(index as u32),
                name: imported.resolve(layer.name).to_string(),
                role,
                side,
                number,
            };
            Some((rank, layer))
        })
        .collect::<Vec<_>>();
    layers.sort_by_key(|(rank, _)| *rank);
    layers.into_iter().map(|(_, layer)| layer).collect()
}

/// The physical rows of the stackup: everything with a thickness.
pub fn stack_rows(stackup: &StackupDetails) -> Vec<&StackupLayerInfo> {
    stackup
        .layers
        .iter()
        .filter(|layer| layer.layer_type != StackupLayerType::Other)
        .collect()
}

/// Finished thickness of one ounce per square foot of copper.
const COPPER_MM_PER_OZ: f64 = 0.0348;

/// A copper thickness as the foil weight it is sold by, where it is within
/// a tenth of one, and in micrometres where it is not: nobody stocks
/// 0.86 oz foil.
pub fn copper_weight(thickness_mm: f64) -> String {
    let oz = thickness_mm / COPPER_MM_PER_OZ;
    // Inner foil is commonly written as its 15 µm finished thickness.
    if (thickness_mm - 0.0152).abs() < 0.0005 {
        return "1/2 oz".to_string();
    }
    let stock = [
        (1.0 / 3.0, "1/3"),
        (0.5, "1/2"),
        (1.0, "1"),
        (1.5, "1.5"),
        (2.0, "2"),
        (3.0, "3"),
        (4.0, "4"),
    ];
    let stocked = stock
        .iter()
        .find(|(weight, _)| (oz - weight).abs() <= 0.1 * weight);
    match stocked {
        Some((_, name)) => format!("{name} oz"),
        None => format!("{:.0} µm", thickness_mm * 1000.0),
    }
}

/// An ink or a finish as a drawing shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ink {
    /// `0xRRGGBB`, where the design says enough to show one.
    pub color: Option<u32>,
    /// What the design calls it.
    pub name: String,
}

/// Inks as fabricators stock them, and the colour each cures to.
const INKS: [(&str, u32); 10] = [
    ("green", 0x1a6b3a),
    ("black", 0x1c1c1c),
    ("white", 0xf4f4f0),
    ("blue", 0x1c3f94),
    ("red", 0xa3231f),
    ("yellow", 0xd9b310),
    ("purple", 0x4b2a7b),
    ("matte black", 0x242424),
    ("matte green", 0x2a5d3c),
    ("orange", 0xd2691e),
];

/// What a design that names no colour is said to have.
const UNSTATED: &str = "NOT SPECIFIED";

impl Ink {
    /// The ink a board is built with where its design names none.
    fn stock(role: LayerRole) -> Self {
        let name = match role {
            LayerRole::Soldermask => "green",
            LayerRole::Legend => "white",
            _ => unreachable!("only a mask and a legend are inked"),
        };
        let stocked = INKS.iter().find(|(stocked, _)| *stocked == name);
        Self {
            color: stocked.map(|(_, color)| *color),
            name: name.to_uppercase(),
        }
    }

    /// The ink a stackup layer's colour states. An exporter writes a colour
    /// nobody chose as words to that effect, and one mixed on screen as a
    /// hex code.
    pub fn of(color: Option<&ColorInfo>) -> Self {
        let rgb = |(red, green, blue): (u8, u8, u8)| u32::from_be_bytes([0, red, green, blue]);
        let unstated = ["not specified", "unknown", "undefined", ""];
        let name = color
            .and_then(|color| color.name.as_deref())
            .map(|name| name.trim().to_string())
            .filter(|name| !unstated.contains(&name.to_lowercase().as_str()));
        let Some(name) = name else {
            let color = color.and_then(|color| color.rgb).map(rgb);
            let name = if color.is_some() {
                "PER DATA"
            } else {
                UNSTATED
            };
            return Self {
                color,
                name: name.to_string(),
            };
        };
        if let Some(hex) = name.strip_prefix('#') {
            // Six digits and an alpha the drawing has no use for, or the
            // short form with each digit doubled.
            let digits = hex.chars().filter(char::is_ascii_hexdigit).count();
            let hex = match (hex.len(), digits == hex.len()) {
                (6 | 8, true) => hex[..6].to_uppercase(),
                (3 | 4, true) => hex[..3].chars().flat_map(|digit| [digit, digit]).collect(),
                _ => return Self::of(None),
            };
            let color = u32::from_str_radix(&hex, 16).ok();
            // A value that is plainly black or white is that ink.
            let name = match color {
                Some(0x000000) => "BLACK".to_string(),
                Some(0xffffff) => "WHITE".to_string(),
                _ => format!("PER DATA #{hex}"),
            };
            return Self { color, name };
        }
        let stocked = INKS
            .iter()
            .find(|(stocked, _)| stocked.eq_ignore_ascii_case(&name))
            .map(|(_, color)| *color);
        Self {
            color: stocked.or_else(|| color.and_then(ColorInfo::rgb_color).map(rgb)),
            name: name.to_uppercase(),
        }
    }
}

/// The ink of the design's layer of `role` on `side`, where it has such a
/// layer. A board is masked in one ink and printed in one, so the side that
/// names it speaks for both; a design that names none gets what a
/// fabricator builds unasked: green mask and white legend.
pub fn layer_ink(source: &Source<'_>, role: LayerRole, side: Side) -> Option<Ink> {
    let of_role = |layer: &ipc2581::types::Layer| layer_role(layer.layer_function) == role;
    let on = |layer: &ipc2581::types::Layer, side: Side| ir_side(layer.side) == side;
    let layers = &source.imported.layer_definitions;
    if !layers.iter().any(|layer| of_role(layer) && on(layer, side)) {
        return None;
    }
    let inks = source.accessor.stackup_inks();
    let stated = [Side::Top, Side::Bottom]
        .into_iter()
        .filter_map(|side| {
            let mut inks = inks.iter();
            inks.find(|(layer, _)| of_role(layer) && on(layer, side))
        })
        .map(|(_, ink)| Ink::of(Some(ink)))
        .find(|ink| ink.name != UNSTATED);
    Some(stated.unwrap_or_else(|| Ink::stock(role)))
}

/// A colour chip beside a specification's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chip {
    Color(u32),
    /// The board has none of it.
    Absent,
    /// The design does not say which.
    Unstated,
}

/// One row of the specification: what, shown in which colour where it has
/// one, and stated how.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecRow {
    pub label: &'static str,
    pub chip: Option<Chip>,
    pub value: String,
}

fn row(label: &'static str, value: impl Into<String>) -> SpecRow {
    SpecRow {
        label,
        chip: None,
        value: value.into(),
    }
}

/// A mask or a legend: its one ink as a chip, and which sides carry it.
fn sides_row(label: &'static str, top: Option<Ink>, bottom: Option<Ink>) -> SpecRow {
    let sides = match (&top, &bottom) {
        (Some(_), Some(_)) => "BOTH SIDES",
        (Some(_), None) => "TOP ONLY",
        (None, Some(_)) => "BOTTOM ONLY",
        (None, None) => "",
    };
    let Some(ink) = top.or(bottom) else {
        return SpecRow {
            label,
            chip: Some(Chip::Absent),
            value: "NONE".to_string(),
        };
    };
    SpecRow {
        label,
        chip: Some(ink.color.map_or(Chip::Unstated, Chip::Color)),
        value: format!("{} · {sides}", ink.name),
    }
}

/// The specification: what a fabricator quotes and builds the board to,
/// each row from the design.
pub fn specification(
    source: &Source<'_>,
    layers: &[(FabLayer, bool)],
    tools: &[DrillTool],
    board: BBox,
    array: Option<&ArrayData>,
) -> Vec<SpecRow> {
    let mut rows = Vec::new();
    if let Some(array) = array {
        // What is delivered, before what each board of it is.
        let (boards, size) = (array.boards.len(), size_mm(array.bounds));
        rows.push(row("DELIVERY", format!("ARRAY OF {boards} · {size}")));
    }
    rows.push(row("BOARD SIZE", size_mm(board)));

    let stackup = source.stackup.as_ref();
    let copper = stackup
        .map(stack_rows)
        .unwrap_or_default()
        .into_iter()
        .filter(|layer| layer.layer_type == StackupLayerType::Conductor)
        .collect::<Vec<_>>();
    // A conductor the stackup gives no thickness and the data no artwork
    // is a layer the board does not have.
    let weighed = |layer: &FabLayer| {
        let mut copper = copper.iter();
        let row = copper.find(|row| row.name == layer.name);
        row.is_some_and(|row| row.thickness_mm.is_some_and(|thickness| thickness > 0.0))
    };
    let copper_layers = layers
        .iter()
        .filter(|(layer, has_artwork)| {
            layer.role == LayerRole::Copper && (*has_artwork || weighed(layer))
        })
        .count();
    rows.push(row("COPPER LAYERS", copper_layers.to_string()));

    let thickness = source.thickness();
    if let Some(thickness) = thickness {
        let declared = source
            .ipc
            .ecad()
            .and_then(|ecad| ecad.cad_data.stackups.first());
        // A tolerance is stated only where the design states one.
        let tolerance = declared.and_then(|stackup| {
            let (plus, minus) = (stackup.tol_plus?, stackup.tol_minus?);
            (plus > 0.0 || minus > 0.0).then(|| {
                let unit = if stackup.tol_percent { " %" } else { "" };
                let letter = |value: f64| {
                    if stackup.tol_percent {
                        value.to_string()
                    } else {
                        mm_fine(value)
                    }
                };
                if (plus - minus).abs() < 1e-9 {
                    format!(" ±{}{unit}", letter(plus))
                } else {
                    format!(" +{}{unit} / -{}{unit}", letter(plus), letter(minus))
                }
            })
        });
        let over = match declared.and_then(|stackup| stackup.where_measured) {
            Some(WhereMeasured::Mask) => " OVER MASK",
            Some(WhereMeasured::Metal) => " OVER COPPER",
            Some(WhereMeasured::Laminate) => " OVER LAMINATE",
            Some(WhereMeasured::Other) | None => "",
        };
        rows.push(row(
            "THICKNESS",
            format!(
                "{} mm{}{over}",
                mm_fine(thickness),
                tolerance.unwrap_or_default()
            ),
        ));
    }

    // The outer layers' copper, then the inner layers' where there are any;
    // each as one weight where the layers agree.
    let weights = |layers: &[&StackupLayerInfo]| {
        let mut weights = layers
            .iter()
            .filter_map(|layer| layer.thickness_mm.filter(|thickness| *thickness > 0.0))
            .map(copper_weight)
            .collect::<Vec<_>>();
        weights.dedup();
        match weights.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            _ => Some("PER LAYER STACK".to_string()),
        }
    };
    if let [top, inner @ .., bottom] = copper.as_slice() {
        match (weights(&[*top, *bottom]), weights(inner)) {
            (Some(outer), Some(inner)) => {
                rows.push(row("COPPER", format!("OUTER {outer} · INNER {inner}")));
            }
            (Some(outer), None) => rows.push(row("COPPER", outer)),
            _ => {}
        }
    } else if let Some(only) = weights(&copper) {
        rows.push(row("COPPER", only));
    }

    let finish = stackup.and_then(|stackup| stackup.surface_finish.as_ref());
    rows.push(match finish {
        Some(finish) => {
            let (red, green, blue) = finish.rgb_color();
            SpecRow {
                label: "SURFACE FINISH",
                chip: Some(Chip::Color(u32::from_be_bytes([0, red, green, blue]))),
                value: finish.name.to_uppercase(),
            }
        }
        None => SpecRow {
            label: "SURFACE FINISH",
            chip: Some(Chip::Unstated),
            value: UNSTATED.to_string(),
        },
    });

    // A mask layer that opens nothing still masks its whole side; a legend
    // layer with nothing on it prints nothing.
    let mask = |side| layer_ink(source, LayerRole::Soldermask, side);
    rows.push(sides_row(
        "SOLDER MASK",
        mask(Side::Top),
        mask(Side::Bottom),
    ));
    let legend = |side| {
        let printed = layers.iter().any(|(layer, has_artwork)| {
            layer.role == LayerRole::Legend && layer.side == side && *has_artwork
        });
        layer_ink(source, LayerRole::Legend, side).filter(|_| printed)
    };
    rows.push(sides_row("LEGEND", legend(Side::Top), legend(Side::Bottom)));

    let holes = match tools.first() {
        None => "NONE".to_string(),
        Some(smallest) => {
            let size = |tool: &DrillTool| micrometres(tool.diameter);
            let sizes = tools.iter().map(size).collect::<HashSet<_>>().len();
            format!(
                "{} · {sizes} SIZE{} · MIN DIA {}",
                hole_count(tools),
                if sizes == 1 { "" } else { "S" },
                mm_fine(smallest.diameter)
            )
        }
    };
    rows.push(row("HOLES", holes));
    rows
}

/// A fiducial an array carries for the assembler, on one side of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fiducial {
    pub at: Point,
    pub diameter: f64,
    pub side: Side,
    /// The board it stands beside, by its place in [`ArrayData::boards`]:
    /// one set in a board's cell repeats with the board. One with none is
    /// the array's own, on its border.
    pub board: Option<usize>,
}

/// What an array adds around its boards.
pub struct ArrayData {
    pub grid: Option<BoardArrayGridInfo>,
    pub bounds: BBox,
    /// Bounds of every placed board.
    pub boards: Vec<BBox>,
    pub outlines: Vec<Vec<ContourBuf>>,
    /// Material routed out of the array, V-score reliefs among it.
    pub removal: Vec<ContourBuf>,
    pub scores: Vec<VScoreLine>,
    pub fiducials: Vec<Fiducial>,
    /// The array's own holes: tooling and tab perforations.
    pub tools: Vec<DrillTool>,
}

/// One breakaway tab of an array, to draw large enough to read.
#[derive(Debug, Clone, Copy)]
pub struct TabDetail {
    /// The tab's perforations and the routed slots either side of them.
    pub bounds: BBox,
    /// The tool that drills the perforations, by its place among the
    /// array's.
    pub tool: usize,
    /// How many perforations the tab has.
    pub holes: usize,
    pub diameter: f64,
    /// Centre to centre of neighbouring perforations.
    pub pitch: f64,
}

impl ArrayData {
    /// The array a design's primary step lays out, if it lays one out.
    pub fn of(source: &Source<'_>) -> Result<Option<Self>> {
        let imported = source.imported;
        let geometry = &imported.geometry;
        if pcb_ir::dialects::ipc::root_panel_step(geometry).is_none() {
            return Ok(None);
        }
        let occurrences = profile_occurrences_for(geometry, ProfileSet::FabricationOutlines);
        let of_role = |role: ProfileOccurrenceRole| {
            let occurrences = occurrences.iter();
            occurrences.filter(move |occurrence| occurrence.role == role)
        };
        let bbox = |occurrence: &ProfileOccurrence<'_>| {
            geometry.transformed_path_bbox(occurrence.profile.outer_path, occurrence.transform)
        };
        let bounds = of_role(ProfileOccurrenceRole::RootPanel)
            .map(bbox)
            .fold(BBox::empty(), BBox::union);
        if bounds.is_empty() {
            return Ok(None);
        }
        // The step each board is placed in is its cell: what else that
        // step carries is the board's.
        let cells = of_role(ProfileOccurrenceRole::BoardInstance)
            .enumerate()
            .filter_map(|(board, occurrence)| {
                let instance = geometry
                    .layout
                    .instances
                    .get(occurrence.instance? as usize)?;
                Some((instance.parent_instance?, board))
            })
            .collect::<HashMap<_, _>>();
        let fiducials = fiducials(imported, &cells)?;
        let scores = crate::geometry::board_array_vscore_lines(imported)?;
        let profile = crate::geometry::board_array_fabrication_profile(
            imported,
            geometry,
            &scores,
            source.resolution,
        )?;
        Ok(Some(Self {
            grid: source
                .accessor
                .board_layout_info()
                .and_then(|layout| layout.board_array?.grid),
            bounds,
            boards: of_role(ProfileOccurrenceRole::BoardInstance)
                .map(bbox)
                .collect(),
            outlines: profile.array_outlines,
            removal: profile.material_removal,
            scores,
            fiducials,
            tools: drill_tools(imported, ArtworkScope::ArraySupport)?,
        }))
    }

    /// How the array's boards come apart.
    pub fn separation(&self) -> &'static str {
        match (self.scores.is_empty(), self.removal.is_empty()) {
            (false, true) => "V-SCORE",
            (false, false) => "V-SCORE, ROUTED RELIEFS",
            (true, false) if self.tab().is_some() => "ROUTED, PERFORATED TABS",
            (true, false) => "ROUTED, TABS",
            (true, true) => "NONE",
        }
    }

    /// The array's tooling holes: every hole of its own but the
    /// perforations of its tabs, each with its tool, left to right.
    pub fn tooling(&self) -> Vec<(Point, &DrillTool)> {
        let perforations = self.tab().map(|tab| tab.tool);
        let mut holes = self
            .tools
            .iter()
            .enumerate()
            .filter(|(tool, _)| perforations != Some(*tool))
            .flat_map(|(_, tool)| tool.hits.iter().map(move |hit| (hit.center(), tool)))
            .collect::<Vec<_>>();
        holes.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.0.y.total_cmp(&b.0.y)));
        holes
    }

    /// The board nearest the array's middle, the one a drawing shows the
    /// fiducials of: by its place in [`Self::boards`].
    pub fn central_board(&self) -> Option<usize> {
        let middle = self.bounds.center();
        let reach = |board: usize| self.boards[board].center().distance_to(middle);
        (0..self.boards.len()).min_by(|a, b| reach(*a).total_cmp(&reach(*b)))
    }

    /// The fiducials on `side` that each board has in its cell, placed
    /// from the lower-left corner of the board's extents: those of the
    /// central board where every board has the same, and nothing where
    /// the boards differ.
    pub fn board_fiducials(&self, side: Side) -> Option<Vec<Fiducial>> {
        /// How far apart the same fiducial of two boards may measure.
        const SAME: f64 = 0.002;
        let of_board = |board: usize| {
            let fiducials = self.fiducials.iter();
            fiducials
                .filter(move |fiducial| fiducial.board == Some(board) && fiducial.side == side)
                .map(move |fiducial| Fiducial {
                    at: fiducial.at - self.boards[board].min,
                    ..*fiducial
                })
        };
        let shown = of_board(self.central_board()?).collect::<Vec<_>>();
        let alike = (0..self.boards.len()).all(|board| {
            let own = of_board(board).collect::<Vec<_>>();
            own.len() == shown.len()
                && own.iter().all(|fiducial| {
                    let mut shown = shown.iter();
                    shown.any(|shown| shown.at.distance_to(fiducial.at) < SAME)
                })
        });
        alike.then_some(shown)
    }

    /// The perforated tab nearest the array's datum, where boards are held
    /// by tabs. Perforations are the array's smallest unplated holes, in
    /// rows a few diameters apart; holes that stand further apart are
    /// tooling, not a tab.
    pub fn tab(&self) -> Option<TabDetail> {
        /// The widest a row of perforations is pitched, in hole diameters.
        const WIDEST_PITCH: f64 = 4.0;
        if !self.scores.is_empty() || self.removal.is_empty() {
            return None;
        }
        let (index, tool) = self
            .tools
            .iter()
            .enumerate()
            .filter(|(_, tool)| tool.kind == HoleKind::NonPlated && tool.slot_length.is_none())
            .filter(|(_, tool)| tool.hits.len() > 2)
            .min_by(|a, b| a.1.diameter.total_cmp(&b.1.diameter))?;
        let holes = tool.hits.iter().map(Hit::center).collect::<Vec<_>>();
        let nearest = |from: Point| {
            let others = holes.iter().map(|hole| hole.distance_to(from));
            others
                .filter(|distance| *distance > 0.0)
                .min_by(f64::total_cmp)
        };
        let datum = self.bounds.min;
        let first = *holes
            .iter()
            .min_by(|a, b| a.distance_to(datum).total_cmp(&b.distance_to(datum)))?;
        let pitch = nearest(first)?;
        if pitch > WIDEST_PITCH * tool.diameter {
            return None;
        }
        // Grow the tab from its first hole: every hole within reach of one
        // already in it.
        let mut tab = vec![first];
        let mut grown = 0;
        while grown < tab.len() {
            let from = tab[grown];
            grown += 1;
            for hole in &holes {
                let near = hole.distance_to(from) <= 1.5 * pitch;
                if near && !tab.iter().any(|member| member.distance_to(*hole) == 0.0) {
                    tab.push(*hole);
                }
            }
        }
        let bounds = tab.iter().fold(BBox::empty(), |bounds, hole| {
            bounds.union(BBox::from_point(*hole))
        });
        // A square window that shows the tab whichever way it runs, with
        // the slots it bridges on either side.
        let half = bounds.width().max(bounds.height()) / 2.0 + 3.0;
        Some(TabDetail {
            bounds: BBox::from_point(bounds.center()).expand(half),
            tool: index,
            holes: tab.len(),
            diameter: tool.diameter,
            pitch,
        })
    }
}

/// The fiducials the array's own steps carry on their outer copper, each
/// with the board whose cell it is in. `cells` maps a cell's step instance
/// to its board.
fn fiducials(imported: &ImportedDesign, cells: &HashMap<u32, usize>) -> Result<Vec<Fiducial>> {
    let mut fiducials = Vec::new();
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        let side = ir_side(layer.side);
        if !is_copper(layer.layer_function) || !matches!(side, Side::Top | Side::Bottom) {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::ArraySupport)?;
        let is_fiducial = |feature: &&Feature| feature.fiducial_kind != FiducialKind::Unknown;
        fiducials.extend(
            doc.features
                .iter()
                .filter(is_fiducial)
                .filter_map(|feature| {
                    let Some(SimpleShape::Circle { diameter }) = feature.shape else {
                        return None;
                    };
                    Some(Fiducial {
                        at: feature.bbox.center(),
                        diameter,
                        side,
                        board: feature
                            .source_instance
                            .and_then(|cell| cells.get(&cell).copied()),
                    })
                }),
        );
    }
    // Top before bottom, then left to right: the order they are tagged in.
    fiducials.sort_by(|a, b| {
        let key =
            |fiducial: &Fiducial| (fiducial.side == Side::Bottom, fiducial.at.x, fiducial.at.y);
        let (a, b) = (key(a), key(b));
        a.0.cmp(&b.0)
            .then(a.1.total_cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
    });
    Ok(fiducials)
}

/// The design's name: the board step every array of it repeats.
pub fn design_name(source: &Source<'_>) -> String {
    source
        .accessor
        .board_step()
        .map(|step| source.ipc.resolve(step.name).to_string())
        .unwrap_or_else(|| "UNNAMED".to_string())
}

/// The revision the design's BOM is released at, when it states one.
pub fn design_revision(source: &Source<'_>) -> Option<String> {
    let header = source.ipc.bom()?.header.as_ref()?;
    let revision = source.ipc.resolve(header.revision).trim();
    // An unexpanded build variable is not a revision.
    (!revision.is_empty() && !revision.contains("${")).then(|| revision.to_string())
}

/// The day the data was last changed, as the source wrote it.
pub fn design_date(source: &Source<'_>) -> Option<String> {
    let history = source.ipc.history_record()?;
    let changed = source.ipc.resolve(history.last_change);
    let day = changed.split('T').next()?;
    (!day.is_empty()).then(|| day.to_string())
}

/// The drawing's notes: the few a fabricator is held to that the
/// specification and the tables do not already state.
pub fn notes(source: &Source<'_>, array: Option<&ArrayData>) -> Vec<String> {
    // A board built on polyimide is a flexible one, bought to its own
    // standard.
    let materials = source.accessor.material_info();
    let flexible = materials.is_some_and(|materials| {
        let mut names = materials.dielectric.iter();
        names.any(|name| {
            let name = name.to_lowercase();
            name.contains("polyimide") || name.contains("kapton")
        })
    });
    let standard = if flexible { "IPC-6013" } else { "IPC-6012" };
    let mut notes = vec![
        format!("FABRICATE AND INSPECT TO {standard} / IPC-A-600 CLASS {CLASS}."),
        "DESIGN DATA GOVERNS. DIMENSIONS ARE mm, FOR REFERENCE.".to_string(),
        "100 % ELECTRICAL TEST AGAINST THE NETLIST.".to_string(),
        "DO NOT ADD OR REMOVE COPPER WITHOUT APPROVAL.".to_string(),
    ];
    if let Some(array) = array {
        notes.push(if array.scores.is_empty() {
            "DELIVER AS ARRAY PER THE ARRAY SHEET. DO NOT MOVE OR ADD TABS.".to_string()
        } else {
            "DELIVER AS ARRAY PER THE ARRAY SHEET. DO NOT MOVE SCORES.".to_string()
        });
    }
    notes
}
