//! KiCad PCB to glTF 2.0 binary (GLB) export: the assembly `pcb-step`
//! writes as STEP, as triangle meshes. The board comes from the same
//! [`Scene`]; each distinct STEP model is tessellated once with foxtrot,
//! or VRML model read, and placed by every footprint that uses it.
//!
//! The output frame is the STEP frame converted to glTF's: metres, y up.

mod board;
mod glb;
mod mesh;
mod models;
mod wrl;

use std::io::Write;

use anyhow::Result;
use pcb_step::scene::Scene;
use pcb_step::{Board, Options, Report};

/// Write the GLB for `board` to `sink`, adding warnings and models that
/// could not be read to `report` as they are found.
pub fn export(
    board: &Board,
    options: &Options,
    sink: &mut dyn Write,
    report: &mut Report,
) -> Result<()> {
    // The models need only the components, so they tessellate while the
    // layers are built, on a pool of their own: in one queue, whichever
    // is on the critical path would wait behind the other's work.
    let mut scene = Scene::components(board, options, &mut report.warnings);
    let mut layer_warnings = Vec::new();
    let board_pool = rayon::ThreadPoolBuilder::new().build()?;
    let (built, mut models) = rayon::join(
        || -> Result<_> {
            board_pool.install(|| {
                let built = Scene::layers(board, options, &mut layer_warnings)?;
                let meshes = board::mesh(&built.layers);
                Ok((built, meshes))
            })
        },
        || models::tessellate(board, options, &scene),
    );
    report.warnings.append(&mut layer_warnings);
    report.warnings.append(&mut models.report.warnings);
    report.failed_models += models.report.failed_models;
    let (built, meshes) = built?;
    scene.layers = built.layers;
    scene.synthetic_outline = built.synthetic_outline;
    scene.resolve(board, options, &models.outcomes);
    report.coverage = scene.coverage.clone();
    glb::write(board, options, &scene, &meshes, &models, sink)
}
