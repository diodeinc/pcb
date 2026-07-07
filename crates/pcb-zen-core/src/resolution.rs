//! Shared dependency resolution logic.
//!
//! This module provides the core resolution map building functionality used by both
//! native (pcb-zen) and WASM (pcb-zen-wasm) builds. The key abstraction is
//! `PackagePathResolver` which allows different strategies for resolving package
//! paths:
//!
//! - Native: checks patches, vendor/, then ~/.pcb/cache
//! - WASM: only checks vendor/ (everything must be pre-vendored)

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, bail};
use semver::Version;

use crate::FileProvider;
use crate::config::{DependencyDetail, DependencySpec, ManifestPart, PcbToml};
use crate::workspace::{LOCAL_WORKSPACE_ROOT_URL, WorkspaceInfo, package_url_covers};
use crate::{STDLIB_MODULE_PATH, is_stdlib_module_path, parse_relaxed_version};

/// Stable identity for package-local evaluation state.
///
/// Frozen package resolution is package-local: the same file path can be loaded
/// under different dependency environments. This key captures the loaded
/// package's semantic resolution scope so eval caches can share modules only
/// when the package identity and its resolved deps match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PackageScopeKey {
    package_identity: String,
    deps: Vec<(String, PathBuf)>,
}

impl PackageScopeKey {
    fn frozen(package: &FrozenPackage) -> Self {
        Self {
            package_identity: package.identity.display(),
            deps: package
                .deps
                .iter()
                .map(|(dep, path)| (dep.clone(), path.clone()))
                .collect(),
        }
    }
}

/// Resolved package-local dependency environment for a file.
///
/// This is a read-only view over the frozen package resolution data.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPackageScope<'a> {
    root: Cow<'a, Path>,
    package_url: Option<Cow<'a, str>>,
    display: Cow<'a, str>,
    deps: Cow<'a, BTreeMap<String, PathBuf>>,
    cache_key: PackageScopeKey,
}

impl<'a> ResolvedPackageScope<'a> {
    fn frozen(root: &'a Path, package: &'a FrozenPackage) -> Self {
        Self {
            root: Cow::Borrowed(root),
            package_url: package.identity.package_url().map(Cow::Borrowed),
            display: Cow::Owned(package.identity.display()),
            deps: Cow::Borrowed(&package.deps),
            cache_key: PackageScopeKey::frozen(package),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_ref()
    }

    pub(crate) fn package_url(&self) -> Option<&str> {
        self.package_url.as_deref()
    }

    pub(crate) fn display(&self) -> &str {
        self.display.as_ref()
    }

    pub(crate) fn load_cache_key(&self) -> PackageScopeKey {
        self.cache_key.clone()
    }

    pub(crate) fn expand_alias(&self, alias: &str) -> Option<&str> {
        self.deps.keys().find_map(|url| {
            url.rsplit('/')
                .next()
                .filter(|last_segment| *last_segment == alias)
                .map(|_| url.as_str())
        })
    }

    pub(crate) fn resolve_package_url<'scope>(
        &'scope self,
        full_url: &str,
    ) -> Option<PackageUrlResolution<'scope>> {
        let own_url = self
            .package_url()
            .filter(|url| package_url_covers(url, full_url));
        let dep = self
            .deps
            .iter()
            .filter(|(dep_url, _)| package_url_covers(dep_url, full_url))
            .max_by_key(|(dep_url, _)| dep_url.len());

        match (own_url, dep) {
            (Some(own_url), Some((dep_url, root))) if dep_url.len() > own_url.len() => {
                Some(PackageUrlResolution::Dependency {
                    dep_url: dep_url.as_str(),
                    root: root.as_path(),
                })
            }
            (Some(_), _) => Some(PackageUrlResolution::OwnPackage),
            (None, Some((dep_url, root))) => Some(PackageUrlResolution::Dependency {
                dep_url: dep_url.as_str(),
                root: root.as_path(),
            }),
            (None, None) => None,
        }
    }
}

pub(crate) enum PackageUrlResolution<'a> {
    OwnPackage,
    Dependency { dep_url: &'a str, root: &'a Path },
}

/// Compute the semver family for a version.
///
/// For 0.x versions, the minor version determines the family (0.2.x and 0.3.x are different).
/// For 1.x+ versions, the major version determines the family.
pub fn semver_family(v: &Version) -> String {
    if v.major == 0 {
        format!("v0.{}", v.minor)
    } else {
        format!("v{}", v.major)
    }
}

/// Module line identifier for MVS grouping.
///
/// A module line represents a semver family:
/// - For v0.x: family is "v0.<minor>" (e.g., v0.2, v0.3 are different families)
/// - For v1.x+: family is "v<major>" (e.g., v1, v2, v3)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleLine {
    pub path: String,   // e.g., "github.com/org/pkg"
    pub family: String, // e.g., "v0.3" or "v1"
}

impl ModuleLine {
    pub fn new(path: String, version: &Version) -> Self {
        ModuleLine {
            path,
            family: semver_family(version),
        }
    }
}

/// Trait for resolving dependency package paths.
pub trait PackagePathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf>;
    fn selected_versions(&self) -> &HashMap<ModuleLine, Version>;

    fn resolve_selected_package(
        &self,
        module_path: &str,
        detail: &DependencyDetail,
    ) -> Option<PathBuf> {
        let version = select_version_for_detail(module_path, detail, self.selected_versions())?;
        self.resolve_package(module_path, &version)
    }
}

fn pseudo_version_commit(version: &Version) -> Option<&str> {
    if !version.pre.starts_with("0.") {
        return None;
    }
    version
        .pre
        .as_str()
        .rsplit_once('-')
        .map(|(_, commit)| commit)
}

pub fn pseudo_matches_rev(version: &Version, rev: &str) -> bool {
    pseudo_version_commit(version)
        .is_some_and(|commit| commit.starts_with(rev) || rev.starts_with(commit))
}

pub fn select_version_for_detail(
    module_path: &str,
    detail: &DependencyDetail,
    selected: &HashMap<ModuleLine, Version>,
) -> Option<String> {
    if let Some(version) = &detail.version {
        return Some(version.clone());
    }

    let candidates: Vec<_> = selected
        .iter()
        .filter(|(line, _)| line.path == module_path)
        .collect();

    if let Some(rev) = detail.rev.as_deref()
        && let Some((_, version)) = candidates
            .iter()
            .find(|(_, version)| pseudo_matches_rev(version, rev))
    {
        return Some(version.to_string());
    }

    candidates
        .into_iter()
        .max_by(|a, b| a.1.cmp(b.1))
        .map(|(_, version)| version.to_string())
}

/// Build the package coordinate → absolute root directory mapping.
///
/// Workspace packages come from `workspace_info.packages`. External deps are
/// discovered from resolved package-local dependency maps.
pub fn build_package_roots<'a>(
    workspace_info: &WorkspaceInfo,
    dependency_maps: impl IntoIterator<Item = &'a BTreeMap<String, PathBuf>>,
) -> BTreeMap<String, PathBuf> {
    let mut roots = BTreeMap::new();
    roots.insert(
        STDLIB_MODULE_PATH.to_string(),
        workspace_info.workspace_stdlib_dir(),
    );

    let has_root_package = workspace_info
        .packages
        .values()
        .any(|pkg| pkg.rel_path.as_os_str().is_empty());

    for (url, pkg) in &workspace_info.packages {
        roots.insert(url.clone(), pkg.dir(&workspace_info.root));
    }

    if !has_root_package {
        roots.insert(
            LOCAL_WORKSPACE_ROOT_URL.to_string(),
            workspace_info.root.clone(),
        );
    }

    for deps in dependency_maps {
        for (module_path, dep_root) in deps {
            let version = dep_root.file_name().and_then(|f| f.to_str());
            let parent_matches = dep_root
                .parent()
                .is_some_and(|p| p.ends_with(Path::new(module_path)));
            if let Some(version) = version
                && parent_matches
            {
                roots
                    .entry(format!("{module_path}@{version}"))
                    .or_insert(dep_root.clone());
            }
        }
    }

    roots
}

fn frozen_dependency_maps(
    resolution: &FrozenResolutionSet,
) -> impl Iterator<Item = &BTreeMap<String, PathBuf>> {
    resolution
        .values()
        .flat_map(|resolution| resolution.packages.values())
        .map(|package| &package.deps)
}

/// Resolve a single dependency to its path.
fn resolve_dep<R: PackagePathResolver>(
    resolver: &R,
    workspace: &WorkspaceInfo,
    base_dir: &Path,
    url: &str,
    spec: &DependencySpec,
) -> Option<PathBuf> {
    // 1. Local path dependency
    if let DependencySpec::Detailed(d) = spec
        && let Some(path_str) = &d.path
    {
        return Some(base_dir.join(path_str));
    }

    // 2. Workspace member
    if let Some(member) = workspace.packages.get(url) {
        return Some(member.dir(&workspace.root));
    }

    // 3. External dependency via resolver
    let version = match spec {
        DependencySpec::Version(v) => Some(v.clone()),
        DependencySpec::Detailed(d) => return resolver.resolve_selected_package(url, d),
    }?;

    resolver.resolve_package(url, &version)
}

/// Build resolution map for a single package's [dependencies].
fn resolve_package_deps<R: PackagePathResolver>(
    resolver: &R,
    workspace: &WorkspaceInfo,
    base_dir: &Path,
    config: &PcbToml,
) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();

    for (url, spec) in &config.dependencies.direct {
        if let Some(path) = resolve_dep(resolver, workspace, base_dir, url, spec) {
            map.insert(url.clone(), path);
        }
    }

    map
}

/// Path resolver that only looks in the vendor directory.
///
/// Used by WASM where all dependencies must be pre-vendored in the zip.
pub struct VendoredPathResolver {
    vendor_dir: PathBuf,
    /// Pre-computed closure: ModuleLine -> Version
    closure: HashMap<ModuleLine, Version>,
}

impl VendoredPathResolver {
    /// Get the closure (ModuleLine -> Version mapping).
    pub fn closure(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
    }

    pub fn from_selected_versions(
        vendor_dir: PathBuf,
        closure: HashMap<ModuleLine, Version>,
    ) -> Self {
        Self {
            vendor_dir,
            closure,
        }
    }
}

impl PackagePathResolver for VendoredPathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
        // Prefer closure-selected version for pcb.toml packages.
        if let Ok(ver) = Version::parse(version) {
            let line = ModuleLine::new(module_path.to_string(), &ver);
            if let Some(selected) = self.closure.get(&line) {
                return Some(self.vendor_dir.join(module_path).join(selected.to_string()));
            }
        }

        // Allow asset dependencies by direct {module}/{version}.
        Some(self.vendor_dir.join(module_path).join(version))
    }

    fn selected_versions(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
    }
}

/// Build the per-package resolution map for workspace packages and all packages in the closure.
///
/// Returns a map from package root path to (dependency URL -> resolved path).
pub fn build_resolution_map<F: FileProvider, R: PackagePathResolver>(
    file_provider: &F,
    resolver: &R,
    workspace: &WorkspaceInfo,
    closure: &HashMap<ModuleLine, Version>,
) -> HashMap<PathBuf, BTreeMap<String, PathBuf>> {
    let mut results = HashMap::new();

    // Build map for each workspace package (already have their configs loaded).
    for package in workspace.packages.values() {
        let package_dir = package.dir(&workspace.root);
        let resolved = resolve_package_deps(resolver, workspace, &package_dir, &package.config);
        results.insert(package_dir, resolved);
    }

    // Build map for workspace root if not already included as a package.
    results.entry(workspace.root.clone()).or_insert_with(|| {
        workspace
            .config
            .as_ref()
            .map(|c| resolve_package_deps(resolver, workspace, &workspace.root, c))
            .unwrap_or_default()
    });

    // Build map for external packages in the closure (need to read their pcb.toml).
    for (line, version) in closure {
        let version_str = version.to_string();
        let Some(pkg_path) = resolver.resolve_package(&line.path, &version_str) else {
            continue;
        };
        if results.contains_key(&pkg_path) {
            continue;
        }

        let pcb_toml_path = pkg_path.join("pcb.toml");
        let Ok(content) = file_provider.read_file(&pcb_toml_path) else {
            continue;
        };
        let Ok(config) = PcbToml::parse(&content) else {
            continue;
        };

        let resolved = resolve_package_deps(resolver, workspace, &pkg_path, &config);
        results.insert(pkg_path, resolved);
    }

    results
}

/// Frozen resolution maps keyed by root package URL.
pub type FrozenResolutionSet = BTreeMap<String, FrozenResolutionMap>;

/// Frozen package-local resolution table for one root package.
#[derive(Debug, Clone)]
pub struct FrozenResolutionMap {
    pub selected_remote: BTreeMap<FrozenDepId, Version>,
    pub packages: BTreeMap<PathBuf, FrozenPackage>,
}

impl FrozenResolutionMap {
    pub fn package_for_file(&self, file: &Path) -> Option<(&PathBuf, &FrozenPackage)> {
        // Walking ancestors finds the longest matching package root first,
        // in O(path depth) map lookups instead of a scan over all packages.
        file.ancestors()
            .find_map(|dir| self.packages.get_key_value(dir))
    }

    fn canonicalize_keys(&mut self, file_provider: &dyn crate::FileProvider) {
        self.packages = self
            .packages
            .iter()
            .map(|(root, package)| {
                let root = file_provider
                    .canonicalize(root)
                    .unwrap_or_else(|_| root.clone());
                let deps = package
                    .deps
                    .iter()
                    .map(|(dep, path)| {
                        let path = file_provider
                            .canonicalize(path)
                            .unwrap_or_else(|_| path.clone());
                        (dep.clone(), path)
                    })
                    .collect();
                (
                    root,
                    FrozenPackage {
                        identity: package.identity.clone(),
                        deps,
                        parts: package.parts.clone(),
                    },
                )
            })
            .collect();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FrozenDepId {
    pub path: String,
    pub lane: String,
}

impl FrozenDepId {
    pub fn new(path: impl Into<String>, lane: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            lane: lane.into(),
        }
    }

    pub fn for_version(path: impl Into<String>, version: &Version) -> Self {
        Self::new(path, compatibility_lane(version))
    }

    pub fn indirect_key(&self) -> String {
        format!("{}@{}", self.path, self.lane)
    }
}

pub fn compatibility_lane(version: &Version) -> String {
    if version.major == 0 {
        format!("0.{}", version.minor)
    } else {
        version.major.to_string()
    }
}

pub fn parse_lane_qualified_key(raw: &str) -> Result<FrozenDepId> {
    let Some((path, lane)) = raw.rsplit_once('@') else {
        bail!(
            "Expected lane-qualified dependency key '<module>@<lane>', got '{}'",
            raw
        );
    };
    if path.is_empty() || lane.is_empty() {
        bail!(
            "Expected lane-qualified dependency key '<module>@<lane>', got '{}'",
            raw
        );
    }
    Ok(FrozenDepId::new(path, lane))
}

pub fn selected_remote_from_hydrated_manifest(
    workspace: &WorkspaceInfo,
    package_url: &str,
) -> Result<BTreeMap<FrozenDepId, Version>> {
    let default_config;
    let config = if let Some(package) = workspace.packages.get(package_url) {
        &package.config
    } else if workspace.packages.is_empty() && package_url == LOCAL_WORKSPACE_ROOT_URL {
        default_config = PcbToml::default();
        workspace.config.as_ref().unwrap_or(&default_config)
    } else {
        bail!("Unknown workspace package {package_url}");
    };

    selected_remote_from_manifest(workspace, config)
}

fn selected_remote_from_manifest(
    workspace: &WorkspaceInfo,
    config: &PcbToml,
) -> Result<BTreeMap<FrozenDepId, Version>> {
    let mut selected = BTreeMap::new();
    for (dep_url, spec) in &config.dependencies.direct {
        if is_remote_manifest_dependency(workspace, dep_url, spec) {
            let version = exact_manifest_version(dep_url, spec)?;
            selected.insert(FrozenDepId::for_version(dep_url.clone(), &version), version);
        }
    }

    for (raw_key, spec) in &config.dependencies.indirect {
        let dep_id = parse_lane_qualified_key(raw_key)?;
        let version = exact_manifest_version(raw_key, spec)?;
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

fn is_remote_manifest_dependency(
    workspace: &WorkspaceInfo,
    dep_url: &str,
    spec: &DependencySpec,
) -> bool {
    !is_stdlib_module_path(dep_url)
        && !workspace
            .packages
            .keys()
            .any(|package_url| package_url_covers(package_url, dep_url))
        && workspace.workspace_base_url().as_deref() != Some(dep_url)
        && !matches!(spec, DependencySpec::Detailed(detail) if detail.path.is_some())
}

fn exact_manifest_version(dep_url: &str, spec: &DependencySpec) -> Result<Version> {
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
    parse_relaxed_version(raw)
        .ok_or_else(|| anyhow::anyhow!("Dependency {} has invalid version '{}'", dep_url, raw))
}

#[derive(Debug, Clone)]
pub struct FrozenPackage {
    pub identity: FrozenPackageIdentity,
    pub deps: BTreeMap<String, PathBuf>,
    pub parts: Vec<ManifestPart>,
}

#[derive(Debug, Clone)]
pub enum FrozenPackageIdentity {
    Workspace(String),
    Remote {
        dep_id: FrozenDepId,
        version: Version,
    },
    Stdlib,
}

impl FrozenPackageIdentity {
    pub fn display(&self) -> String {
        match self {
            Self::Workspace(url) => url.clone(),
            Self::Remote { dep_id, version } => {
                format!("{}@{} = {}", dep_id.path, dep_id.lane, version)
            }
            Self::Stdlib => STDLIB_MODULE_PATH.to_string(),
        }
    }

    pub fn package_url(&self) -> Option<&str> {
        match self {
            Self::Workspace(url) => Some(url),
            Self::Remote { dep_id, .. } => Some(&dep_id.path),
            Self::Stdlib => Some(STDLIB_MODULE_PATH),
        }
    }
}

/// Result of dependency resolution.
///
/// This is a data-only type defined in core so it can be referenced by
/// `EvalContext` / `EvalOutput`. Construction happens in `pcb-zen` which
/// performs the actual resolution.
#[derive(Debug, Clone)]
pub struct ResolutionResult {
    /// Snapshot of workspace info at the time of resolution
    pub workspace_info: WorkspaceInfo,
    resolution: FrozenResolutionSet,
    /// Symbol-to-parts mapping built from `[parts]` sections across all manifests.
    ///
    /// Keys are `package://` URIs for `.kicad_sym` files. Values are ordered lists
    /// of parts declared for that symbol (preserving manifest order).
    pub symbol_parts: HashMap<String, Vec<ManifestPart>>,
    indexes: Arc<PackageIndexes>,
}

/// Lookup tables derived from the workspace and resolution maps, rebuilt
/// whenever they change. Package-ownership queries walk a path's ancestors
/// over these instead of scanning every package.
#[derive(Debug, Default)]
struct PackageIndexes {
    /// Coordinate → absolute package root.
    package_roots: BTreeMap<String, PathBuf>,
    /// `package_roots` with `workspace_cache_path` applied to each root — the
    /// map returned by [`ResolutionResult::package_roots`].
    workspace_package_roots: BTreeMap<String, PathBuf>,
    /// Reverse of `workspace_package_roots`: package root path → coordinate,
    /// for `format_package_uri`.
    root_coords: HashMap<PathBuf, String>,
    /// Package root path → root package URL, for resolution maps whose own
    /// workspace package sits at that path, for `frozen_root_for_file`.
    own_roots: HashMap<PathBuf, String>,
    /// Workspace-relative package dir → package URL, for
    /// `workspace_package_url_for_path`.
    rel_dirs: HashMap<PathBuf, String>,
}

impl PackageIndexes {
    fn new(workspace_info: &WorkspaceInfo, resolution: &FrozenResolutionSet) -> Self {
        let package_roots = build_package_roots(workspace_info, frozen_dependency_maps(resolution));

        let workspace_package_roots: BTreeMap<String, PathBuf> = package_roots
            .iter()
            .map(|(coord, root)| (coord.clone(), workspace_cache_path(workspace_info, root)))
            .collect();
        // On duplicate root paths the later coordinate wins, matching the
        // last-maximum tie-break of the longest-prefix scan this replaces.
        let root_coords = workspace_package_roots
            .iter()
            .map(|(coord, root)| (root.clone(), coord.clone()))
            .collect();

        let mut own_roots = HashMap::new();
        for (root_package, map) in resolution {
            for (root, package) in &map.packages {
                if matches!(&package.identity, FrozenPackageIdentity::Workspace(url) if url == root_package)
                {
                    own_roots.insert(root.clone(), root_package.clone());
                }
            }
        }

        let rel_dirs = workspace_info
            .packages
            .iter()
            .map(|(url, pkg)| (pkg.rel_path.clone(), url.clone()))
            .collect();

        Self {
            package_roots,
            workspace_package_roots,
            root_coords,
            own_roots,
            rel_dirs,
        }
    }
}

/// Map a global-cache path to its workspace-local `.pcb/cache` equivalent.
fn workspace_cache_path(workspace_info: &WorkspaceInfo, path: &Path) -> PathBuf {
    if workspace_info.cache_dir.as_os_str().is_empty() {
        return path.to_path_buf();
    }
    path.strip_prefix(&workspace_info.cache_dir)
        .map(|rel| workspace_info.workspace_cache_dir().join(rel))
        .unwrap_or_else(|_| path.to_path_buf())
}

impl ResolutionResult {
    pub fn frozen(
        workspace_info: WorkspaceInfo,
        resolution: FrozenResolutionSet,
        symbol_parts: HashMap<String, Vec<ManifestPart>>,
    ) -> Self {
        let indexes = Arc::new(PackageIndexes::new(&workspace_info, &resolution));
        Self {
            workspace_info,
            resolution,
            symbol_parts,
            indexes,
        }
    }

    /// Create an empty resolution result with no dependencies.
    pub fn empty() -> Self {
        Self::frozen(
            WorkspaceInfo {
                root: PathBuf::new(),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            FrozenResolutionSet::new(),
            HashMap::new(),
        )
    }

    /// Resolve the package-local dependency scope for a file.
    ///
    /// When `active_root_package` is present, lookup is intentionally scoped to
    /// that frozen root package. The same physical package root can appear in
    /// multiple dependency environments, so callers must not use a global
    /// file-to-scope lookup for frozen resolution.
    pub(crate) fn package_scope_for_file<'a>(
        &'a self,
        file: &Path,
        active_root_package: Option<&str>,
    ) -> Option<ResolvedPackageScope<'a>> {
        if let Some(root_package) = active_root_package {
            return self
                .frozen_root(root_package)
                .and_then(|resolution| resolution.package_for_file(file))
                .map(|(root, package)| ResolvedPackageScope::frozen(root, package));
        }

        None
    }

    pub fn package_url_for_package_root(
        &self,
        root: &Path,
        file_provider: &dyn FileProvider,
    ) -> Option<String> {
        let canonical_root = file_provider
            .canonicalize(root)
            .unwrap_or_else(|_| root.to_path_buf());

        let stdlib_root = self.workspace_info.workspace_stdlib_dir();
        let canonical_stdlib = file_provider
            .canonicalize(&stdlib_root)
            .unwrap_or(stdlib_root);
        if canonical_root == canonical_stdlib {
            return Some(STDLIB_MODULE_PATH.to_string());
        }

        for (url, package) in &self.workspace_info.packages {
            let package_root = package.dir(&self.workspace_info.root);
            let canonical_package = file_provider
                .canonicalize(&package_root)
                .unwrap_or(package_root);
            if canonical_root == canonical_package {
                return Some(url.clone());
            }
        }

        let has_root_package = self
            .workspace_info
            .packages
            .values()
            .any(|pkg| pkg.rel_path.as_os_str().is_empty());
        if !has_root_package {
            let workspace_root = self.workspace_info.root.clone();
            let canonical_workspace = file_provider
                .canonicalize(&workspace_root)
                .unwrap_or(workspace_root);
            if canonical_root == canonical_workspace {
                return Some(LOCAL_WORKSPACE_ROOT_URL.to_string());
            }
        }

        None
    }

    pub(crate) fn package_url_for_file(
        &self,
        file: &Path,
        active_root_package: Option<&str>,
        file_provider: &dyn FileProvider,
    ) -> Option<String> {
        let scope = self.package_scope_for_file(file, active_root_package)?;
        let package_url = scope.package_url()?.to_string();
        let canonical_root = file_provider
            .canonicalize(scope.root())
            .unwrap_or_else(|_| scope.root().to_path_buf());
        let rel = file
            .strip_prefix(&canonical_root)
            .or_else(|_| file.strip_prefix(scope.root()))
            .unwrap_or(Path::new(""));

        if rel.as_os_str().is_empty() {
            Some(package_url)
        } else {
            Some(format!("{}/{}", package_url, rel.display()))
        }
    }

    pub(crate) fn load_cache_scope_key_for_file(
        &self,
        file: &Path,
        active_root_package: Option<&str>,
    ) -> Option<PackageScopeKey> {
        self.package_scope_for_file(file, active_root_package)
            .map(|scope| scope.load_cache_key())
    }

    pub fn canonicalize_keys(&mut self, file_provider: &dyn crate::FileProvider) {
        if !self.workspace_info.cache_dir.as_os_str().is_empty() {
            self.workspace_info.cache_dir = file_provider
                .canonicalize(&self.workspace_info.cache_dir)
                .unwrap_or_else(|_| self.workspace_info.cache_dir.clone());
        }
        for resolution in self.resolution.values_mut() {
            resolution.canonicalize_keys(file_provider);
        }
        self.refresh_package_roots();
    }

    fn refresh_package_roots(&mut self) {
        self.indexes = Arc::new(PackageIndexes::new(&self.workspace_info, &self.resolution));
    }

    /// Return the most specific workspace package URL that owns `path`.
    /// Equivalent to [`WorkspaceInfo::package_url_for_path`], but answered by
    /// walking the path's ancestors over a precomputed index.
    pub(crate) fn workspace_package_url_for_path(
        &self,
        file_provider: &dyn FileProvider,
        path: &Path,
    ) -> Option<&str> {
        let canonical_root = file_provider
            .canonicalize(&self.workspace_info.root)
            .unwrap_or_else(|_| self.workspace_info.root.clone());
        let canonical_path = file_provider
            .canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf());
        let workspace_relative = canonical_path.strip_prefix(&canonical_root).ok()?;

        // The final empty ancestor matches a root package with an empty rel_path.
        workspace_relative
            .ancestors()
            .find_map(|dir| self.indexes.rel_dirs.get(dir))
            .map(String::as_str)
    }

    pub fn package_roots(&self) -> BTreeMap<String, PathBuf> {
        self.indexes.workspace_package_roots.clone()
    }

    pub fn remote_package_versions(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut package_versions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for resolution in self.resolution.values() {
            for (dep_id, version) in &resolution.selected_remote {
                package_versions
                    .entry(dep_id.path.clone())
                    .or_default()
                    .insert(version.to_string());
            }
        }

        package_versions
    }

    pub fn frozen_root(&self, package_url: &str) -> Option<&FrozenResolutionMap> {
        self.resolution.get(package_url)
    }

    pub fn frozen_root_for_file(&self, file: &Path) -> Option<(&str, &FrozenResolutionMap)> {
        // Stdlib files are owned by the synthetic stdlib package in every map,
        // never by a map's own root package; they root at the stdlib
        // resolution when one was requested (e.g. `pcb doc @stdlib`). Both
        // `file` and the workspace root are canonical, so a prefix check works.
        if file.starts_with(self.workspace_info.workspace_stdlib_dir()) {
            return self
                .resolution
                .get_key_value(STDLIB_MODULE_PATH)
                .map(|(root_package, resolution)| (root_package.as_str(), resolution));
        }

        // A resolution map is a candidate only when its own longest match for
        // `file` is its root workspace package; the longest such root wins.
        // Walk the file's ancestors (longest first) over the precomputed
        // root-path index and verify each candidate against its map, which is
        // equivalent to scanning every resolution map but O(path depth).
        file.ancestors().find_map(|dir| {
            let root_package = self.indexes.own_roots.get(dir)?;
            let resolution = self.resolution.get(root_package)?;
            let (root, package) = resolution.package_for_file(file)?;
            let is_own_root = root == dir
                && matches!(&package.identity, FrozenPackageIdentity::Workspace(url) if url == root_package);
            is_own_root.then_some((root_package.as_str(), resolution))
        })
    }

    /// Resolve a package URI (`package://…`) to an absolute filesystem path.
    pub fn resolve_package_uri(&self, uri: &str) -> anyhow::Result<PathBuf> {
        pcb_sch::resolve_package_uri(uri, &self.indexes.package_roots)
    }

    /// Format an absolute path as a stable URI (`package://…`).
    ///
    /// The owning package is the longest package root that prefixes the path,
    /// found by walking the path's ancestors over the precomputed root index.
    pub fn format_package_uri(&self, abs: &Path) -> Option<String> {
        let effective_abs = workspace_cache_path(&self.workspace_info, abs);
        let (root, coord) = effective_abs
            .ancestors()
            .find_map(|dir| self.indexes.root_coords.get(dir).map(|coord| (dir, coord)))?;
        pcb_sch::package_uri(coord, effective_abs.strip_prefix(root).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;
    use crate::config::DependencyDetail;

    #[test]
    fn resolves_nested_package_url() {
        let nested_root = PathBuf::from("/workspace/boards/demo/modules/usb");
        let package = FrozenPackage {
            identity: FrozenPackageIdentity::Workspace("github.com/acme/repo/boards/demo".into()),
            deps: BTreeMap::from([(
                "github.com/acme/repo/boards/demo/modules/usb".into(),
                nested_root.clone(),
            )]),
            parts: Vec::new(),
        };
        let scope = ResolvedPackageScope::frozen(Path::new("/workspace/boards/demo"), &package);

        let resolved =
            scope.resolve_package_url("github.com/acme/repo/boards/demo/modules/usb/Usb.zen");

        match resolved {
            Some(PackageUrlResolution::Dependency { dep_url, root }) => {
                assert_eq!(dep_url, "github.com/acme/repo/boards/demo/modules/usb");
                assert_eq!(root, nested_root.as_path());
            }
            _ => panic!("expected nested dependency resolution"),
        }

        let resolved = scope.resolve_package_url("github.com/acme/repo/boards/demo/src/Main.zen");

        assert!(matches!(resolved, Some(PackageUrlResolution::OwnPackage)));
    }

    #[test]
    fn package_roots_reflect_frozen_dependency_roots() {
        let dep_root = PathBuf::from("/cache/github.com/acme/dep/1.2.3");
        let dep_coord = "github.com/acme/dep@1.2.3";
        let result = ResolutionResult::frozen(
            WorkspaceInfo {
                root: PathBuf::from("/workspace"),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            BTreeMap::from([(
                "github.com/acme/root".into(),
                FrozenResolutionMap {
                    selected_remote: BTreeMap::new(),
                    packages: BTreeMap::from([(
                        PathBuf::from("/workspace"),
                        FrozenPackage {
                            identity: FrozenPackageIdentity::Workspace(
                                "github.com/acme/root".into(),
                            ),
                            deps: BTreeMap::from([(
                                "github.com/acme/dep".into(),
                                dep_root.clone(),
                            )]),
                            parts: Vec::new(),
                        },
                    )]),
                },
            )]),
            HashMap::new(),
        );

        assert_eq!(result.package_roots().get(dep_coord), Some(&dep_root));
    }

    #[test]
    fn frozen_root_for_file_uses_frozen_package_roots() {
        let result = ResolutionResult::frozen(
            WorkspaceInfo {
                root: PathBuf::from("/workspace"),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            BTreeMap::from([(
                LOCAL_WORKSPACE_ROOT_URL.to_string(),
                FrozenResolutionMap {
                    selected_remote: BTreeMap::new(),
                    packages: BTreeMap::from([(
                        PathBuf::from("/private/workspace"),
                        FrozenPackage {
                            identity: FrozenPackageIdentity::Workspace(
                                LOCAL_WORKSPACE_ROOT_URL.to_string(),
                            ),
                            deps: BTreeMap::new(),
                            parts: Vec::new(),
                        },
                    )]),
                },
            )]),
            HashMap::new(),
        );

        assert_eq!(
            result
                .frozen_root_for_file(Path::new("/private/workspace/Board.zen"))
                .map(|(package_url, _)| package_url),
            Some(LOCAL_WORKSPACE_ROOT_URL)
        );
    }

    #[test]
    fn frozen_scope_cache_key_is_package_local() {
        let shared_root = PathBuf::from("/cache/github.com/acme/shared/1.0.0");
        let shared_package = FrozenPackage {
            identity: FrozenPackageIdentity::Remote {
                dep_id: FrozenDepId {
                    path: "github.com/acme/shared".into(),
                    lane: "v1".into(),
                },
                version: Version::parse("1.0.0").unwrap(),
            },
            deps: BTreeMap::from([(
                "github.com/acme/base".into(),
                PathBuf::from("/cache/github.com/acme/base/1.0.0"),
            )]),
            parts: Vec::new(),
        };
        let resolution = ResolutionResult::frozen(
            WorkspaceInfo {
                root: PathBuf::from("/workspace"),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            BTreeMap::from([
                (
                    "github.com/acme/root-a".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(shared_root.clone(), shared_package.clone())]),
                    },
                ),
                (
                    "github.com/acme/root-b".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(shared_root.clone(), shared_package.clone())]),
                    },
                ),
            ]),
            HashMap::new(),
        );
        let file = shared_root.join("lib.zen");

        assert_eq!(
            resolution.load_cache_scope_key_for_file(&file, Some("github.com/acme/root-a"),),
            resolution.load_cache_scope_key_for_file(&file, Some("github.com/acme/root-b"),)
        );
    }

    #[test]
    fn frozen_scope_cache_key_changes_with_package_deps() {
        let shared_root = PathBuf::from("/cache/github.com/acme/shared/1.0.0");
        let package_with_dep = |dep_root: &str| FrozenPackage {
            identity: FrozenPackageIdentity::Remote {
                dep_id: FrozenDepId {
                    path: "github.com/acme/shared".into(),
                    lane: "v1".into(),
                },
                version: Version::parse("1.0.0").unwrap(),
            },
            deps: BTreeMap::from([("github.com/acme/base".into(), PathBuf::from(dep_root))]),
            parts: Vec::new(),
        };
        let resolution = ResolutionResult::frozen(
            WorkspaceInfo {
                root: PathBuf::from("/workspace"),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            BTreeMap::from([
                (
                    "github.com/acme/root-a".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(
                            shared_root.clone(),
                            package_with_dep("/cache/github.com/acme/base/1.0.0"),
                        )]),
                    },
                ),
                (
                    "github.com/acme/root-b".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(
                            shared_root.clone(),
                            package_with_dep("/cache/github.com/acme/base/2.0.0"),
                        )]),
                    },
                ),
            ]),
            HashMap::new(),
        );
        let file = shared_root.join("lib.zen");

        assert_ne!(
            resolution.load_cache_scope_key_for_file(&file, Some("github.com/acme/root-a"),),
            resolution.load_cache_scope_key_for_file(&file, Some("github.com/acme/root-b"),)
        );
    }

    #[test]
    fn frozen_scope_requires_active_root_for_shared_package() {
        let shared_root = PathBuf::from("/cache/github.com/acme/shared/1.0.0");
        let package_with_dep = |dep_root: &str| FrozenPackage {
            identity: FrozenPackageIdentity::Remote {
                dep_id: FrozenDepId {
                    path: "github.com/acme/shared".into(),
                    lane: "v1".into(),
                },
                version: Version::parse("1.0.0").unwrap(),
            },
            deps: BTreeMap::from([("github.com/acme/base".into(), PathBuf::from(dep_root))]),
            parts: Vec::new(),
        };
        let resolution = ResolutionResult::frozen(
            WorkspaceInfo {
                root: PathBuf::from("/workspace"),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            BTreeMap::from([
                (
                    "github.com/acme/root-a".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(
                            shared_root.clone(),
                            package_with_dep("/cache/github.com/acme/base/1.0.0"),
                        )]),
                    },
                ),
                (
                    "github.com/acme/root-b".into(),
                    FrozenResolutionMap {
                        selected_remote: BTreeMap::new(),
                        packages: BTreeMap::from([(
                            shared_root.clone(),
                            package_with_dep("/cache/github.com/acme/base/2.0.0"),
                        )]),
                    },
                ),
            ]),
            HashMap::new(),
        );

        assert_eq!(
            resolution.load_cache_scope_key_for_file(&shared_root.join("lib.zen"), None),
            None
        );
    }

    #[test]
    fn test_format_package_uri_cache_rewrite() {
        let workspace_root = PathBuf::from("/workspace");
        let global_cache = PathBuf::from("/Users/test/.pcb/cache");
        let workspace = WorkspaceInfo {
            root: workspace_root.clone(),
            cache_dir: global_cache.clone(),
            config: None,
            packages: BTreeMap::new(),
            errors: vec![],
        };

        let result =
            ResolutionResult::frozen(workspace, FrozenResolutionSet::new(), HashMap::new());

        let abs = workspace_root
            .join(".pcb")
            .join(STDLIB_MODULE_PATH)
            .join("test.kicad_mod");
        let uri = result.format_package_uri(&abs);
        assert_eq!(uri.as_deref(), Some("package://stdlib/test.kicad_mod"));
    }

    #[test]
    fn test_package_roots_include_workspace_fallback_for_standalone_files() {
        let workspace_root = PathBuf::from("/workspace");
        let result = ResolutionResult::frozen(
            WorkspaceInfo {
                root: workspace_root.clone(),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                errors: vec![],
            },
            FrozenResolutionSet::new(),
            HashMap::new(),
        );

        let abs = workspace_root.join("lib.kicad_sym");
        let uri = result.format_package_uri(&abs);
        assert_eq!(uri.as_deref(), Some("package://workspace/lib.kicad_sym"));
        assert_eq!(
            result.resolve_package_uri(uri.as_deref().unwrap()).unwrap(),
            abs
        );
    }

    #[test]
    fn test_rev_dep_uses_selected_path() {
        struct RecordingResolver {
            expected_version: String,
            resolved_path: PathBuf,
            closure: HashMap<ModuleLine, Version>,
        }

        impl PackagePathResolver for RecordingResolver {
            fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
                (module_path == "github.com/diodeinc/registry/modules/CastellatedHoles"
                    && version == self.expected_version)
                    .then_some(self.resolved_path.clone())
            }

            fn selected_versions(&self) -> &HashMap<ModuleLine, Version> {
                &self.closure
            }
        }

        let workspace_root = PathBuf::from("/workspace");
        let package_root = workspace_root.join("boards/IP0003");
        let dep_url = "github.com/diodeinc/registry/modules/CastellatedHoles".to_string();
        let stable_version = Version::parse("0.3.1").unwrap();
        let pseudo_version =
            Version::parse("0.4.3-0.20260318022845-ef7e97a27f6e57783bfbeece051aa2d81a365ace")
                .unwrap();
        let resolved_path = PathBuf::from(format!("/cache/{}/{}", dep_url, pseudo_version));

        let workspace = WorkspaceInfo {
            root: workspace_root.clone(),
            cache_dir: PathBuf::new(),
            config: None,
            packages: BTreeMap::from([(
                "github.com/dioderobot/diode/boards/IP0003".to_string(),
                crate::workspace::WorkspacePackage {
                    rel_path: PathBuf::from("boards/IP0003"),
                    config: PcbToml {
                        dependencies: crate::config::DependencyTable {
                            direct: BTreeMap::from([(
                                dep_url.clone(),
                                DependencySpec::Detailed(DependencyDetail {
                                    version: None,
                                    branch: Some("diode/boards/IP0003".into()),
                                    rev: Some("ef7e97a27f6e57783bfbeece051aa2d81a365ace".into()),
                                    path: None,
                                }),
                            )]),
                            indirect: BTreeMap::new(),
                        },
                        ..PcbToml::default()
                    },
                    version: None,
                    published_at: None,
                    preferred: false,
                    dirty: false,
                    entrypoints: Vec::new(),
                    symbol_files: Vec::new(),
                },
            )]),
            errors: vec![],
        };
        let stable_line = ModuleLine::new(dep_url.clone(), &stable_version);
        let pseudo_line = ModuleLine::new(dep_url.clone(), &pseudo_version);
        let closure = HashMap::from([
            (stable_line, stable_version),
            (pseudo_line, pseudo_version.clone()),
        ]);
        let resolver = RecordingResolver {
            expected_version: pseudo_version.to_string(),
            resolved_path: resolved_path.clone(),
            closure: closure.clone(),
        };

        let results = build_resolution_map(
            &InMemoryFileProvider::new(HashMap::new()),
            &resolver,
            &workspace,
            &closure,
        );

        assert_eq!(
            results
                .get(&package_root)
                .and_then(|deps| deps.get(&dep_url))
                .cloned(),
            Some(resolved_path)
        );
    }

    #[test]
    fn test_rev_dep_ignores_non_pseudo_prerelease() {
        let dep = "github.com/diodeinc/registry/modules/CastellatedHoles";
        let prerelease = Version::parse("1.0.0-alpha-1").unwrap();
        let pseudo =
            Version::parse("1.0.0-0.20260319233030-1cdbd386c7adffd8373fbedf7532122b55092108")
                .unwrap();
        let rev = "1cdbd386c7adffd8373fbedf7532122b55092108";
        let prerelease_line = ModuleLine::new(dep.to_string(), &prerelease);
        let pseudo_line = ModuleLine::new(dep.to_string(), &pseudo);
        let selected = HashMap::from([(prerelease_line, prerelease), (pseudo_line, pseudo)]);
        let detail = DependencyDetail {
            version: None,
            branch: Some("main".into()),
            rev: Some(rev.into()),
            path: None,
        };

        let version = select_version_for_detail(dep, &detail, &selected).unwrap();
        assert_eq!(
            version,
            "1.0.0-0.20260319233030-1cdbd386c7adffd8373fbedf7532122b55092108"
        );
    }
}
