//! The board as parsed from a `.kicad_pcb` file: only the fields that
//! produce STEP geometry, in flat arrays, in KiCad's own y-down millimetre
//! frame. Everything is borrowed from the source text.

use std::borrow::Cow;

use crate::Error;
use crate::geom::{Vec2, Vec3, bezier_point, ccw_sweep, circle_center, rotate_kicad};
use crate::sexpr::{Parser, unescape};

pub struct Board<'a> {
    pub(crate) version: u32,
    pub(crate) thickness: f64,
    pub(crate) copper_layers: Vec<&'a str>,
    pub(crate) stackup: Option<Stackup>,
    pub(crate) aux_origin: Vec2,
    pub(crate) grid_origin: Vec2,
    /// Board-wide via fill and cap defaults from `(setup ...)`.
    pub(crate) fill_vias: bool,
    pub(crate) cap_vias: bool,
    pub(crate) footprints: Vec<Footprint<'a>>,
    pub(crate) models: Vec<ModelRef<'a>>,
    pub(crate) holes: Vec<Hole>,
    pub(crate) vias: Vec<Via>,
    pub(crate) edges: Vec<RawEdge>,
    pub(crate) embedded: Vec<Embedded<'a>>,
    /// Text variables the title block defines: TITLE, REVISION, COMMENT1..
    pub(crate) title_block: Vec<(String, String)>,
    pub(crate) pads: Vec<Pad>,
    /// Custom pad primitives, in pad-local coordinates.
    pub(crate) primitives: Vec<Primitive>,
    pub(crate) primitive_points: Vec<Vec2>,
    pub(crate) tracks: Vec<Track>,
    pub(crate) fills: Vec<Fill>,
    /// Polygon points of every zone fill, referenced by range.
    pub(crate) fill_points: Vec<Vec2>,
    /// Graphics and text on the silkscreen and solder mask layers.
    pub(crate) shapes: Vec<Shape>,
    pub(crate) shape_points: Vec<Vec2>,
    pub(crate) texts: Vec<Text>,
    /// Solder mask expansion around pads from `(setup ...)`, in mm.
    pub(crate) mask_expansion: f64,
    /// Whether vias are tented by default on the front and back.
    pub(crate) tent: [bool; 2],
    /// Net ids by the name or number written in the file; 0 is no net.
    nets: std::collections::HashMap<&'a str, u32>,
}

/// A layer drawn on the outside of the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tech {
    FrontSilk,
    BackSilk,
    FrontMask,
    BackMask,
}

impl Tech {
    fn from_layer(name: &str) -> Option<Self> {
        match name {
            "F.SilkS" => Some(Self::FrontSilk),
            "B.SilkS" => Some(Self::BackSilk),
            "F.Mask" => Some(Self::FrontMask),
            "B.Mask" => Some(Self::BackMask),
            _ => None,
        }
    }

    pub(crate) fn front(self) -> bool {
        matches!(self, Self::FrontSilk | Self::FrontMask)
    }

    pub(crate) fn silk(self) -> bool {
        matches!(self, Self::FrontSilk | Self::BackSilk)
    }
}

/// A graphic on a silkscreen or mask layer, in board coordinates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shape {
    pub(crate) layer: Tech,
    pub(crate) kind: ShapeKind,
    /// Stroke width; zero for an unstroked filled shape.
    pub(crate) width: f64,
    pub(crate) filled: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ShapeKind {
    Line {
        a: Vec2,
        b: Vec2,
    },
    Arc {
        a: Vec2,
        mid: Vec2,
        b: Vec2,
    },
    Circle {
        center: Vec2,
        radius: f64,
    },
    /// Indices into `shape_points`; rectangles and curves are polygons.
    Poly {
        points: (u32, u32),
    },
}

/// A text on a silkscreen or mask layer, in board coordinates.
#[derive(Debug, Clone)]
pub(crate) struct Text {
    pub(crate) text: String,
    pub(crate) at: Vec2,
    pub(crate) layer: Tech,
    pub(crate) style: crate::font::TextStyle,
}

/// Copper and dielectric layers top to bottom, plus the mask colour.
pub(crate) struct Stackup {
    pub(crate) layers: Vec<StackLayer>,
    pub(crate) front_mask_color: Option<[f64; 3]>,
    pub(crate) back_mask_color: Option<[f64; 3]>,
    pub(crate) front_silk_color: Option<[f64; 3]>,
    pub(crate) back_silk_color: Option<[f64; 3]>,
    pub(crate) core_color: Option<[f64; 3]>,
}

#[derive(Clone, Copy)]
pub(crate) struct StackLayer {
    pub(crate) copper: bool,
    pub(crate) thickness: f64,
}

pub(crate) struct Footprint<'a> {
    pub(crate) reference: &'a str,
    pub(crate) at: Vec2,
    pub(crate) rotation: f64,
    pub(crate) back: bool,
    pub(crate) dnp: bool,
    pub(crate) unspecified: bool,
    pub(crate) models: std::ops::Range<u32>,
}

/// The front and back answer of a `(tenting ...)`-style setting, in the
/// old `front back none` form or the `(front yes) (back no)` form.
/// Consumes the setting's list.
fn parse_front_back<'a>(p: &mut Parser<'a>) -> Result<[Option<bool>; 2], Error> {
    let mut out = [None, None];
    while let Some(side) = p.atom()? {
        match side {
            "front" => out[0] = Some(true),
            "back" => out[1] = Some(true),
            "none" => out = [None, None],
            _ => {}
        }
    }
    while let Some(side) = p.open()? {
        let value = parse_yes_no(p.atom()?);
        match side {
            "front" => out[0] = value,
            "back" => out[1] = value,
            _ => {}
        }
        p.skip()?;
    }
    Ok(out)
}

pub(crate) struct ModelRef<'a> {
    pub(crate) name: &'a str,
    pub(crate) offset: Vec3,
    pub(crate) rotate: Vec3,
    pub(crate) scale: Vec3,
}

impl ModelRef<'_> {
    pub(crate) fn path(&self) -> Cow<'_, str> {
        unescape(self.name)
    }
}

/// A drill: a circle, or a stadium of radius `r` swept from `a` to `b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hole {
    pub(crate) a: Vec2,
    pub(crate) b: Vec2,
    pub(crate) r: f64,
    pub(crate) machining: Machining,
    /// Net of the plated pad drilled by it; 0 for an unplated hole.
    pub(crate) net: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Via {
    pub(crate) at: Vec2,
    pub(crate) drill: f64,
    /// Copper layer indices, top first.
    pub(crate) top: u32,
    pub(crate) bottom: u32,
    /// Filled or capped: the bore is solid and is not cut.
    pub(crate) filled: bool,
    pub(crate) machining: Machining,
    /// Annular ring diameter.
    pub(crate) size: f64,
    pub(crate) net: u32,
    pub(crate) remove_unused: bool,
    pub(crate) keep_ends: bool,
    /// Front and back tenting stated on the via; the board's default
    /// applies where absent.
    pub(crate) tented: [Option<bool>; 2],
}

/// A pad's copper, in board coordinates. The shape sits at `at + offset`
/// (offset rotated with the pad); the hole, if any, sits at `at`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pad {
    pub(crate) at: Vec2,
    pub(crate) rotation: f64,
    pub(crate) shape: PadShape,
    pub(crate) size: Vec2,
    pub(crate) offset: Vec2,
    /// Copper layers the pad is on, one bit per copper layer index.
    pub(crate) layers: u64,
    pub(crate) kind: PadKind,
    pub(crate) net: u32,
    pub(crate) has_hole: bool,
    /// Drill size; zero without a hole.
    pub(crate) drill: Vec2,
    pub(crate) remove_unused: bool,
    pub(crate) keep_ends: bool,
    pub(crate) primitives: std::ops::Range<u32>,
    /// Whether the pad opens the front and back solder mask.
    pub(crate) mask: [bool; 2],
    /// Its own or its footprint's mask margin; the board's applies otherwise.
    pub(crate) mask_margin: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PadKind {
    Smd,
    ThroughHole,
    NoPlate,
    Connect,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PadShape {
    Circle,
    Rect,
    Oval,
    Trapezoid {
        delta: Vec2,
    },
    /// Rounded and optionally chamfered rectangle; ratios are of the
    /// shorter side, corners are top-left, top-right, bottom-left,
    /// bottom-right as KiCad orders them.
    RoundRect {
        round_ratio: f64,
        chamfer_ratio: f64,
        chamfer: [bool; 4],
    },
    Custom {
        anchor_circle: bool,
    },
}

/// A custom pad primitive in pad-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Primitive {
    Line {
        a: Vec2,
        b: Vec2,
        width: f64,
    },
    Arc {
        a: Vec2,
        mid: Vec2,
        b: Vec2,
        width: f64,
    },
    Circle {
        center: Vec2,
        radius: f64,
        width: f64,
        filled: bool,
    },
    Rect {
        a: Vec2,
        b: Vec2,
        width: f64,
        filled: bool,
    },
    Poly {
        /// Range into `Board::primitive_points`.
        points: (u32, u32),
        width: f64,
        filled: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Track {
    pub(crate) a: Vec2,
    /// Arc midpoint; equal to `a` for a straight segment.
    pub(crate) mid: Vec2,
    pub(crate) b: Vec2,
    pub(crate) width: f64,
    pub(crate) layer: u32,
    pub(crate) net: u32,
}

/// One filled zone polygon on one copper layer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Fill {
    pub(crate) layer: u32,
    pub(crate) net: u32,
    pub(crate) points: std::ops::Range<u32>,
}

/// Material removed around a drill after plating.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Machining {
    pub(crate) front: Option<Mouth>,
    pub(crate) back: Option<Mouth>,
    pub(crate) backdrills: [Option<Backdrill>; 2],
}

/// Post-machining at one face. Sizes are radii; depth is measured from
/// the outer copper surface, as KiCad does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Mouth {
    Counterbore {
        r: f64,
        depth: f64,
    },
    /// `depth` of `None` runs the cone to its apex.
    Countersink {
        r: f64,
        depth: Option<f64>,
        half_angle: f64,
    },
}

/// A larger drill from one outer copper layer down through `end`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Backdrill {
    pub(crate) r: f64,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RawEdge {
    Line { a: Vec2, b: Vec2 },
    Arc { a: Vec2, mid: Vec2, b: Vec2 },
    Circle { center: Vec2, radius: f64 },
}

pub(crate) struct Embedded<'a> {
    pub(crate) name: &'a str,
    /// Base64 text as written, whitespace included.
    pub(crate) data: &'a [u8],
}

pub(crate) const DEFAULT_THICKNESS: f64 = 1.6;
/// First file format version carrying via fill, cap and cover state.
const VIA_PROTECTION_VERSION: u32 = 20250228;
const DEFAULT_COPPER_THICKNESS: f64 = 0.035;
const DEFAULT_MASK_THICKNESS: f64 = 0.01;
const DEFAULT_MASK_COLOR: [f64; 3] = [0.08, 0.2, 0.14];
const DEFAULT_CORE_COLOR: [f64; 3] = [0.42, 0.45, 0.29];
const MASK_DARKEN: f64 = 0.2;

/// Z extents derived from the stackup the way KiCad's exporter derives them.
pub(crate) struct Physical {
    /// The body spans `[0, body_top]`: dielectrics plus inner copper.
    pub(crate) body_top: f64,
    pub(crate) front_copper: f64,
    pub(crate) back_copper: f64,
    /// `(z_min, z_max)` of each copper layer, top first.
    pub(crate) copper_z: Vec<(f64, f64)>,
}

impl<'a> Board<'a> {
    pub fn parse(src: &'a [u8]) -> Result<Self, Error> {
        Self::parse_with(src, true, true)
    }

    /// Parse a board; with `copper` false, pad copper, tracks and zone
    /// fills are skipped, which is a fifth of the parse time on large
    /// boards and all an export without copper needs; with `graphics`
    /// false, silkscreen and mask graphics and text are skipped.
    pub fn parse_with(src: &'a [u8], copper: bool, graphics: bool) -> Result<Self, Error> {
        std::str::from_utf8(src).map_err(|_| Error::Utf8)?;
        let mut p = Parser::new(src);
        let mut board = Board {
            version: 0,
            thickness: DEFAULT_THICKNESS,
            copper_layers: Vec::new(),
            stackup: None,
            aux_origin: Vec2::ZERO,
            grid_origin: Vec2::ZERO,
            fill_vias: false,
            cap_vias: false,
            footprints: Vec::new(),
            models: Vec::new(),
            holes: Vec::new(),
            vias: Vec::new(),
            edges: Vec::new(),
            embedded: Vec::new(),
            title_block: Vec::new(),
            pads: Vec::new(),
            primitives: Vec::new(),
            primitive_points: Vec::new(),
            tracks: Vec::new(),
            fills: Vec::new(),
            fill_points: Vec::new(),
            shapes: Vec::new(),
            shape_points: Vec::new(),
            texts: Vec::new(),
            mask_expansion: 0.0,
            tent: [false; 2],
            nets: std::collections::HashMap::new(),
        };
        let mut scratch = FootprintScratch {
            copper,
            graphics,
            ..FootprintScratch::default()
        };

        if p.open()? != Some("kicad_pcb") {
            return Err(Error::Syntax("expected kicad_pcb"));
        }
        while let Some(name) = p.open()? {
            match name {
                "version" => {
                    board.version = p.atom()?.and_then(|v| v.parse().ok()).unwrap_or(0);
                    p.skip()?;
                }
                "general" => board.parse_general(&mut p)?,
                "layers" => board.parse_layers(&mut p)?,
                "setup" => board.parse_setup(&mut p)?,
                "footprint" => board.parse_footprint(&mut p, &mut scratch)?,
                "via" => board.parse_via(&mut p)?,
                "segment" | "arc" if copper => board.parse_track(&mut p, name == "arc")?,
                "zone" if copper => board.parse_zone(&mut p)?,
                "embedded_files" => board.parse_embedded_files(&mut p)?,
                "title_block" if graphics => board.parse_title_block(&mut p)?,
                "gr_text" if graphics => {
                    let value = p.atom()?.unwrap_or("");
                    board.texts.extend(parse_text(&mut p, value, false)?);
                }
                _ => {
                    let mut shapes =
                        graphics.then_some((&mut board.shapes, &mut board.shape_points));
                    parse_graphic(&mut p, name, "gr_", &mut board.edges, &mut shapes)?
                }
            }
        }
        if !p.at_end() {
            return Err(Error::Syntax("trailing data after board"));
        }
        Ok(board)
    }

    /// The title block as the text variables KiCad derives from it.
    fn parse_title_block(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        while let Some(name) = p.open()? {
            let key = match name {
                "title" => "TITLE".to_owned(),
                "date" => "ISSUE_DATE".to_owned(),
                "rev" => "REVISION".to_owned(),
                "company" => "COMPANY".to_owned(),
                "comment" => format!("COMMENT{}", p.atom()?.unwrap_or("")),
                _ => {
                    p.skip()?;
                    continue;
                }
            };
            let value = unescape(p.atom()?.unwrap_or("")).into_owned();
            self.title_block.push((key, value));
            p.skip()?;
        }
        Ok(())
    }

    fn parse_general(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        while let Some(name) = p.open()? {
            if name == "thickness" {
                self.thickness = p.f64("board thickness")?;
            }
            p.skip()?;
        }
        Ok(())
    }

    fn parse_layers(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        while p.open()?.is_some() {
            if let Some(layer) = p.atom()?
                && layer.ends_with(".Cu")
            {
                self.copper_layers.push(layer);
            }
            p.skip()?;
        }
        Ok(())
    }

    fn parse_setup(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        while let Some(name) = p.open()? {
            match name {
                "stackup" => self.parse_stackup(p)?,
                "aux_axis_origin" => {
                    self.aux_origin = p.xy("aux_axis_origin")?;
                    p.skip()?;
                }
                "grid_origin" => {
                    self.grid_origin = p.xy("grid_origin")?;
                    p.skip()?;
                }
                "filling" => {
                    self.fill_vias = p.atom()? == Some("yes");
                    p.skip()?;
                }
                "capping" => {
                    self.cap_vias = p.atom()? == Some("yes");
                    p.skip()?;
                }
                "pad_to_mask_clearance" => {
                    self.mask_expansion = p.f64("pad_to_mask_clearance")?;
                    p.skip()?;
                }
                "tenting" => {
                    let [front, back] = parse_front_back(p)?;
                    self.tent = [front.unwrap_or(false), back.unwrap_or(false)];
                }
                _ => p.skip()?,
            }
        }
        Ok(())
    }

    fn parse_stackup(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        let mut stackup = Stackup {
            layers: Vec::new(),
            front_mask_color: None,
            back_mask_color: None,
            front_silk_color: None,
            back_silk_color: None,
            core_color: None,
        };
        while let Some(name) = p.open()? {
            if name != "layer" {
                p.skip()?;
                continue;
            }
            let layer_name = p.atom()?.unwrap_or("");
            let mut kind = "";
            let mut thickness = 0.0;
            let mut color = None;
            while let Some(field) = p.open()? {
                match field {
                    "type" => kind = p.atom()?.unwrap_or(""),
                    "thickness" => thickness += p.f64("stackup thickness")?,
                    "color" => color = p.atom()?,
                    _ => {}
                }
                p.skip()?;
            }
            match kind {
                "copper" => stackup.layers.push(StackLayer {
                    copper: true,
                    thickness,
                }),
                "core" | "prepreg" => {
                    stackup.layers.push(StackLayer {
                        copper: false,
                        thickness,
                    });
                    if kind == "core"
                        && let Some(c) = color.and_then(mask_color)
                    {
                        stackup.core_color = Some(c);
                    }
                }
                _ => {
                    let slot = match layer_name {
                        "F.Mask" => &mut stackup.front_mask_color,
                        "B.Mask" => &mut stackup.back_mask_color,
                        "F.SilkS" => &mut stackup.front_silk_color,
                        "B.SilkS" => &mut stackup.back_silk_color,
                        _ => continue,
                    };
                    *slot = color.and_then(mask_color);
                }
            }
        }
        if stackup.layers.iter().any(|l| l.copper) {
            self.stackup = Some(stackup);
        }
        Ok(())
    }

    fn parse_footprint(
        &mut self,
        p: &mut Parser<'a>,
        scratch: &mut FootprintScratch<'a>,
    ) -> Result<(), Error> {
        p.atom()?; // library id
        let mut fp = Footprint {
            reference: "",
            at: Vec2::ZERO,
            rotation: 0.0,
            back: false,
            dnp: false,
            unspecified: true,
            models: self.models.len() as u32..self.models.len() as u32,
        };
        scratch.holes.clear();
        scratch.edges.clear();
        scratch.pads.clear();
        scratch.shapes.clear();
        scratch.shape_points.clear();
        scratch.texts.clear();
        scratch.value = Cow::Borrowed("");
        let mut mask_margin = None;

        while let Some(name) = p.open()? {
            match name {
                "at" => {
                    fp.at = p.xy("footprint at")?;
                    fp.rotation = p.f64_or(0.0)?;
                    p.skip()?;
                }
                "layer" => {
                    fp.back = p.atom()?.is_some_and(|l| l.starts_with("B."));
                    p.skip()?;
                }
                "attr" => {
                    while let Some(a) = p.atom()? {
                        match a {
                            "smd" | "through_hole" => fp.unspecified = false,
                            "dnp" => fp.dnp = true,
                            _ => {}
                        }
                    }
                    p.skip()?;
                }
                "property" => {
                    let key = p.atom()?;
                    let value = p.atom()?.unwrap_or("");
                    match key {
                        Some("Reference") => fp.reference = value,
                        Some("Value") => scratch.value = unescape(value),
                        _ => {}
                    }
                    if scratch.graphics {
                        scratch.texts.extend(parse_text(p, value, true)?);
                    } else {
                        p.skip()?;
                    }
                }
                "fp_text" => {
                    let kind = p.atom()?;
                    let value = p.atom()?.unwrap_or("");
                    if kind == Some("reference") && fp.reference.is_empty() {
                        fp.reference = value;
                    }
                    if kind == Some("value") && scratch.value.is_empty() {
                        scratch.value = unescape(value);
                    }
                    if scratch.graphics {
                        scratch.texts.extend(parse_text(p, value, true)?);
                    } else {
                        p.skip()?;
                    }
                }
                "solder_mask_margin" => {
                    mask_margin = Some(p.f64("solder_mask_margin")?);
                    p.skip()?;
                }
                "pad" => {
                    let (hole, pad) = parse_pad(
                        p,
                        fp.rotation,
                        &self.copper_layers,
                        &mut self.primitives,
                        &mut self.primitive_points,
                    )?;
                    let net = match &pad {
                        Some((pad, net)) if scratch.copper && pad.kind != PadKind::NoPlate => {
                            self.net_id(*net)
                        }
                        _ => 0,
                    };
                    if let Some(mut hole) = hole {
                        hole.net = net;
                        scratch.holes.push(hole);
                    }
                    // Pads are copper, and also what opens the mask.
                    if let Some((mut pad, _)) = pad
                        && (scratch.copper || scratch.graphics)
                    {
                        pad.net = net;
                        scratch.pads.push(pad);
                    }
                }
                "model" => {
                    if let Some(model) = parse_model(p)? {
                        self.models.push(model);
                    }
                }
                "embedded_files" => self.parse_embedded_files(p)?,
                _ => {
                    let mut shapes = scratch
                        .graphics
                        .then_some((&mut scratch.shapes, &mut scratch.shape_points));
                    parse_graphic(p, name, "fp_", &mut scratch.edges, &mut shapes)?
                }
            }
        }

        let place = |local: Vec2| fp.at + rotate_kicad(local, fp.rotation);
        let base = self.shape_points.len() as u32;
        self.shape_points
            .extend(scratch.shape_points.iter().map(|p| place(*p)));
        for shape in &scratch.shapes {
            let kind = match shape.kind {
                ShapeKind::Line { a, b } => ShapeKind::Line {
                    a: place(a),
                    b: place(b),
                },
                ShapeKind::Arc { a, mid, b } => ShapeKind::Arc {
                    a: place(a),
                    mid: place(mid),
                    b: place(b),
                },
                ShapeKind::Circle { center, radius } => ShapeKind::Circle {
                    center: place(center),
                    radius,
                },
                ShapeKind::Poly { points } => ShapeKind::Poly {
                    points: (points.0 + base, points.1 + base),
                },
            };
            self.shapes.push(Shape { kind, ..*shape });
        }
        for text in &scratch.texts {
            let content = text
                .text
                .replace("${REFERENCE}", fp.reference)
                .replace("${VALUE}", &scratch.value);
            self.texts.push(Text {
                text: content,
                at: place(text.at),
                ..text.clone()
            });
        }
        for hole in &scratch.holes {
            self.holes.push(Hole {
                a: place(hole.a),
                b: place(hole.b),
                ..*hole
            });
        }
        for edge in &scratch.edges {
            self.edges.push(match *edge {
                RawEdge::Line { a, b } => RawEdge::Line {
                    a: place(a),
                    b: place(b),
                },
                RawEdge::Arc { a, mid, b } => RawEdge::Arc {
                    a: place(a),
                    mid: place(mid),
                    b: place(b),
                },
                RawEdge::Circle { center, radius } => RawEdge::Circle {
                    center: place(center),
                    radius,
                },
            });
        }
        for pad in &scratch.pads {
            self.pads.push(Pad {
                at: place(pad.at),
                mask_margin: pad.mask_margin.or(mask_margin),
                ..pad.clone()
            });
        }
        fp.models.end = self.models.len() as u32;
        self.footprints.push(fp);
        Ok(())
    }

    fn parse_via(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        let mut via = Via {
            at: Vec2::ZERO,
            drill: 0.0,
            top: 0,
            bottom: self.copper_layers.len().saturating_sub(1) as u32,
            filled: false,
            machining: Machining::default(),
            size: 0.0,
            net: 0,
            remove_unused: false,
            keep_ends: false,
            tented: [None; 2],
        };
        let mut placed = false;
        let mut filling = None;
        let mut capping = None;
        while let Some(name) = p.open()? {
            match name {
                "at" => {
                    via.at = p.xy("via at")?;
                    placed = true;
                }
                "drill" => via.drill = p.f64("via drill")?.abs(),
                "size" => via.size = p.f64("via size")?.abs(),
                "net" => {
                    let atom = p.atom()?;
                    via.net = self.net_id(atom);
                }
                "remove_unused_layers" => via.remove_unused = p.atom()? != Some("no"),
                "keep_end_layers" => via.keep_ends = p.atom()? != Some("no"),
                "layers" => {
                    let top = p.atom()?;
                    let bottom = p.atom()?;
                    match (self.copper_index(top), self.copper_index(bottom)) {
                        (Some(a), Some(b)) => {
                            via.top = a.min(b);
                            via.bottom = a.max(b);
                        }
                        _ => return Err(Error::Syntax("via references an unknown copper layer")),
                    }
                }
                "filling" => filling = parse_yes_no(p.atom()?),
                "capping" => capping = parse_yes_no(p.atom()?),
                "tenting" => {
                    via.tented = parse_front_back(p)?;
                    continue;
                }
                _ => {
                    if parse_machining(p, name, &self.copper_layers, &mut via.machining)? {
                        continue;
                    }
                }
            }
            p.skip()?;
        }
        // Files older than the via protection format default to unfilled
        // rather than inheriting the board setting, as KiCad reads them.
        let legacy = self.version < VIA_PROTECTION_VERSION;
        let default_fill = !legacy && self.fill_vias;
        let default_cap = !legacy && self.cap_vias;
        via.filled = filling.unwrap_or(default_fill) || capping.unwrap_or(default_cap);
        if placed && via.drill > 0.0 {
            self.vias.push(via);
        }
        Ok(())
    }

    fn parse_track(&mut self, p: &mut Parser<'a>, arc: bool) -> Result<(), Error> {
        let mut track = Track {
            a: Vec2::ZERO,
            mid: Vec2::ZERO,
            b: Vec2::ZERO,
            width: 0.0,
            layer: u32::MAX,
            net: 0,
        };
        let mut has_mid = false;
        while let Some(name) = p.open()? {
            match name {
                "start" => track.a = p.xy("track start")?,
                "mid" => {
                    track.mid = p.xy("track mid")?;
                    has_mid = true;
                }
                "end" => track.b = p.xy("track end")?,
                "width" => track.width = p.f64("track width")?,
                "layer" => track.layer = self.copper_index(p.atom()?).unwrap_or(u32::MAX),
                "net" => {
                    let atom = p.atom()?;
                    track.net = self.net_id(atom);
                }
                _ => {}
            }
            p.skip()?;
        }
        if !arc || !has_mid {
            track.mid = track.a;
        }
        if track.layer != u32::MAX && track.width > 0.0 {
            self.tracks.push(track);
        }
        Ok(())
    }

    fn parse_zone(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        let mut net = 0;
        while let Some(name) = p.open()? {
            match name {
                "net" => {
                    let atom = p.atom()?;
                    net = self.net_id(atom);
                    p.skip()?;
                }
                "filled_polygon" => {
                    let mut layer = u32::MAX;
                    let start = self.fill_points.len() as u32;
                    while let Some(field) = p.open()? {
                        match field {
                            "layer" => layer = self.copper_index(p.atom()?).unwrap_or(u32::MAX),
                            "pts" => {
                                let mut pts = Vec::new();
                                parse_pts(p, &mut pts)?;
                                flatten_pts(&pts, &mut self.fill_points);
                                continue;
                            }
                            _ => {}
                        }
                        p.skip()?;
                    }
                    let end = self.fill_points.len() as u32;
                    if layer != u32::MAX && end - start >= 3 {
                        self.fills.push(Fill {
                            layer,
                            net,
                            points: start..end,
                        });
                    } else {
                        self.fill_points.truncate(start as usize);
                    }
                }
                _ => p.skip()?,
            }
        }
        Ok(())
    }

    /// Id of the net named by a `(net ...)` list's first atom. KiCad writes
    /// a number in older files and a name in newer ones; both intern to
    /// the same id space here. Unconnected items are net 0.
    fn net_id(&mut self, atom: Option<&'a str>) -> u32 {
        match atom {
            None | Some("0") | Some("") => 0,
            Some(name) => {
                let next = self.nets.len() as u32 + 1;
                *self.nets.entry(name).or_insert(next)
            }
        }
    }

    fn copper_index(&self, layer: Option<&str>) -> Option<u32> {
        let layer = layer?;
        self.copper_layers
            .iter()
            .position(|c| *c == layer)
            .map(|i| i as u32)
    }

    fn parse_embedded_files(&mut self, p: &mut Parser<'a>) -> Result<(), Error> {
        while let Some(name) = p.open()? {
            if name != "file" {
                p.skip()?;
                continue;
            }
            let mut file_name = None;
            let mut data = None;
            while let Some(field) = p.open()? {
                match field {
                    "name" => file_name = p.atom()?,
                    "data" => data = p.bar()?,
                    _ => {}
                }
                p.skip()?;
            }
            if let (Some(name), Some(data)) = (file_name, data) {
                self.embedded.push(Embedded { name, data });
            }
        }
        Ok(())
    }

    /// Copper and dielectric layers, top to bottom, from the stackup or
    /// KiCad's default construction for boards without one.
    fn physical_layers(&self) -> Vec<StackLayer> {
        if let Some(stackup) = &self.stackup {
            return stackup.layers.clone();
        }
        let copper_count = self.copper_layers.len().max(2);
        let dielectric = (self.thickness
            - DEFAULT_COPPER_THICKNESS * copper_count as f64
            - DEFAULT_MASK_THICKNESS * 2.0)
            / (copper_count - 1) as f64;
        let mut layers = Vec::with_capacity(copper_count * 2 - 1);
        for i in 0..copper_count {
            if i > 0 {
                layers.push(StackLayer {
                    copper: false,
                    thickness: dielectric,
                });
            }
            layers.push(StackLayer {
                copper: true,
                thickness: DEFAULT_COPPER_THICKNESS,
            });
        }
        layers
    }

    pub(crate) fn physical(&self) -> Physical {
        let layers = self.physical_layers();
        let copper_count = layers.iter().filter(|l| l.copper).count();
        let mut copper_z = Vec::with_capacity(copper_count);
        // Walk bottom-up. Outer copper sits outside the body; inner copper
        // is part of it.
        let mut z = 0.0;
        let mut seen_copper = 0;
        for layer in layers.iter().rev() {
            if layer.copper {
                seen_copper += 1;
                if seen_copper == 1 {
                    copper_z.push((-layer.thickness, 0.0));
                } else if seen_copper == copper_count {
                    copper_z.push((z, z + layer.thickness));
                } else {
                    copper_z.push((z, z + layer.thickness));
                    z += layer.thickness;
                }
            } else {
                z += layer.thickness;
            }
        }
        copper_z.reverse();
        let mut body_top = z;
        if body_top < 0.01 {
            body_top = if self.thickness > 0.01 {
                self.thickness
            } else {
                DEFAULT_THICKNESS
            };
        }
        let front_copper = layers
            .iter()
            .find(|l| l.copper)
            .map_or(0.0, |l| l.thickness);
        let back_copper = layers
            .iter()
            .rev()
            .find(|l| l.copper)
            .map_or(0.0, |l| l.thickness);
        Physical {
            body_top,
            front_copper,
            back_copper,
            copper_z,
        }
    }

    /// Board body colour as written to STEP: the front mask colour darkened
    /// the way KiCad does, then encoded from linear to sRGB by OCCT.
    pub(crate) fn body_color(&self) -> [f64; 3] {
        self.mask_color(true)
    }

    /// The body colour when the mask is exported as its own layer: the
    /// stackup's core colour, or KiCad's default board green.
    pub(crate) fn core_color(&self) -> [f64; 3] {
        self.stackup
            .as_ref()
            .and_then(|s| s.core_color)
            .unwrap_or(DEFAULT_CORE_COLOR)
            .map(linear_to_srgb)
    }

    /// Solder mask colour of one side, darkened as KiCad darkens it.
    pub(crate) fn mask_color(&self, front: bool) -> [f64; 3] {
        let stackup = self.stackup.as_ref();
        stackup
            .and_then(|s| {
                if front {
                    s.front_mask_color
                } else {
                    s.back_mask_color
                }
            })
            .map(|c| c.map(|v| v * (1.0 - MASK_DARKEN)))
            .unwrap_or(DEFAULT_MASK_COLOR)
            .map(linear_to_srgb)
    }

    /// Silkscreen colour of one side; white unless the stackup says.
    pub(crate) fn silk_color(&self, front: bool) -> [f64; 3] {
        let stackup = self.stackup.as_ref();
        stackup
            .and_then(|s| {
                if front {
                    s.front_silk_color
                } else {
                    s.back_silk_color
                }
            })
            .unwrap_or([1.0; 3])
            .map(linear_to_srgb)
    }
}

#[derive(Default)]
struct FootprintScratch<'a> {
    copper: bool,
    graphics: bool,
    holes: Vec<Hole>,
    edges: Vec<RawEdge>,
    pads: Vec<Pad>,
    shapes: Vec<Shape>,
    shape_points: Vec<Vec2>,
    texts: Vec<Text>,
    /// The footprint's Value field, for `${VALUE}` in its texts.
    value: Cow<'a, str>,
}

pub(crate) fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn mask_color(name: &str) -> Option<[f64; 3]> {
    if let Some(hex) = name.strip_prefix('#') {
        if hex.len() < 6 {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some([channel(0)?, channel(2)?, channel(4)?].map(|c| c as f64 / 255.0));
    }
    let rgb: [u8; 3] = match name {
        "Green" => [20, 51, 36],
        "Red" => [181, 19, 21],
        "Blue" => [2, 59, 162],
        "Purple" => [32, 2, 53],
        "Black" => [11, 11, 11],
        "White" => [245, 245, 245],
        "Yellow" => [194, 195, 0],
        "User defined" => [128, 128, 128],
        _ => return None,
    };
    Some(rgb.map(|c| c as f64 / 255.0))
}

/// One pad: its drill (footprint-local, following
/// `PAD::GetEffectiveHoleShape`: centred on the pad position, a slot along
/// the pad's stored absolute orientation) and its copper.
fn parse_pad<'a>(
    p: &mut Parser<'a>,
    footprint_rotation: f64,
    copper_layers: &[&str],
    primitives: &mut Vec<Primitive>,
    primitive_points: &mut Vec<Vec2>,
) -> Result<ParsedPad<'a>, Error> {
    p.atom()?; // number
    let kind = match p.atom()? {
        Some("thru_hole") => PadKind::ThroughHole,
        Some("np_thru_hole") => PadKind::NoPlate,
        Some("connect") => PadKind::Connect,
        _ => PadKind::Smd,
    };
    let mut shape = match p.atom()? {
        Some("circle") => PadShape::Circle,
        Some("oval") => PadShape::Oval,
        Some("trapezoid") => PadShape::Trapezoid { delta: Vec2::ZERO },
        Some("roundrect") => PadShape::RoundRect {
            round_ratio: 0.25,
            chamfer_ratio: 0.0,
            chamfer: [false; 4],
        },
        Some("custom") => PadShape::Custom {
            anchor_circle: false,
        },
        _ => PadShape::Rect,
    };
    let mut at = Vec2::ZERO;
    let mut rotation = 0.0;
    let mut size = Vec2::ZERO;
    let mut offset = Vec2::ZERO;
    let mut drill = Vec2::ZERO;
    let mut layers = 0u64;
    let mut net_atom = None;
    let mut remove_unused = false;
    let mut keep_ends = false;
    let mut machining = Machining::default();
    let mut mask = [false; 2];
    let mut mask_margin = None;
    let first_primitive = primitives.len() as u32;
    while let Some(name) = p.open()? {
        match name {
            "at" => {
                at = p.xy("pad at")?;
                rotation = p.f64_or(0.0)?;
            }
            "size" => size = p.xy("pad size")?,
            "net" => net_atom = p.atom()?,
            "layers" => {
                while let Some(layer) = p.atom()? {
                    layers |= copper_layer_mask(layer, copper_layers);
                    match layer {
                        "F.Mask" => mask[0] = true,
                        "B.Mask" => mask[1] = true,
                        "*.Mask" | "F&B.Mask" => mask = [true; 2],
                        _ => {}
                    }
                }
            }
            "solder_mask_margin" => mask_margin = Some(p.f64("solder_mask_margin")?),
            "rect_delta" => {
                if let PadShape::Trapezoid { delta } = &mut shape {
                    *delta = p.xy("rect_delta")?;
                }
            }
            "roundrect_rratio" => {
                if let PadShape::RoundRect { round_ratio, .. } = &mut shape {
                    *round_ratio = p.f64("roundrect_rratio")?;
                }
            }
            "chamfer_ratio" => {
                if let PadShape::RoundRect { chamfer_ratio, .. } = &mut shape {
                    *chamfer_ratio = p.f64("chamfer_ratio")?;
                }
            }
            "chamfer" => {
                if let PadShape::RoundRect { chamfer, .. } = &mut shape {
                    while let Some(corner) = p.atom()? {
                        match corner {
                            "top_left" => chamfer[0] = true,
                            "top_right" => chamfer[1] = true,
                            "bottom_left" => chamfer[2] = true,
                            "bottom_right" => chamfer[3] = true,
                            _ => {}
                        }
                    }
                }
            }
            "remove_unused_layers" => remove_unused = p.atom()? != Some("no"),
            "keep_end_layers" => keep_ends = p.atom()? != Some("no"),
            "options" => {
                while let Some(field) = p.open()? {
                    if field == "anchor"
                        && let PadShape::Custom { anchor_circle } = &mut shape
                    {
                        *anchor_circle = p.atom()? == Some("circle");
                    }
                    p.skip()?;
                }
                continue;
            }
            "primitives" => {
                parse_primitives(p, primitives, primitive_points)?;
                continue;
            }
            "drill" => {
                match p.atom()? {
                    Some("oval") => {
                        drill.x = p.f64("drill width")?;
                        drill.y = p.f64_or(drill.x)?;
                    }
                    Some(size) => {
                        drill.x = size.parse().map_err(|_| Error::Number("drill"))?;
                        drill.y = drill.x;
                    }
                    None => {}
                }
                while let Some(field) = p.open()? {
                    if field == "offset" {
                        offset = p.xy("drill offset")?;
                    }
                    p.skip()?;
                }
                continue;
            }
            _ => {
                if parse_machining(p, name, copper_layers, &mut machining)? {
                    continue;
                }
            }
        }
        p.skip()?;
    }
    let has_hole = drill.x > 0.0 && drill.y > 0.0;
    let hole = has_hole.then(|| {
        let half = drill * 0.5;
        let (r, half_len) = if half.x > half.y {
            (half.y, Vec2::new(half.x - half.y, 0.0))
        } else {
            (half.x, Vec2::new(0.0, half.y - half.x))
        };
        // The pad's angle is absolute; the hole is placed with the
        // footprint, so only the pad's own share of it is applied here.
        let half_len = rotate_kicad(half_len, rotation - footprint_rotation);
        Hole {
            a: at - half_len,
            b: at + half_len,
            r,
            machining,
            net: 0,
        }
    });
    let pad = (layers != 0 && size.x > 0.0 && size.y > 0.0).then_some(Pad {
        at,
        rotation,
        shape,
        size,
        offset,
        layers,
        kind,
        net: 0,
        has_hole,
        drill,
        remove_unused,
        keep_ends,
        primitives: first_primitive..primitives.len() as u32,
        mask,
        mask_margin,
    });
    Ok((hole, pad.map(|pad| (pad, net_atom))))
}

/// Copper layers named by one `(layers ...)` entry: a layer name, `*.Cu`
/// for all of them, or `F&B.Cu` for the outer two.
fn copper_layer_mask(name: &str, copper_layers: &[&str]) -> u64 {
    let all = (1u64 << copper_layers.len().min(63)) - 1;
    match name {
        "*.Cu" => all,
        "F&B.Cu" => {
            let last = copper_layers.len().saturating_sub(1).min(63);
            1 | (1u64 << last)
        }
        _ => copper_layers
            .iter()
            .position(|c| *c == name)
            .map_or(0, |i| 1u64 << i.min(63)),
    }
}

fn parse_primitives(
    p: &mut Parser<'_>,
    out: &mut Vec<Primitive>,
    all_points: &mut Vec<Vec2>,
) -> Result<(), Error> {
    while let Some(kind) = p.open()? {
        let mut start = Vec2::ZERO;
        let mut mid = Vec2::ZERO;
        let mut end = Vec2::ZERO;
        let mut center = Vec2::ZERO;
        let mut width = 0.0;
        let mut filled = false;
        let mut points: Vec<Vec2> = Vec::new();
        while let Some(field) = p.open()? {
            match field {
                "start" => start = p.xy("start")?,
                "mid" => mid = p.xy("mid")?,
                "end" => end = p.xy("end")?,
                "center" => center = p.xy("center")?,
                "width" => width = p.f64("width")?,
                "fill" => filled = matches!(p.atom()?, Some("yes" | "solid")),
                "pts" => {
                    let mut pts = Vec::new();
                    parse_pts(p, &mut pts)?;
                    flatten_pts(&pts, &mut points);
                    continue;
                }
                _ => {}
            }
            p.skip()?;
        }
        match kind {
            "gr_line" => out.push(Primitive::Line {
                a: start,
                b: end,
                width,
            }),
            "gr_arc" => out.push(Primitive::Arc {
                a: start,
                mid,
                b: end,
                width,
            }),
            "gr_circle" => out.push(Primitive::Circle {
                center,
                radius: center.distance(end),
                width,
                filled,
            }),
            "gr_rect" => out.push(Primitive::Rect {
                a: start,
                b: end,
                width,
                filled,
            }),
            "gr_poly" if points.len() >= 3 => {
                let start = all_points.len() as u32;
                all_points.extend_from_slice(&points);
                out.push(Primitive::Poly {
                    points: (start, all_points.len() as u32),
                    width,
                    filled,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// A pad's drill, and its copper with the net atom still to intern.
type ParsedPad<'a> = (Option<Hole>, Option<(Pad, Option<&'a str>)>);

fn parse_yes_no(atom: Option<&str>) -> Option<bool> {
    match atom {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    }
}

/// Parse one post-machining or backdrill list if `name` is one, consuming
/// it. Returns `false`, consuming nothing, otherwise.
fn parse_machining(
    p: &mut Parser<'_>,
    name: &str,
    copper_layers: &[&str],
    machining: &mut Machining,
) -> Result<bool, Error> {
    match name {
        "front_post_machining" | "back_post_machining" => {
            let mode = p.atom()?.unwrap_or("");
            let mut size = 0.0;
            let mut depth = None;
            let mut angle = None;
            while let Some(field) = p.open()? {
                match field {
                    "size" => size = p.f64("post machining size")?,
                    "depth" => depth = Some(p.f64("post machining depth")?),
                    "angle" => angle = Some(p.f64("post machining angle")?),
                    _ => {}
                }
                p.skip()?;
            }
            let mouth = match mode {
                "counterbore" if size > 0.0 => {
                    depth.filter(|d| *d > 0.0).map(|depth| Mouth::Counterbore {
                        r: size * 0.5,
                        depth,
                    })
                }
                "countersink" if size > 0.0 => {
                    angle.filter(|a| *a > 0.0).map(|angle| Mouth::Countersink {
                        r: size * 0.5,
                        depth: depth.filter(|d| *d > 0.0),
                        half_angle: (angle * 0.5).to_radians(),
                    })
                }
                _ => None,
            };
            if name.starts_with("front") {
                machining.front = mouth;
            } else {
                machining.back = mouth;
            }
            Ok(true)
        }
        "backdrill" | "tertiary_drill" => {
            let mut size = 0.0;
            let mut layers = (None, None);
            while let Some(field) = p.open()? {
                match field {
                    "size" => size = p.f64("backdrill size")?,
                    "layers" => {
                        let index = |layer: Option<&str>| {
                            layer.and_then(|l| copper_layers.iter().position(|c| *c == l))
                        };
                        layers = (index(p.atom()?), index(p.atom()?));
                    }
                    _ => {}
                }
                p.skip()?;
            }
            if let (Some(start), Some(end)) = layers
                && size > 0.0
            {
                let slot = if name == "backdrill" { 0 } else { 1 };
                machining.backdrills[slot] = Some(Backdrill {
                    r: size * 0.5,
                    start: start as u32,
                    end: end as u32,
                });
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn parse_model<'a>(p: &mut Parser<'a>) -> Result<Option<ModelRef<'a>>, Error> {
    let Some(name) = p.atom()? else {
        p.skip()?;
        return Ok(None);
    };
    let mut model = ModelRef {
        name,
        offset: Vec3::ZERO,
        rotate: Vec3::ZERO,
        scale: Vec3::ONE,
    };
    let mut hidden = false;
    while let Some(field) = p.open()? {
        match field {
            "hide" => hidden = p.atom()? == Some("yes"),
            "offset" | "scale" | "rotate" => {
                while let Some(inner) = p.open()? {
                    if inner == "xyz" {
                        let x = p.f64("xyz")?;
                        let y = p.f64("xyz")?;
                        let z = p.f64("xyz")?;
                        let v = Vec3::new(x, y, z);
                        match field {
                            "offset" => model.offset = v,
                            "scale" => model.scale = v,
                            _ => model.rotate = v,
                        }
                    }
                    p.skip()?;
                }
                continue;
            }
            _ => {}
        }
        p.skip()?;
    }
    let lower = name.to_ascii_lowercase();
    let is_step = lower.ends_with(".step") || lower.ends_with(".stp");
    Ok((!hidden && is_step).then_some(model))
}

/// One graphic item, appended to `out` as line and arc edges if it lies on
/// `Edge.Cuts`. `prefix` is `gr_` for board items and `fp_` for footprint
/// items.
/// A `gr_*` or `fp_*` graphic: on `Edge.Cuts` it joins `out`; on a
/// silkscreen or mask layer it joins `shapes` when those are wanted.
fn parse_graphic<'a>(
    p: &mut Parser<'a>,
    name: &str,
    prefix: &str,
    out: &mut Vec<RawEdge>,
    shapes: &mut Option<(&mut Vec<Shape>, &mut Vec<Vec2>)>,
) -> Result<(), Error> {
    let Some(kind) = name.strip_prefix(prefix) else {
        return p.skip();
    };
    match kind {
        "line" | "arc" | "circle" | "rect" | "poly" | "curve" => {}
        _ => return p.skip(),
    }
    let mut start = Vec2::ZERO;
    let mut mid = Vec2::ZERO;
    let mut end = Vec2::ZERO;
    let mut center = Vec2::ZERO;
    let mut on_edge = false;
    let mut tech = None;
    let mut width = 0.0;
    let mut filled = false;
    let mut radius = 0.0;
    let mut pts: Vec<PolyPoint> = Vec::new();
    while let Some(field) = p.open()? {
        match field {
            "start" => start = p.xy("start")?,
            "mid" => mid = p.xy("mid")?,
            "end" => end = p.xy("end")?,
            "center" => center = p.xy("center")?,
            "radius" => radius = p.f64("radius")?,
            "layer" => {
                let layer = p.atom()?.unwrap_or("");
                on_edge = layer == "Edge.Cuts";
                tech = Tech::from_layer(layer);
            }
            "width" => width = p.f64("width")?,
            "stroke" => {
                while let Some(f) = p.open()? {
                    if f == "width" {
                        width = p.f64("stroke width")?;
                    }
                    p.skip()?;
                }
                continue;
            }
            "fill" => filled = matches!(p.atom()?, Some("yes" | "solid")),
            "pts" => {
                parse_pts(p, &mut pts)?;
                continue;
            }
            _ => {}
        }
        p.skip()?;
    }
    if let (Some(layer), Some((shapes, points))) = (tech, shapes.as_mut()) {
        let polygon = |points: &mut Vec<Vec2>, corners: &[Vec2]| {
            let first = points.len() as u32;
            points.extend_from_slice(corners);
            ShapeKind::Poly {
                points: (first, points.len() as u32),
            }
        };
        let shape = match kind {
            "line" => ShapeKind::Line { a: start, b: end },
            "arc" => ShapeKind::Arc {
                a: start,
                mid,
                b: end,
            },
            "circle" => ShapeKind::Circle {
                center,
                radius: center.distance(end),
            },
            "rect" => polygon(points, &rounded_rect_points(start, end, radius)),
            "curve" => {
                let control = bezier_control_points(&pts)?;
                let corners: Vec<Vec2> = (0..=BEZIER_SEGMENTS)
                    .map(|i| bezier_point(control, i as f64 / BEZIER_SEGMENTS as f64))
                    .collect();
                polygon(points, &corners)
            }
            _ => {
                let mut corners = Vec::new();
                flatten_pts(&pts, &mut corners);
                polygon(points, &corners)
            }
        };
        shapes.push(Shape {
            layer,
            kind: shape,
            width,
            filled,
        });
    }
    if !on_edge {
        return Ok(());
    }
    match kind {
        "line" => out.push(RawEdge::Line { a: start, b: end }),
        "arc" => out.push(RawEdge::Arc {
            a: start,
            mid,
            b: end,
        }),
        "circle" => out.push(RawEdge::Circle {
            center,
            radius: center.distance(end),
        }),
        "rect" => rounded_rect_edges(start, end, radius, out),
        "curve" => {
            let control = bezier_control_points(&pts)?;
            let mut prev = control[0];
            for i in 1..=BEZIER_SEGMENTS {
                let next = bezier_point(control, i as f64 / BEZIER_SEGMENTS as f64);
                out.push(RawEdge::Line { a: prev, b: next });
                prev = next;
            }
        }
        _ => push_polygon(&pts, out),
    }
    Ok(())
}

/// The rest of a text item after its value: position, layer, effects.
/// Consumes the item's list. Returns `None` for hidden text and text off
/// the silkscreen and mask layers. Footprint text keeps itself upright
/// unless unlocked.
fn parse_text<'a>(
    p: &mut Parser<'a>,
    value: &str,
    in_footprint: bool,
) -> Result<Option<Text>, Error> {
    let mut at = Vec2::ZERO;
    let mut angle = 0.0;
    let mut layer = None;
    let mut hidden = false;
    let mut keep_upright = in_footprint;
    let mut style = crate::font::TextStyle {
        size: Vec2::new(1.0, 1.0),
        thickness: 0.0,
        bold: false,
        italic: false,
        halign: 0,
        valign: 0,
        mirror: false,
        angle: 0.0,
    };
    while let Some(field) = p.open()? {
        match field {
            "at" => {
                at = p.xy("text at")?;
                angle = p.f64_or(0.0)?;
                if p.atom()? == Some("unlocked") {
                    keep_upright = false;
                }
            }
            "layer" => layer = p.atom()?.and_then(Tech::from_layer),
            "hide" => hidden = p.atom()? != Some("no"),
            "unlocked" => keep_upright = p.atom()? == Some("no"),
            "effects" => {
                while let Some(effect) = p.open()? {
                    match effect {
                        "font" => {
                            loop {
                                // Older files write bold and italic as
                                // bare words, newer ones as lists.
                                while let Some(flag) = p.atom()? {
                                    match flag {
                                        "bold" => style.bold = true,
                                        "italic" => style.italic = true,
                                        _ => {}
                                    }
                                }
                                let Some(f) = p.open()? else {
                                    break;
                                };
                                match f {
                                    "size" => {
                                        let size = p.xy("font size")?;
                                        style.size = Vec2::new(size.y, size.x);
                                    }
                                    "thickness" => style.thickness = p.f64("font thickness")?,
                                    "bold" => style.bold = p.atom()? != Some("no"),
                                    "italic" => style.italic = p.atom()? != Some("no"),
                                    _ => {}
                                }
                                p.skip()?;
                            }
                            continue;
                        }
                        "justify" => {
                            while let Some(j) = p.atom()? {
                                match j {
                                    "left" => style.halign = -1,
                                    "right" => style.halign = 1,
                                    "top" => style.valign = -1,
                                    "bottom" => style.valign = 1,
                                    "mirror" => style.mirror = true,
                                    _ => {}
                                }
                            }
                        }
                        "hide" => hidden = p.atom()? != Some("no"),
                        _ => {}
                    }
                    p.skip()?;
                }
                continue;
            }
            _ => {}
        }
        p.skip()?;
    }
    let Some(layer) = layer else {
        return Ok(None);
    };
    if hidden || value.is_empty() {
        return Ok(None);
    }
    if keep_upright {
        // Keep the angle in (-90, 90] so the text reads upright.
        while angle > 90.0 {
            angle -= 180.0;
        }
        while angle <= -90.0 {
            angle += 180.0;
        }
    }
    style.angle = angle;
    Ok(Some(Text {
        text: unescape(value).into_owned(),
        at,
        layer,
        style,
    }))
}

enum PolyPoint {
    Point(Vec2),
    Arc([Vec2; 3]),
}

/// The `(pts ...)` list of a polygon, already opened: `xy` points and
/// three-point `arc` entries.
fn parse_pts<'a>(p: &mut Parser<'a>, pts: &mut Vec<PolyPoint>) -> Result<(), Error> {
    while let Some(pt) = p.open()? {
        match pt {
            "xy" => pts.push(PolyPoint::Point(p.xy("xy")?)),
            "arc" => {
                let mut a = [Vec2::ZERO; 3];
                while let Some(f) = p.open()? {
                    match f {
                        "start" => a[0] = p.xy("arc start")?,
                        "mid" => a[1] = p.xy("arc mid")?,
                        "end" => a[2] = p.xy("arc end")?,
                        _ => {}
                    }
                    p.skip()?;
                }
                pts.push(PolyPoint::Arc(a));
                continue;
            }
            _ => {}
        }
        p.skip()?;
    }
    Ok(())
}

/// Chord error when an arc in a point list is flattened, KiCad's default
/// maximum error.
const ARC_CHORD_ERROR: f64 = 0.005;

/// The vertices of a polygon whose arcs are flattened to chords.
fn flatten_pts(pts: &[PolyPoint], out: &mut Vec<Vec2>) {
    for pt in pts {
        match *pt {
            PolyPoint::Point(p) => out.push(p),
            PolyPoint::Arc([a, mid, b]) => {
                let Some(c) = circle_center(a, mid, b) else {
                    out.push(a);
                    out.push(b);
                    continue;
                };
                let r = a.distance(c);
                let from = (a - c).y.atan2((a - c).x);
                let to_mid = ccw_sweep(from, (mid - c).y.atan2((mid - c).x));
                let to_end = ccw_sweep(from, (b - c).y.atan2((b - c).x));
                let sweep = if to_mid < to_end {
                    to_end
                } else {
                    to_end - std::f64::consts::TAU
                };
                let step = 2.0 * (1.0 - ARC_CHORD_ERROR / r).clamp(-1.0, 1.0).acos();
                let n = ((sweep.abs() / step.max(1e-3)).ceil() as usize).max(1);
                for k in 0..n {
                    let angle = from + sweep * k as f64 / n as f64;
                    out.push(c + Vec2::new(angle.cos(), angle.sin()) * r);
                }
                out.push(b);
            }
        }
    }
}

/// Close a polygon whose vertices may be joined by arcs.
/// The corners of a rectangle with its corners rounded by `radius`, which
/// KiCad clamps to half the shorter side. Each corner is one arc.
fn rounded_rect_corners(start: Vec2, end: Vec2, radius: f64) -> (Vec2, Vec2, f64) {
    let lo = start.min(end);
    let hi = start.max(end);
    let r = radius.max(0.0).min((hi.x - lo.x).min(hi.y - lo.y) * 0.5);
    (lo, hi, r)
}

/// A rectangle's outline as edges: four lines, and four arcs when the
/// corners are rounded.
fn rounded_rect_edges(start: Vec2, end: Vec2, radius: f64, out: &mut Vec<RawEdge>) {
    let (lo, hi, r) = rounded_rect_corners(start, end, radius);
    if r <= 1e-9 {
        let corners = [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
        for i in 0..4 {
            out.push(RawEdge::Line {
                a: corners[i],
                b: corners[(i + 1) % 4],
            });
        }
        return;
    }
    let d = r * (1.0 - std::f64::consts::FRAC_1_SQRT_2);
    // Around the rectangle: each side between its two arc ends, then the
    // arc at the corner that follows it.
    let sides = [
        (Vec2::new(lo.x + r, lo.y), Vec2::new(hi.x - r, lo.y)),
        (Vec2::new(hi.x, lo.y + r), Vec2::new(hi.x, hi.y - r)),
        (Vec2::new(hi.x - r, hi.y), Vec2::new(lo.x + r, hi.y)),
        (Vec2::new(lo.x, hi.y - r), Vec2::new(lo.x, lo.y + r)),
    ];
    let arcs = [
        (
            Vec2::new(hi.x - r, lo.y),
            Vec2::new(hi.x - d, lo.y + d),
            Vec2::new(hi.x, lo.y + r),
        ),
        (
            Vec2::new(hi.x, hi.y - r),
            Vec2::new(hi.x - d, hi.y - d),
            Vec2::new(hi.x - r, hi.y),
        ),
        (
            Vec2::new(lo.x + r, hi.y),
            Vec2::new(lo.x + d, hi.y - d),
            Vec2::new(lo.x, hi.y - r),
        ),
        (
            Vec2::new(lo.x, lo.y + r),
            Vec2::new(lo.x + d, lo.y + d),
            Vec2::new(lo.x + r, lo.y),
        ),
    ];
    for i in 0..4 {
        let (a, b) = sides[i];
        if a.distance(b) > 1e-9 {
            out.push(RawEdge::Line { a, b });
        }
        let (a, mid, b) = arcs[i];
        out.push(RawEdge::Arc { a, mid, b });
    }
}

/// A rectangle as a polygon, rounded corners sampled.
fn rounded_rect_points(start: Vec2, end: Vec2, radius: f64) -> Vec<Vec2> {
    let (lo, hi, r) = rounded_rect_corners(start, end, radius);
    if r <= 1e-9 {
        return vec![lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    }
    const STEPS: usize = 8;
    let centers = [
        (Vec2::new(hi.x - r, lo.y + r), -std::f64::consts::FRAC_PI_2),
        (Vec2::new(hi.x - r, hi.y - r), 0.0),
        (Vec2::new(lo.x + r, hi.y - r), std::f64::consts::FRAC_PI_2),
        (Vec2::new(lo.x + r, lo.y + r), std::f64::consts::PI),
    ];
    let mut points = Vec::with_capacity(4 * (STEPS + 1));
    for (c, from) in centers {
        for k in 0..=STEPS {
            let angle = from + std::f64::consts::FRAC_PI_2 * k as f64 / STEPS as f64;
            points.push(c + Vec2::new(angle.cos(), angle.sin()) * r);
        }
    }
    points
}

fn push_polygon(pts: &[PolyPoint], out: &mut Vec<RawEdge>) {
    let first = match pts.first() {
        Some(PolyPoint::Point(p)) => *p,
        Some(PolyPoint::Arc(a)) => a[0],
        None => return,
    };
    let mut prev = None;
    for pt in pts {
        match *pt {
            PolyPoint::Point(p) => {
                if let Some(q) = prev {
                    out.push(RawEdge::Line { a: q, b: p });
                }
                prev = Some(p);
            }
            PolyPoint::Arc([a, mid, b]) => {
                if let Some(q) = prev
                    && q.distance(a) > 1e-6
                {
                    out.push(RawEdge::Line { a: q, b: a });
                }
                out.push(RawEdge::Arc { a, mid, b });
                prev = Some(b);
            }
        }
    }
    if let Some(q) = prev
        && q.distance(first) > 1e-6
    {
        out.push(RawEdge::Line { a: q, b: first });
    }
}

fn bezier_control_points(pts: &[PolyPoint]) -> Result<[Vec2; 4], Error> {
    let mut out = [Vec2::ZERO; 4];
    let mut n = 0;
    for pt in pts {
        if let PolyPoint::Point(p) = pt
            && n < 4
        {
            out[n] = *p;
            n += 1;
        }
    }
    if n != 4 {
        return Err(Error::Outline("bezier needs four control points"));
    }
    Ok(out)
}

const BEZIER_SEGMENTS: usize = 24;
