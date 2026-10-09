//! KiCad PCB to glTF 2.0 binary (GLB) export: the assembly `pcb-step`
//! writes as STEP, as triangle meshes. The board comes from the same
//! [`Scene`]; each distinct embedded STEP model is tessellated once with
//! foxtrot and placed by every footprint that uses it.
//!
//! The output frame is the STEP frame converted to glTF's: metres, y up.

mod board;
mod glb;
mod mesh;
mod models;

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
    // layers are built.
    let mut scene = Scene::components(board, options, &mut report.warnings);
    let mut layer_warnings = Vec::new();
    let (layers, mut models) = rayon::join(
        || -> Result<_> {
            let layers = Scene::layers(board, options, &mut layer_warnings)?;
            let meshes = board::mesh(&layers, options.cut_vias);
            Ok((layers, meshes))
        },
        || models::tessellate(board, &scene),
    );
    report.warnings.append(&mut layer_warnings);
    report.warnings.append(&mut models.warnings);
    report.failed_models += models.failed;
    let (layers, meshes) = layers?;
    scene.layers = layers;
    glb::write(&options.name, &scene, &meshes, &models, sink)
}
