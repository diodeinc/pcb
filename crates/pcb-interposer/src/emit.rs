//! Emit the interposer as a KiCad board via `pcb_ir::dialects::kicad`.
//!
//! The deterministic board: the panel's outline, tooling holes, and
//! fiducials, the bed's mate lands on the bottom copper (mate-detect
//! pairs shorted by emitted traces), and full-sheet
//! GND pours on both faces — GND rides the pours, unrouted; the
//! post-route stitching pass (`crate::stitch`) adds the vias that tie
//! the faces together around whatever copper the router drew. With a
//! fixture plan, the board is also populated — a pogo pad on the top
//! face at every tested contact, and the plan's nets bound on both the
//! pogo and its mate land — so the unrouted airwires are exactly the
//! routing pass's work list. Without a plan (no ICT contacts), GND
//! lands join the pour and everything else stays un-netted. The
//! four-layer variants add two inner layers: both carrying the same GND
//! pour, or a GND plane under the top face and a signal layer.

use pcb_ir::dialects::kicad::{
    At, Document, Footprint, FootprintAttrs, Graphic, LayerKind, Mount, Pad, PadKind, PadShape,
    Property, Segment, Setup, Stackup, StackupLayer, Stroke, UuidGen, Zone, ZoneConnect, ZoneFill,
};
use pcb_ir::geom::Point;

use crate::bed::{Alignment, Bed};
use crate::panel::{Outline, Panel};
use crate::pattern::{Land, Role};
use crate::plan::Plan;
use crate::pogo::PogoTemplate;

/// Copper layer count of the generated board.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Layers {
    #[default]
    Two,
    /// Two inner GND planes on the stdlib's default 4-layer stackup. Every
    /// track rides a plane, so on a bed with high voltage the board holds
    /// the exposed clearance everywhere instead of per item (see
    /// [`Layers::uniform_clearance`]).
    Four,
    /// The same stackup with In1 a GND plane and In2 a signal layer, so
    /// routing gets a layer of its own and every route a reference plane.
    FourSignal,
}

impl std::str::FromStr for Layers {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "2" => Ok(Self::Two),
            "4" => Ok(Self::Four),
            "4-signal" => Ok(Self::FourSignal),
            _ => Err("expected 2, 4 or 4-signal".into()),
        }
    }
}

impl Layers {
    /// The one clearance every net, pad and track keeps, for a stackup
    /// that holds a single rule: [`Layers::Four`] on a bed with high
    /// voltage takes its exposed clearance. With both inner layers GND,
    /// a 0.6 mm gap still leaves room for a track between lands on a
    /// 3 mm pitch, and the single rule needs no custom rules or clearance
    /// matrix, which FreeRouting holds only loosely at vias.
    pub fn uniform_clearance(self, bed: Bed) -> Option<f64> {
        match self {
            Self::Four => bed.hv_clearance().map(|hv| hv.exposed),
            Self::Two | Self::FourSignal => None,
        }
    }
}

/// ID strap track width; the straps carry static levels.
const STRAP_TRACK_MM: f64 = 0.25;
/// A strap lane's gap to the lands it passes: Default clearance plus a
/// hair, or the board's uniform clearance.
const STRAP_GAP_MM: f64 = 0.25;
/// Copper-to-edge clearance (the project's `min_copper_edge_clearance`),
/// clear of the standard PDK's 0.381 mm preferred minimum.
const EDGE_CLEARANCE_MM: f64 = 0.4;

/// The ID straps' fixed routes, as (lane y, (net, land x positions)): the
/// strap nets' lands share one row, and the two nets' terminals
/// interleave along it, which no single-sided routing can untangle. So
/// one net runs a lane just above the row and the other just below it,
/// between the lands and the board edge; each land drops a stub to its
/// lane. Straps that don't fit that shape (other than GND, which the pour
/// joins) are left to the router.
fn strap_lanes<'a>(
    panel: &Panel,
    lands: &[Land],
    land_dia: f64,
    gap: f64,
    straps: &[(usize, &'a str)],
) -> Vec<(f64, (&'a str, Vec<f64>))> {
    let mut nets: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for &(land, net) in straps.iter().filter(|(_, net)| *net != "GND") {
        nets.entry(net).or_default().push(land);
    }
    let rows: Vec<f64> = nets
        .values()
        .flatten()
        .map(|&land| lands[land].xy[1])
        .collect();
    let Some(&row) = rows.first() else {
        return Vec::new();
    };
    if nets.len() > 2
        || rows.iter().any(|y| (y - row).abs() > 1e-6)
        || nets.values().any(|l| l.len() < 2)
    {
        return Vec::new();
    }
    let offset = land_dia / 2.0 + gap + STRAP_TRACK_MM / 2.0;
    let bottom = panel
        .outline
        .iter()
        .flat_map(|outline| match *outline {
            Outline::Line { start, end } => [start[1], end[1], end[1]],
            Outline::Arc { start, mid, end } => [start[1], mid[1], end[1]],
        })
        .fold(f64::MIN, f64::max);
    if row + offset + STRAP_TRACK_MM / 2.0 + EDGE_CLEARANCE_MM > bottom {
        return Vec::new();
    }
    nets.into_iter()
        .zip([row - offset, row + offset])
        .map(|((net, ends), lane)| {
            (
                lane,
                (net, ends.iter().map(|&land| lands[land].xy[0]).collect()),
            )
        })
        .collect()
}

/// The inner layer [`Layers::FourSignal`] routes on, away from the top
/// face's pogo field.
const SIGNAL_INNER: &str = "In2.Cu";

/// The finished interposer's thickness: the 2-layer default, or the
/// 4-layer stackup's sum.
pub fn thickness(layers: Layers) -> f64 {
    match layers {
        Layers::Two => Document::two_layer().thickness_mm,
        Layers::Four | Layers::FourSignal => base_4l_stackup()
            .layers
            .iter()
            .filter_map(|l| l.thickness)
            .sum(),
    }
}

/// Build the `.kicad_pcb` source for `bed`'s lands and alignment;
/// `straps` nets the bed's ID lands, as (land index, net).
pub fn board(
    panel: &Panel,
    lands: &[Land],
    bed: Bed,
    plan: Option<&Plan>,
    straps: &[(usize, &str)],
    layers: Layers,
) -> String {
    let land_dia = bed.land_dia();
    let mut doc = match layers {
        Layers::Two => Document::two_layer(),
        Layers::Four | Layers::FourSignal => {
            let stackup = base_4l_stackup();
            Document {
                thickness_mm: stackup.layers.iter().filter_map(|l| l.thickness).sum(),
                setup: Setup {
                    stackup: Some(stackup),
                    ..Setup::default()
                },
                ..Document::four_layer()
            }
        }
    };
    doc.generator = "pcb-interposer".into();
    let mut uuids = UuidGen::new();
    let gnd = doc.net("GND");

    // The plan's nets, keyed both ways: per land index and per contact.
    let mut land_nets: std::collections::BTreeMap<usize, (u32, String)> = Default::default();
    let mut contact_nets: Vec<(usize, (u32, String))> = Vec::new();
    if let Some(plan) = plan {
        for binding in &plan.bindings {
            let net = (doc.net(&binding.net), binding.net.clone());
            if let Some(land) = binding.land {
                land_nets.insert(land, net.clone());
            }
            contact_nets.push((binding.contact, net));
        }
        for (land, net) in &plan.paralleled {
            land_nets.insert(*land, (doc.net(net), net.clone()));
        }
    }

    for outline in &panel.outline {
        doc.graphics.push(match *outline {
            Outline::Line { start, end } => Graphic::Line {
                start: Point::new(start[0], start[1]),
                end: Point::new(end[0], end[1]),
                stroke: Stroke::solid(0.1),
                layer: "Edge.Cuts".into(),
                uuid: uuids.next_uuid(),
            },
            Outline::Arc { start, mid, end } => Graphic::Arc {
                start: Point::new(start[0], start[1]),
                mid: Point::new(mid[0], mid[1]),
                end: Point::new(end[0], end[1]),
                stroke: Stroke::solid(0.1),
                layer: "Edge.Cuts".into(),
                uuid: uuids.next_uuid(),
            },
        });
    }

    for (index, (at, dia)) in panel.holes.iter().enumerate() {
        doc.footprints
            .push(hole_footprint(&mut uuids, index, *at, *dia));
    }
    for alignment in bed.alignment() {
        doc.raw_footprints
            .push(alignment_footprint(&mut uuids, &alignment));
    }
    for (index, at) in panel.fids_top.iter().enumerate() {
        doc.footprints
            .push(fid_footprint(&mut uuids, index, *at, true));
    }
    for (index, at) in panel.fids_bottom.iter().enumerate() {
        let ordinal = panel.fids_top.len() + index;
        doc.footprints
            .push(fid_footprint(&mut uuids, ordinal, *at, false));
    }
    // Mate-detect loops: each pair is one isolated net, shorted on this
    // face so the bed proves seating by continuity through the mated
    // plate.
    let mut detect_nets: std::collections::BTreeMap<u32, (u32, String)> = Default::default();
    for land in lands.iter().filter(|land| land.role == Role::Detect) {
        let next = detect_nets.len() + 1;
        detect_nets
            .entry(land.block)
            .or_insert_with(|| (doc.net(&format!("DETECT_{next}")), format!("DETECT_{next}")));
    }
    for &(land, net) in straps {
        let code = doc.net(net);
        land_nets.insert(land, (code, net.to_string()));
    }
    for (index, land) in lands.iter().enumerate() {
        let net = land_nets
            .get(&index)
            .cloned()
            .or_else(|| (land.role == Role::Gnd).then(|| (gnd, "GND".to_string())))
            .or_else(|| (land.role == Role::Detect).then(|| detect_nets[&land.block].clone()));
        doc.footprints
            .push(land_footprint(&mut uuids, index, land, land_dia, net));
    }
    for (block, net) in &detect_nets {
        let pair: Vec<&Land> = lands
            .iter()
            .filter(|land| land.role == Role::Detect && land.block == *block)
            .collect();
        doc.segments.push(Segment {
            start: Point::new(pair[0].xy[0], pair[0].xy[1]),
            end: Point::new(pair[1].xy[0], pair[1].xy[1]),
            width: 0.3,
            layer: "B.Cu".into(),
            net: net.0,
            uuid: uuids.next_uuid(),
            locked: true,
        });
    }
    // A paralleled VTarget land ties to its bound neighbor on this face,
    // where the straight run between them crosses no other land.
    let power_gap = layers
        .uniform_clearance(bed)
        .or(bed.hv_clearance().map(|hv| hv.coated))
        .unwrap_or(0.2);
    if let Some(plan) = plan {
        for (land, net) in &plan.paralleled {
            let from = lands[*land].xy;
            let partner = plan
                .bindings
                .iter()
                .filter_map(|binding| binding.land)
                .filter(|&other| {
                    lands[other].block == lands[*land].block && land_nets[&other].1 == *net
                })
                .min_by(|&a, &b| {
                    let d = |l: usize| (lands[l].xy[0] - from[0]).hypot(lands[l].xy[1] - from[1]);
                    d(a).total_cmp(&d(b))
                });
            let Some(partner) = partner else { continue };
            let to = lands[partner].xy;
            let clear = lands.iter().enumerate().all(|(index, other)| {
                index == *land
                    || index == partner
                    || segment_distance(other.xy, from, to)
                        >= land_dia / 2.0 + POWER_TRACK_MM / 2.0 + power_gap
            });
            if clear {
                doc.segments.push(Segment {
                    start: Point::new(from[0], from[1]),
                    end: Point::new(to[0], to[1]),
                    width: POWER_TRACK_MM,
                    layer: "B.Cu".into(),
                    net: land_nets[land].0,
                    uuid: uuids.next_uuid(),
                    locked: true,
                });
            }
        }
    }
    for (lane, (net, ends)) in strap_lanes(
        panel,
        lands,
        land_dia,
        layers.uniform_clearance(bed).unwrap_or(STRAP_GAP_MM),
        straps,
    ) {
        let code = doc.net(net);
        let (x0, x1) = ends
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
        let row = lands[straps
            .iter()
            .find(|(_, n)| *n == net)
            .expect("strap net has lands")
            .0]
            .xy[1];
        let runs = ends
            .iter()
            .map(|&x| ([x, row], [x, lane]))
            .chain(std::iter::once(([x0, lane], [x1, lane])));
        for (start, end) in runs {
            doc.segments.push(Segment {
                start: Point::new(start[0], start[1]),
                end: Point::new(end[0], end[1]),
                width: STRAP_TRACK_MM,
                layer: "B.Cu".into(),
                net: code,
                uuid: uuids.next_uuid(),
                locked: true,
            });
        }
    }
    if let Some(plan) = plan {
        let template = PogoTemplate::load().expect("vendored pogo template is valid");
        for (ordinal, (contact_index, net)) in contact_nets.iter().enumerate() {
            let contact = &plan.contacts[*contact_index];
            doc.raw_footprints.push(template.stamp(
                contact.xy,
                &format!("P{}", ordinal + 1),
                net.clone(),
                &mut uuids,
            ));
        }
    }

    let copper: Vec<String> = doc
        .layers
        .iter()
        .filter(|layer| layer.kind != LayerKind::User)
        .map(|layer| layer.canonical.clone())
        .collect();
    if layers == Layers::FourSignal {
        for layer in doc
            .layers
            .iter_mut()
            .filter(|l| l.canonical == SIGNAL_INNER)
        {
            layer.kind = LayerKind::Signal;
        }
    }
    for layer in copper
        .into_iter()
        .filter(|l| layers != Layers::FourSignal || l != SIGNAL_INNER)
    {
        doc.zones.push(Zone {
            net: gnd,
            net_name: "GND".into(),
            layers: vec![layer],
            uuid: uuids.next_uuid(),
            name: None,
            priority: None,
            hatch_pitch: 0.5,
            connect_pads: ZoneConnect::Thermal,
            connect_clearance: 0.25,
            min_thickness: 0.25,
            fill: ZoneFill {
                enabled: true,
                thermal_gap: 0.3,
                thermal_bridge_width: 0.4,
            },
            polygon: {
                // The sheet, grown to an interposer that overhangs it.
                let [x0, y0, x1, y1] = panel.envelope.map_or(
                    [0.0, 0.0, panel.width, panel.height],
                    |[x0, y0, x1, y1]| {
                        [
                            x0.min(0.0),
                            y0.min(0.0),
                            x1.max(panel.width),
                            y1.max(panel.height),
                        ]
                    },
                );
                vec![
                    Point::new(x0, y0),
                    Point::new(x1, y0),
                    Point::new(x1, y1),
                    Point::new(x0, y1),
                ]
            },
        });
    }

    pcb_ir::dialects::kicad::write(&doc)
}

/// The stdlib's `BASE_4L_STACKUP` (`lib/std/board_config.zen`): 1.6 mm,
/// 1 oz outer / 0.5 oz inner copper.
fn base_4l_stackup() -> Stackup {
    let prepreg = |index| StackupLayer::dielectric(index, "prepreg", 0.2104, "Prepreg", 4.4, 0.025);
    Stackup {
        layers: vec![
            StackupLayer::technical("F.SilkS", "Top Silk Screen", Some("White"), None),
            StackupLayer::technical("F.Paste", "Top Solder Paste", None, None),
            StackupLayer::technical("F.Mask", "Top Solder Mask", Some("Black"), Some(0.01)),
            StackupLayer::copper("F.Cu", 0.035),
            prepreg(1),
            StackupLayer::copper("In1.Cu", 0.0152),
            StackupLayer::dielectric(2, "core", 1.065, "FR4-Core", 4.6, 0.025),
            StackupLayer::copper("In2.Cu", 0.0152),
            prepreg(3),
            StackupLayer::copper("B.Cu", 0.035),
            StackupLayer::technical("B.Mask", "Bottom Solder Mask", Some("Black"), Some(0.01)),
            StackupLayer::technical("B.Paste", "Bottom Solder Paste", None, None),
            StackupLayer::technical("B.SilkS", "Bottom Silk Screen", Some("White"), None),
        ],
        copper_finish: Some("ENIG".into()),
    }
}

fn hidden_properties(uuids: &mut UuidGen, reference: &str, top: bool) -> Vec<Property> {
    let (silk, fab) = if top {
        ("F.SilkS", "F.Fab")
    } else {
        ("B.SilkS", "B.Fab")
    };
    vec![
        Property {
            key: "Reference".into(),
            value: reference.into(),
            at: At::xy(0.0, -1.8),
            layer: silk.into(),
            hide: true,
            uuid: uuids.next_uuid(),
        },
        Property {
            key: "Value".into(),
            value: String::new(),
            at: At::xy(0.0, 1.8),
            layer: fab.into(),
            hide: true,
            uuid: uuids.next_uuid(),
        },
    ]
}

fn hole_footprint(uuids: &mut UuidGen, index: usize, at: [f64; 2], dia: f64) -> Footprint {
    Footprint {
        lib_id: "Interposer:ToolingHole".into(),
        layer: "F.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(at[0], at[1]),
        properties: hidden_properties(uuids, &format!("H{}", index + 1), true),
        attrs: FootprintAttrs {
            mount: None,
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: String::new(),
            kind: PadKind::NpThruHole,
            shape: PadShape::Circle,
            at: At::default(),
            size: (dia, dia),
            drill: Some(dia),
            layers: vec!["*.Cu".into(), "*.Mask".into()],
            net: None,
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    }
}

/// An alignment hole or slot. The writer's pads drill only round holes,
/// so the footprint is emitted as text. The pad-local clearance holds
/// copper outside the keep-out on every face; measured from the drill's
/// narrow side, it over-covers a slot's ends.
fn alignment_footprint(uuids: &mut UuidGen, alignment: &Alignment) -> String {
    let [w, h] = alignment.drill;
    let (shape, drill) = if (w - h).abs() < 1e-9 {
        ("circle", format!("(drill {})", fmt(w)))
    } else {
        ("oval", format!("(drill oval {} {})", fmt(w), fmt(h)))
    };
    let clearance = (alignment.keepout_dia - w.min(h)) / 2.0;
    let property = |uuids: &mut UuidGen, key: &str, value: &str, y: f64, layer: &str| {
        format!(
            "\t(property \"{key}\" \"{value}\"\n\t\t(at 0 {} 0)\n\t\t(layer \"{layer}\")\n\t\t(hide yes)\n\t\t(uuid \"{}\")\n\t\t(effects (font (size 1 1) (thickness 0.15)))\n\t)\n",
            fmt(y),
            uuids.next_uuid()
        )
    };
    let mut out = format!(
        "(footprint \"Interposer:Alignment_{}\"\n\t(layer \"F.Cu\")\n\t(uuid \"{}\")\n\t(at {} {})\n",
        alignment.name,
        uuids.next_uuid(),
        fmt(alignment.at[0]),
        fmt(alignment.at[1])
    );
    out.push_str(&property(
        uuids,
        "Reference",
        alignment.name,
        -4.0,
        "F.SilkS",
    ));
    out.push_str(&property(uuids, "Value", "", 4.0, "F.Fab"));
    out.push_str("\t(attr exclude_from_pos_files exclude_from_bom)\n");
    out.push_str(&format!(
        "\t(pad \"\" np_thru_hole {shape}\n\t\t(at 0 0)\n\t\t(size {} {})\n\t\t{drill}\n\t\t(layers \"*.Cu\" \"*.Mask\")\n\t\t(clearance {})\n\t\t(uuid \"{}\")\n\t)\n)",
        fmt(w),
        fmt(h),
        fmt(clearance),
        uuids.next_uuid()
    ));
    out
}

fn fmt(value: f64) -> String {
    let formatted = format!("{value:.4}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// A global fiducial: Ø1 copper dot with a Ø2 mask opening. The pad-level
/// clearance keeps pour copper outside the mask aperture so the aperture
/// never bridges the dot with poured GND.
fn fid_footprint(uuids: &mut UuidGen, index: usize, at: [f64; 2], top: bool) -> Footprint {
    let (copper, mask) = if top {
        ("F.Cu", "F.Mask")
    } else {
        ("B.Cu", "B.Mask")
    };
    Footprint {
        lib_id: "Interposer:Fiducial_1.0_2.0".into(),
        layer: copper.into(),
        uuid: uuids.next_uuid(),
        at: At::xy(at[0], at[1]),
        properties: hidden_properties(uuids, &format!("FID{}", index + 1), top),
        attrs: FootprintAttrs {
            mount: Some(Mount::Smd),
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: String::new(),
            kind: PadKind::Smd,
            shape: PadShape::Circle,
            at: At::default(),
            size: (1.0, 1.0),
            drill: None,
            layers: vec![copper.into(), mask.into()],
            net: None,
            solder_mask_margin: Some(0.5),
            clearance: Some(0.6),
            uuid: uuids.next_uuid(),
        }],
    }
}

fn land_footprint(
    uuids: &mut UuidGen,
    index: usize,
    land: &Land,
    dia: f64,
    net: Option<(u32, String)>,
) -> Footprint {
    let mut properties = hidden_properties(uuids, &format!("L{}", index + 1), false);
    properties.push(Property {
        key: "Ict".into(),
        value: land.role.name().into(),
        at: At::xy(0.0, 3.0),
        layer: "B.Fab".into(),
        hide: true,
        uuid: uuids.next_uuid(),
    });
    Footprint {
        lib_id: format!("Interposer:Mate_Pad_D{dia}mm"),
        layer: "B.Cu".into(),
        uuid: uuids.next_uuid(),
        at: At::xy(land.xy[0], land.xy[1]),
        properties,
        attrs: FootprintAttrs {
            mount: Some(Mount::Smd),
            exclude_from_pos_files: true,
            exclude_from_bom: true,
        },
        pads: vec![Pad {
            number: "1".into(),
            kind: PadKind::Smd,
            shape: PadShape::Circle,
            at: At::default(),
            size: (dia, dia),
            drill: None,
            layers: vec!["B.Cu".into(), "B.Mask".into()],
            net,
            solder_mask_margin: None,
            clearance: None,
            uuid: uuids.next_uuid(),
        }],
    }
}

/// Distance from `p` to the segment `a`–`b`.
fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a[0] + t * dx - p[0]).hypot(a[1] + t * dy - p[1])
}

/// VTarget track width. A board draws up to 3 A over its two VTarget
/// contacts; 0.8 mm of 1 oz outer copper carries ~2 A at a 10 °C rise
/// (IPC-2221).
pub(crate) const POWER_TRACK_MM: f64 = 0.8;

/// Default signal track width.
pub(crate) const SIGNAL_TRACK_MM: f64 = 0.25;

/// The sibling `.kicad_pro`: design rules the later routing passes are
/// built for, so every interposer artifact carries one consistent rule
/// set from the start. Net classes are bound by exact net name from the
/// plan's roles, since net names carry DUT paths, not roles: VTarget
/// nets take the wide `Power` class and USB pairs the `USB` class. On a
/// bed whose lands may carry high voltage, VTarget and the signal lands
/// (protected digital and analog) keep its coated clearance (`HV`), and
/// [`rules`] adds the exposed one; VLogic, USB, the detect loops and the
/// ID straps stay low-voltage. A stackup with a uniform clearance
/// ([`Layers::uniform_clearance`]) gives every class that one.
pub fn project(bed: Bed, plan: Option<&Plan>, layers: Layers) -> String {
    let uniform = layers.uniform_clearance(bed);
    let clearance = uniform.or(bed.hv_clearance().map(|hv| hv.coated));
    let class = |name: &str, clearance: f64, track_width: f64| {
        serde_json::json!({
            "name": name,
            "clearance": clearance,
            "track_width": track_width,
            "via_diameter": 0.6,
            "via_drill": 0.3,
        })
    };
    let mut classes = vec![
        class("Default", uniform.unwrap_or(0.2), SIGNAL_TRACK_MM),
        class("Power", clearance.unwrap_or(0.2), POWER_TRACK_MM),
        // FS USB is electrically short here; at the uniform clearance a
        // 0.25 mm pair routes as easily as any signal.
        class(
            "USB",
            uniform.unwrap_or(0.1),
            if uniform.is_some() { 0.25 } else { 0.2 },
        ),
    ];
    if let Some(clearance) = clearance {
        classes.push(class("HV", clearance, 0.25));
    }
    let mut patterns: Vec<(&str, String)> = Vec::new();
    if let Some(plan) = plan {
        for binding in plan.bindings.iter().filter(|b| b.land.is_some()) {
            let class = match (plan.contacts[binding.contact].role, clearance) {
                (Role::Vtarget, _) => "Power",
                (Role::UsbDp | Role::UsbDm, _) => "USB",
                (Role::Ls | Role::Analog, Some(_)) => "HV",
                _ => continue,
            };
            patterns.push((class, binding.net.clone()));
        }
    }
    let doc = serde_json::json!({
        "board": {
            "design_settings": {
                "rules": {
                    "max_error": 0.005,
                    "min_clearance": 0.1,
                    "min_connection": 0.0,
                    "min_copper_edge_clearance": EDGE_CLEARANCE_MM,
                    "min_hole_clearance": 0.25,
                    "min_hole_to_hole": 0.25,
                    "min_microvia_diameter": 0.2,
                    "min_microvia_drill": 0.1,
                    "min_resolved_spokes": 1,
                    "min_silk_clearance": 0.0,
                    "min_text_height": 0.8,
                    "min_text_thickness": 0.08,
                    "min_through_hole_diameter": 0.3,
                    "min_track_width": 0.15,
                    "min_via_annular_width": 0.1,
                    "min_via_diameter": 0.5
                }
            }
        },
        "net_settings": {
            "classes": classes,
            "netclass_patterns": patterns
                .iter()
                .map(|(class, pattern)| serde_json::json!({ "netclass": class, "pattern": pattern }))
                .collect::<Vec<_>>(),
        },
        "meta": { "version": 3 }
    });
    serde_json::to_string_pretty(&doc).expect("project serializes") + "\n"
}

/// Net classes that carry the bed's high voltage, when it has any.
pub const HV_CLASSES: [&str; 2] = ["HV", "Power"];

/// The sibling `.kicad_dru`, for a bed with high voltage. Net-class
/// clearance is the coated one, right between tracks and tented vias;
/// these rules hold the exposed one wherever bare copper is involved: a
/// high-voltage pad against anything, and any pad against high-voltage
/// copper. A stackup with a uniform clearance needs none.
pub fn rules(bed: Bed, layers: Layers) -> Option<String> {
    if layers.uniform_clearance(bed).is_some() {
        return None;
    }
    let exposed = bed.hv_clearance()?.exposed;
    let hot = |item: &str| {
        HV_CLASSES
            .iter()
            .map(|class| format!("{item}.hasNetclass('{class}')"))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    Some(format!(
        "(version 1)\n\n\
         (rule \"High-voltage pad\"\n\
         \t(condition \"A.Type == 'Pad' && ({a})\")\n\
         \t(constraint clearance (min {exposed}mm)))\n\n\
         (rule \"Pad facing high voltage\"\n\
         \t(condition \"A.Type == 'Pad' && A.Net != B.Net && ({b})\")\n\
         \t(constraint clearance (min {exposed}mm)))\n",
        a = hot("A"),
        b = hot("B"),
    ))
}
