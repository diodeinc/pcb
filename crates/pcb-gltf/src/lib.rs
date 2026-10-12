//! KiCad PCB to glTF 2.0 binary (GLB) export: the assembly `pcb-step`
//! writes as STEP, as triangle meshes. The board comes from the same
//! [`Scene`]; each distinct embedded STEP model is tessellated once with
//! foxtrot and placed by every footprint that uses it.
//!
//! The output frame is the STEP frame converted to glTF's: metres, y up.

mod board;
mod cache;
mod glb;
mod mesh;
mod models;

use std::io::Write;

pub use cache::{CacheStats, ModelCache};

use anyhow::Result;
use pcb_step::scene::Scene;
use pcb_step::{Board, Options, Report};

/// Write the GLB for `board` to `sink`, adding warnings and models that
/// could not be read to `report` as they are found.
///
/// Tessellated models are cached on disk when `PCB_GLTF_MODEL_CACHE_DIR`
/// names a directory; see [`ModelCache`].
pub fn export(
    board: &Board,
    options: &Options,
    sink: &mut dyn Write,
    report: &mut Report,
) -> Result<()> {
    export_with_cache(
        board,
        options,
        ModelCache::from_env().as_ref(),
        sink,
        report,
    )
}

/// [`export`] with the model cache given rather than read from the
/// environment. The output and the report are the same with any cache, or
/// none.
pub fn export_with_cache(
    board: &Board,
    options: &Options,
    cache: Option<&ModelCache>,
    sink: &mut dyn Write,
    report: &mut Report,
) -> Result<()> {
    // The models need only the components, so they tessellate while the
    // layers are built, on a pool of their own: in one queue, whichever
    // is on the critical path would wait behind the other's work.
    let mut scene = Scene::components(board, options, &mut report.warnings);
    let mut layer_warnings = Vec::new();
    let board_pool = rayon::ThreadPoolBuilder::new().build()?;
    let (layers, mut models) = rayon::join(
        || -> Result<_> {
            board_pool.install(|| {
                let layers = Scene::layers(board, options, &mut layer_warnings)?;
                let meshes = board::mesh(&layers);
                Ok((layers, meshes))
            })
        },
        || models::tessellate(board, &scene, cache),
    );
    report.warnings.append(&mut layer_warnings);
    report.warnings.append(&mut models.report.warnings);
    report.failed_models += models.report.failed_models;
    let (layers, meshes) = layers?;
    scene.layers = layers;
    glb::write(
        &options.name,
        &scene,
        board.copper_color(),
        &meshes,
        &models,
        sink,
    )
}
