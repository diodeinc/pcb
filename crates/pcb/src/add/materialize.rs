use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use pcb_zen::WorkspaceInfo;
use pcb_zen::resolve::materialize_asset_deps;
use pcb_zen_core::kicad_library::{KicadRepoMatch, match_kicad_managed_repo};
use pcb_zen_core::resolution::{ModuleLine, ResolutionResult};
use semver::Version;

use super::dep_id::ResolvedDepId;

pub(crate) fn materialize_and_vendor(
    workspace: &WorkspaceInfo,
    selected_remote: &BTreeMap<ResolvedDepId, Version>,
) -> Result<()> {
    if selected_remote.is_empty() {
        return Ok(());
    }

    let closure = selected_closure(selected_remote);
    materialize_kicad_assets(workspace, &closure)?;

    let resolution = ResolutionResult {
        workspace_info: workspace.clone(),
        package_resolutions: HashMap::new(),
        closure,
        lockfile_changed: false,
        symbol_parts: HashMap::new(),
    };
    pcb_zen::vendor_deps(&resolution, &[], None, false)?;

    Ok(())
}

fn selected_closure(
    selected_remote: &BTreeMap<ResolvedDepId, Version>,
) -> HashMap<ModuleLine, Version> {
    selected_remote
        .iter()
        .map(|(dep_id, version)| {
            (
                ModuleLine::new(dep_id.path.clone(), version),
                version.clone(),
            )
        })
        .collect()
}

fn materialize_kicad_assets(
    workspace: &WorkspaceInfo,
    closure: &HashMap<ModuleLine, Version>,
) -> Result<()> {
    let mut kicad_assets = HashMap::new();
    let kicad_entries = workspace.kicad_library_entries();

    for (line, version) in closure {
        if match_kicad_managed_repo(&kicad_entries, &line.path, version)
            == KicadRepoMatch::SelectorMatched
        {
            kicad_assets.insert(line.clone(), version.clone());
        }
    }

    materialize_asset_deps(workspace, &kicad_assets, false)?;
    Ok(())
}
