use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ipc2581::types::ecad::Step;

use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

/// Options for exporting the IPC-2581 board outline.
#[derive(Debug, Clone)]
pub struct OutlineOptions {
    pub output: PathBuf,
}

/// Export the board outline from Step/Profile as a DXF file.
pub fn execute(input_file: &Path, options: &OutlineOptions) -> Result<()> {
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = Ipc2581::parse(&content)?;
    let step = primary_step(&ipc)?;
    let profile = step.profile.as_ref().with_context(|| {
        format!(
            "IPC-2581 step '{}' has no board Profile outline",
            ipc.resolve(step.name)
        )
    })?;

    let dxf = geometry::dxf::render_outline_dxf(profile);
    std::fs::write(&options.output, dxf)
        .with_context(|| format!("Failed to write DXF to {}", options.output.display()))?;
    println!(
        "✓ IPC-2581 board outline exported to {}",
        options.output.display()
    );
    Ok(())
}

fn primary_step(ipc: &Ipc2581) -> Result<&Step> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    if let Some(step_ref) = ipc.content().step_refs.first()
        && let Some(step) = ecad
            .cad_data
            .steps
            .iter()
            .find(|step| step.name == *step_ref)
    {
        return Ok(step);
    }

    ecad.cad_data
        .steps
        .first()
        .context("IPC-2581 ECAD section has no Step")
}
