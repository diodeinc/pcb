//! The assembly as geometry: what [`crate::export`] writes as STEP, for
//! writers of other formats. Coordinates are millimetres in the output
//! frame (origin applied, y up, z up from the bottom of the board body).

use glam::DMat4;

use crate::board::{self, Board, Physical, Tech};
use crate::geom::{Transform, Vec2};
use crate::outline::{Frame, board_solids, cut_holes, cut_round};
use crate::{Error, Options, Origin, copper, faces, glob_match, holes, model_key, worker_threads};

pub use crate::faces::Face;
pub use crate::holes::RoundHole;
pub use crate::outline::{Loop, Solid, Vertex};

/// Standoff KiCad leaves between the copper surface and a model.
const MODEL_STANDOFF: f64 = 0.05;

pub struct Scene {
    /// Distinct model files at one scale, in order of first use.
    pub models: Vec<Model>,
    /// Model placements, in footprint order.
    pub components: Vec<Component>,
    /// Board body, copper, silkscreen and solder mask, in output order.
    pub layers: Vec<Layer>,
}

/// A model file used at one uniform scale.
pub struct Model {
    /// The model's name among the board's embedded files.
    pub key: String,
    pub scale: f64,
}

/// One footprint's use of a model.
pub struct Component {
    /// Index into [`Scene::models`].
    pub model: usize,
    pub reference: String,
    /// Model frame to output frame, scale not included.
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
        scene.layers = Self::layers(board, options, warnings)?;
        Ok(scene)
    }

    /// The scene without its layers, so a writer can start on the models
    /// while [`Scene::layers`] runs.
    pub fn components(board: &Board, options: &Options, warnings: &mut Vec<String>) -> Self {
        let mut scene = Scene {
            models: Vec::new(),
            components: Vec::new(),
            layers: Vec::new(),
        };
        if options.components {
            scene.place_components(
                board,
                options,
                frame(board, options),
                &board.physical(),
                warnings,
            );
        }
        scene
    }

    /// The scene's layers, as [`Scene::build`] gives them.
    pub fn layers(
        board: &Board,
        options: &Options,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Layer>, Error> {
        let frame = frame(board, options);
        let physical = board.physical();
        let mut layers = Vec::new();
        if options.board_body {
            layers.push(body(board, options, frame, &physical, warnings)?);
        }
        let copper_options = copper::CopperOptions {
            pads: options.pads,
            tracks: options.tracks,
            zones: options.zones,
            inner: options.inner_copper,
        };
        if copper_options.any() {
            let copper = copper::build(
                board,
                frame,
                &physical,
                copper_options,
                worker_threads(),
                warnings,
            );
            let copper_rgb = [0.7, 0.61, 0.0];
            let pad_rgb = if options.components {
                [0.5; 3]
            } else {
                copper_rgb
            };
            for (solids, kind, color) in [
                (copper.islands, LayerKind::Copper, copper_rgb),
                (copper.pads, LayerKind::Pads, pad_rgb),
                (copper.vias, LayerKind::Vias, copper_rgb),
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
        if options.silkscreen || options.soldermask {
            let variables: Vec<(String, String)> = board
                .title_block
                .iter()
                .chain(&options.text_variables)
                .cloned()
                .collect();
            let tech_layers = faces::build(
                board,
                frame,
                &physical,
                options.silkscreen,
                options.soldermask,
                &variables,
                worker_threads(),
                warnings,
            )?;
            for layer in tech_layers {
                if layer.faces.is_empty() {
                    continue;
                }
                let front = layer.tech.front();
                let (kind, color, transparency) = match layer.tech {
                    Tech::FrontSilk | Tech::BackSilk => (
                        LayerKind::Silkscreen { front },
                        board.silk_color(front),
                        0.1,
                    ),
                    Tech::FrontMask | Tech::BackMask => (
                        LayerKind::Soldermask { front },
                        board.mask_color(front),
                        0.17,
                    ),
                };
                layers.push(Layer {
                    kind,
                    color,
                    transparency: Some(transparency),
                    shape: Shape::Faces {
                        z: layer.z,
                        up: front,
                        faces: layer.faces,
                    },
                });
            }
        }
        Ok(layers)
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
        for fp in &board.footprints {
            if (fp.dnp && !options.include_dnp)
                || (fp.unspecified && !options.include_unspecified)
                || !(options.component_filter.is_empty()
                    || options
                        .component_filter
                        .iter()
                        .any(|g| glob_match(g, fp.reference)))
            {
                continue;
            }
            for model in &board.models[fp.models.start as usize..fp.models.end as usize] {
                let scale = model.scale;
                if (scale.x - scale.y).abs() > 1e-9
                    || (scale.x - scale.z).abs() > 1e-9
                    || scale.x <= 0.0
                {
                    warnings.push(format!(
                        "{}: skipped model with non-uniform scale: {}",
                        fp.reference, model.name
                    ));
                    continue;
                }
                let path = model.path();
                let key = model_key(&path);
                let index = match self
                    .models
                    .iter()
                    .position(|m| m.key == key && m.scale == scale.x)
                {
                    Some(i) => i,
                    None => {
                        self.models.push(Model {
                            key: key.to_owned(),
                            scale: scale.x,
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
                    model: index,
                    reference: fp.reference.to_owned(),
                    transform: transform.0,
                });
            }
        }
    }
}

fn frame(board: &Board, options: &Options) -> Frame {
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
    warnings: &mut Vec<String>,
) -> Result<Layer, Error> {
    let solids = board_solids(board, frame)?;
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
    if options.cut_vias {
        for via in &board.vias {
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
    Ok(Layer {
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
    })
}
