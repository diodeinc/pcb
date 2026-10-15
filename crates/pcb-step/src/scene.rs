//! The assembly as geometry: what [`crate::export`] writes as STEP, for
//! writers of other formats. Coordinates are millimetres in the output
//! frame (origin applied, y up, z up from the bottom of the board body).

use glam::{DMat4, DVec3};
use rayon::prelude::*;

use crate::board::{self, Board, Physical, Tech};
use crate::coverage::{Coverage, ModelCoverage, Status};
use crate::geom::{Transform, Vec2};
use crate::models::ModelError;
use crate::outline::{Frame, cut_holes, cut_round, outline_solids};
use crate::outline_font::Fonts;
use crate::rings::loops_of;
use crate::{Error, Options, Origin, copper, faces, glob_match, holes};

pub use crate::faces::Face;
pub use crate::geom::{KICAD_MAX_ERROR, chord_count};
pub use crate::holes::RoundHole;
pub use crate::outline::{Loop, OutlinePolicy, Solid, Vertex};

/// Standoff KiCad leaves between the copper surface and a model.
const MODEL_STANDOFF: f64 = 0.05;
const PROXY_HEIGHT: f64 = 1.0;
pub(crate) const PASTE_RGB: [f64; 3] = [0.5; 3];
/// Heights of the tech layers' faces above the outer copper.
pub(crate) const MASK_ABOVE_COPPER: f64 = 0.015;
pub(crate) const PASTE_ABOVE_COPPER: f64 = 0.025;
pub(crate) const SILK_ABOVE_COPPER: f64 = 0.04;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyMode {
    #[default]
    Off,
    Missing,
    /// Every footprint, and no models.
    All,
}

pub struct Scene {
    /// Distinct model references at one scale, in order of first use.
    pub models: Vec<Model>,
    /// Model placements, in footprint order.
    pub components: Vec<Component>,
    pub proxies: Vec<Proxy>,
    /// Board body, copper, silkscreen and solder mask, in output order.
    pub layers: Vec<Layer>,
    /// What became of every model reference. Until a writer has loaded the
    /// models and called [`Scene::resolve`], placed models read `Rendered`.
    pub coverage: Coverage,
    pub synthetic_outline: bool,
}

/// A model reference used at one scale.
pub struct Model {
    /// The model as the footprint names it, with `kicad-embed://` if so.
    pub key: String,
    /// Per-axis, before the model's rotation.
    pub scale: DVec3,
}

impl Model {
    /// The model's display name: the file stem of its key.
    pub fn name(&self) -> &str {
        std::path::Path::new(self.key.rsplit(['/', '\\']).next().unwrap_or(&self.key))
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.key)
    }

    pub fn uniform_scale(&self) -> Option<f64> {
        let s = self.scale;
        ((s.x - s.y).abs() <= 1e-9 && (s.x - s.z).abs() <= 1e-9).then_some(s.x)
    }
}

/// One footprint's use of a model.
pub struct Component {
    /// Index into [`Scene::models`].
    pub model: usize,
    pub reference: String,
    pub footprint: usize,
    pub coverage: usize,
    /// Model frame to output frame, scale not included.
    pub transform: DMat4,
}

pub struct Proxy {
    pub reference: String,
    pub footprint: usize,
    /// Places the unit cube.
    pub transform: DMat4,
}

pub struct Layer {
    pub kind: LayerKind,
    /// KiCad's colour for the layer. The STEP writer encodes it from linear
    /// to sRGB as KiCad's STEP export does; other writers can take it as
    /// sRGB, as KiCad's VRML export does.
    pub color: [f64; 3],
    pub transparency: Option<f64>,
    pub shape: Shape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Body,
    /// Tracks, zone fills and via rings.
    Copper,
    /// Pad prisms and their plating.
    Pads,
    /// Via barrels.
    Vias,
    Silkscreen {
        front: bool,
    },
    Soldermask {
        front: bool,
    },
    Paste {
        front: bool,
    },
}

impl LayerKind {
    /// Copper of any kind: tracks, zones, via rings, pads and via barrels.
    pub fn is_copper(self) -> bool {
        matches!(self, LayerKind::Copper | LayerKind::Pads | LayerKind::Vias)
    }
}

pub enum Shape {
    Solids(Vec<Prism>),
    /// Flat faces at height `z`, facing up or down.
    Faces {
        z: f64,
        up: bool,
        faces: Vec<Face>,
    },
}

/// A solid extruded between two heights.
pub struct Prism {
    pub z0: f64,
    pub z1: f64,
    pub solid: Solid,
}

pub struct Built {
    pub layers: Vec<Layer>,
    pub synthetic_outline: bool,
}

impl Scene {
    pub fn build(
        board: &Board,
        options: &Options,
        warnings: &mut Vec<String>,
    ) -> Result<Self, Error> {
        if !options.board_body
            && !options.components
            && !(options.pads || options.tracks || options.zones)
            && !(options.silkscreen || options.soldermask)
        {
            return Err(Error::NothingToExport);
        }
        let mut scene = Self::components(board, options, warnings);
        let built = Self::layers(board, options, warnings)?;
        scene.layers = built.layers;
        scene.synthetic_outline = built.synthetic_outline;
        Ok(scene)
    }

    /// The scene without its layers, so a writer can start on the models
    /// while [`Scene::layers`] runs.
    pub fn components(board: &Board, options: &Options, warnings: &mut Vec<String>) -> Self {
        let mut scene = Scene {
            models: Vec::new(),
            components: Vec::new(),
            proxies: Vec::new(),
            layers: Vec::new(),
            coverage: Coverage::default(),
            synthetic_outline: false,
        };
        scene.place_components(
            board,
            options,
            frame(board, options),
            &board.physical(),
            warnings,
        );
        scene
    }

    /// The scene's layers, as [`Scene::build`] gives them.
    pub fn layers(
        board: &Board,
        options: &Options,
        warnings: &mut Vec<String>,
    ) -> Result<Built, Error> {
        let frame = frame(board, options);
        let physical = board.physical();
        let mut layers = Vec::new();
        let mut synthetic_outline = false;
        let copper_options = copper::CopperOptions {
            pads: options.pads,
            tracks: options.tracks,
            zones: options.zones,
            inner: options.inner_copper,
            separate_pads: true,
        };
        let variables = board.text_variables(&options.text_variables);
        let fonts = Fonts::load(board, warnings);
        let mut solids = Vec::new();
        if options.board_body
            || options.silkscreen
            || options.soldermask
            || (options.visual && copper_options.any())
        {
            (solids, synthetic_outline) =
                outline_solids(board, frame, options.outline, &fonts, &variables, warnings)?;
        }
        if options.board_body && !solids.is_empty() {
            layers.push(body(
                board,
                options,
                frame,
                &physical,
                solids.clone(),
                warnings,
            ));
        }
        if copper_options.any() {
            let edge = options.visual.then(|| faces::BoardEdge::new(&solids));
            let copper = copper::build(
                board,
                frame,
                &physical,
                copper_options,
                edge.as_ref(),
                warnings,
            );
            let copper_rgb = [0.7, 0.61, 0.0];
            let pad_rgb = if options.components {
                [0.5; 3]
            } else {
                copper_rgb
            };
            let z = &physical.copper_z;
            let islands: Vec<Prism> = copper
                .islands
                .par_iter()
                .flat_map_iter(|island| {
                    let (z0, z1) = z[island.layer as usize];
                    loops_of(&island.region)
                        .into_iter()
                        .map(move |(outer, holes)| Prism {
                            z0,
                            z1,
                            solid: Solid {
                                outer,
                                holes,
                                round: Vec::new(),
                            },
                        })
                })
                .collect();
            let prisms = |pieces: Vec<copper::Piece>| -> Vec<Prism> {
                pieces
                    .into_iter()
                    .map(|piece| Prism {
                        z0: z[piece.bottom as usize].0,
                        z1: z[piece.top as usize].1,
                        solid: piece.solid,
                    })
                    .filter(|prism| prism.z1 - prism.z0 > 1e-9)
                    .collect()
            };
            for (solids, kind, color) in [
                (islands, LayerKind::Copper, copper_rgb),
                (prisms(copper.pads), LayerKind::Pads, pad_rgb),
                (prisms(copper.vias), LayerKind::Vias, copper_rgb),
            ] {
                if !solids.is_empty() {
                    layers.push(Layer {
                        kind,
                        color,
                        transparency: None,
                        shape: Shape::Solids(solids),
                    });
                }
            }
        }
        if options.visual {
            for (front, islands) in faces::paste(board, frame, &fonts, true, &variables, warnings) {
                layers.push(Layer {
                    kind: LayerKind::Paste { front },
                    color: PASTE_RGB,
                    transparency: None,
                    shape: Shape::Faces {
                        z: above_copper(&physical, front, PASTE_ABOVE_COPPER),
                        up: front,
                        faces: islands.par_iter().map(faces::face).collect(),
                    },
                });
            }
        }
        if options.silkscreen || options.soldermask {
            let edge = faces::BoardEdge::new(&solids);
            let tech_layers = faces::build(
                board,
                frame,
                &edge,
                options.silkscreen,
                options.soldermask,
                options.visual,
                &fonts,
                &variables,
                warnings,
            );
            let built: Vec<_> = tech_layers
                .into_par_iter()
                .with_max_len(1)
                .map(|(tech, islands)| {
                    let mut notes = Vec::new();
                    let faces = if tech.silk() {
                        islands.iter().map(faces::face).collect()
                    } else {
                        faces::mask_coverage(&solids, tech.front(), islands, &mut notes)
                    };
                    (tech, faces, notes)
                })
                .collect();
            for (tech, faces, notes) in built {
                warnings.extend(notes);
                if faces.is_empty() {
                    continue;
                }
                let front = tech.front();
                let (kind, color, transparency, above) = match tech {
                    Tech::FrontSilk | Tech::BackSilk => (
                        LayerKind::Silkscreen { front },
                        board.silk_color(front),
                        0.1,
                        SILK_ABOVE_COPPER,
                    ),
                    Tech::FrontMask | Tech::BackMask => (
                        LayerKind::Soldermask { front },
                        board.mask_color(front),
                        0.17,
                        MASK_ABOVE_COPPER,
                    ),
                    Tech::FrontPaste | Tech::BackPaste | Tech::Copper(_) => {
                        unreachable!("faces are built for silkscreen and mask only")
                    }
                };
                layers.push(Layer {
                    kind,
                    color,
                    transparency: Some(transparency),
                    shape: Shape::Faces {
                        z: above_copper(&physical, front, above),
                        up: front,
                        faces,
                    },
                });
            }
        }
        Ok(Built {
            layers,
            synthetic_outline,
        })
    }

    /// Record the writer's outcome for each of [`Scene::models`] and add
    /// missing-model proxies.
    pub fn resolve(
        &mut self,
        board: &Board,
        options: &Options,
        outcomes: &[Result<(), ModelError>],
    ) {
        for component in &self.components {
            if let Err(err) = &outcomes[component.model] {
                let entry = &mut self.coverage.models[component.coverage];
                entry.status = err.status();
                entry.detail = Some(err.to_string());
            }
        }
        if options.proxies != ProxyMode::Missing {
            return;
        }
        let models = &self.coverage.models;
        let mut footprints: Vec<usize> = models
            .iter()
            .filter(|m| m.status.is_missing())
            .map(|m| m.footprint)
            .collect();
        footprints.dedup();
        footprints.retain(|&fp| {
            !models
                .iter()
                .any(|m| m.footprint == fp && m.status == Status::Rendered)
        });
        let (frame, physical) = (frame(board, options), board.physical());
        self.proxies.extend(
            footprints
                .into_iter()
                .filter_map(|fp| proxy(board, fp, frame, &physical)),
        );
    }

    /// Place every selected footprint's models with KiCad's transform:
    /// position, rotation, bottom flip, model offset, then the model's own
    /// rotation, `MODEL_STANDOFF` above the copper.
    fn place_components(
        &mut self,
        board: &Board,
        options: &Options,
        frame: Frame,
        physical: &Physical,
        warnings: &mut Vec<String>,
    ) {
        let top = physical.body_top + physical.front_copper;
        let bottom = -physical.back_copper;
        for (index, fp) in board.footprints.iter().enumerate() {
            let filtered = if !options.components {
                Some("components are not exported")
            } else if fp.dnp && !options.include_dnp {
                Some("DNP")
            } else if fp.mount == board::Mount::Unspecified && !options.include_unspecified {
                Some("no SMD or through-hole attribute")
            } else if !(options.component_filter.is_empty()
                || options
                    .component_filter
                    .iter()
                    .any(|g| glob_match(g, fp.reference)))
            {
                Some("not in the component filter")
            } else if options.proxies == ProxyMode::All {
                self.proxies.extend(proxy(board, index, frame, physical));
                Some("footprint-only")
            } else {
                None
            };
            for model in &board.models[fp.models.start as usize..fp.models.end as usize] {
                let mut entry = |status, detail: Option<String>| {
                    self.coverage.models.push(ModelCoverage {
                        footprint: index,
                        reference: fp.reference.to_owned(),
                        model: model.path().into_owned(),
                        status,
                        detail,
                    })
                };
                let scale = model.scale;
                if let Some(why) = filtered {
                    entry(Status::Filtered, Some(why.to_owned()));
                    continue;
                }
                if model.hidden {
                    entry(Status::Hidden, None);
                    continue;
                }
                if !scale.is_finite() || scale.min_element() <= 0.0 {
                    warnings.push(format!(
                        "{}: skipped model with non-positive scale: {}",
                        fp.reference, model.name
                    ));
                    entry(
                        Status::Unsupported,
                        Some(format!("scale {scale} is not positive")),
                    );
                    continue;
                }
                entry(Status::Rendered, None);
                let key = model.path();
                let index_of = match self
                    .models
                    .iter()
                    .position(|m| m.key == key && m.scale == scale)
                {
                    Some(i) => i,
                    None => {
                        self.models.push(Model {
                            key: key.into_owned(),
                            scale,
                        });
                        self.models.len() - 1
                    }
                };
                let position = frame.point(fp.at);
                let mut offset = model.offset;
                offset.z += MODEL_STANDOFF;
                let mut transform = Transform::translation(position.extend(0.0))
                    .then(&Transform::rotation_z(fp.rotation.to_radians()));
                if fp.back {
                    offset.z -= bottom;
                    transform = transform.then(&Transform::rotation_x(std::f64::consts::PI));
                } else {
                    offset.z += top;
                }
                let rotate = model.rotate;
                let transform = transform
                    .then(&Transform::translation(offset))
                    .then(&Transform::rotation_z(-rotate.z.to_radians()))
                    .then(&Transform::rotation_y(-rotate.y.to_radians()))
                    .then(&Transform::rotation_x(-rotate.x.to_radians()));
                self.components.push(Component {
                    model: index_of,
                    reference: fp.reference.to_owned(),
                    footprint: index,
                    coverage: self.coverage.models.len() - 1,
                    transform: transform.0,
                });
            }
        }
    }
}

/// The box over a footprint's pads.
fn proxy(board: &Board, index: usize, frame: Frame, physical: &Physical) -> Option<Proxy> {
    let fp = &board.footprints[index];
    let (lo, hi) = fp.extent?;
    // The footprint's frame has y down; the output's has y up.
    let (lo, hi) = (Vec2::new(lo.x, -hi.y), Vec2::new(hi.x, -lo.y));
    let z0 = if fp.back {
        -physical.back_copper - PROXY_HEIGHT
    } else {
        physical.body_top + physical.front_copper + MODEL_STANDOFF
    };
    let size = DVec3::new((hi.x - lo.x).max(0.1), (hi.y - lo.y).max(0.1), PROXY_HEIGHT);
    let center = ((lo + hi) * 0.5).extend(z0 + PROXY_HEIGHT * 0.5);
    let placement = Transform::translation(frame.point(fp.at).extend(0.0))
        .then(&Transform::rotation_z(fp.rotation.to_radians()));
    Some(Proxy {
        reference: fp.reference.to_owned(),
        footprint: index,
        transform: placement.0 * DMat4::from_translation(center) * DMat4::from_scale(size),
    })
}

/// Height of a face `above` the outer copper of one side, below it for
/// the back.
pub(crate) fn above_copper(physical: &Physical, front: bool, above: f64) -> f64 {
    let z = &physical.copper_z;
    if front {
        z[0].1 + above
    } else {
        z[z.len() - 1].0 - above
    }
}

pub(crate) fn frame(board: &Board, options: &Options) -> Frame {
    let origin = match options.origin {
        Origin::Board => Vec2::ZERO,
        Origin::Drill => board.aux_origin,
        Origin::Grid => board.grid_origin,
        Origin::User { x, y } => Vec2::new(x, y),
    };
    Frame { origin }
}

/// The board body: one solid per outline with its drills cut. Plain
/// through drills go first, in one boolean; machined holes after, each
/// falling back to a plain drill where its shape cannot stand clear of
/// everything else.
fn body(
    board: &Board,
    options: &Options,
    frame: Frame,
    physical: &Physical,
    solids: Vec<Solid>,
    warnings: &mut Vec<String>,
) -> Layer {
    let mut drills = Vec::new();
    let mut machined = Vec::new();
    for hole in &board.holes {
        let plain = hole.machining == board::Machining::default();
        if plain || hole.a.distance(hole.b) > 1e-6 {
            if !plain {
                warnings.push(format!(
                    "slot at ({:.3}, {:.3}) mm: machining is only cut on round drills",
                    hole.a.x, hole.a.y
                ));
            }
            drills.push(Loop::stadium(
                frame.point(hole.a),
                frame.point(hole.b),
                hole.r,
            ));
        } else if let Some(round) =
            holes::pad_hole(frame.point(hole.a), hole.r, hole.machining, physical)
        {
            machined.push(round);
        }
    }
    if options.cut_vias || options.visual {
        let last = physical.copper_z.len().saturating_sub(1) as u32;
        for via in &board.vias {
            if options.visual {
                // As KiCad's VRML export: filled, tented or blind alike.
                if via.top == 0 || via.bottom == last {
                    drills.push(Loop::stadium(
                        frame.point(via.at),
                        frame.point(via.at),
                        via.drill * 0.5,
                    ));
                }
                continue;
            }
            let Some(round) = holes::via_hole(frame.point(via.at), via, physical, warnings) else {
                continue;
            };
            if round.is_plain() {
                let r = round.profile[0].1;
                drills.push(Loop::stadium(round.center, round.center, r));
            } else {
                machined.push(round);
            }
        }
    }
    let mut solids = cut_holes(solids, drills, warnings);
    let fallbacks: Vec<Loop> = machined
        .into_iter()
        .filter_map(|round| cut_round(&mut solids, round, warnings))
        .collect();
    if !fallbacks.is_empty() {
        solids = cut_holes(solids, fallbacks, warnings);
    }
    // KiCad paints the body in the mask colour unless the mask is
    // exported as a layer of its own.
    let color = if options.soldermask {
        board.core_color()
    } else {
        board.body_color()
    };
    Layer {
        kind: LayerKind::Body,
        color,
        transparency: None,
        shape: Shape::Solids(
            solids
                .into_iter()
                .map(|solid| Prism {
                    z0: 0.0,
                    z1: physical.body_top,
                    solid,
                })
                .collect(),
        ),
    }
}
