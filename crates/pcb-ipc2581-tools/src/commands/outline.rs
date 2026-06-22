use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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
    let profiles = geometry::extract_profiles(&ipc)?;
    if pcb_ir::dialects::ipc::render_profiles(&profiles)
        .next()
        .is_none()
    {
        bail!("IPC-2581 primary step and repeated child steps have no board Profile outline");
    }

    let dxf = geometry::dxf::render_profiles_dxf(&profiles);
    std::fs::write(&options.output, dxf)
        .with_context(|| format!("Failed to write DXF to {}", options.output.display()))?;
    println!(
        "✓ IPC-2581 board outline exported to {}",
        options.output.display()
    );
    Ok(())
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

        let profiles = geometry::extract_profiles(&ipc).unwrap();

        assert_eq!(pcb_ir::dialects::ipc::board_step_count(&profiles), 1);
        assert_eq!(pcb_ir::dialects::ipc::panel_step_count(&profiles), 1);
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&profiles), 1);
        assert_eq!(profiles.profiles.len(), 2);
        assert_eq!(pcb_ir::dialects::ipc::render_profiles(&profiles).count(), 1);
    }
}
