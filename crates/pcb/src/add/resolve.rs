use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pcb_zen::WorkspaceInfo;
use pcb_zen::cache_index::{CacheIndex, ensure_workspace_cache_symlink};
use pcb_zen::resolve::ensure_package_manifest_in_cache;
use pcb_zen::tags;
use pcb_zen::workspace::WorkspaceInfoExt;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::kicad_library::{KicadRepoMatch, match_kicad_managed_repo};
use pcb_zen_core::{STDLIB_MODULE_PATH, is_stdlib_module_path};
use semver::Version;

use crate::file_walker;

use super::dep_id::{ResolvedDepId, compatibility_lane, parse_lane_qualified_key};
use super::manifest::ManifestLoader;
use super::materialize::materialize_selected;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PackageNode {
    Workspace(String),
    Remote {
        dep_id: ResolvedDepId,
        version: Version,
    },
}

#[derive(Debug, Clone)]
pub(super) struct FrozenResolutionMap {
    pub(super) selected_remote: BTreeMap<ResolvedDepId, Version>,
    pub(super) packages: BTreeMap<PathBuf, FrozenPackage>,
}

#[derive(Debug, Clone)]
pub(super) struct FrozenPackage {
    pub(super) identity: PackageIdentity,
    pub(super) deps: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Clone)]
pub(super) enum PackageIdentity {
    Workspace(String),
    Remote {
        dep_id: ResolvedDepId,
        version: Version,
    },
    Stdlib,
}

impl PackageIdentity {
    pub(super) fn display(&self) -> String {
        match self {
            Self::Workspace(url) => url.clone(),
            Self::Remote { dep_id, version } => {
                format!("{}@{} = {}", dep_id.path, dep_id.lane, version)
            }
            Self::Stdlib => STDLIB_MODULE_PATH.to_string(),
        }
    }
}

pub(crate) fn target_package_urls_for_path(
    workspace: &WorkspaceInfo,
    path: &Path,
) -> Result<Vec<String>> {
    let path = path.canonicalize()?;
    if path.is_file() {
        return package_url_for_zen(workspace, &path).map(|url| vec![url]);
    }

    if let Some(package_url) = package_url_for_package_dir(workspace, &path) {
        return Ok(vec![package_url]);
    }

    let mut package_urls = BTreeSet::new();
    for zen_file in file_walker::collect_workspace_zen_files(Some(&path), workspace)? {
        package_urls.insert(package_url_for_zen(workspace, &zen_file)?);
    }
    Ok(package_urls.into_iter().collect())
}

pub(crate) fn build_frozen_resolution_map(
    workspace: &WorkspaceInfo,
    package_url: &str,
) -> Result<FrozenResolutionMap> {
    FrozenResolutionBuilder::new(workspace.clone())?.build(package_url)
}

struct FrozenResolutionBuilder {
    workspace: WorkspaceInfo,
    cache_index: CacheIndex,
    manifest_loader: ManifestLoader,
    selected_remote: BTreeMap<ResolvedDepId, Version>,
    packages: BTreeMap<PathBuf, FrozenPackage>,
}

impl FrozenResolutionBuilder {
    fn new(workspace: WorkspaceInfo) -> Result<Self> {
        ensure_workspace_cache_symlink(&workspace.root)?;
        Ok(Self {
            cache_index: CacheIndex::open()?,
            manifest_loader: ManifestLoader::new(workspace.clone()),
            workspace,
            selected_remote: BTreeMap::new(),
            packages: BTreeMap::new(),
        })
    }

    fn build(mut self, package_url: &str) -> Result<FrozenResolutionMap> {
        self.selected_remote = self
            .selected_remote_from_root_manifest(package_url)
            .with_context(|| format!("while reading resolved closure for {}", package_url))?;

        materialize_selected(&self.workspace, &self.selected_remote)?;

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
            selected_remote: self.selected_remote,
            packages: self.packages,
        })
    }

    fn resolve_package_node(
        &mut self,
        node: PackageNode,
        queue: &mut VecDeque<PackageNode>,
    ) -> Result<()> {
        let (identity, package_root, direct_deps) = match node {
            PackageNode::Workspace(package_url) => {
                let (package_root, config) = self.workspace_manifest(&package_url)?;
                (
                    PackageIdentity::Workspace(package_url),
                    package_root,
                    config.dependencies.direct,
                )
            }
            PackageNode::Remote { dep_id, version } => {
                let package_root = self.remote_package_root(&dep_id.path, &version)?;
                let manifest = self
                    .manifest_loader
                    .load(&self.cache_index, &dep_id.path, &version)
                    .with_context(|| format!("Failed to load {}@{}", dep_id.path, version))?;
                (
                    PackageIdentity::Remote { dep_id, version },
                    package_root,
                    manifest.direct,
                )
            }
        };

        let deps = self.resolve_direct_deps(&package_root, &direct_deps, queue)?;
        self.packages.insert(
            canonicalize(&package_root),
            FrozenPackage { identity, deps },
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

        Ok(resolved)
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
                identity: PackageIdentity::Stdlib,
                deps,
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
        let version_str = version.to_string();
        let vendor_root = self
            .workspace
            .root
            .join("vendor")
            .join(module_path)
            .join(&version_str);
        if vendor_root.exists() {
            return Ok(vendor_root);
        }

        let cache_root = self
            .workspace
            .workspace_cache_dir()
            .join(module_path)
            .join(&version_str);
        let managed_kicad = self.is_managed_kicad_dep(module_path, version);
        if cache_root.join("pcb.toml").exists() || managed_kicad && cache_root.exists() {
            return Ok(cache_root);
        }

        if !managed_kicad {
            ensure_package_manifest_in_cache(module_path, version, &self.cache_index)?;
        }
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
                "{} does not contain a hydrated MVS v2 dependency closure; run `pcb mod sync` first",
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
                "Dependency {} must have an exact version for frozen MVS v2 resolution",
                dep_url
            );
        }
    };
    tags::parse_relaxed_version(raw)
        .ok_or_else(|| anyhow::anyhow!("Dependency {} has invalid version '{}'", dep_url, raw))
}

fn local_path_dependency_root(package_root: &Path, spec: &DependencySpec) -> Option<PathBuf> {
    let DependencySpec::Detailed(detail) = spec else {
        return None;
    };
    detail.path.as_ref().map(|path| package_root.join(path))
}
