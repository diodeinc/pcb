use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use pcb_zen_core::kicad_library::{KicadRepoMatch, match_kicad_managed_repo};
use pcb_zen_core::resolution::ModuleLine;
use semver::Version;

use super::ResolvedDepId;

pub fn materialize_selected(
    workspace: &crate::WorkspaceInfo,
    selected_remote: &BTreeMap<ResolvedDepId, Version>,
    offline: bool,
) -> Result<BTreeSet<(String, String)>> {
    let mut package_roots = BTreeSet::new();
    let mut kicad_assets = HashMap::new();
    let kicad_entries = workspace.kicad_library_entries();

    for (dep_id, version) in selected_remote {
        package_roots.insert((dep_id.path.clone(), version.to_string()));
        let line = ModuleLine::new(dep_id.path.clone(), version);
        if match_kicad_managed_repo(&kicad_entries, &line.path, version)
            == KicadRepoMatch::SelectorMatched
        {
            kicad_assets.insert(line, version.clone());
        }
    }

    crate::resolve::materialize_asset_deps(workspace, &kicad_assets, offline)?;
    Ok(package_roots)
}

pub fn vendor_selected(
    workspace: &crate::WorkspaceInfo,
    package_roots: &BTreeSet<(String, String)>,
    prune: bool,
) -> Result<()> {
    crate::resolve::vendor_package_roots(workspace, package_roots, &[], None, prune)?;
    Ok(())
}
