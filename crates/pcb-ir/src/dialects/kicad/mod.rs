//! Generated KiCad board documents (`.kicad_pcb`), millimeters.
//!
//! Writer-oriented: this dialect models what board *generators* produce —
//! layers, nets, footprints with pads, tracks, vias, zones, and board
//! graphics — not everything KiCad can read back. Coordinates are KiCad
//! layout coordinates (millimeters, Y down). [`write`] serializes a
//! document into KiCad 9/10 s-expression text.
//!
//! Identity is deterministic by design: every element carries a `uuid`
//! string (KiCad requires one), and [`UuidGen`] hands out well-formed
//! sequential UUIDs so re-generating the same board yields the same file.

mod write;

pub use write::{quote, write};

use crate::geom::Point;

/// A complete `.kicad_pcb` document.
#[derive(Debug, Clone)]
pub struct Document {
    /// KiCad file format version stamp (e.g. `20260206` for KiCad 10).
    pub version: u32,
    pub generator: String,
    pub generator_version: String,
    /// Overall board thickness for the `general` section.
    pub thickness_mm: f64,
    pub paper: String,
    pub layers: Vec<Layer>,
    pub setup: Setup,
    /// Net table; the net *number* is the index. Index 0 is KiCad's
    /// "no net" and should stay the empty string.
    pub nets: Vec<String>,
    pub footprints: Vec<Footprint>,
    /// Library footprints spliced in verbatim as already-serialized
    /// `(footprint …)` blocks — the escape hatch for real parts whose
    /// definitions (graphics, embedded 3D models) come from `.kicad_mod`
    /// files rather than this dialect's generated primitives. Each block
    /// must be a complete well-formed footprint expression; the writer
    /// only re-indents it.
    pub raw_footprints: Vec<String>,
    pub segments: Vec<Segment>,
    pub arcs: Vec<TrackArc>,
    pub vias: Vec<Via>,
    pub zones: Vec<Zone>,
    pub graphics: Vec<Graphic>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            version: 20260206,
            generator: "pcb-ir".into(),
            generator_version: env!("CARGO_PKG_VERSION").into(),
            thickness_mm: 1.6,
            paper: "A4".into(),
            layers: Vec::new(),
            setup: Setup::default(),
            nets: vec![String::new()],
            footprints: Vec::new(),
            raw_footprints: Vec::new(),
            segments: Vec::new(),
            arcs: Vec::new(),
            vias: Vec::new(),
            zones: Vec::new(),
            graphics: Vec::new(),
        }
    }
}

impl Document {
    /// A document with the standard two-copper-layer stack (KiCad 10
    /// ordinals: F.Cu = 0, B.Cu = 2).
    pub fn two_layer() -> Self {
        Self {
            layers: vec![
                Layer::signal(0, "F.Cu"),
                Layer::signal(2, "B.Cu"),
                Layer::user(13, "F.Paste"),
                Layer::user(15, "B.Paste"),
                Layer::named_user(5, "F.SilkS", "F.Silkscreen"),
                Layer::named_user(7, "B.SilkS", "B.Silkscreen"),
                Layer::user(1, "F.Mask"),
                Layer::user(3, "B.Mask"),
                Layer::user(25, "Edge.Cuts"),
                Layer::named_user(29, "B.CrtYd", "B.Courtyard"),
                Layer::named_user(31, "F.CrtYd", "F.Courtyard"),
                Layer::user(33, "B.Fab"),
                Layer::user(35, "F.Fab"),
            ],
            ..Self::default()
        }
    }

    /// Intern a net name, returning its net number.
    pub fn net(&mut self, name: &str) -> u32 {
        if let Some(index) = self.nets.iter().position(|net| net == name) {
            return index as u32;
        }
        self.nets.push(name.to_string());
        (self.nets.len() - 1) as u32
    }
}

/// One entry of the board's layer table.
#[derive(Debug, Clone)]
pub struct Layer {
    pub ordinal: u32,
    /// Canonical KiCad name (`F.Cu`, `Edge.Cuts`, …).
    pub canonical: String,
    pub kind: LayerKind,
    /// Optional user-facing rename (`F.Silkscreen` for `F.SilkS`).
    pub user_name: Option<String>,
}

impl Layer {
    pub fn signal(ordinal: u32, canonical: &str) -> Self {
        Self {
            ordinal,
            canonical: canonical.into(),
            kind: LayerKind::Signal,
            user_name: None,
        }
    }

    pub fn user(ordinal: u32, canonical: &str) -> Self {
        Self {
            ordinal,
            canonical: canonical.into(),
            kind: LayerKind::User,
            user_name: None,
        }
    }

    pub fn named_user(ordinal: u32, canonical: &str, user_name: &str) -> Self {
        Self {
            user_name: Some(user_name.into()),
            ..Self::user(ordinal, canonical)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Signal,
    Power,
    Mixed,
    Jumper,
    User,
}

/// Board `setup` section. Only the knobs generators set; everything else
/// keeps KiCad's defaults on load.
#[derive(Debug, Clone)]
pub struct Setup {
    pub pad_to_mask_clearance: f64,
    pub allow_soldermask_bridges_in_footprints: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            pad_to_mask_clearance: 0.0,
            allow_soldermask_bridges_in_footprints: false,
        }
    }
}

/// Position with rotation, KiCad `(at x y rot)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct At {
    pub x: f64,
    pub y: f64,
    /// Degrees. KiCad omits the third element when zero; the writer does
    /// the same.
    pub rot: f64,
}

impl At {
    pub fn xy(x: f64, y: f64) -> Self {
        Self { x, y, rot: 0.0 }
    }
}

/// A footprint instance. Pad positions are footprint-relative, exactly as
/// KiCad stores them; pad rotation is absolute (it includes the parent's),
/// again matching the file format.
#[derive(Debug, Clone)]
pub struct Footprint {
    /// Library id, e.g. `Interposer:Pad_D1.0mm`.
    pub lib_id: String,
    pub layer: String,
    pub uuid: String,
    pub at: At,
    pub properties: Vec<Property>,
    pub attrs: FootprintAttrs,
    pub pads: Vec<Pad>,
}

/// A footprint property (`Reference`, `Value`, or a custom field).
#[derive(Debug, Clone)]
pub struct Property {
    pub key: String,
    pub value: String,
    /// Footprint-relative text position.
    pub at: At,
    pub layer: String,
    pub hide: bool,
    pub uuid: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FootprintAttrs {
    pub mount: Option<Mount>,
    pub exclude_from_pos_files: bool,
    pub exclude_from_bom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    Smd,
    ThroughHole,
}

#[derive(Debug, Clone)]
pub struct Pad {
    /// Pad number; empty is legal (tooling holes).
    pub number: String,
    pub kind: PadKind,
    pub shape: PadShape,
    pub at: At,
    pub size: (f64, f64),
    /// Drill diameter for through-hole pad kinds.
    pub drill: Option<f64>,
    pub layers: Vec<String>,
    pub net: Option<(u32, String)>,
    pub solder_mask_margin: Option<f64>,
    /// Pad-level clearance override.
    pub clearance: Option<f64>,
    pub uuid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadKind {
    Smd,
    ThruHole,
    NpThruHole,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadShape {
    Circle,
    Rect,
    Oval,
    RoundRect {
        /// Corner radius as a ratio of the smaller pad dimension.
        ratio: f64,
    },
}

/// A straight track segment.
#[derive(Debug, Clone)]
pub struct Segment {
    pub start: Point,
    pub end: Point,
    pub width: f64,
    pub layer: String,
    pub net: u32,
    pub uuid: String,
}

/// A curved track segment through `mid`.
#[derive(Debug, Clone)]
pub struct TrackArc {
    pub start: Point,
    pub mid: Point,
    pub end: Point,
    pub width: f64,
    pub layer: String,
    pub net: u32,
    pub uuid: String,
}

#[derive(Debug, Clone)]
pub struct Via {
    pub at: Point,
    /// Pad (annular) diameter.
    pub size: f64,
    pub drill: f64,
    pub layers: (String, String),
    pub net: u32,
    pub uuid: String,
}

#[derive(Debug, Clone)]
pub struct Zone {
    pub net: u32,
    pub net_name: String,
    pub layers: Vec<String>,
    pub uuid: String,
    pub name: Option<String>,
    pub priority: Option<u32>,
    /// Hatch display pitch (`(hatch edge …)`).
    pub hatch_pitch: f64,
    pub connect_pads: ZoneConnect,
    /// Zone-to-pad clearance.
    pub connect_clearance: f64,
    pub min_thickness: f64,
    pub fill: ZoneFill,
    /// Zone outline.
    pub polygon: Vec<Point>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneConnect {
    Thermal,
    Solid,
    ThruHoleOnly,
    None,
}

/// Fill settings only — computed fill polygons are KiCad's job, not the
/// generator's.
#[derive(Debug, Clone)]
pub struct ZoneFill {
    pub enabled: bool,
    pub thermal_gap: f64,
    pub thermal_bridge_width: f64,
}

impl Default for ZoneFill {
    fn default() -> Self {
        Self {
            enabled: true,
            thermal_gap: 0.5,
            thermal_bridge_width: 0.5,
        }
    }
}

/// Board-level graphic items.
#[derive(Debug, Clone)]
pub enum Graphic {
    Line {
        start: Point,
        end: Point,
        stroke: Stroke,
        layer: String,
        uuid: String,
    },
    Rect {
        start: Point,
        end: Point,
        stroke: Stroke,
        fill: bool,
        layer: String,
        uuid: String,
    },
    Circle {
        center: Point,
        /// A point on the circumference, KiCad's radius encoding.
        end: Point,
        stroke: Stroke,
        fill: bool,
        layer: String,
        uuid: String,
    },
    Arc {
        start: Point,
        mid: Point,
        end: Point,
        stroke: Stroke,
        layer: String,
        uuid: String,
    },
    Poly {
        pts: Vec<Point>,
        stroke: Stroke,
        fill: bool,
        layer: String,
        uuid: String,
    },
    Text {
        text: String,
        at: At,
        layer: String,
        /// Font (width, height).
        size: (f64, f64),
        thickness: f64,
        uuid: String,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Stroke {
    pub width: f64,
}

impl Stroke {
    pub fn solid(width: f64) -> Self {
        Self { width }
    }
}

/// Deterministic well-formed UUIDs, so regenerating a board reproduces the
/// same file byte for byte.
#[derive(Debug, Default)]
pub struct UuidGen(u64);

impl UuidGen {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn next_uuid(&mut self) -> String {
        self.0 += 1;
        format!("00000000-0000-4000-8000-{:012x}", self.0)
    }
}
