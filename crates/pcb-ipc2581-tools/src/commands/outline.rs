use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ipc2581::Symbol;
use ipc2581::types::Profile;
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
    let profile = board_profile(&ipc)?;

    let dxf = geometry::dxf::render_outline_dxf(profile);
    std::fs::write(&options.output, dxf)
        .with_context(|| format!("Failed to write DXF to {}", options.output.display()))?;
    println!(
        "✓ IPC-2581 board outline exported to {}",
        options.output.display()
    );
    Ok(())
}

fn board_profile(ipc: &Ipc2581) -> Result<&Profile> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let primary = geometry::primary_step(ipc, &ecad.cad_data.steps)?;
    find_profile_step(primary, &ecad.cad_data.steps, &mut HashSet::new())
        .and_then(|step| step.profile.as_ref())
        .with_context(|| {
            format!(
                "IPC-2581 step '{}' and its repeated child steps have no board Profile outline",
                ipc.resolve(primary.name)
            )
        })
}

fn find_profile_step<'a>(
    step: &'a Step,
    steps: &'a [Step],
    visited: &mut HashSet<Symbol>,
) -> Option<&'a Step> {
    if !visited.insert(step.name) {
        return None;
    }
    if step.profile.is_some() {
        return Some(step);
    }

    step.step_repeats.iter().find_map(|repeat| {
        steps
            .iter()
            .find(|candidate| candidate.name == repeat.step_ref)
            .and_then(|child| find_profile_step(child, steps, visited))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_repeated_child_profile_when_primary_step_is_panel() {
        let ipc = Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="0" y="0" nx="1" ny="1"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let profile = board_profile(&ipc).unwrap();

        assert_eq!(profile.polygon.steps.len(), 3);
    }
}
