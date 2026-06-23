use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pcb_ir::dialects::ipc::profile_occurrences_for;

use crate::LayoutTarget;
use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

/// Options for exporting IPC-2581 profile outlines.
#[derive(Debug, Clone)]
pub struct OutlineOptions {
    pub output: PathBuf,
    pub layout_target: LayoutTarget,
}

/// Export Step/Profile outlines as a DXF file.
pub fn execute(input_file: &Path, options: &OutlineOptions) -> Result<()> {
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = Ipc2581::parse(&content)?;
    let layout = geometry::extract_layout(&ipc)?;
    let profile_set = options.layout_target.geometry_view().profile_set();
    if profile_occurrences_for(&layout, profile_set).is_empty() {
        bail!("IPC-2581 primary step and repeated child steps have no board Profile outline");
    }

    let dxf = geometry::dxf::render_profile_set_dxf(&layout, profile_set);
    std::fs::write(&options.output, dxf)
        .with_context(|| format!("Failed to write DXF to {}", options.output.display()))?;
    println!(
        "✓ IPC-2581 outline exported to {}",
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

        let layout = geometry::extract_layout(&ipc).unwrap();

        assert_eq!(pcb_ir::dialects::ipc::board_step_count(&layout), 1);
        assert_eq!(pcb_ir::dialects::ipc::panel_step_count(&layout), 1);
        assert_eq!(pcb_ir::dialects::ipc::board_instance_count(&layout), 1);
        assert_eq!(layout.profiles.len(), 1);
        assert_eq!(
            profile_occurrences_for(
                &layout,
                LayoutTarget::BoardArray.geometry_view().profile_set()
            )
            .len(),
            1
        );
    }
}
