//! What a fabrication drawing states, read from the design.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use ipc2581::types::{LayerFunction, WhereMeasured};
use ipc2581::{Ipc2581, Symbol};
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
    pub ipc: &'a Ipc2581,
    pub imported: &'a ImportedDesign,
    pub accessor: IpcAccessor<'a>,
    pub resolution: Resolution,
}

/// What the drawing requires of a fabricator where the design data states
/// nothing: the ordinary terms a rigid board is bought to.
#[derive(Debug, Clone, Copy)]
pub struct Requirements {
    /// IPC-6012 performance class.
    pub class: u8,
    /// Included angle of a V-score, in degrees.
    pub score_angle: f64,
    /// Material a V-score leaves, as a share of the board's thickness, and
    /// how far it may be off, in millimetres.
    pub score_web: f64,
    pub score_tolerance: f64,
}

pub const REQUIREMENTS: Requirements = Requirements {
    class: 2,
    score_angle: 30.0,
    score_web: 1.0 / 3.0,
    score_tolerance: 0.1,
};

/// A length as a drawing letters it: millimetres to two places.
pub fn mm(value: f64) -> String {
    // Rounded before it is lettered, so nothing reads "-0.00".
    format!("{:.2}", (value * 100.0).round() / 100.0 + 0.0)
}

/// A length to the micrometre, lettered to two places where that says it
/// all: a tool's size must not round away what the data states.
pub fn mm_fine(value: f64) -> String {
    let text = format!("{:.3}", (value * 1000.0).round() / 1000.0 + 0.0);
    text.strip_suffix('0').map_or(text.clone(), str::to_string)
}

pub fn mil(value_mm: f64) -> String {
    format!("{:.1}", value_mm / 0.0254)
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
        let place = stack
            .iter()
            .position(|layer| imported.resolve(layer.layer_ref) == name);
        place.unwrap_or(usize::MAX)
    };
    copper.sort_by_key(|name| place(*name));
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
    pub fn is_through(&self) -> bool {
        self.layers == (1, self.layer_count.max(1))
    }

    /// What the hole is, as a drill table calls it.
    pub fn usage(&self) -> &'static str {
        let outer = self.layers.0 == 1 || self.layers.1 == self.layer_count;
        match (self.kind, self.is_through(), outer) {
            (HoleKind::NonPlated, ..) => "NPTH",
            (HoleKind::Plated, ..) => "PTH",
            (HoleKind::Via, true, _) => "VIA",
            (HoleKind::Via, false, true) => "BLIND VIA",
            (HoleKind::Via, false, false) => "BURIED VIA",
        }
    }

    pub fn span(&self) -> String {
        if self.is_through() {
            "THRU".to_string()
        } else {
            format!("L{}-L{}", self.layers.0, self.layers.1)
        }
    }

    /// The hole's shape, where it is not round.
    pub fn shape(&self) -> Option<String> {
        let length = self.slot_length?;
        let routed = self.hits.iter().any(|hit| matches!(hit, Hit::Routed(_)));
        Some(if routed {
            "ROUTED PER DATA".to_string()
        } else {
            format!("SLOT {}", mm_fine(length))
        })
    }
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
    // Sizes compare by the micrometre: two holes that differ by less are
    // drilled with one tool.
    let micrometres = |length: f64| (length * 1000.0).round() as i64;
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
            let run = end - start;
            let length = run.x.hypot(run.y);
            // An oval as wide as it is long is one plunge.
            Some(if length > 0.0 {
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
        match self.role {
            LayerRole::Copper => format!("{side}COPPER"),
            LayerRole::Soldermask => format!("{side}SOLDER MASK"),
            LayerRole::Legend => format!("{side}LEGEND"),
            _ => side.trim_end().to_string(),
        }
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
pub const UNSTATED: &str = "NOT SPECIFIED";

impl Ink {
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
            let hex = hex.get(..6).unwrap_or(hex);
            return Self {
                color: u32::from_str_radix(hex, 16).ok(),
                name: format!("PER DATA #{}", hex.to_uppercase()),
            };
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
/// layer.
pub fn layer_ink(source: &Source<'_>, role: LayerRole, side: Side) -> Option<Ink> {
    let is = |layer: &ipc2581::types::Layer| {
        layer_role(layer.layer_function) == role && ir_side(layer.side) == side
    };
    let inks = source.accessor.stackup_inks();
    let ink = inks.iter().find(|(layer, _)| is(layer)).map(|(_, ink)| ink);
    let exists = source.imported.layer_definitions.iter().any(is);
    exists.then(|| Ink::of(ink))
}

/// A colour chip beside a specification's value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Chip {
    Color(u32),
    /// The side has none of it.
    Absent,
    /// The design does not say which.
    Unstated,
}

/// One row of the specification: what, shown in which colours, and stated
/// how.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecRow {
    pub label: &'static str,
    pub chips: Vec<Chip>,
    pub value: String,
}

fn row(label: &'static str, value: impl Into<String>) -> SpecRow {
    SpecRow {
        label,
        chips: Vec::new(),
        value: value.into(),
    }
}

/// What each side of the board carries of a mask or a legend: a chip for
/// the top and one for the bottom, and the words for both.
fn sides_row(label: &'static str, top: Option<Ink>, bottom: Option<Ink>) -> SpecRow {
    let chip = |ink: &Option<Ink>| match ink {
        None => Chip::Absent,
        Some(Ink { color: None, .. }) => Chip::Unstated,
        Some(Ink {
            color: Some(color), ..
        }) => Chip::Color(*color),
    };
    let chips = vec![chip(&top), chip(&bottom)];
    let value = match (top, bottom) {
        (None, None) => "NONE".to_string(),
        (Some(top), None) => format!("{} · TOP ONLY", top.name),
        (None, Some(bottom)) => format!("{} · BOTTOM ONLY", bottom.name),
        (Some(top), Some(bottom)) if top.name == bottom.name => {
            format!("{} · BOTH SIDES", top.name)
        }
        (Some(top), Some(bottom)) => format!("{} TOP · {} BOTTOM", top.name, bottom.name),
    };
    SpecRow {
        label,
        chips,
        value,
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
) -> Result<Vec<SpecRow>> {
    let mut rows = Vec::new();
    let size = |bounds: BBox| format!("{} × {} mm", mm(bounds.width()), mm(bounds.height()));
    if let Some(array) = array {
        // What is delivered, before what each board of it is.
        let boards = array.boards.len();
        rows.push(row(
            "DELIVERY",
            format!(
                "ARRAY OF {boards} · {} × {} mm",
                mm(array.bounds.width()),
                mm(array.bounds.height())
            ),
        ));
    }
    rows.push(row("BOARD SIZE", size(board)));

    let stackup = source.accessor.stackup_details();
    let stack = stackup.as_ref().map(stack_rows).unwrap_or_default();
    let copper = stack
        .iter()
        .filter(|layer| layer.layer_type == StackupLayerType::Conductor)
        .collect::<Vec<_>>();
    let copper_layers = layers
        .iter()
        .filter(|(layer, _)| layer.role == LayerRole::Copper)
        .count();
    rows.push(row("COPPER LAYERS", copper_layers.to_string()));

    let thickness = stackup
        .as_ref()
        .and_then(|stackup| stackup.overall_thickness_mm);
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
                if (plus - minus).abs() < 1e-9 {
                    format!(" ±{plus}{unit}")
                } else {
                    format!(" +{plus}{unit} / -{minus}{unit}")
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
                mm(thickness),
                tolerance.unwrap_or_default()
            ),
        ));
    }
    if let Some(materials) = source.accessor.material_info() {
        rows.push(row("MATERIAL", materials.dielectric.join(", ")));
    }

    // The outer layers' copper, then the inner layers' where there are any;
    // each as one weight where the layers agree.
    let weights = |layers: &[&&&StackupLayerInfo]| {
        let mut weights = layers
            .iter()
            .filter_map(|layer| layer.thickness_mm.map(copper_weight))
            .collect::<Vec<_>>();
        weights.dedup();
        match weights.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            _ => Some("PER LAYER STACK".to_string()),
        }
    };
    if let [top, inner @ .., bottom] = copper.as_slice() {
        match (
            weights(&[top, bottom]),
            weights(&inner.iter().collect::<Vec<_>>()),
        ) {
            (Some(outer), Some(inner)) => {
                rows.push(row("COPPER", format!("OUTER {outer} · INNER {inner}")));
            }
            (Some(outer), None) => rows.push(row("COPPER", outer)),
            _ => {}
        }
    }

    let finish = stackup
        .as_ref()
        .and_then(|stackup| stackup.surface_finish.as_ref());
    rows.push(match finish {
        Some(finish) => {
            let (red, green, blue) = finish.rgb_color();
            SpecRow {
                label: "SURFACE FINISH",
                chips: vec![Chip::Color(u32::from_be_bytes([0, red, green, blue]))],
                value: finish.name.to_uppercase(),
            }
        }
        None => SpecRow {
            label: "SURFACE FINISH",
            chips: vec![Chip::Unstated],
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

    let holes = tools.iter().map(|tool| tool.hits.len()).sum::<usize>();
    if let Some(smallest) = tools.first() {
        // What plating has to reach: the board's thickness over its
        // narrowest plated hole.
        let plated = tools.iter().find(|tool| tool.kind != HoleKind::NonPlated);
        let aspect = thickness
            .zip(plated)
            .map(|(thickness, tool)| format!(" · ASPECT {:.1}:1", thickness / tool.diameter))
            .unwrap_or_default();
        rows.push(row(
            "HOLES",
            format!(
                "{holes} · {} SIZE{} · MIN Ø{}{aspect}",
                tools.len(),
                if tools.len() == 1 { "" } else { "S" },
                mm_fine(smallest.diameter)
            ),
        ));
    }
    let vias = tools
        .iter()
        .filter(|tool| tool.kind == HoleKind::Via)
        .map(|tool| tool.hits.len())
        .sum::<usize>();
    if vias > 0 {
        let open = open_vias(source, tools)?;
        let side = |side: Side| if side == Side::Top { "TOP" } else { "BOTTOM" };
        let sides = if open.len() == 2 { " BOTH SIDES" } else { "" };
        let value = if open.is_empty() {
            None
        } else if open.iter().all(|(_, open)| *open == 0) {
            Some(format!("{vias} · TENTED{sides}"))
        } else if open.iter().all(|(_, open)| *open == vias) {
            Some(format!("{vias} · OPEN{sides}"))
        } else {
            let sides = open
                .iter()
                .map(|(at, open)| format!("{open} OPEN {}", side(*at)))
                .collect::<Vec<_>>();
            Some(format!("{vias} · {}", sides.join(" · ")))
        };
        rows.extend(value.map(|value| row("VIAS", value)));
    }
    if let Some(width) = min_track_width(source.imported)? {
        rows.push(row(
            "MIN TRACK",
            format!("{} mm ({} mil) AS DRAWN", mm_fine(width), mil(width)),
        ));
    }

    // Processes a fabricator prices apart, named only where the board has
    // them.
    let has = |test: &dyn Fn(&DrillTool) -> bool| tools.iter().any(test);
    let cutouts = profile_occurrences_for(&source.imported.geometry, ProfileSet::BoardOutlines)
        .iter()
        .any(|occurrence| !occurrence.profile.cutouts.is_empty());
    let special = [
        (
            has(&|tool| tool.kind != HoleKind::NonPlated && tool.slot_length.is_some()),
            "PLATED SLOTS",
        ),
        (has(&|tool| tool.usage() == "BLIND VIA"), "BLIND VIAS"),
        (has(&|tool| tool.usage() == "BURIED VIA"), "BURIED VIAS"),
        (cutouts, "INTERNAL CUTOUTS"),
    ];
    let special = special
        .into_iter()
        .filter_map(|(present, name)| present.then_some(name))
        .collect::<Vec<_>>();
    if !special.is_empty() {
        rows.push(row("SPECIAL", special.join(" · ")));
    }
    Ok(rows)
}

/// The narrowest track as drawn: the thinnest stroke on a copper layer that
/// carries a net. Pours neck narrower than this where they must; lettering
/// and graphics in copper carry no net and are not tracks.
pub fn min_track_width(imported: &ImportedDesign) -> Result<Option<f64>> {
    let mut narrowest = None::<f64>;
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        if !is_copper(layer.layer_function) {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::Board)?;
        let widths = doc
            .features
            .iter()
            .filter(|feature| feature.net.is_some())
            .flat_map(|feature| feature.paths.indices())
            .filter_map(|path| doc.arena.path(path).stroke())
            .map(|stroke| stroke.width)
            .filter(|width| *width > 0.0);
        narrowest = widths.fold(narrowest, |narrowest, width| {
            Some(narrowest.map_or(width, |narrowest| narrowest.min(width)))
        });
    }
    Ok(narrowest)
}

/// How the solder mask treats the board's vias: on each masked side, how
/// many it leaves open. A via is open where its centre lies in an opening.
pub fn open_vias(source: &Source<'_>, tools: &[DrillTool]) -> Result<Vec<(Side, usize)>> {
    let imported = source.imported;
    let vias = tools
        .iter()
        .filter(|tool| tool.kind == HoleKind::Via)
        .flat_map(|tool| tool.hits.iter().map(Hit::center))
        .collect::<Vec<_>>();
    let mut open = Vec::new();
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        let side = ir_side(layer.side);
        if layer_role(layer.layer_function) != LayerRole::Soldermask
            || !matches!(side, Side::Top | Side::Bottom)
        {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::Board)?;
        let openings =
            doc.into_layer_image(0, LayerRole::Soldermask, Side::None, source.resolution)?;
        let count = openings.contains_points_batch(&vias);
        open.push((side, count.into_iter().filter(|open| *open).count()));
    }
    Ok(open)
}

/// A fiducial an array carries for the assembler.
#[derive(Debug, Clone, Copy)]
pub struct Fiducial {
    pub at: Point,
    pub diameter: f64,
    pub side: Side,
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

impl ArrayData {
    /// The array a design's primary step lays out, if it lays one out.
    pub fn of(source: &Source<'_>) -> Result<Option<Self>> {
        let imported = source.imported;
        let geometry = &imported.geometry;
        if pcb_ir::dialects::ipc::root_panel_step(geometry).is_none() {
            return Ok(None);
        }
        let occurrences = profile_occurrences_for(geometry, ProfileSet::FabricationOutlines);
        let bounds_of = |role: ProfileOccurrenceRole| {
            occurrences
                .iter()
                .filter(|occurrence| occurrence.role == role)
                .map(|occurrence| {
                    geometry
                        .transformed_path_bbox(occurrence.profile.outer_path, occurrence.transform)
                })
                .collect::<Vec<_>>()
        };
        let bounds = bounds_of(ProfileOccurrenceRole::RootPanel)
            .into_iter()
            .fold(BBox::empty(), BBox::union);
        if bounds.is_empty() {
            return Ok(None);
        }
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
            boards: bounds_of(ProfileOccurrenceRole::BoardInstance),
            outlines: profile.array_outlines,
            removal: profile.material_removal,
            scores,
            fiducials: fiducials(imported)?,
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
}

/// One breakaway tab of an array, to draw large enough to read.
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
        let tool = self
            .tools
            .iter()
            .filter(|tool| tool.kind == HoleKind::NonPlated && tool.slot_length.is_none())
            .filter(|tool| tool.hits.len() > 2)
            .min_by(|a, b| a.diameter.total_cmp(&b.diameter))?;
        let holes = tool.hits.iter().map(Hit::center).collect::<Vec<_>>();
        let distance = |a: Point, b: Point| (a.x - b.x).hypot(a.y - b.y);
        let nearest = |from: Point| {
            let others = holes.iter().filter(|hole| distance(**hole, from) > 0.0);
            others
                .map(|hole| distance(*hole, from))
                .min_by(f64::total_cmp)
        };
        let first = *holes.iter().min_by(|a, b| {
            distance(**a, self.bounds.min).total_cmp(&distance(**b, self.bounds.min))
        })?;
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
                let near = distance(*hole, from) <= 1.5 * pitch;
                if near && !tab.iter().any(|member| distance(*member, *hole) == 0.0) {
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
            holes: tab.len(),
            diameter: tool.diameter,
            pitch,
        })
    }
}

/// The fiducials the array's rails and cells carry on their outer copper.
fn fiducials(imported: &ImportedDesign) -> Result<Vec<Fiducial>> {
    let mut fiducials = Vec::new();
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        let side = ir_side(layer.side);
        if !is_copper(layer.layer_function) || !matches!(side, Side::Top | Side::Bottom) {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::ArraySupport)?;
        fiducials.extend(doc.features.iter().filter_map(|feature| {
            let Some(SimpleShape::Circle { diameter }) = feature.shape else {
                return None;
            };
            (feature.fiducial_kind != FiducialKind::Unknown).then_some(Fiducial {
                at: feature.bbox.center(),
                diameter,
                side,
            })
        }));
    }
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
pub fn notes(array: Option<&ArrayData>, requirements: &Requirements) -> Vec<String> {
    let class = requirements.class;
    let mut notes = vec![
        format!("FABRICATE AND INSPECT TO IPC-6012 / IPC-A-600 CLASS {class}."),
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
