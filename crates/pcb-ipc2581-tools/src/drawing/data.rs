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

pub struct Source<'a> {
    ipc: &'a Ipc2581,
    pub imported: &'a ImportedDesign,
    accessor: IpcAccessor<'a>,
    pub resolution: Resolution,
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

    pub fn thickness(&self) -> Option<f64> {
        self.stackup.as_ref()?.overall_thickness_mm
    }
}

// What the drawing requires where the design data states nothing.

/// IPC-6012 performance class.
const CLASS: u8 = 2;
/// Included angle of a V-score, in degrees.
pub const SCORE_ANGLE: f64 = 30.0;
/// The web a V-score leaves, as a share of thickness, and the thinnest, mm.
const SCORE_WEB: f64 = 1.0 / 3.0;
const THINNEST_WEB: f64 = 0.25;

pub fn score_web(thickness: f64) -> Option<f64> {
    let web = thickness * SCORE_WEB;
    (web >= THINNEST_WEB).then_some(web)
}

/// Sizes compare by the micrometre: two that differ by less are one size.
pub fn micrometres(length: f64) -> i64 {
    (length * 1000.0).round() as i64
}

pub fn mm(value: f64) -> String {
    // To the micrometre first, so equal lengths letter alike; never "-0.00".
    let micrometres = (value * 1000.0).round();
    format!("{:.2}", (micrometres / 10.0).round() / 100.0 + 0.0)
}

/// To the micrometre, lettered to two places where that says it all.
pub fn mm_fine(value: f64) -> String {
    let text = format!("{:.3}", (value * 1000.0).round() / 1000.0 + 0.0);
    text.strip_suffix('0').map_or(text.clone(), str::to_string)
}

pub fn size_mm(bounds: BBox) -> String {
    format!("{} × {} mm", mm(bounds.width()), mm(bounds.height()))
}

/// Copper layers top to bottom: as the stackup orders them, else as declared.
pub fn copper_order(imported: &ImportedDesign) -> Vec<Symbol> {
    let mut copper = imported
        .layer_definitions
        .iter()
        .filter(|layer| is_copper(layer.layer_function))
        .map(|layer| layer.name)
        .collect::<Vec<_>>();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HoleKind {
    Via,
    Plated,
    NonPlated,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    Hole(Point),
    /// A slot's centre line.
    Slot {
        start: Point,
        end: Point,
    },
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
    pub layer_count: usize,
    pub hits: Vec<Hit>,
}

impl DrillTool {
    fn is_through(&self) -> bool {
        self.layers == (1, self.layer_count.max(1))
    }

    fn is_blind(&self) -> bool {
        let outer = self.layers.0 == 1 || self.layers.1 == self.layer_count;
        self.kind == HoleKind::Via && !self.is_through() && outer
    }

    fn is_buried(&self) -> bool {
        self.kind == HoleKind::Via && !self.is_through() && !self.is_blind()
    }

    pub fn usage(&self) -> &'static str {
        match self.kind {
            HoleKind::NonPlated => "NPTH",
            HoleKind::Plated => "PTH",
            HoleKind::Via if self.is_blind() => "BLIND VIA",
            HoleKind::Via if self.is_buried() => "BURIED VIA",
            HoleKind::Via => "VIA",
        }
    }

    pub fn span(&self) -> Option<String> {
        (!self.is_through()).then(|| format!("L{}-L{}", self.layers.0, self.layers.1))
    }

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

pub fn hole_count(tools: &[DrillTool]) -> usize {
    tools.iter().map(|tool| tool.hits.len()).sum()
}

/// Counted from one. An end the span leaves unnamed is its side's outer layer.
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
pub fn drill_tools(imported: &ImportedDesign, scope: ArtworkScope) -> Result<Vec<DrillTool>> {
    let copper = copper_order(imported);
    let layer_count = copper.len().max(1);
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

/// A feature's width, its length where it is not round, and where it is cut.
fn opening(doc: &GeometryDocument, feature: &Feature) -> Option<(f64, Option<f64>, Hit)> {
    match (feature.kind, feature.shape) {
        (FeatureKind::Hole, Some(SimpleShape::Circle { diameter })) if diameter > 0.0 => {
            Some((diameter, None, Hit::Hole(feature.center)))
        }
        (FeatureKind::Slot, Some(SimpleShape::Oval { .. })) => {
            let (diameter, start, end) = nc_linear_slot(feature)?;
            let length = start.distance_to(end);
            // An oval as wide as it is long, as lettered, is one plunge.
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

/// The copper, mask and legend layers as they are built up, top to bottom.
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

pub fn stack_rows(stackup: &StackupDetails) -> Vec<&StackupLayerInfo> {
    stackup
        .layers
        .iter()
        .filter(|layer| layer.layer_type != StackupLayerType::Other)
        .collect()
}

/// Finished thickness of one ounce per square foot of copper.
const COPPER_MM_PER_OZ: f64 = 0.0348;

/// The foil weight where the thickness is within a tenth of one, else in µm.
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

#[derive(Debug, Clone, PartialEq)]
pub struct Ink {
    /// `0xRRGGBB`, where the design says enough to show one.
    pub color: Option<u32>,
    pub name: String,
}

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

const UNSTATED: &str = "NOT SPECIFIED";

impl Ink {
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

    /// Exporters write an unchosen colour as words, a custom one as a hex code.
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
            // Six digits and an optional alpha, or the short form doubled.
            let digits = hex.chars().filter(char::is_ascii_hexdigit).count();
            let hex = match (hex.len(), digits == hex.len()) {
                (6 | 8, true) => hex[..6].to_uppercase(),
                (3 | 4, true) => hex[..3].chars().flat_map(|digit| [digit, digit]).collect(),
                _ => return Self::of(None),
            };
            let color = u32::from_str_radix(&hex, 16).ok();
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

/// A board has one mask ink and one legend ink: either side names it for both.
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chip {
    Color(u32),
    Absent,
    Unstated,
}

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

pub fn specification(
    source: &Source<'_>,
    layers: &[(FabLayer, bool)],
    tools: &[DrillTool],
    board: BBox,
    array: Option<&ArrayData>,
) -> Vec<SpecRow> {
    let mut rows = Vec::new();
    if let Some(array) = array {
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
    // A conductor with no thickness and no artwork is not a layer of the board.
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

    // Outer copper, then inner; each as one weight where the layers agree.
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

    // An empty mask layer still masks its side; an empty legend prints nothing.
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fiducial {
    pub at: Point,
    pub diameter: f64,
    pub side: Side,
    /// The board whose cell it is in, by place in [`ArrayData::boards`].
    pub board: Option<usize>,
}

pub struct ArrayData {
    pub grid: Option<BoardArrayGridInfo>,
    pub bounds: BBox,
    pub boards: Vec<BBox>,
    pub outlines: Vec<Vec<ContourBuf>>,
    /// Material routed out of the array, V-score reliefs among it.
    pub removal: Vec<ContourBuf>,
    pub scores: Vec<VScoreLine>,
    pub fiducials: Vec<Fiducial>,
    /// The array's own holes: tooling and tab perforations.
    pub tools: Vec<DrillTool>,
    /// The tooling holes, each with its tool's place in `tools`, left to
    /// right.
    pub tooling: Vec<(Point, usize)>,
    /// The perforated tab nearest the datum, of a routed array.
    pub tab: Option<TabDetail>,
}

#[derive(Debug, Clone, Copy)]
pub struct TabDetail {
    /// The tab's perforations and the routed slots either side of them.
    pub bounds: BBox,
    pub holes: usize,
    pub diameter: f64,
    /// Centre to centre of neighbouring perforations.
    pub pitch: f64,
}

impl ArrayData {
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
        anyhow::ensure!(
            !bounds.is_empty(),
            "IPC-2581 array has no profile to draw; draw its board with --layout-target board"
        );
        // What else the step a board is placed in carries is that board's.
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
        let tools = drill_tools(imported, ArtworkScope::ArraySupport)?;
        let (rows, apart): (Vec<_>, Vec<_>) = tools.iter().map(perforations).unzip();
        let mut tooling = apart
            .into_iter()
            .enumerate()
            .flat_map(|(tool, holes)| holes.into_iter().map(move |at| (at, tool)))
            .collect::<Vec<_>>();
        tooling.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then(a.0.y.total_cmp(&b.0.y)));
        let routed = scores.is_empty() && !profile.material_removal.is_empty();
        let tab = tools
            .iter()
            .zip(rows)
            .filter(|_| routed)
            .filter_map(|(tool, rows)| tab(tool, rows, bounds.min))
            .min_by(|a, b| a.diameter.total_cmp(&b.diameter));
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
            tools,
            tooling,
            tab,
        }))
    }

    pub fn separation(&self) -> &'static str {
        match (self.scores.is_empty(), self.removal.is_empty()) {
            (false, true) => "V-SCORE",
            (false, false) => "V-SCORE, ROUTED RELIEFS",
            (true, false) if self.tab.is_some() => "ROUTED, PERFORATED TABS",
            (true, false) => "ROUTED, TABS",
            (true, true) => "NONE",
        }
    }

    pub fn central_board(&self) -> Option<usize> {
        let middle = self.bounds.center();
        let reach = |board: usize| self.boards[board].center().distance_to(middle);
        (0..self.boards.len()).min_by(|a, b| reach(*a).total_cmp(&reach(*b)))
    }

    /// From the board's lower-left corner; nothing where the boards differ.
    pub fn board_fiducials(&self, side: Side) -> Vec<Fiducial> {
        let Some(board) = self.central_board() else {
            return Vec::new();
        };
        let fiducials = self.fiducials.iter();
        fiducials
            .filter(|fiducial| fiducial.board == Some(board) && fiducial.side == side)
            .map(|fiducial| Fiducial {
                at: fiducial.at - self.boards[board].min,
                ..*fiducial
            })
            .collect()
    }
}

/// The row of `rows` nearest `datum`, as the tab a detail shows.
fn tab(tool: &DrillTool, rows: Vec<Vec<Point>>, datum: Point) -> Option<TabDetail> {
    let from_datum = |row: &Vec<Point>| {
        let distances = row.iter().map(|hole| hole.distance_to(datum));
        distances.fold(f64::INFINITY, f64::min)
    };
    let row = rows
        .into_iter()
        .min_by(|a, b| from_datum(a).total_cmp(&from_datum(b)))?;
    let pairs = row.iter().enumerate();
    let pitch = pairs
        .flat_map(|(index, hole)| row[index + 1..].iter().map(|next| hole.distance_to(*next)))
        .fold(f64::INFINITY, f64::min);
    let bounds = row.iter().fold(BBox::empty(), |bounds, hole| {
        bounds.union(BBox::from_point(*hole))
    });
    let half = bounds.width().max(bounds.height()) / 2.0 + 3.0;
    Some(TabDetail {
        bounds: BBox::from_point(bounds.center()).expand(half),
        holes: row.len(),
        diameter: tool.diameter,
        pitch,
    })
}

/// The holes of `tool` as rows of perforations and the rest, which are
/// tooling: a row is three or more unplated round holes, each within a few
/// diameters of the next.
pub fn perforations(tool: &DrillTool) -> (Vec<Vec<Point>>, Vec<Point>) {
    /// The widest a row of perforations is pitched, in hole diameters.
    const WIDEST_PITCH: f64 = 4.0;
    let mut loose = tool.hits.iter().map(Hit::center).collect::<Vec<_>>();
    if tool.kind != HoleKind::NonPlated || tool.slot_length.is_some() {
        return (Vec::new(), loose);
    }
    let reach = WIDEST_PITCH * tool.diameter;
    loose.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut groups = Vec::new();
    while let Some(first) = loose.pop() {
        let mut group = vec![first];
        let mut grown = 0;
        while grown < group.len() {
            let from = group[grown];
            grown += 1;
            // Sorted by X, so only a window of the rest can be in reach.
            let within = loose.partition_point(|hole| hole.x < from.x - reach)
                ..loose.partition_point(|hole| hole.x <= from.x + reach);
            group.extend(loose.extract_if(within, |hole| hole.distance_to(from) <= reach));
        }
        groups.push(group);
    }
    let (rows, apart): (Vec<_>, Vec<_>) = groups.into_iter().partition(|group| group.len() >= 3);
    (rows, apart.into_iter().flatten().collect())
}

/// Fiducials on outer copper; `cells` maps a cell's step instance to its board.
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

pub fn design_name(source: &Source<'_>) -> String {
    source
        .accessor
        .board_step()
        .map(|step| source.ipc.resolve(step.name).to_string())
        .unwrap_or_else(|| "UNNAMED".to_string())
}

pub fn design_revision(source: &Source<'_>) -> Option<String> {
    let header = source.ipc.bom()?.header.as_ref()?;
    let revision = source.ipc.resolve(header.revision).trim();
    // An unexpanded build variable is not a revision.
    (!revision.is_empty() && !revision.contains("${")).then(|| revision.to_string())
}

pub fn design_date(source: &Source<'_>) -> Option<String> {
    let history = source.ipc.history_record()?;
    let changed = source.ipc.resolve(history.last_change);
    let day = changed.split('T').next()?;
    (!day.is_empty()).then(|| day.to_string())
}

pub fn notes(source: &Source<'_>, array: Option<&ArrayData>) -> Vec<String> {
    // A board built on polyimide is a flexible one, bought to its own standard.
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
