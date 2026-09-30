use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::cache_index::CacheIndex;
use anyhow::{Result, bail};
use semver::Version;

use super::ResolvedDepId;

pub(crate) fn materialize_selected<'a>(
    workspace: &crate::WorkspaceInfo,
    selected_remote: impl IntoIterator<Item = (&'a ResolvedDepId, &'a Version)>,
    offline: bool,
    cache_index: &CacheIndex,
) -> Result<BTreeSet<(String, String)>> {
    let selected: Vec<_> = selected_remote
        .into_iter()
        .map(|(dep_id, version)| (dep_id.path.as_str(), version))
        .collect();
    let unvendored: Vec<_> = selected
        .iter()
        .copied()
        .filter(|(module_path, version)| {
            !package_manifest(&workspace.root.join("vendor"), module_path, version).exists()
        })
        .collect();

    if offline {
        if let Some((module_path, version)) = unvendored.iter().find(|(module_path, version)| {
            !package_manifest(&workspace.cache_dir, module_path, version).exists()
        }) {
            bail!(
                "{}@{} is not cached. Run `pcb build` once online to fetch it.",
                module_path,
                version
            );
        }
    } else {
        crate::resolve::ensure_packages_in_cache(unvendored, cache_index)?;
    }

    Ok(selected
        .into_iter()
        .map(|(module_path, version)| (module_path.to_string(), version.to_string()))
        .collect())
}

fn package_manifest(root: &Path, module_path: &str, version: &Version) -> PathBuf {
    root.join(module_path)
        .join(version.to_string())
        .join("pcb.toml")
}

pub fn plan_vendor_selected(
    workspace: &crate::WorkspaceInfo,
    package_roots: &BTreeSet<(String, String)>,
    prune: bool,
) -> Result<crate::resolve::VendorPlan> {
    crate::resolve::plan_vendor_package_roots(workspace, package_roots, &[], None, prune)
}
