use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::WorkspaceInfo;
use crate::cache_index::{CacheIndex, ensure_workspace_cache_symlink};
use crate::resolve::ensure_package_manifest_in_cache;
use crate::workspace::WorkspaceInfoExt;
use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::file_extensions;
use pcb_zen_core::is_stdlib_module_path;
use pcb_zen_core::kicad_library::{
    KicadRepoMatch, effective_kicad_library_for_repo, match_kicad_managed_repo,
};
use pcb_zen_core::resolution::{
    FrozenPackage, FrozenPackageIdentity, FrozenResolutionMap, FrozenResolutionSet,
    ResolutionResult,
};
use semver::Version;

use super::manifest::{ManifestLoader, package_version_root};
use super::materialize::materialize_selected;
use super::{ResolvedDepId, compatibility_lane, parse_lane_qualified_key};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PackageNode {
    Workspace(String),
    Remote {
        dep_id: ResolvedDepId,
        version: Version,
    },
}

pub fn target_package_urls_for_path(workspace: &WorkspaceInfo, path: &Path) -> Result<Vec<String>> {
    let path = path.canonicalize()?;
    if path.is_file() {
        return package_url_for_zen(workspace, &path).map(|url| vec![url]);
    }

    if let Some(package_url) = package_url_for_package_dir(workspace, &path) {
        return Ok(vec![package_url]);
    }

    let mut package_urls = BTreeSet::new();
    for zen_file in collect_workspace_zen_files(&path, workspace)? {
        package_urls.insert(package_url_for_zen(workspace, &zen_file)?);
    }
    Ok(package_urls.into_iter().collect())
}

pub fn build_frozen_resolution_maps(
    workspace: &WorkspaceInfo,
    package_urls: impl IntoIterator<Item = String>,
    offline: bool,
) -> Result<BTreeMap<String, FrozenResolutionMap>> {
    let mut builder = FrozenResolutionBuilder::new(workspace.clone(), offline)?;
    let mut resolutions = BTreeMap::new();
    for package_url in package_urls {
        let resolution = builder
            .build(&package_url)
            .with_context(|| format!("while resolving dependencies for {package_url}"))?;
        resolutions.insert(package_url, resolution);
    }
    Ok(resolutions)
}

pub fn resolve_workspace_dependencies(
    mut workspace_info: WorkspaceInfo,
    path: &Path,
    offline: bool,
    locked: bool,
) -> Result<ResolutionResult> {
    let package_urls = target_package_urls_for_path(&workspace_info, path)
        .inspect_err(|err| {
            log::debug!(
                "Skipping MVS v2 target discovery for {}: {err:#}",
                path.display()
            );
        })
        .unwrap_or_default();
    if all_packages_have_indirect(&workspace_info, &package_urls) {
        return resolve_frozen(workspace_info, package_urls, offline);
    }

    let mut res = crate::resolve_dependencies(&mut workspace_info, offline, locked)?;
    attach_mvs_v2_resolution_for_packages(&mut res, package_urls, offline);
    Ok(res)
}

fn resolve_frozen(
    workspace_info: WorkspaceInfo,
    package_urls: Vec<String>,
    offline: bool,
) -> Result<ResolutionResult> {
    if workspace_info.stdlib_patch_path().is_none() {
        crate::cache_index::ensure_stdlib_materialized(&workspace_info.root)?;
    }

    let mut resolution_set = FrozenResolutionSet::default();
    let mut symbol_parts = HashMap::new();

    for (package_url, resolution) in
        build_frozen_resolution_maps(&workspace_info, package_urls, offline)?
    {
        symbol_parts.extend(crate::resolve::build_frozen_symbol_parts(
            &workspace_info,
            &resolution,
        )?);
        resolution_set.insert(package_url, resolution);
    }

    Ok(ResolutionResult::frozen(
        workspace_info,
        resolution_set,
        symbol_parts,
    ))
}

pub fn attach_mvs_v2_resolution_for_packages(
    res: &mut ResolutionResult,
    package_urls: impl IntoIterator<Item = String>,
    offline: bool,
) {
    let package_urls: Vec<_> = package_urls
        .into_iter()
        .filter(|package_url| package_has_indirect(&res.workspace_info, package_url))
        .collect();
    if package_urls.is_empty() {
        return;
    }

    match build_frozen_resolution_maps(&res.workspace_info, package_urls, offline) {
        Ok(resolution) => res.set_mvs_v2_resolution(resolution),
        Err(err) => log::debug!("Skipping shadow MVS v2 resolution: {err:#}"),
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

fn collect_workspace_zen_files(
    path: &Path,
    workspace_info: &WorkspaceInfo,
) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(path);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .filter_entry(crate::ast_utils::skip_vendor);

    let mut zen_files = Vec::new();
    for result in builder.build() {
        let entry = result?;
        let path = entry.path();
        if path.is_file() && file_extensions::is_starlark_file(path.extension()) {
            zen_files.push(path.to_path_buf());
        }
    }
    if !workspace_info.packages.is_empty() {
        zen_files.retain(|p| {
            workspace_info
                .packages
                .values()
                .any(|pkg| p.starts_with(pkg.dir(&workspace_info.root)))
        });
    }
    if zen_files.is_empty() {
        bail!("No .zen source files found in {}", path.display());
    }
    zen_files.sort();
    Ok(zen_files)
}

struct FrozenResolutionBuilder {
    workspace: WorkspaceInfo,
    offline: bool,
    cache_index: CacheIndex,
    manifest_loader: ManifestLoader,
    selected_remote: BTreeMap<ResolvedDepId, Version>,
    materialized_remote: BTreeSet<(ResolvedDepId, Version)>,
    remote_roots: BTreeMap<(String, Version), PathBuf>,
    packages: BTreeMap<PathBuf, FrozenPackage>,
}

impl FrozenResolutionBuilder {
    fn new(workspace: WorkspaceInfo, offline: bool) -> Result<Self> {
        ensure_workspace_cache_symlink(&workspace.root)?;
        Ok(Self {
            cache_index: CacheIndex::open()?,
            manifest_loader: ManifestLoader::new(workspace.clone(), offline),
            workspace,
            offline,
            selected_remote: BTreeMap::new(),
            materialized_remote: BTreeSet::new(),
            remote_roots: BTreeMap::new(),
            packages: BTreeMap::new(),
        })
    }

    fn build(&mut self, package_url: &str) -> Result<FrozenResolutionMap> {
        self.selected_remote = self
            .selected_remote_from_root_manifest(package_url)
            .with_context(|| format!("while reading resolved closure for {}", package_url))?;

        self.materialize_selected_remote()?;
        self.packages.clear();

        let mut queue = VecDeque::from([PackageNode::Workspace(package_url.to_string())]);
        let mut seen = BTreeSet::new();
        while let Some(node) = queue.pop_front() {
            if !seen.insert(node.clone()) {
                continue;
            }
            self.resolve_package_node(node, &mut queue)?;
        }
        self.add_stdlib_package()?;

        Ok(FrozenResolutionMap {
            selected_remote: self.selected_remote.clone().into_iter().collect(),
            packages: std::mem::take(&mut self.packages),
        })
    }

    fn materialize_selected_remote(&mut self) -> Result<()> {
        let pending: BTreeMap<_, _> = self
            .selected_remote
            .iter()
            .filter(|(dep_id, version)| {
                !self
                    .materialized_remote
                    .contains(&((*dep_id).clone(), (*version).clone()))
            })
            .map(|(dep_id, version)| (dep_id.clone(), version.clone()))
            .collect();

        if pending.is_empty() {
            return Ok(());
        }

        materialize_selected(&self.workspace, &pending, self.offline)?;
        self.materialized_remote.extend(pending);
        Ok(())
    }

    fn resolve_package_node(
        &mut self,
        node: PackageNode,
        queue: &mut VecDeque<PackageNode>,
    ) -> Result<()> {
        let (identity, package_root, direct_deps, parts) = match node {
            PackageNode::Workspace(package_url) => {
                let (package_root, config) = self.workspace_manifest(&package_url)?;
                (
                    FrozenPackageIdentity::Workspace(package_url),
                    package_root,
                    config.dependencies.direct,
                    config.parts,
                )
            }
            PackageNode::Remote { dep_id, version } => {
                let package_root = self.remote_package_root(&dep_id.path, &version)?;
                let manifest = self
                    .manifest_loader
                    .load(&self.cache_index, &dep_id.path, &version)
                    .with_context(|| format!("Failed to load {}@{}", dep_id.path, version))?;
                (
                    FrozenPackageIdentity::Remote { dep_id, version },
                    package_root,
                    manifest.direct,
                    manifest.parts,
                )
            }
        };

        let deps = self.resolve_direct_deps(&package_root, &direct_deps, queue)?;
        self.packages.insert(
            canonicalize(&package_root),
            FrozenPackage {
                identity,
                deps,
                parts,
            },
        );
        Ok(())
    }

    fn resolve_direct_deps(
        &mut self,
        package_root: &Path,
        direct_deps: &BTreeMap<String, DependencySpec>,
        queue: &mut VecDeque<PackageNode>,
    ) -> Result<BTreeMap<String, PathBuf>> {
        let mut resolved = BTreeMap::new();

        for (dep_url, spec) in direct_deps {
            if is_stdlib_module_path(dep_url) {
                continue;
            }

            if let Some(path) = local_path_dependency_root(package_root, spec) {
                resolved.insert(dep_url.clone(), canonicalize(&path));
                continue;
            }

            if let Some(workspace_root) = self.workspace_dep_root(dep_url) {
                resolved.insert(dep_url.clone(), canonicalize(&workspace_root));
                queue.push_back(PackageNode::Workspace(dep_url.clone()));
                continue;
            }

            let requested_version = exact_spec_version(dep_url, spec)?;
            let dep_id = ResolvedDepId::for_version(dep_url.clone(), &requested_version);
            let selected_version = self.selected_remote.get(&dep_id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "Resolved closure is missing {}@{} required by {}",
                    dep_id.path,
                    dep_id.lane,
                    package_root.display()
                )
            })?;
            let dep_root = self.remote_package_root(&dep_id.path, &selected_version)?;
            resolved.insert(dep_url.clone(), canonicalize(&dep_root));
            queue.push_back(PackageNode::Remote {
                dep_id,
                version: selected_version,
            });
        }

        self.add_kicad_sibling_deps(&mut resolved, queue)?;

        Ok(resolved)
    }

    fn add_kicad_sibling_deps(
        &mut self,
        resolved: &mut BTreeMap<String, PathBuf>,
        queue: &mut VecDeque<PackageNode>,
    ) -> Result<()> {
        let kicad_entries = self.workspace.kicad_library_entries();
        let resolved_kicad_repos: Vec<_> = resolved
            .iter()
            .filter_map(|(repo, root)| {
                let version = root.file_name().and_then(|name| name.to_str())?;
                let version = Version::parse(version).ok()?;
                effective_kicad_library_for_repo(&kicad_entries, repo, &version)
                    .map(|entry| (entry, version))
            })
            .collect();

        for (entry, version) in resolved_kicad_repos {
            for sibling_repo in entry.repo_urls() {
                if resolved.contains_key(sibling_repo) {
                    continue;
                }

                let dep_id = ResolvedDepId::for_version(sibling_repo.to_string(), &version);
                let selected_version =
                    self.selected_remote.get(&dep_id).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "Resolved closure is missing {}@{} required by KiCad library expansion",
                            dep_id.path,
                            dep_id.lane
                        )
                    })?;
                let dep_root = self.remote_package_root(sibling_repo, &selected_version)?;
                resolved.insert(sibling_repo.to_string(), canonicalize(&dep_root));
                queue.push_back(PackageNode::Remote {
                    dep_id,
                    version: selected_version,
                });
            }
        }

        Ok(())
    }

    fn add_stdlib_package(&mut self) -> Result<()> {
        let mut deps = BTreeMap::new();
        for (repo, version) in self.workspace.stdlib_asset_dep_versions() {
            let root = self.remote_package_root(&repo, &version)?;
            deps.insert(repo, canonicalize(&root));
        }

        self.packages.insert(
            canonicalize(&self.workspace.workspace_stdlib_dir()),
            FrozenPackage {
                identity: FrozenPackageIdentity::Stdlib,
                deps,
                parts: Vec::new(),
            },
        );
        Ok(())
    }

    fn workspace_manifest(&self, package_url: &str) -> Result<(PathBuf, PcbToml)> {
        if let Some(pkg) = self.workspace.packages.get(package_url) {
            return Ok((pkg.dir(&self.workspace.root), pkg.config.clone()));
        }

        if self.workspace.workspace_base_url().as_deref() == Some(package_url)
            && let Some(config) = self.workspace.config.clone()
        {
            return Ok((self.workspace.root.clone(), config));
        }

        bail!("Unknown workspace package {}", package_url)
    }

    fn workspace_dep_root(&self, dep_url: &str) -> Option<PathBuf> {
        if let Some(pkg) = self.workspace.packages.get(dep_url) {
            return Some(pkg.dir(&self.workspace.root));
        }
        (self.workspace.workspace_base_url().as_deref() == Some(dep_url))
            .then(|| self.workspace.root.clone())
    }

    fn remote_package_root(&mut self, module_path: &str, version: &Version) -> Result<PathBuf> {
        let key = (module_path.to_string(), version.clone());
        if let Some(root) = self.remote_roots.get(&key) {
            return Ok(root.clone());
        }

        let version_str = version.to_string();
        let vendor_root =
            package_version_root(self.workspace.root.join("vendor"), module_path, version);
        if vendor_root.exists() {
            self.remote_roots.insert(key, vendor_root.clone());
            return Ok(vendor_root);
        }

        let cache_root =
            package_version_root(self.workspace.workspace_cache_dir(), module_path, version);
        let managed_kicad = self.is_managed_kicad_dep(module_path, version);
        if cache_root.join("pcb.toml").exists() || managed_kicad && cache_root.exists() {
            self.remote_roots.insert(key, cache_root.clone());
            return Ok(cache_root);
        }

        if self.offline {
            bail!(
                "{}@{} is not cached. Run `pcb build` once online to fetch it.",
                module_path,
                version_str
            );
        }

        if !managed_kicad {
            ensure_package_manifest_in_cache(module_path, version, &self.cache_index)?;
        }
        self.remote_roots.insert(key, cache_root.clone());
        Ok(cache_root)
    }

    fn is_managed_kicad_dep(&self, path: &str, version: &Version) -> bool {
        matches!(
            match_kicad_managed_repo(&self.workspace.kicad_library_entries(), path, version),
            KicadRepoMatch::SelectorMatched
        )
    }

    fn selected_remote_from_root_manifest(
        &self,
        package_url: &str,
    ) -> Result<BTreeMap<ResolvedDepId, Version>> {
        let (_, config) = self.workspace_manifest(package_url)?;
        if config.dependencies.indirect.is_empty() {
            bail!(
                "{} is missing resolved dependency entries; run `pcb sync` first",
                package_url
            );
        }

        let mut selected = BTreeMap::new();
        for (dep_url, spec) in &config.dependencies.direct {
            if self.is_remote_dependency(dep_url, spec) {
                let version = exact_spec_version(dep_url, spec)?;
                selected.insert(
                    ResolvedDepId::for_version(dep_url.clone(), &version),
                    version,
                );
            }
        }

        for (raw_key, spec) in &config.dependencies.indirect {
            let dep_id = parse_lane_qualified_key(raw_key)?;
            let version = exact_spec_version(raw_key, spec)?;
            let expected_lane = compatibility_lane(&version);
            if dep_id.lane != expected_lane {
                bail!(
                    "Indirect dependency {} resolves to lane {}, not {}",
                    raw_key,
                    expected_lane,
                    dep_id.lane
                );
            }
            selected.insert(dep_id, version);
        }

        Ok(selected)
    }

    fn is_remote_dependency(&self, dep_url: &str, spec: &DependencySpec) -> bool {
        !is_stdlib_module_path(dep_url)
            && !self.workspace.packages.contains_key(dep_url)
            && self.workspace.workspace_base_url().as_deref() != Some(dep_url)
            && local_path_dependency_root(Path::new("."), spec).is_none()
    }
}

fn canonicalize(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn package_url_for_zen(workspace: &WorkspaceInfo, path: &Path) -> Result<String> {
    workspace
        .package_url_for_zen(path)
        .ok_or_else(|| anyhow::anyhow!("No workspace package contains {}", path.display()))
}

fn package_url_for_package_dir(workspace: &WorkspaceInfo, path: &Path) -> Option<String> {
    workspace
        .packages
        .iter()
        .find(|(_, pkg)| {
            pkg.dir(&workspace.root)
                .canonicalize()
                .is_ok_and(|dir| dir == path)
        })
        .map(|(url, _)| url.clone())
}

fn exact_spec_version(dep_url: &str, spec: &DependencySpec) -> Result<Version> {
    let raw = match spec {
        DependencySpec::Version(version) => version,
        DependencySpec::Detailed(detail) if detail.version.is_some() => {
            detail.version.as_ref().expect("checked above")
        }
        DependencySpec::Detailed(_) => {
            bail!(
                "Dependency {} must specify an exact version; run `pcb sync` to update dependency versions",
                dep_url
            );
        }
    };
    pcb_zen_core::parse_relaxed_version(raw)
        .ok_or_else(|| anyhow::anyhow!("Dependency {} has invalid version '{}'", dep_url, raw))
}

fn local_path_dependency_root(package_root: &Path, spec: &DependencySpec) -> Option<PathBuf> {
    let DependencySpec::Detailed(detail) = spec else {
        return None;
    };
    detail.path.as_ref().map(|path| package_root.join(path))
}
