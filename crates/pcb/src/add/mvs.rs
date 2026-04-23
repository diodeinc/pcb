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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum DepGraphNode {
    Package(String),
    Remote {
        dep_id: ResolvedDepId,
        version: Version,
    },
}

impl DepGraphNode {
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Package(package_url) => package_url.clone(),
            Self::Remote { dep_id, version } => format!("{}@{}", dep_id.path, version),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DepGraph {
    root: DepGraphNode,
    edges: BTreeMap<DepGraphNode, BTreeSet<DepGraphNode>>,
}

impl DepGraph {
    fn new(root_package_url: &str) -> Self {
        Self {
            root: DepGraphNode::Package(root_package_url.to_string()),
            edges: BTreeMap::new(),
        }
    }

    fn add_edge(&mut self, from: DepGraphNode, to: DepGraphNode) {
        self.edges.entry(from).or_default().insert(to);
    }

    pub(crate) fn formatted_edges(&self) -> Vec<(String, String)> {
        let mut edges = Vec::new();
        for (from, children) in &self.edges {
            for to in children {
                edges.push((from.display(), to.display()));
            }
        }
        edges
    }

    pub(crate) fn shortest_path_to(&self, target: &DepGraphNode) -> Option<Vec<DepGraphNode>> {
        let mut queue = VecDeque::from([self.root.clone()]);
        let mut parents = BTreeMap::<DepGraphNode, Option<DepGraphNode>>::new();
        parents.insert(self.root.clone(), None);

        while let Some(node) = queue.pop_front() {
            if &node == target {
                let mut path = Vec::new();
                let mut current = Some(node);
                while let Some(node) = current {
                    current = parents.get(&node).cloned().flatten();
                    path.push(node);
                }
                path.reverse();
                return Some(path);
            }

            for child in self.edges.get(&node).into_iter().flatten() {
                if parents.contains_key(child) {
                    continue;
                }
                parents.insert(child.clone(), Some(node.clone()));
                queue.push_back(child.clone());
            }
        }

        None
    }

    pub(crate) fn contains_package(&self, package_url: &str) -> bool {
        self.edges
            .keys()
            .chain(self.edges.values().flatten())
            .any(|node| matches!(node, DepGraphNode::Package(url) if url == package_url))
    }

    pub(crate) fn find_remote_exact(&self, path: &str, version: &Version) -> Option<DepGraphNode> {
        self.remote_nodes()
            .into_iter()
            .find(|node| matches!(node, DepGraphNode::Remote { dep_id, version: node_version } if dep_id.path == path && node_version == version))
    }

    pub(crate) fn find_remote_by_path(&self, path: &str) -> Vec<DepGraphNode> {
        self.remote_nodes()
            .into_iter()
            .filter(
                |node| matches!(node, DepGraphNode::Remote { dep_id, .. } if dep_id.path == path),
            )
            .collect()
    }

    fn remote_nodes(&self) -> Vec<DepGraphNode> {
        self.edges
            .keys()
            .chain(self.edges.values().flatten())
            .filter_map(|node| match node {
                DepGraphNode::Remote { .. } => Some(node.clone()),
                DepGraphNode::Package(_) => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
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

        let result = self.build_package_resolution(package_url, None);
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

    pub(crate) fn resolve_package_with_direct_overrides(
        &mut self,
        package_url: &str,
        direct_overrides: Option<&BTreeMap<String, DependencySpec>>,
    ) -> Result<PackageResolution> {
        if direct_overrides.is_none_or(BTreeMap::is_empty) {
            return self.resolve_package(package_url);
        }
        self.build_package_resolution(package_url, direct_overrides)
    }

    pub(crate) fn build_package_graph(&mut self, package_url: &str) -> Result<DepGraph> {
        let root_resolution = self.resolve_package(package_url)?;
        let mut graph = DepGraph::new(package_url);
        let mut seen_packages = BTreeSet::new();
        let mut seen_remote = BTreeSet::new();
        self.populate_package_graph(
            package_url,
            &root_resolution.resolved_remote,
            &mut graph,
            &mut seen_packages,
            &mut seen_remote,
        )?;
        Ok(graph)
    }

    fn build_package_resolution(
        &mut self,
        package_url: &str,
        direct_overrides: Option<&BTreeMap<String, DependencySpec>>,
    ) -> Result<PackageResolution> {
        let (package_dir, current_config) = self.package_manifest_source(package_url)?;
        let mut scanned = scan_package_direct_deps(
            &self.workspace,
            package_url,
            &package_dir,
            &current_config,
            &self.cache_index,
        )
        .with_context(|| format!("while scanning package {}", package_url))?;
        if let Some(direct_overrides) = direct_overrides {
            for (module_path, spec) in direct_overrides {
                scanned.remote.insert(module_path.clone(), spec.clone());
            }
        }

        let imported_workspace_floors = self.import_workspace_floors(&scanned)?;

        self.run_remote_mvs(&scanned, &imported_workspace_floors)
            .with_context(|| {
                format!(
                    "while resolving remote dependency closure for {}",
                    package_url
                )
            })
    }

    fn populate_package_graph(
        &mut self,
        package_url: &str,
        selected_remote: &BTreeMap<ResolvedDepId, Version>,
        graph: &mut DepGraph,
        seen_packages: &mut BTreeSet<String>,
        seen_remote: &mut BTreeSet<(ResolvedDepId, Version)>,
    ) -> Result<()> {
        if !seen_packages.insert(package_url.to_string()) {
            return Ok(());
        }

        let resolution = self.resolve_package(package_url)?;
        let from = DepGraphNode::Package(package_url.to_string());

        for dep_path in resolution.direct.keys() {
            if let Some(workspace_dep) = self.workspace_package_dep(dep_path).map(str::to_string) {
                let to = DepGraphNode::Package(workspace_dep.clone());
                graph.add_edge(from.clone(), to);
                self.populate_package_graph(
                    &workspace_dep,
                    selected_remote,
                    graph,
                    seen_packages,
                    seen_remote,
                )?;
            }
        }

        for dep_id in &resolution.direct_remote_ids {
            let version = selected_remote.get(dep_id).ok_or_else(|| {
                anyhow::anyhow!("Resolved closure is missing graph node {}", dep_id.path)
            })?;
            let to = DepGraphNode::Remote {
                dep_id: dep_id.clone(),
                version: version.clone(),
            };
            graph.add_edge(from.clone(), to.clone());
            self.populate_remote_graph(dep_id, version, selected_remote, graph, seen_remote)?;
        }

        Ok(())
    }

    fn populate_remote_graph(
        &mut self,
        dep_id: &ResolvedDepId,
        version: &Version,
        selected_remote: &BTreeMap<ResolvedDepId, Version>,
        graph: &mut DepGraph,
        seen_remote: &mut BTreeSet<(ResolvedDepId, Version)>,
    ) -> Result<()> {
        if !seen_remote.insert((dep_id.clone(), version.clone())) {
            return Ok(());
        }

        let from = DepGraphNode::Remote {
            dep_id: dep_id.clone(),
            version: version.clone(),
        };
        let loaded = self
            .manifest_loader
            .load(&self.cache_index, &dep_id.path, version)
            .with_context(|| format!("Failed to load {}@{}", dep_id.path, version))?;

        for (child_path, child_spec) in loaded.direct {
            if is_stdlib_module_path(&child_path) {
                continue;
            }
            let child_version = self
                .spec_resolver
                .resolve_spec(&child_path, &child_spec)
                .with_context(|| format!("Failed to resolve graph dependency {}", child_path))?;
            let child_dep_id = ResolvedDepId::for_version(child_path, &child_version);
            self.add_remote_graph_edge(
                from.clone(),
                child_dep_id,
                selected_remote,
                graph,
                seen_remote,
            )?;
        }

        for (child_dep_id, _) in loaded.indirect {
            self.add_remote_graph_edge(
                from.clone(),
                child_dep_id,
                selected_remote,
                graph,
                seen_remote,
            )?;
        }

        Ok(())
    }

    fn add_remote_graph_edge(
        &mut self,
        from: DepGraphNode,
        child_dep_id: ResolvedDepId,
        selected_remote: &BTreeMap<ResolvedDepId, Version>,
        graph: &mut DepGraph,
        seen_remote: &mut BTreeSet<(ResolvedDepId, Version)>,
    ) -> Result<()> {
        let selected_version = selected_remote.get(&child_dep_id).ok_or_else(|| {
            anyhow::anyhow!(
                "Resolved closure is missing graph node {}",
                child_dep_id.path
            )
        })?;
        let to = DepGraphNode::Remote {
            dep_id: child_dep_id.clone(),
            version: selected_version.clone(),
        };
        graph.add_edge(from, to.clone());
        self.populate_remote_graph(
            &child_dep_id,
            selected_version,
            selected_remote,
            graph,
            seen_remote,
        )
    }

    fn workspace_package_dep<'a>(&'a self, dep_path: &'a str) -> Option<&'a str> {
        if self.workspace.packages.contains_key(dep_path) {
            return Some(dep_path);
        }
        (self.workspace.workspace_base_url().as_deref() == Some(dep_path)).then_some(dep_path)
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
