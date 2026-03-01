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
use crate::config::{DependencySpec, Lockfile, PcbToml};
use crate::workspace::WorkspaceInfo;

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
    pub path: String,   // e.g., "github.com/diodeinc/stdlib"
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
    let version_str = match spec {
        DependencySpec::Version(v) => v.as_str(),
        DependencySpec::Detailed(d) => d
            .version
            .as_deref()
            .or(d.rev.as_deref())
            .or(d.branch.as_deref())?,
    };
    resolver.resolve_package(url, version_str)
}

/// Build resolution map for a single package's dependencies.
pub fn build_package_map<R: PackagePathResolver>(
    resolver: &R,
    workspace: &WorkspaceInfo,
    base_dir: &Path,
    deps: &BTreeMap<String, DependencySpec>,
) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();

    for (url, spec) in deps {
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
        let resolved = build_package_map(
            resolver,
            workspace,
            &member_dir,
            &member.config.dependencies,
        );
        results.insert(member_dir, resolved);
    }

    // Build map for workspace root if not already included as a package.
    if !results.contains_key(&workspace.root) {
        if let Some(config) = workspace.config.as_ref() {
            let resolved =
                build_package_map(resolver, workspace, &workspace.root, &config.dependencies);
            results.insert(workspace.root.clone(), resolved);
        } else {
            results.insert(workspace.root.clone(), BTreeMap::new());
        }
    }

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

        let resolved = build_package_map(resolver, workspace, &pkg_path, &config.dependencies);
        results.insert(pkg_path, resolved);
    }

    // stdlib is implicit for all packages; inject into each map.
    let stdlib_path = workspace
        .packages
        .get(STDLIB_MODULE_PATH)
        .map(|member| member.dir(&workspace.root))
        .or_else(|| {
            closure
                .iter()
                .filter(|(line, _)| line.path == STDLIB_MODULE_PATH)
                .max_by(|(_, v1), (_, v2)| v1.cmp(v2))
                .and_then(|(_, v)| resolver.resolve_package(STDLIB_MODULE_PATH, &v.to_string()))
        });
    if let Some(path) = stdlib_path {
        for deps in results.values_mut() {
            deps.entry(STDLIB_MODULE_PATH.to_string())
                .or_insert_with(|| path.clone());
        }
    }

    // Asset dependency repos (symbols, footprints, models) are configured at workspace level.
    // Inject them everywhere so alias resolution, symbol->footprint inference, and model
    // embedding work even before explicit deps exist.
    let configured_asset_paths: Vec<(String, PathBuf)> = workspace
        .asset_dep_versions()
        .into_iter()
        .filter_map(|(repo, version)| {
            resolver
                .resolve_package(&repo, &version.to_string())
                .map(|path| (repo, path))
        })
        .collect();
    for deps in results.values_mut() {
        for (repo, path) in &configured_asset_paths {
            deps.entry(repo.clone()).or_insert_with(|| path.clone());
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
        }
    }

    /// Canonicalize `package_resolutions` keys using the given file provider.
    pub fn canonicalize_keys(&mut self, file_provider: &dyn crate::FileProvider) {
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
        let mut roots = BTreeMap::new();

        for (url, pkg) in &self.workspace_info.packages {
            roots.insert(url.clone(), pkg.dir(&self.workspace_info.root));
        }

        for deps in self.package_resolutions.values() {
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

    /// KiCad model variable → resolved directory mapping.
    ///
    /// Looks up model repo paths from the resolution map (which goes through the
    /// resolver: patches → vendor → cache) rather than hardcoding cache paths.
    pub fn kicad_model_dirs(&self) -> BTreeMap<String, PathBuf> {
        let any_deps = self.package_resolutions.values().next();
        let mut model_dirs = BTreeMap::new();
        for entry in self.workspace_info.kicad_library_entries() {
            for (var, repo) in &entry.models {
                if let Some(path) = any_deps.and_then(|deps| deps.get(repo)) {
                    model_dirs.insert(var.clone(), path.clone());
                }
            }
        }
        model_dirs
    }

    /// Resolve a `package://…` URI to an absolute filesystem path.
    pub fn resolve_package_uri(&self, uri: &str) -> anyhow::Result<PathBuf> {
        pcb_sch::resolve_package_uri(uri, &self.package_roots())
    }

    /// Format an absolute path as a `package://…` URI.
    ///
    /// Uses longest-prefix matching to find the owning package.
    pub fn format_package_uri(&self, abs: &Path) -> Option<String> {
        let package_roots = self.package_roots();
        let workspace_cache = self.workspace_info.workspace_cache_dir();
        let effective_abs = abs
            .strip_prefix(&self.workspace_info.cache_dir)
            .map(|rel| workspace_cache.join(rel))
            .unwrap_or_else(|_| abs.to_path_buf());
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
    use crate::workspace::MemberPackage;

    struct NoOpResolver;

    impl PackagePathResolver for NoOpResolver {
        fn resolve_package(&self, _: &str, _: &str) -> Option<PathBuf> {
            None
        }
    }

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

    /// Test that stdlib is resolved correctly when it's a workspace member (path-patched fork).
    ///
    /// Regression test for: `pcb fork github.com/diodeinc/stdlib` would cause builds to fail
    /// with "Unknown alias '@stdlib'" because the stdlib injection only searched the closure,
    /// but workspace members are excluded from the closure.
    #[test]
    fn test_stdlib_resolved_from_workspace_member() {
        let workspace_root = PathBuf::from("/workspace");
        let stdlib_fork_path = PathBuf::from("fork/github.com/diodeinc/stdlib/0.5.0");
        let board_path = PathBuf::from("boards/myboard");

        // Create workspace with:
        // 1. A board package (simulating boards/myboard/)
        // 2. stdlib as a workspace member (simulating a path-patched fork)
        let mut packages = BTreeMap::new();

        // Board package
        packages.insert(
            "github.com/test/proj/boards/myboard".to_string(),
            MemberPackage {
                rel_path: board_path.clone(),
                config: PcbToml::default(),
                version: None,
                dirty: false,
            },
        );

        // Forked stdlib as workspace member (this is what `pcb fork` creates)
        packages.insert(
            STDLIB_MODULE_PATH.to_string(),
            MemberPackage {
                rel_path: stdlib_fork_path.clone(),
                config: PcbToml::default(),
                version: Some("0.5.0".to_string()),
                dirty: false,
            },
        );

        let workspace = WorkspaceInfo {
            root: workspace_root.clone(),
            cache_dir: PathBuf::new(),
            config: None,
            packages,
            lockfile: None,
            errors: vec![],
        };

        // Empty closure - stdlib is NOT in the closure because it's a workspace member
        let closure: HashMap<ModuleLine, Version> = HashMap::new();

        let file_provider = crate::DefaultFileProvider::default();
        let results = build_resolution_map(&file_provider, &NoOpResolver, &workspace, &closure);
        // stdlib should resolve to the forked workspace member path via merged package map.
        let board_dir = workspace_root.join(&board_path);
        let board_map = results
            .get(&board_dir)
            .expect("board should have resolution map");
        assert_eq!(
            board_map.get(STDLIB_MODULE_PATH),
            Some(&workspace_root.join(&stdlib_fork_path)),
            "stdlib should resolve to the forked workspace member path"
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

        let version = Version::parse("0.5.9").unwrap();
        let line = ModuleLine::new(STDLIB_MODULE_PATH.to_string(), &version);
        let mut closure = HashMap::new();
        closure.insert(line, version);

        let stdlib_path = workspace_root.join(".pcb/cache/github.com/diodeinc/stdlib/0.5.9");
        let mut root_deps = BTreeMap::new();
        root_deps.insert(STDLIB_MODULE_PATH.to_string(), stdlib_path);
        let mut package_resolutions = HashMap::new();
        package_resolutions.insert(workspace_root.clone(), root_deps);

        let result = ResolutionResult {
            workspace_info: workspace,
            package_resolutions,
            closure,
            lockfile_changed: false,
        };

        let abs = global_cache.join("github.com/diodeinc/stdlib/0.5.9/test.kicad_mod");
        let uri = result.format_package_uri(&abs);
        assert_eq!(
            uri.as_deref(),
            Some("package://github.com/diodeinc/stdlib@0.5.9/test.kicad_mod")
        );
    }
}
