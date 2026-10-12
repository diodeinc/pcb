//! Footprint models: every distinct STEP or VRML payload, decoded and
//! tessellated once (STEP with foxtrot), largest first, on the rayon pool.

use std::collections::HashMap;

use glam::Vec3;
use pcb_step::scene::{Model, Scene};
use pcb_step::{Board, ModelError, ModelFormat, Options, Report, content_hash};
use rayon::prelude::*;
use triangulate::colored_mesh::{ColoredSubmesh, tessellate_step_bytes};

use crate::mesh::Primitive;
use crate::wrl;

/// How far simplification may move a model's surface, in output
/// millimetres: the tolerance models are tessellated to.
const SIMPLIFY_ERROR: f32 = 0.01;

/// A tessellated model: one simplified, optimized primitive per colour, in
/// model millimetres.
pub(crate) struct Mesh {
    pub(crate) name: String,
    /// sRGB colour and its triangles, by colour.
    pub(crate) primitives: Vec<([f32; 4], Primitive)>,
}

pub(crate) struct Models {
    pub(crate) meshes: Vec<Mesh>,
    /// For each of the scene's models, its mesh, if it could be made.
    pub(crate) of_model: Vec<Option<usize>>,
    pub(crate) outcomes: Vec<Result<(), ModelError>>,
    /// Warnings, and models that were found but could not be used.
    pub(crate) report: Report,
}

struct Payload<'a> {
    model: &'a Model,
    bytes: Vec<u8>,
    format: ModelFormat,
    /// Largest scale it is placed at, which bounds simplification.
    scale: f64,
}

pub(crate) fn tessellate(board: &Board, options: &Options, scene: &Scene) -> Models {
    let loaded: Vec<_> = scene
        .models
        .par_iter()
        .map(|model| board.load_model(&options.models, &model.key))
        .collect();

    let mut report = Report::default();
    // Distinct payloads by content, so one file embedded under two names,
    // or used at two scales, is tessellated once.
    let mut payloads: Vec<Payload> = Vec::new();
    let mut by_hash: HashMap<(u64, ModelFormat), usize> = HashMap::new();
    let mut payload_of = Vec::with_capacity(loaded.len());
    for (model, loaded) in scene.models.iter().zip(loaded) {
        if let Err(err) = &loaded {
            report.model_error(&model.key, err);
        }
        payload_of.push(loaded.map(|loaded| {
            let key = (content_hash(&loaded.bytes), loaded.format);
            let i = *by_hash.entry(key).or_insert_with(|| {
                payloads.push(Payload {
                    model,
                    bytes: loaded.bytes,
                    format: loaded.format,
                    scale: 0.0,
                });
                payloads.len() - 1
            });
            payloads[i].scale = payloads[i].scale.max(model.scale.max_element());
            i
        }));
    }

    let mut order: Vec<usize> = (0..payloads.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(payloads[i].bytes.len()));
    let mut results: Vec<(usize, Tessellated)> = order
        .into_par_iter()
        .with_max_len(1)
        .map(|i| {
            let p = &payloads[i];
            (i, tessellate_one(p, SIMPLIFY_ERROR / p.scale as f32))
        })
        .collect();
    results.sort_by_key(|(i, _)| *i);

    let mut meshes = Vec::new();
    let mesh_of_payload: Vec<Result<usize, ModelError>> = results
        .into_iter()
        .map(|(i, result)| {
            report.warnings.extend(result.notes);
            result
                .mesh
                .map(|mesh| {
                    meshes.push(mesh);
                    meshes.len() - 1
                })
                .inspect_err(|err| report.model_error(&payloads[i].model.key, err))
        })
        .collect();
    let (of_model, outcomes) = payload_of
        .into_iter()
        .map(|p| match p.and_then(|p| mesh_of_payload[p].clone()) {
            Ok(mesh) => (Some(mesh), Ok(())),
            Err(err) => (None, Err(err)),
        })
        .unzip();
    Models {
        meshes,
        of_model,
        outcomes,
        report,
    }
}

/// What tessellating one payload gave.
struct Tessellated {
    mesh: Result<Mesh, ModelError>,
    notes: Vec<String>,
}

/// Tessellate one payload and simplify it within `simplify_error` model
/// millimetres.
fn tessellate_one(payload: &Payload, simplify_error: f32) -> Tessellated {
    let key = &payload.model.key;
    let mut notes = Vec::new();
    let primitives = match payload.format {
        ModelFormat::Wrl => wrl::parse(&payload.bytes).map(|parsed| {
            notes.extend(parsed.notes.iter().map(|n| format!("model {key}: {n}")));
            parsed.primitives
        }),
        ModelFormat::Step => step_primitives(&payload.bytes, key, &mut notes),
    };
    let mesh = primitives
        .map_err(ModelError::Failed)
        .and_then(|primitives| {
            let mut primitives: Vec<([f32; 4], Primitive)> = primitives
                .into_par_iter()
                .map(|(color, mut primitive)| {
                    primitive.simplify(simplify_error);
                    primitive.optimize();
                    (color, primitive)
                })
                // Simplification drops solids smaller than the error budget.
                .filter(|(_, p)| !p.is_empty())
                .collect();
            if primitives.is_empty() {
                return Err(ModelError::Empty);
            }
            primitives.sort_by(|a, b| a.0.map(f32::to_bits).cmp(&b.0.map(f32::to_bits)));
            Ok(Mesh {
                name: payload.model.name().to_owned(),
                primitives,
            })
        });
    Tessellated { mesh, notes }
}

fn step_primitives(
    bytes: &[u8],
    key: &str,
    notes: &mut Vec<String>,
) -> Result<Vec<([f32; 4], Primitive)>, String> {
    let (tessellated, stats) = tessellate_step_bytes(bytes)?;
    if !stats.failures.is_empty() {
        notes.push(format!(
            "model {key}: {} of {} faces could not be tessellated",
            stats.failures.len(),
            stats.num_faces
        ));
    }
    let primitives: Vec<_> = tessellated
        .submeshes
        .into_par_iter()
        .filter(|s| !s.indices.is_empty())
        .map(|s| {
            let ColoredSubmesh {
                color,
                positions,
                mut normals,
                indices,
            } = s;
            repair_normals(&positions, &mut normals, &indices);
            let primitive = Primitive {
                positions,
                normals,
                indices,
            };
            (color, primitive)
        })
        .collect();
    if primitives.is_empty() && !stats.failures.is_empty() {
        return Err("no faces could be tessellated".to_owned());
    }
    Ok(primitives)
}

/// Give every vertex a unit normal, as glTF requires. Foxtrot leaves a
/// zero normal where the surface has none, at a cone tip or a degenerate
/// patch; those take the area-weighted normal of their triangles.
fn repair_normals(positions: &[[f32; 3]], normals: &mut [[f32; 3]], indices: &[u32]) {
    let unit = |n: Vec3| (n.is_finite() && (n.length() - 1.0).abs() < 1e-3).then_some(n);
    let mut bad: Vec<Option<Vec3>> = normals
        .iter()
        .map(|&n| match unit(Vec3::from(n)) {
            Some(_) => None,
            None => Some(Vec3::ZERO),
        })
        .collect();
    if bad.iter().all(Option::is_none) {
        return;
    }
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| Vec3::from(positions[i as usize]));
        let face = (b - a).cross(c - a);
        for &i in t {
            if let Some(sum) = &mut bad[i as usize] {
                *sum += face;
            }
        }
    }
    for (normal, sum) in normals.iter_mut().zip(bad) {
        if let Some(sum) = sum {
            *normal = sum.try_normalize().unwrap_or(Vec3::Z).to_array();
        }
    }
}
