use std::{collections::HashMap, path::Path};

use anyhow::{Result, bail};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::resolution::{FrozenResolutionSet, ResolutionResult};
use pcb_zen_core::workspace::WorkspaceInfo;
use tracing::instrument;

use pcb_zen::{get_workspace_info, resolve_dependencies};

/// Resolve dependencies for a workspace/board.
/// This is a shared helper used by build, bom, layout, open, etc.
///
/// If `input_path` is None or empty, defaults to the current working directory.
///
/// When `locked` is true:
/// - Auto-deps will not modify pcb.toml files
/// - The lockfile (pcb.sum) will not be written
/// - An existing pcb.sum is verified, but a missing one does not cause failure
#[instrument(name = "resolve_dependencies", skip_all)]
pub fn resolve(input_path: Option<&Path>, offline: bool, locked: bool) -> Result<ResolutionResult> {
    let cwd;
    let path = match input_path {
        // Handle both None and empty paths (e.g., "file.zen".parent() returns Some(""))
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => {
            cwd = std::env::current_dir()?;
            &cwd
        }
    };
    let mut workspace_info = get_workspace_info(&DefaultFileProvider::new(), path)?;

    // Fail on workspace discovery errors (invalid pcb.toml files)
    if !workspace_info.errors.is_empty() {
        for err in &workspace_info.errors {
            eprintln!("{}: {}", err.path.display(), err.error);
        }
        bail!(
            "Found {} invalid pcb.toml file(s)",
            workspace_info.errors.len()
        );
    }

    let package_urls = crate::add::target_package_urls_for_path(&workspace_info, path)
        .inspect_err(|err| {
            log::debug!(
                "Skipping MVS v2 target discovery for {}: {err:#}",
                path.display()
            );
        })
        .unwrap_or_default();
    if all_packages_have_indirect(&workspace_info, &package_urls) {
        return resolve_mvs_v2(workspace_info, package_urls, offline);
    }

    let mut res = resolve_dependencies(&mut workspace_info, offline, locked)?;

    // Sync vendor dir: add missing, prune stale (only prune when not offline and not locked)
    let prune = !offline && !locked;
    let vendor_result = pcb_zen::vendor_deps(&res, &[], None, prune)?;

    // If we pruned stale entries, re-run resolution so the dep map points to valid paths
    if vendor_result.pruned_count > 0 {
        log::debug!(
            "Pruned {} stale vendor entries, re-running resolution",
            vendor_result.pruned_count
        );
        res = resolve_dependencies(&mut workspace_info, offline, locked)?;
    }

    Ok(res)
}

fn resolve_mvs_v2(
    workspace_info: WorkspaceInfo,
    package_urls: Vec<String>,
    offline: bool,
) -> Result<ResolutionResult> {
    if workspace_info.stdlib_patch_path().is_none() {
        pcb_zen::cache_index::ensure_stdlib_materialized(&workspace_info.root)?;
    }

    let mut resolution_set = FrozenResolutionSet::default();
    let mut symbol_parts = HashMap::new();

    for package_url in package_urls {
        let resolution =
            crate::add::build_frozen_resolution_map(&workspace_info, &package_url, offline)?;
        symbol_parts.extend(pcb_zen::resolve::build_frozen_symbol_parts(
            &workspace_info,
            &resolution,
        )?);
        resolution_set.root_packages.insert(package_url, resolution);
    }

    Ok(ResolutionResult {
        workspace_info,
        package_resolutions: HashMap::new(),
        closure: HashMap::new(),
        mvs_v2_resolution: Some(resolution_set),
        lockfile_changed: false,
        symbol_parts,
    })
}

pub(crate) fn attach_mvs_v2_resolution_for_packages(
    res: &mut ResolutionResult,
    package_urls: impl IntoIterator<Item = String>,
    offline: bool,
) {
    let mut resolution_set = FrozenResolutionSet::default();

    for package_url in package_urls {
        if !package_has_indirect(&res.workspace_info, &package_url) {
            continue;
        }
        match crate::add::build_frozen_resolution_map(&res.workspace_info, &package_url, offline) {
            Ok(resolution) => {
                resolution_set
                    .root_packages
                    .insert(package_url.to_string(), resolution);
            }
            Err(err) => {
                log::debug!("Skipping shadow MVS v2 resolution for {package_url}: {err:#}");
            }
        }
    }

    if !resolution_set.root_packages.is_empty() {
        res.mvs_v2_resolution = Some(resolution_set);
    }
}

fn all_packages_have_indirect(workspace_info: &WorkspaceInfo, package_urls: &[String]) -> bool {
    !package_urls.is_empty()
        && package_urls
            .iter()
            .all(|package_url| package_has_indirect(workspace_info, package_url))
}

fn package_has_indirect(workspace_info: &WorkspaceInfo, package_url: &str) -> bool {
    workspace_info
        .packages
        .get(package_url)
        .is_some_and(|package| !package.config.dependencies.indirect.is_empty())
}
