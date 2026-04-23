use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_zen::cache_index::CacheIndex;
use pcb_zen::git;
use pcb_zen::tags;
use pcb_zen_core::config::{DependencyDetail, DependencySpec, PcbToml, split_repo_and_subpath};
use pcb_zen_core::{initial_package_version, is_stdlib_module_path};
use semver::Version;

use super::dep_id::ResolvedDepId;
use super::manifest::ManifestLoader;
use super::scan::{ScannedDirectDeps, scan_package_direct_deps};

#[derive(Debug, Clone)]
pub(crate) struct PackageResolution {
    pub(crate) direct: BTreeMap<String, DependencySpec>,
    pub(crate) direct_remote_ids: BTreeSet<ResolvedDepId>,
    pub(crate) resolved_remote: BTreeMap<ResolvedDepId, Version>,
}

#[derive(Debug, Clone)]
enum PackageResolutionState {
    InProgress,
    Resolved(PackageResolution),
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

    pub(crate) fn resolve_package(&mut self, package_url: &str) -> Result<PackageResolution> {
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

    fn build_package_resolution(&mut self, package_url: &str) -> Result<PackageResolution> {
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

        self.run_remote_mvs(&scanned, &imported_workspace_floors)
            .with_context(|| {
                format!(
                    "while resolving remote dependency closure for {}",
                    package_url
                )
            })
    }

    fn import_workspace_floors(
        &mut self,
        scanned: &ScannedDirectDeps,
    ) -> Result<BTreeMap<ResolvedDepId, Version>> {
        let mut imported = BTreeMap::new();
        for workspace_dep in &scanned.workspace {
            let child = self.resolve_package(workspace_dep)?;
            for (dep_id, version) in child.resolved_remote {
                merge_floor_version(&mut imported, dep_id, version);
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
        imported_workspace_floors: &BTreeMap<ResolvedDepId, Version>,
    ) -> Result<PackageResolution> {
        let mut selected = BTreeMap::<ResolvedDepId, Version>::new();
        let mut queue = VecDeque::<ResolvedDepId>::new();

        let direct_remote_ids = self.seed_specs(
            &scanned.remote,
            "direct dependency",
            &mut selected,
            &mut queue,
        )?;
        self.seed_specs(
            &scanned.implicit_remote,
            "implicit dependency",
            &mut selected,
            &mut queue,
        )?;
        for (dep_id, version) in imported_workspace_floors {
            enqueue_floor_version(&mut selected, dep_id.clone(), version.clone(), &mut queue);
        }

        while let Some(dep_id) = queue.pop_front() {
            let Some(version) = selected.get(&dep_id).cloned() else {
                continue;
            };
            let loaded = self
                .manifest_loader
                .load(&self.cache_index, &dep_id.path, &version)
                .with_context(|| format!("Failed to load {}@{}", dep_id.path, version))?;
            for (dep_path, dep_spec) in loaded.direct {
                if is_stdlib_module_path(&dep_path) {
                    continue;
                }
                let dep_version = self
                    .spec_resolver
                    .resolve_spec(&dep_path, &dep_spec)
                    .with_context(|| {
                        format!("Failed to resolve transitive dependency {}", dep_path)
                    })?;
                enqueue_floor_version(
                    &mut selected,
                    ResolvedDepId::for_version(dep_path, &dep_version),
                    dep_version,
                    &mut queue,
                );
            }
            for (transitive_id, dep_version) in loaded.indirect {
                enqueue_floor_version(&mut selected, transitive_id, dep_version, &mut queue);
            }
        }

        Ok(PackageResolution {
            direct: fold_direct_dependencies(
                &self.workspace,
                scanned,
                &selected,
                &direct_remote_ids,
            )?,
            direct_remote_ids,
            resolved_remote: selected,
        })
    }

    fn seed_specs(
        &mut self,
        deps: &BTreeMap<String, DependencySpec>,
        label: &str,
        selected: &mut BTreeMap<ResolvedDepId, Version>,
        queue: &mut VecDeque<ResolvedDepId>,
    ) -> Result<BTreeSet<ResolvedDepId>> {
        let mut dep_ids = BTreeSet::new();
        for (module_path, spec) in deps {
            let version = self
                .spec_resolver
                .resolve_spec(module_path, spec)
                .with_context(|| format!("Failed to resolve {} {}", label, module_path))?;
            let dep_id = ResolvedDepId::for_version(module_path.clone(), &version);
            enqueue_floor_version(selected, dep_id.clone(), version, queue);
            dep_ids.insert(dep_id);
        }
        Ok(dep_ids)
    }
}

fn fold_direct_dependencies(
    workspace: &pcb_zen::WorkspaceInfo,
    scanned: &ScannedDirectDeps,
    resolved_remote: &BTreeMap<ResolvedDepId, Version>,
    direct_remote_ids: &BTreeSet<ResolvedDepId>,
) -> Result<BTreeMap<String, DependencySpec>> {
    let mut direct = BTreeMap::new();

    for dep_id in direct_remote_ids {
        let version = resolved_remote.get(dep_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Resolved closure is missing direct dependency {}",
                dep_id.path
            )
        })?;
        direct.insert(
            dep_id.path.clone(),
            DependencySpec::Version(version.to_string()),
        );
    }

    for module_path in &scanned.workspace {
        direct.insert(
            module_path.clone(),
            DependencySpec::Version(workspace_package_version(workspace, module_path)?),
        );
    }

    Ok(direct)
}

fn workspace_package_version(
    workspace: &pcb_zen::WorkspaceInfo,
    package_url: &str,
) -> Result<String> {
    let Some(pkg) = workspace.packages.get(package_url) else {
        anyhow::bail!(
            "Workspace dependency {} is not a workspace member",
            package_url
        );
    };
    Ok(pkg
        .version
        .clone()
        .unwrap_or_else(|| initial_package_version().to_string()))
}

fn merge_floor_version(
    selected: &mut BTreeMap<ResolvedDepId, Version>,
    dep_id: ResolvedDepId,
    version: Version,
) -> bool {
    if is_stdlib_module_path(&dep_id.path) {
        return false;
    }
    let should_update = match selected.get(&dep_id) {
        Some(current) => version > *current,
        None => true,
    };
    if should_update {
        selected.insert(dep_id, version);
    }
    should_update
}

fn enqueue_floor_version(
    selected: &mut BTreeMap<ResolvedDepId, Version>,
    dep_id: ResolvedDepId,
    version: Version,
    queue: &mut VecDeque<ResolvedDepId>,
) {
    if merge_floor_version(selected, dep_id.clone(), version) {
        queue.push_back(dep_id);
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use pcb_zen::{MemberPackage, WorkspaceInfo};

    use super::*;

    #[test]
    fn fold_direct_dependencies_combines_remote_and_workspace_deps() {
        let workspace_dep = "github.com/example/workspace/components/Local".to_string();
        let workspace = WorkspaceInfo {
            root: PathBuf::from("/repo"),
            cache_dir: PathBuf::from("/repo/.pcb/cache"),
            config: None,
            packages: BTreeMap::from([(
                workspace_dep.clone(),
                MemberPackage {
                    rel_path: PathBuf::from("components/Local"),
                    config: PcbToml::default(),
                    version: Some("0.4.5".to_string()),
                    published_at: None,
                    preferred: false,
                    dirty: false,
                },
            )]),
            lockfile: None,
            errors: vec![],
        };
        let scanned = ScannedDirectDeps {
            remote: BTreeMap::from([(
                "github.com/example/remote".to_string(),
                DependencySpec::Version("0.1.0".to_string()),
            )]),
            workspace: BTreeSet::from([workspace_dep.clone()]),
            implicit_remote: BTreeMap::new(),
        };
        let direct_remote_ids =
            BTreeSet::from([ResolvedDepId::new("github.com/example/remote", "0.2")]);
        let resolved_remote = BTreeMap::from([(
            ResolvedDepId::new("github.com/example/remote", "0.2"),
            Version::parse("0.2.0").unwrap(),
        )]);

        let direct =
            fold_direct_dependencies(&workspace, &scanned, &resolved_remote, &direct_remote_ids)
                .unwrap();

        assert_eq!(
            direct.get("github.com/example/remote"),
            Some(&DependencySpec::Version("0.2.0".to_string()))
        );
        assert_eq!(
            direct.get(&workspace_dep),
            Some(&DependencySpec::Version("0.4.5".to_string()))
        );
    }
}
