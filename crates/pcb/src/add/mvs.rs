use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_zen::cache_index::CacheIndex;
use pcb_zen::git;
use pcb_zen::tags;
use pcb_zen_core::config::{DependencyDetail, DependencySpec, PcbToml, split_repo_and_subpath};
use pcb_zen_core::{initial_package_version, is_stdlib_module_path};
use semver::Version;

use super::manifest::ManifestLoader;
use super::scan::{ScannedDirectDeps, scan_package_direct_deps};

#[derive(Debug, Clone)]
pub(crate) struct PackageResolutionDebug {
    pub(crate) scanned: ScannedDirectDeps,
    pub(crate) imported_workspace_floors: BTreeMap<String, Version>,
    pub(crate) resolved_remote: BTreeMap<String, Version>,
}

#[derive(Debug, Clone)]
enum PackageResolutionState {
    InProgress,
    Resolved(PackageResolutionDebug),
}

pub(crate) struct PackageResolver {
    workspace: pcb_zen::WorkspaceInfo,
    cache_index: CacheIndex,
    manifest_loader: ManifestLoader,
    spec_resolver: SpecVersionResolver,
    package_states: BTreeMap<String, PackageResolutionState>,
}

impl PackageResolver {
    pub(crate) fn new(workspace: pcb_zen::WorkspaceInfo) -> Result<Self> {
        Ok(Self {
            cache_index: CacheIndex::open()?,
            manifest_loader: ManifestLoader::new(workspace.clone()),
            workspace,
            spec_resolver: SpecVersionResolver::default(),
            package_states: BTreeMap::new(),
        })
    }

    pub(crate) fn resolve_package(&mut self, package_url: &str) -> Result<PackageResolutionDebug> {
        if let Some(state) = self.package_states.get(package_url) {
            match state {
                PackageResolutionState::InProgress => {
                    anyhow::bail!(
                        "Detected workspace dependency cycle while resolving {}",
                        package_url
                    );
                }
                PackageResolutionState::Resolved(existing) => {
                    return Ok(existing.clone());
                }
            }
        }

        self.package_states
            .insert(package_url.to_string(), PackageResolutionState::InProgress);

        let result = self.build_package_resolution(package_url);
        match result {
            Ok(resolution) => {
                self.package_states.insert(
                    package_url.to_string(),
                    PackageResolutionState::Resolved(resolution.clone()),
                );
                Ok(resolution)
            }
            Err(err) => {
                self.package_states.remove(package_url);
                Err(err)
            }
        }
    }

    fn build_package_resolution(&mut self, package_url: &str) -> Result<PackageResolutionDebug> {
        let (package_dir, current_config) = self.package_manifest_source(package_url)?;
        let scanned = scan_package_direct_deps(
            &self.workspace,
            package_url,
            &package_dir,
            &current_config,
            &self.cache_index,
        )
        .with_context(|| format!("while scanning package {}", package_url))?;

        let imported_workspace_floors = self.import_workspace_floors(&scanned)?;

        let resolved_remote = self
            .run_remote_mvs(&scanned, &imported_workspace_floors)
            .with_context(|| {
                format!(
                    "while resolving remote dependency closure for {}",
                    package_url
                )
            })?;

        Ok(PackageResolutionDebug {
            scanned,
            imported_workspace_floors,
            resolved_remote,
        })
    }

    fn import_workspace_floors(
        &mut self,
        scanned: &ScannedDirectDeps,
    ) -> Result<BTreeMap<String, Version>> {
        let mut imported = BTreeMap::new();
        for workspace_dep in &scanned.workspace {
            let child = self.resolve_package(workspace_dep)?;
            for (module_path, version) in child.resolved_remote {
                merge_floor_version(&mut imported, module_path, version);
            }
        }
        Ok(imported)
    }

    fn package_manifest_source(&self, package_url: &str) -> Result<(PathBuf, PcbToml)> {
        if let Some(pkg) = self.workspace.packages.get(package_url) {
            return Ok((pkg.dir(&self.workspace.root), pkg.config.clone()));
        }

        let root_package_url = self
            .workspace
            .workspace_base_url()
            .unwrap_or_else(|| "workspace".to_string());
        if package_url == root_package_url
            && let Some(config) = self.workspace.config.clone()
        {
            return Ok((self.workspace.root.clone(), config));
        }

        anyhow::bail!("Unknown package target {}", package_url)
    }

    fn run_remote_mvs(
        &mut self,
        scanned: &ScannedDirectDeps,
        imported_workspace_floors: &BTreeMap<String, Version>,
    ) -> Result<BTreeMap<String, Version>> {
        let mut selected = BTreeMap::<String, Version>::new();
        let mut queue = VecDeque::<String>::new();

        for (deps, label) in [
            (&scanned.remote, "direct dependency"),
            (&scanned.implicit_remote, "implicit dependency"),
        ] {
            self.seed_specs(deps, label, &mut selected, &mut queue)?;
        }
        seed_floor_versions(imported_workspace_floors, &mut selected, &mut queue);

        while let Some(module_path) = queue.pop_front() {
            let Some(version) = selected.get(&module_path).cloned() else {
                continue;
            };
            let loaded = self
                .manifest_loader
                .load(&self.cache_index, &module_path, &version)
                .with_context(|| format!("Failed to load {}@{}", module_path, version))?;
            for (dep_path, dep_spec) in loaded {
                if is_stdlib_module_path(&dep_path) {
                    continue;
                }
                let dep_version = self
                    .spec_resolver
                    .resolve_spec(&dep_path, &dep_spec)
                    .with_context(|| {
                        format!("Failed to resolve transitive dependency {}", dep_path)
                    })?;
                enqueue_floor_version(&mut selected, dep_path, dep_version, &mut queue);
            }
        }

        Ok(selected)
    }

    fn seed_specs(
        &mut self,
        deps: &BTreeMap<String, DependencySpec>,
        label: &str,
        selected: &mut BTreeMap<String, Version>,
        queue: &mut VecDeque<String>,
    ) -> Result<()> {
        for (module_path, spec) in deps {
            let version = self
                .spec_resolver
                .resolve_spec(module_path, spec)
                .with_context(|| format!("Failed to resolve {} {}", label, module_path))?;
            enqueue_floor_version(selected, module_path.clone(), version, queue);
        }
        Ok(())
    }
}

fn merge_floor_version(
    selected: &mut BTreeMap<String, Version>,
    module_path: String,
    version: Version,
) -> bool {
    if is_stdlib_module_path(&module_path) {
        return false;
    }
    let should_update = match selected.get(&module_path) {
        Some(current) => version > *current,
        None => true,
    };
    if should_update {
        selected.insert(module_path, version);
    }
    should_update
}

fn enqueue_floor_version(
    selected: &mut BTreeMap<String, Version>,
    module_path: String,
    version: Version,
    queue: &mut VecDeque<String>,
) {
    if merge_floor_version(selected, module_path.clone(), version) {
        queue.push_back(module_path);
    }
}

fn seed_floor_versions(
    deps: &BTreeMap<String, Version>,
    selected: &mut BTreeMap<String, Version>,
    queue: &mut VecDeque<String>,
) {
    for (module_path, version) in deps {
        enqueue_floor_version(selected, module_path.clone(), version.clone(), queue);
    }
}

#[derive(Default)]
struct SpecVersionResolver {
    bare_repos: BTreeMap<String, PathBuf>,
    base_versions: BTreeMap<String, BTreeMap<String, Version>>,
}

impl SpecVersionResolver {
    fn resolve_spec(&mut self, module_path: &str, spec: &DependencySpec) -> Result<Version> {
        match spec {
            DependencySpec::Version(version) => parse_version_string(version),
            DependencySpec::Detailed(detail) => self.resolve_detail(module_path, detail),
        }
    }

    fn resolve_detail(&mut self, module_path: &str, detail: &DependencyDetail) -> Result<Version> {
        if let Some(version) = &detail.version {
            return parse_version_string(version);
        }
        if let Some(rev) = &detail.rev {
            return self.generate_pseudo_version(module_path, rev);
        }
        if let Some(branch) = &detail.branch {
            let commit = git::resolve_branch_head(module_path, branch)?;
            return self.generate_pseudo_version(module_path, &commit);
        }
        if detail.path.is_some() {
            anyhow::bail!(
                "Path dependency in remote MVS state is not supported yet for {}",
                module_path
            );
        }
        anyhow::bail!(
            "Dependency has no version, rev, or branch for {}",
            module_path
        )
    }

    fn generate_pseudo_version(&mut self, module_path: &str, commit: &str) -> Result<Version> {
        let (repo_url, subpath) = split_repo_and_subpath(module_path);
        let bare_dir = self.ensure_bare_repo(repo_url)?;
        let commit_full = git::rev_parse(&bare_dir, commit).ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to resolve rev '{}' in {}",
                &commit[..commit.len().min(12)],
                repo_url
            )
        })?;
        let timestamp = git::show_commit_timestamp(&bare_dir, &commit_full)
            .ok_or_else(|| anyhow::anyhow!("Failed to read timestamp for {}", commit_full))?;
        let base_version = self
            .latest_tagged_version(repo_url, subpath, &bare_dir)
            .unwrap_or_else(initial_package_version);
        let dt = jiff::Timestamp::from_second(timestamp)?;
        let pseudo = format!(
            "{}.{}.{}-0.{}-{}",
            base_version.major,
            base_version.minor,
            base_version.patch + 1,
            dt.strftime("%Y%m%d%H%M%S"),
            commit_full
        );
        Version::parse(&pseudo)
            .map_err(|e| anyhow::anyhow!("Failed to parse pseudo-version {}: {}", pseudo, e))
    }

    fn ensure_bare_repo(&mut self, repo_url: &str) -> Result<PathBuf> {
        if let Some(path) = self.bare_repos.get(repo_url) {
            return Ok(path.clone());
        }
        let path = pcb_zen::cache_index::ensure_bare_repo(repo_url)?;
        self.bare_repos.insert(repo_url.to_string(), path.clone());
        Ok(path)
    }

    fn latest_tagged_version(
        &mut self,
        repo_url: &str,
        subpath: &str,
        bare_dir: &std::path::Path,
    ) -> Option<Version> {
        if !self.base_versions.contains_key(repo_url) {
            let mut versions = BTreeMap::new();
            if let Ok(tags) = git::list_all_tags(bare_dir) {
                for tag in tags {
                    if let Some((pkg_path, version)) = tags::parse_tag(&tag) {
                        versions
                            .entry(pkg_path)
                            .and_modify(|current| {
                                if version > *current {
                                    *current = version.clone();
                                }
                            })
                            .or_insert(version);
                    }
                }
            }
            self.base_versions.insert(repo_url.to_string(), versions);
        }

        self.base_versions
            .get(repo_url)
            .and_then(|versions| versions.get(subpath))
            .cloned()
    }
}

fn parse_version_string(raw: &str) -> Result<Version> {
    tags::parse_relaxed_version(raw)
        .ok_or_else(|| anyhow::anyhow!("Invalid version string '{}'", raw))
}
