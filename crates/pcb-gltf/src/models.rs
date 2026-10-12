//! Footprint models: every distinct embedded STEP payload, decoded and
//! tessellated once with foxtrot, largest first, on the rayon pool.

use std::collections::HashMap;

use glam::Vec3;
use pcb_step::scene::{Model, Scene};
use pcb_step::{Board, Report, content_hash};
use rayon::prelude::*;
use triangulate::colored_mesh::{ColoredSubmesh, tessellate_step_bytes};

use crate::cache::{ModelCache, Tessellation};
use crate::mesh::Primitive;

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
    /// Warnings, and models that were found but could not be used.
    pub(crate) report: Report,
}

pub(crate) fn tessellate(board: &Board, scene: &Scene, cache: Option<&ModelCache>) -> Models {
    let decoded: Vec<_> = scene
        .models
        .par_iter()
        .map(|model| board.model(&model.key))
        .collect();

    let mut report = Report::default();
    // Distinct payloads by content, so one file embedded under two names,
    // or used at two scales, is tessellated once. Each keeps the largest
    // scale it is placed at.
    let mut payloads: Vec<(&Model, Vec<u8>, f64)> = Vec::new();
    let mut by_hash: HashMap<u64, usize> = HashMap::new();
    let mut payload_of: Vec<Option<usize>> = Vec::with_capacity(decoded.len());
    for (model, decoded) in scene.models.iter().zip(decoded) {
        payload_of.push(match decoded {
            None => {
                report.missing_model(&model.key);
                None
            }
            Some(Err(err)) => {
                report.failed_model(&model.key, &err);
                None
            }
            Some(Ok(bytes)) => {
                let i = *by_hash.entry(content_hash(&bytes)).or_insert_with(|| {
                    payloads.push((model, bytes, 0.0));
                    payloads.len() - 1
                });
                payloads[i].2 = payloads[i].2.max(model.scale);
                Some(i)
            }
        });
    }

    let mut order: Vec<usize> = (0..payloads.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(payloads[i].1.len()));
    let mut outcomes: Vec<(usize, Outcome)> = order
        .into_par_iter()
        .with_max_len(1)
        .map(|i| {
            let (model, bytes, scale) = &payloads[i];
            let simplify_error = SIMPLIFY_ERROR / *scale as f32;
            let tessellation = match cache {
                None => tessellate_one(bytes, simplify_error),
                Some(cache) => {
                    let key = cache.key(bytes, simplify_error);
                    cache.get(&key).unwrap_or_else(|| {
                        let tessellation = tessellate_one(bytes, simplify_error);
                        cache.put(&key, &tessellation);
                        tessellation
                    })
                }
            };
            (i, outcome(model, tessellation))
        })
        .collect();
    outcomes.sort_by_key(|(i, _)| *i);
    if let Some(cache) = cache {
        cache.log_summary();
    }

    let mut meshes = Vec::new();
    let mut mesh_of_payload = Vec::with_capacity(outcomes.len());
    for (i, outcome) in outcomes {
        report.warnings.extend(outcome.notes);
        mesh_of_payload.push(match outcome.mesh {
            Ok(Some(mesh)) => {
                meshes.push(mesh);
                Some(meshes.len() - 1)
            }
            Ok(None) => {
                report.empty_model(&payloads[i].0.key);
                None
            }
            Err(err) => {
                report.failed_model(&payloads[i].0.key, &err);
                None
            }
        });
    }
    Models {
        meshes,
        of_model: payload_of
            .into_iter()
            .map(|p| p.and_then(|p| mesh_of_payload[p]))
            .collect(),
        report,
    }
}

/// What tessellating one payload gave.
struct Outcome {
    /// The model's mesh, or `None` when it has no faces at all.
    mesh: Result<Option<Mesh>, String>,
    notes: Vec<String>,
}

/// The mesh and notes for `model` from tessellating its payload.
fn outcome(model: &Model, tessellation: Tessellation) -> Outcome {
    let key = &model.key;
    let notes = tessellation
        .face_failures
        .map(|(failures, faces)| {
            format!("model {key}: {failures} of {faces} faces could not be tessellated")
        })
        .into_iter()
        .collect();
    let mesh = tessellation.primitives.map(|primitives| {
        (!primitives.is_empty()).then(|| Mesh {
            name: model.name().to_owned(),
            primitives,
        })
    });
    Outcome { mesh, notes }
}

/// Tessellate one payload and simplify it within `simplify_error` model
/// millimetres.
fn tessellate_one(bytes: &[u8], simplify_error: f32) -> Tessellation {
    let (tessellated, stats) = match tessellate_step_bytes(bytes) {
        Ok(ok) => ok,
        Err(err) => {
            return Tessellation {
                face_failures: None,
                primitives: Err(err),
            };
        }
    };
    let face_failures = (!stats.failures.is_empty())
        .then_some((stats.failures.len() as u64, stats.num_faces as u64));
    let tessellated_any = tessellated.submeshes.iter().any(|s| !s.indices.is_empty());
    let mut primitives: Vec<([f32; 4], Primitive)> = tessellated
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
            let mut primitive = Primitive {
                positions,
                normals,
                indices,
            };
            primitive.simplify(simplify_error);
            primitive.optimize();
            (color, primitive)
        })
        // Simplification drops solids smaller than the error budget.
        .filter(|(_, p)| !p.indices.is_empty())
        .collect();
    if primitives.is_empty() && !tessellated_any && !stats.failures.is_empty() {
        return Tessellation {
            face_failures,
            primitives: Err("no faces could be tessellated".to_owned()),
        };
    }
    // Order primitives by colour.
    primitives.sort_by(|a, b| a.0.map(f32::to_bits).cmp(&b.0.map(f32::to_bits)));
    Tessellation {
        face_failures,
        primitives: Ok(primitives),
    }
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
