//! The board's final 2D layer geometry, for renderers that draw the board
//! themselves: copper per layer and net, mask openings, paste and
//! silkscreen as pcb-ir regions, with the outline, stackup and drills.
//! It is built by the same code as the [`Scene`](crate::scene::Scene)'s
//! layers, so every consumer gets one interpretation of the KiCad board.
//!
//! Coordinates are millimetres in KiCad's origin, y up.

use std::collections::BTreeMap;

use pcb_intern::Interner;
use pcb_ir::dialects::nc;
use pcb_ir::geom::{ContourBuf, ContourSet, FillRule, Point};
use rayon::prelude::*;

use crate::Error;
use crate::board::Board;
use crate::copper::{self, CopperOptions, resolution, solid_contours};
use crate::faces::{self, BoardEdge, island_contours};
use crate::geom::Vec2;
use crate::outline::{Frame, OutlinePolicy, outline_solids};
use crate::outline_font::Fonts;
use crate::scene::{
    MASK_ABOVE_COPPER, PASTE_ABOVE_COPPER, PASTE_RGB, SILK_ABOVE_COPPER, above_copper,
};

/// Index into [`BoardImage::nets`]; 0 is no net.
pub type NetId = u32;

#[derive(Debug, Clone)]
pub struct ImageOptions {
    /// Inner copper layers as well as the outer two.
    pub inner_copper: bool,
    /// Paste on the pads of DNP footprints, as KiCad's 3D viewer draws it.
    pub include_dnp_paste: bool,
    pub outline: OutlinePolicy,
    /// Project text variables, substituted into `${NAME}` in silkscreen
    /// text as KiCad substitutes them.
    pub text_variables: Vec<(String, String)>,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            inner_copper: false,
            include_dnp_paste: true,
            outline: OutlinePolicy::Strict,
            text_variables: Vec::new(),
        }
    }
}

pub struct BoardImage {
    /// Bottom to top.
    pub stackup: Vec<PhysicalLayer>,
    /// The board body with its cutouts; drills are in `drills`.
    pub outline: ContourSet,
    pub layers: Vec<LayerImage>,
    /// Pad and via drills; their net, component and pin are symbols of
    /// `names`, as are the copper layer names of a via's span.
    pub drills: nc::Document,
    pub names: Interner,
    /// Net names by [`NetId`]; `nets[0]` is empty.
    pub nets: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LayerImage {
    /// Index into [`BoardImage::stackup`].
    pub layer: usize,
    pub kind: ImageKind,
    /// The copper's net; 0 for every other kind.
    pub net: NetId,
    pub region: ContourSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImageKind {
    Copper,
    /// Where the solder mask is open; the mask covers the outline less
    /// its openings.
    MaskOpening,
    Paste,
    Silkscreen,
}

#[derive(Debug, Clone)]
pub struct PhysicalLayer {
    /// KiCad's name for it: `F.Cu`, `B.Mask`, `F.SilkS`; `Body` for the
    /// dielectric stack.
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    /// Copper and the body span `z0..z1` up from the bottom of the body.
    /// Mask, paste and silkscreen are surfaces with `z0 == z1`, at the
    /// heights the scene draws them.
    pub z0: f64,
    pub z1: f64,
    /// KiCad's colour for the layer, as the scene gives it.
    pub color: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerRole {
    Body,
    Copper,
    SolderMask,
    Paste,
    Silkscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Front,
    Inner,
    Back,
}

/// Build the board's final 2D layer geometry.
pub fn board_image(board: &Board, options: &ImageOptions) -> Result<BoardImage, Error> {
    let frame = Frame { origin: Vec2::ZERO };
    let physical = board.physical();
    let mut warnings = Vec::new();
    let (solids, _) = outline_solids(board, frame, options.outline, &mut warnings)?;
    let edge = BoardEdge::new(&solids);
    let outline = edge
        .region(&mut warnings)
        .unwrap_or_else(|| ContourSet::empty(resolution()));

    // Bottom to top: the back's tech layers, B.Cu, the body, the inner
    // layers, F.Cu, the front's tech layers. The stack has at least the
    // two copper layers KiCad requires, so a board declaring F.Cu alone
    // still has a B.Cu.
    let n = physical.copper_z.len();
    let copper_name = |i: usize| match board.copper_layers.get(i) {
        Some(name) => (*name).to_owned(),
        None if i + 1 == n => "B.Cu".to_owned(),
        None => format!("In{i}.Cu"),
    };
    let mut stackup = Vec::new();
    let mut copper_at = vec![0; n];
    let mut tech_at = BTreeMap::new();
    let mut tech = |stackup: &mut Vec<PhysicalLayer>, front: bool| {
        let mut layers = [
            (
                ImageKind::MaskOpening,
                LayerRole::SolderMask,
                "Mask",
                MASK_ABOVE_COPPER,
                board.mask_color(front),
            ),
            (
                ImageKind::Paste,
                LayerRole::Paste,
                "Paste",
                PASTE_ABOVE_COPPER,
                PASTE_RGB,
            ),
            (
                ImageKind::Silkscreen,
                LayerRole::Silkscreen,
                "SilkS",
                SILK_ABOVE_COPPER,
                board.silk_color(front),
            ),
        ];
        if !front {
            layers.reverse();
        }
        for (kind, role, name, above, color) in layers {
            let z = above_copper(&physical, front, above);
            tech_at.insert((kind, front), stackup.len());
            stackup.push(PhysicalLayer {
                name: format!("{}.{name}", if front { "F" } else { "B" }),
                role,
                side: if front { Side::Front } else { Side::Back },
                z0: z,
                z1: z,
                color,
            });
        }
    };
    let mut copper_layer = |stackup: &mut Vec<PhysicalLayer>, i: usize| {
        copper_at[i] = stackup.len();
        stackup.push(PhysicalLayer {
            name: copper_name(i),
            role: LayerRole::Copper,
            side: match i {
                0 => Side::Front,
                i if i == n - 1 => Side::Back,
                _ => Side::Inner,
            },
            z0: physical.copper_z[i].0,
            z1: physical.copper_z[i].1,
            color: board.copper_color(),
        });
    };
    tech(&mut stackup, false);
    if n > 1 {
        copper_layer(&mut stackup, n - 1);
    }
    stackup.push(PhysicalLayer {
        name: "Body".to_owned(),
        role: LayerRole::Body,
        side: Side::Inner,
        z0: 0.0,
        z1: physical.body_top,
        color: board.core_color(),
    });
    for i in (0..n - usize::from(n > 1)).rev() {
        copper_layer(&mut stackup, i);
    }
    tech(&mut stackup, true);

    let copper_options = CopperOptions {
        pads: true,
        tracks: true,
        zones: true,
        inner: options.inner_copper,
        separate_pads: false,
    };
    // Copper off the board is not made, so it is cut at the edge.
    let copper = copper::build(
        board,
        frame,
        &physical,
        copper_options,
        Some(&edge),
        &mut warnings,
    );
    // Each layer and net: its islands, unioned with its pads.
    let mut nets: BTreeMap<(u32, u32), (Option<ContourSet>, Vec<ContourBuf>)> = BTreeMap::new();
    for island in copper.islands {
        nets.entry((island.layer, island.net)).or_default().0 = Some(island.region);
    }
    for piece in copper.pads.iter().filter(|p| !p.barrel) {
        let pads = &mut nets.entry((piece.top, piece.net)).or_default().1;
        pads.extend(solid_contours(&piece.solid));
    }
    let merged: Vec<_> = nets
        .into_par_iter()
        .map(|((layer, net), (island, pads))| {
            let region = ContourSet::from_contours(&pads, FillRule::NonZero, resolution())
                .and_then(|pads| match &island {
                    Some(island) => island.union(&pads),
                    None => Ok(pads),
                });
            let image = |region| LayerImage {
                layer: copper_at[layer as usize],
                kind: ImageKind::Copper,
                net,
                region,
            };
            match region {
                Ok(region) => (Some(image(region)), None),
                Err(err) => (
                    island.map(image),
                    Some(format!(
                        "copper on {} net {net}: {err}",
                        copper_name(layer as usize)
                    )),
                ),
            }
        })
        .collect();
    let mut layers = Vec::new();
    for (image, note) in merged {
        layers.extend(image);
        warnings.extend(note);
    }

    let variables = board.text_variables(&options.text_variables);
    let fonts = Fonts::load(board, &mut warnings);
    let tech = faces::build(
        board,
        frame,
        &edge,
        true,
        true,
        false,
        &fonts,
        &variables,
        &mut warnings,
    );
    let paste = faces::paste(
        board,
        frame,
        &fonts,
        options.include_dnp_paste,
        &variables,
        &mut warnings,
    );
    let tech = tech
        .into_iter()
        .map(|(tech, islands)| {
            let kind = if tech.silk() {
                ImageKind::Silkscreen
            } else {
                ImageKind::MaskOpening
            };
            (kind, tech.front(), islands)
        })
        .chain(
            paste
                .into_iter()
                .map(|(front, islands)| (ImageKind::Paste, front, islands)),
        );
    for (kind, front, islands) in tech {
        let contours: Vec<ContourBuf> = islands.iter().flat_map(island_contours).collect();
        if contours.is_empty() {
            continue;
        }
        match ContourSet::from_contours(&contours, FillRule::NonZero, resolution()) {
            Ok(region) => layers.push(LayerImage {
                layer: tech_at[&(kind, front)],
                kind,
                net: 0,
                region,
            }),
            Err(err) => warnings.push(format!("{}: {err}", stackup[tech_at[&(kind, front)]].name)),
        }
    }
    layers.sort_by_key(|l| (l.layer, l.kind, l.net));

    let mut nets = vec![String::new(); board.nets.len() + 1];
    for (name, &id) in &board.nets {
        nets[id as usize] = (*name).to_owned();
    }
    let mut names = Interner::new();
    let drills = drills(board, frame, &nets, &mut names, &copper_name);
    Ok(BoardImage {
        stackup,
        outline,
        layers,
        drills,
        names,
        nets,
        warnings,
    })
}

/// Every pad and via drill as an NC operation.
fn drills(
    board: &Board,
    frame: Frame,
    nets: &[String],
    names: &mut Interner,
    copper_name: &dyn Fn(usize) -> String,
) -> nc::Document {
    let point = |v: Vec2| {
        let v = frame.point(v);
        Point { x: v.x, y: v.y }
    };
    let geometry = |a: Vec2, b: Vec2, r: f64| {
        if a.distance(b) < 1e-9 {
            nc::Geometry::Drill {
                at: point(a),
                diameter: 2.0 * r,
            }
        } else {
            nc::Geometry::Slot {
                start: point(a),
                end: point(b),
                diameter: 2.0 * r,
            }
        }
    };
    let net = |names: &mut Interner, id: u32| (id != 0).then(|| names.intern(&nets[id as usize]));
    let mut doc = nc::Document::new();
    for hole in &board.holes {
        let footprint = board.footprints.get(hole.footprint as usize);
        doc.objects.push(nc::Object {
            geometry: geometry(hole.a, hole.b, hole.r),
            plating: if hole.plated {
                nc::Plating::Plated
            } else {
                nc::Plating::NonPlated
            },
            span: nc::DrillSpan::ThroughBoard,
            function: nc::Function::Component,
            net: net(names, hole.net),
            component: footprint
                .filter(|fp| !fp.reference.is_empty())
                .map(|fp| names.intern(fp.reference)),
            pin: (!hole.pin.is_empty()).then(|| names.intern(hole.pin)),
        });
    }
    let last = board.copper_layers.len().saturating_sub(1) as u32;
    for via in board.vias.iter().filter(|v| v.drill > 0.0) {
        let span = if via.top == 0 && via.bottom >= last {
            nc::DrillSpan::ThroughBoard
        } else {
            nc::DrillSpan::FromTo {
                from: Some(names.intern(&copper_name(via.top as usize))),
                to: Some(names.intern(&copper_name(via.bottom.min(last) as usize))),
            }
        };
        doc.objects.push(nc::Object {
            geometry: geometry(via.at, via.at, via.drill * 0.5),
            plating: nc::Plating::Plated,
            span,
            function: nc::Function::Via,
            net: net(names, via.net),
            component: None,
            pin: None,
        });
    }
    doc
}
