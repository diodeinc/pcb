use crate::{datum::FootprintDatums, idf, locate, placement};
use pcb_sch::Schematic;
use pcb_zen_core::resolution::ResolutionResult;
use std::path::Path;

/// Apply MCAD-owned placements from the board's IDF input (if any) onto the
/// schematic's component instances as fixed poses.
pub fn apply_mcad_positions(
    schematic: &mut Schematic,
    board_zen_path: &Path,
    resolution: &ResolutionResult,
) -> anyhow::Result<()> {
    let Some(emn) = locate::idf_for_board(schematic, board_zen_path, resolution)? else {
        return Ok(());
    };

    let datums = FootprintDatums::load_for_board(board_zen_path, &resolution.workspace_info.root)?;
    let claims = idf::load_placement_claims(&emn)?;
    let positions = placement::resolve_mcad_positions(schematic, &claims, &emn, &datums)?;

    log::info!(
        "applied {} MCAD position(s) from IDF {}",
        positions.len(),
        emn.display()
    );
    for (instance_ref, placement) in positions {
        let instance = schematic
            .instances
            .get_mut(&instance_ref)
            .expect("resolved MCAD position came from schematic");
        instance.placement = Some(placement);
    }
    Ok(())
}
