use std::collections::BTreeSet;

use crate::cache_index::CacheIndex;
use anyhow::{Context, Result};
use semver::Version;

use super::ResolvedDepId;

pub(crate) fn materialize_selected<'a>(
    workspace: &crate::WorkspaceInfo,
    selected_remote: impl IntoIterator<Item = (&'a ResolvedDepId, &'a Version)>,
    offline: bool,
    cache_index: &CacheIndex,
) -> Result<BTreeSet<(String, String)>> {
    let mut package_roots = BTreeSet::new();

    for (dep_id, version) in selected_remote {
        package_roots.insert((dep_id.path.clone(), version.to_string()));
        ensure_remote_package_materialized(workspace, &dep_id.path, version, offline, cache_index)?;
    }

    Ok(package_roots)
}

fn ensure_remote_package_materialized(
    workspace: &crate::WorkspaceInfo,
    module_path: &str,
    version: &Version,
    offline: bool,
    cache_index: &CacheIndex,
) -> Result<()> {
    let version_str = version.to_string();
    let manifest_rel = std::path::Path::new(module_path)
        .join(&version_str)
        .join("pcb.toml");
    if workspace.root.join("vendor").join(&manifest_rel).exists()
        || workspace.cache_dir.join(&manifest_rel).exists()
    {
        return Ok(());
    }

    if offline {
        anyhow::bail!(
            "{}@{} is not cached. Run `pcb build` once online to fetch it.",
            module_path,
            version_str
        );
    }

    crate::resolve::ensure_package_manifest_in_cache(module_path, version, cache_index)
        .with_context(|| format!("Failed to materialize {}@{}", module_path, version))?;
    Ok(())
}

pub fn plan_vendor_selected(
    workspace: &crate::WorkspaceInfo,
    package_roots: &BTreeSet<(String, String)>,
    prune: bool,
) -> Result<crate::resolve::VendorPlan> {
    crate::resolve::plan_vendor_package_roots(workspace, package_roots, &[], None, prune)
}
