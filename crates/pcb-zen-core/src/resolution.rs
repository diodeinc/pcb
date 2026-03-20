//! Shared dependency resolution logic.
//!
//! This module provides the core resolution map building functionality used by both
//! native (pcb-zen) and WASM (pcb-zen-wasm) builds. The key abstraction is
//! `PackagePathResolver` which allows different strategies for resolving package
//! paths:
//!
//! - Native: checks patches, vendor/, then ~/.pcb/cache
//! - WASM: only checks vendor/ (everything must be pre-vendored)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use semver::Version;

use crate::FileProvider;
use crate::STDLIB_MODULE_PATH;
use crate::config::{DependencySpec, Lockfile, ManifestPart, PcbToml};
use crate::workspace::{LOCAL_WORKSPACE_ROOT_URL, WorkspaceInfo};

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
        detail: &crate::config::DependencyDetail,
    ) -> Option<PathBuf> {
        let version = if let Some(version) = &detail.version {
            version.clone()
        } else {
            self.selected_versions()
                .iter()
                .find(|(line, _)| line.path == module_path)
                .map(|(_, version)| version.to_string())?
        };
        self.resolve_package(module_path, &version)
    }
}

/// Build the package coordinate → absolute root directory mapping.
///
/// Workspace members come from `workspace_info.packages`. External deps
/// are discovered from `package_resolutions` values (already resolved by the
/// resolver through patches → vendor → cache).
pub fn build_package_roots(
    workspace_info: &WorkspaceInfo,
    package_resolutions: &HashMap<PathBuf, BTreeMap<String, PathBuf>>,
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

    for deps in package_resolutions.values() {
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

/// Build resolution map for a single package's [dependencies] and promoted [assets].
fn resolve_package_deps<R: PackagePathResolver>(
    resolver: &R,
    workspace: &WorkspaceInfo,
    base_dir: &Path,
    config: &PcbToml,
) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();

    for (url, spec) in &config.dependencies {
        if let Some(path) = resolve_dep(resolver, workspace, base_dir, url, spec) {
            map.insert(url.clone(), path);
        }
    }

    // If any repo from a [[workspace.kicad_library]] entry is referenced via
    // dependencies, resolve all sibling repos from that entry.
    // e.g. adding kicad-symbols as a dep also brings in kicad-footprints + models.
    for entry in workspace.kicad_library_entries() {
        let repos: Vec<&String> = [&entry.symbols, &entry.footprints]
            .into_iter()
            .chain(entry.models.values())
            .collect();
        let has_reference = repos.iter().any(|repo| map.contains_key(repo.as_str()));
        if !has_reference {
            continue;
        }
        // Use the version from an already-resolved sibling repo path so this
        // stays aligned with selected/MVS resolution.
        let Some(version_str) = repos.iter().find_map(|repo| {
            map.get(*repo).and_then(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            })
        }) else {
            // No resolved sibling to anchor version selection; skip promotion.
            continue;
        };
        for repo in repos {
            if !map.contains_key(repo.as_str())
                && let Some(path) = resolver.resolve_package(repo, &version_str)
            {
                map.insert(repo.clone(), path);
            }
        }
    }

    map
}

/// Path resolver that only looks in the vendor directory.
///
/// Used by WASM where all dependencies must be pre-vendored in the zip.
pub struct VendoredPathResolver {
    vendor_dir: PathBuf,
    /// Pre-computed closure from lockfile: ModuleLine -> Version
    closure: HashMap<ModuleLine, Version>,
}

impl VendoredPathResolver {
    /// Get the closure (ModuleLine -> Version mapping).
    pub fn closure(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
    }

    /// Create a new vendored path resolver from a lockfile.
    ///
    /// Package closure is loaded from lockfile entries that include `manifest_hash`.
    pub fn from_lockfile<F: FileProvider>(
        file_provider: F,
        vendor_dir: PathBuf,
        lockfile: &Lockfile,
    ) -> Self {
        let mut closure = HashMap::new();

        for entry in lockfile.iter() {
            if entry.manifest_hash.is_some() {
                let path = vendor_dir.join(&entry.module_path).join(&entry.version);
                if file_provider.exists(&path)
                    && let Ok(version) = Version::parse(&entry.version)
                {
                    let line = ModuleLine::new(entry.module_path.clone(), &version);
                    closure.insert(line, version);
                }
            }
        }

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

        // Allow non-lockfile deps (e.g. asset dependencies) by direct {module}/{version}.
        Some(self.vendor_dir.join(module_path).join(version))
    }

    fn selected_versions(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
    }
}

/// Build the per-package resolution map for workspace members and all packages in the closure.
///
/// Returns a map from package root path to (dependency URL -> resolved path).
pub fn build_resolution_map<F: FileProvider, R: PackagePathResolver>(
    file_provider: &F,
    resolver: &R,
    workspace: &WorkspaceInfo,
    closure: &HashMap<ModuleLine, Version>,
) -> HashMap<PathBuf, BTreeMap<String, PathBuf>> {
    let mut results = HashMap::new();

    // Build map for each workspace member (already have their configs loaded).
    for member in workspace.packages.values() {
        let member_dir = member.dir(&workspace.root);
        let resolved = resolve_package_deps(resolver, workspace, &member_dir, &member.config);
        results.insert(member_dir, resolved);
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

    // Stdlib has implicit managed KiCad dependencies from workspace config.
    let stdlib_root = workspace.workspace_stdlib_dir();
    let stdlib_deps = results.entry(stdlib_root).or_default();
    for (repo, version) in workspace.asset_dep_versions() {
        if let Some(path) = resolver.resolve_package(&repo, &version.to_string()) {
            stdlib_deps.insert(repo, path);
        }
    }

    results
}

/// Path resolver for native CLI that supports patches, vendor, and cache.
///
/// Resolution order: patches → vendor → cache.
///
/// Note: Workspace members are handled directly in `build_resolution_map` before
/// calling the resolver, so they don't need to be tracked here.
pub struct NativePathResolver {
    pub vendor_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub patches: HashMap<String, PathBuf>,
    pub closure: HashMap<ModuleLine, Version>,
}

impl PackagePathResolver for NativePathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
        if let Some(patch_path) = self.patches.get(module_path) {
            return Some(patch_path.clone());
        }

        let vendor_path = self.vendor_dir.join(module_path).join(version);
        if vendor_path.exists() {
            return Some(vendor_path);
        }

        let cache_path = self.cache_dir.join(module_path).join(version);
        if cache_path.exists() {
            return Some(cache_path);
        }

        None
    }

    fn selected_versions(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
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
    /// Map from Package Root (Absolute Path) -> Import URL -> Resolved Absolute Path
    pub package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    /// Package dependencies in the build closure: ModuleLine -> Version
    pub closure: HashMap<ModuleLine, Version>,
    /// Whether the lockfile (pcb.sum) was updated during resolution
    pub lockfile_changed: bool,
    /// Symbol-to-parts mapping built from `[parts]` sections across all manifests.
    ///
    /// Keys are `package://` URIs for `.kicad_sym` files. Values are ordered lists
    /// of parts declared for that symbol (preserving manifest order).
    pub symbol_parts: HashMap<String, Vec<ManifestPart>>,
}

impl ResolutionResult {
    /// Create an empty resolution result with no dependencies.
    pub fn empty() -> Self {
        Self {
            workspace_info: WorkspaceInfo {
                root: PathBuf::new(),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                lockfile: None,
                errors: vec![],
            },
            package_resolutions: HashMap::new(),
            closure: HashMap::new(),
            lockfile_changed: false,
            symbol_parts: HashMap::new(),
        }
    }

    /// Canonicalize `package_resolutions` keys using the given file provider.
    pub fn canonicalize_keys(&mut self, file_provider: &dyn crate::FileProvider) {
        if !self.workspace_info.cache_dir.as_os_str().is_empty() {
            self.workspace_info.cache_dir = file_provider
                .canonicalize(&self.workspace_info.cache_dir)
                .unwrap_or_else(|_| self.workspace_info.cache_dir.clone());
        }
        self.package_resolutions = self
            .package_resolutions
            .iter()
            .map(|(root, deps)| {
                let canon = file_provider
                    .canonicalize(root)
                    .unwrap_or_else(|_| root.clone());
                (canon, deps.clone())
            })
            .collect();
    }

    /// Build the package coordinate → absolute root directory mapping.
    ///
    /// Workspace members come from `workspace_info.packages`. External deps
    /// are discovered from `package_resolutions` values (already resolved by the
    /// resolver through patches → vendor → cache).
    pub fn package_roots(&self) -> BTreeMap<String, PathBuf> {
        build_package_roots(&self.workspace_info, &self.package_resolutions)
    }

    /// KiCad model variable → resolved directory mapping.
    pub fn kicad_model_dirs(&self) -> BTreeMap<String, PathBuf> {
        let mut model_dirs = BTreeMap::new();
        for entry in self.workspace_info.kicad_library_entries() {
            for (var, repo) in &entry.models {
                if let Some(path) = self
                    .package_resolutions
                    .values()
                    .find_map(|deps| deps.get(repo))
                {
                    model_dirs.insert(var.clone(), path.clone());
                }
            }
        }
        model_dirs
    }

    /// Resolve a package URI (`package://…`) to an absolute filesystem path.
    pub fn resolve_package_uri(&self, uri: &str) -> anyhow::Result<PathBuf> {
        pcb_sch::resolve_package_uri(uri, &self.package_roots())
    }

    /// Format an absolute path as a stable URI (`package://…`).
    ///
    /// Uses longest-prefix matching to find the owning package.
    pub fn format_package_uri(&self, abs: &Path) -> Option<String> {
        let package_roots = self.package_roots();
        let effective_abs = if self.workspace_info.cache_dir.as_os_str().is_empty() {
            abs.to_path_buf()
        } else {
            let workspace_cache = self.workspace_info.workspace_cache_dir();
            abs.strip_prefix(&self.workspace_info.cache_dir)
                .map(|rel| workspace_cache.join(rel))
                .unwrap_or_else(|_| abs.to_path_buf())
        };
        pcb_sch::format_package_uri(&effective_abs, &package_roots)
    }

    /// Compute the transitive dependency closure for a package.
    pub fn package_closure(&self, package_url: &str) -> PackageClosure {
        let workspace_info = &self.workspace_info;
        let mut closure = PackageClosure::default();
        let mut visited: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = vec![package_url.to_string()];

        while let Some(url) = stack.pop() {
            if !visited.insert(url.clone()) {
                continue;
            }

            if let Some(pkg) = workspace_info.packages.get(&url) {
                closure.local_packages.insert(url.clone());
                for dep_url in pkg.config.dependencies.keys() {
                    stack.push(dep_url.clone());
                }
            } else if let Some((_, version)) = self.closure.iter().find(|(l, _)| l.path == url) {
                closure
                    .remote_packages
                    .insert((url.clone(), version.to_string()));
                // Find resolved root from any package that depends on this one
                let pkg_root = self
                    .package_resolutions
                    .values()
                    .find_map(|deps| deps.get(&url));
                if let Some(deps) = pkg_root.and_then(|root| self.package_resolutions.get(root)) {
                    for dep_url in deps.keys() {
                        stack.push(dep_url.clone());
                    }
                }
            }
        }

        closure
    }
}

/// Transitive dependency closure for a package
#[derive(Debug, Clone, Default)]
pub struct PackageClosure {
    pub local_packages: HashSet<String>,
    pub remote_packages: HashSet<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;
    use crate::config::DependencyDetail;

    #[test]
    fn test_vendored_path_resolver_basic() {
        // Use platform-appropriate paths
        let vendor_dir = PathBuf::from("/workspace/vendor");
        let pkg_path = vendor_dir.join("github.com/user/pkg/1.0.0");
        let toml_path = pkg_path.join("pcb.toml");

        let mut files = HashMap::new();
        files.insert(
            toml_path.to_string_lossy().to_string(),
            "[board]\nname = \"test\"\npath = \"test.zen\"\n".to_string(),
        );

        let provider = InMemoryFileProvider::new(files);
        let lockfile = Lockfile::parse(
            "github.com/user/pkg 1.0.0 h1:abc123\n\
             github.com/user/pkg 1.0.0/pcb.toml h1:def456\n",
        )
        .unwrap();

        let resolver = VendoredPathResolver::from_lockfile(provider, vendor_dir, &lockfile);

        let path = resolver.resolve_package("github.com/user/pkg", "1.0.0");
        assert_eq!(path, Some(pkg_path));
    }

    #[test]
    fn test_vendored_path_resolver_direct_vendor_fallback() {
        let vendor_dir = PathBuf::from("/workspace/vendor");
        let provider = InMemoryFileProvider::new(HashMap::from([(
            "/workspace/vendor/gitlab.com/kicad/libraries/kicad-symbols/9.0.3/.sentinel"
                .to_string(),
            "".to_string(),
        )]));
        let lockfile = Lockfile::default();
        let resolver = VendoredPathResolver::from_lockfile(provider, vendor_dir.clone(), &lockfile);

        let path = resolver.resolve_package("gitlab.com/kicad/libraries/kicad-symbols", "9.0.3");
        assert_eq!(
            path,
            Some(vendor_dir.join("gitlab.com/kicad/libraries/kicad-symbols/9.0.3"))
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
            lockfile: None,
            errors: vec![],
        };

        let result = ResolutionResult {
            workspace_info: workspace,
            package_resolutions: HashMap::new(),
            closure: HashMap::new(),
            lockfile_changed: false,
            symbol_parts: HashMap::new(),
        };

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
        let result = ResolutionResult {
            workspace_info: WorkspaceInfo {
                root: workspace_root.clone(),
                cache_dir: PathBuf::new(),
                config: None,
                packages: BTreeMap::new(),
                lockfile: None,
                errors: vec![],
            },
            package_resolutions: HashMap::new(),
            closure: HashMap::new(),
            lockfile_changed: false,
            symbol_parts: HashMap::new(),
        };

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
                crate::workspace::MemberPackage {
                    rel_path: PathBuf::from("boards/IP0003"),
                    config: PcbToml {
                        dependencies: BTreeMap::from([(
                            dep_url.clone(),
                            DependencySpec::Detailed(DependencyDetail {
                                version: None,
                                branch: Some("diode/boards/IP0003".into()),
                                rev: Some("ef7e97a27f6e57783bfbeece051aa2d81a365ace".into()),
                                path: None,
                            }),
                        )]),
                        ..PcbToml::default()
                    },
                    version: None,
                    published_at: None,
                    preferred: false,
                    dirty: false,
                },
            )]),
            lockfile: None,
            errors: vec![],
        };
        let closure = HashMap::from([(
            ModuleLine::new(dep_url.clone(), &pseudo_version),
            pseudo_version.clone(),
        )]);
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
}
