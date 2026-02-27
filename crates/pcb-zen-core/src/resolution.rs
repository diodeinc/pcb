//! Shared dependency resolution logic.
//!
//! This module provides the core resolution map building functionality used by both
//! native (pcb-zen) and WASM (pcb-zen-wasm) builds. The key abstraction is
//! `PackagePathResolver` which allows different strategies for resolving package
//! and asset paths:
//!
//! - Native: checks patches, vendor/, then ~/.pcb/cache
//! - WASM: only checks vendor/ (everything must be pre-vendored)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use semver::Version;

use crate::FileProvider;
use crate::STDLIB_MODULE_PATH;
use crate::config::{
    AssetDependencySpec, DependencySpec, Lockfile, PcbToml, extract_asset_ref,
    split_asset_repo_and_subpath,
};
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

/// Trait for resolving package and asset paths.
pub trait PackagePathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf>;
    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf>;
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
    assets: &BTreeMap<String, AssetDependencySpec>,
) -> BTreeMap<String, PathBuf> {
    let mut map = BTreeMap::new();

    for (url, spec) in deps {
        if let Some(path) = resolve_dep(resolver, workspace, base_dir, url, spec) {
            map.insert(url.clone(), path);
        }
    }

    for (asset_key, asset_spec) in assets {
        if let Some(ref_str) = extract_asset_ref(asset_spec)
            && let Some(path) = resolver.resolve_asset(asset_key, &ref_str)
        {
            map.insert(asset_key.clone(), path);
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
    /// Pre-computed asset repo paths: (repo_url, ref) -> vendored repo root
    asset_repos: HashMap<(String, String), PathBuf>,
}

impl VendoredPathResolver {
    /// Get the closure (ModuleLine -> Version mapping).
    pub fn closure(&self) -> &HashMap<ModuleLine, Version> {
        &self.closure
    }

    /// Create a new vendored path resolver from a lockfile.
    ///
    /// The lockfile contains two types of entries:
    /// - Packages (code dependencies): have a manifest_hash, stored at vendor/{module_path}/{version}
    /// - Assets (data files like KiCad libs): no manifest_hash, stored at vendor/{repo_url}/{version}/{subpath}
    pub fn from_lockfile<F: FileProvider>(
        file_provider: F,
        vendor_dir: PathBuf,
        lockfile: &Lockfile,
    ) -> Self {
        let mut closure = HashMap::new();
        let mut asset_repos = HashMap::new();

        for entry in lockfile.iter() {
            if entry.manifest_hash.is_some() {
                let path = vendor_dir.join(&entry.module_path).join(&entry.version);
                if file_provider.exists(&path)
                    && let Ok(version) = Version::parse(&entry.version)
                {
                    let line = ModuleLine::new(entry.module_path.clone(), &version);
                    closure.insert(line, version);
                }
            } else {
                let (repo_url, _subpath) = split_asset_repo_and_subpath(&entry.module_path);
                let repo_path = vendor_dir.join(repo_url).join(&entry.version);
                if file_provider.exists(&repo_path) {
                    asset_repos.insert((repo_url.to_string(), entry.version.clone()), repo_path);
                }
            }
        }

        Self {
            vendor_dir,
            closure,
            asset_repos,
        }
    }
}

impl PackagePathResolver for VendoredPathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
        // Look up by (path, family) and return the path using the selected version from closure
        let ver = Version::parse(version).ok()?;
        let line = ModuleLine::new(module_path.to_string(), &ver);
        self.closure
            .get(&line)
            .map(|selected| self.vendor_dir.join(module_path).join(selected.to_string()))
    }

    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf> {
        let (repo_url, subpath) = split_asset_repo_and_subpath(asset_key);
        let key = (repo_url.to_string(), ref_str.to_string());

        self.asset_repos.get(&key).map(|repo_path| {
            if subpath.is_empty() {
                repo_path.clone()
            } else {
                repo_path.join(subpath)
            }
        })
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

    // Build map for each workspace member (already have their configs loaded)
    for member in workspace.packages.values() {
        let member_dir = member.dir(&workspace.root);
        results.insert(
            member_dir.clone(),
            build_package_map(
                resolver,
                workspace,
                &member_dir,
                &member.config.dependencies,
                &member.config.assets,
            ),
        );
    }

    // Build map for workspace root if not already included as a package
    if !results.contains_key(&workspace.root) {
        let empty_deps = BTreeMap::new();
        let empty_assets = BTreeMap::new();
        let (root_deps, root_assets) = workspace
            .config
            .as_ref()
            .map(|c| (&c.dependencies, &c.assets))
            .unwrap_or((&empty_deps, &empty_assets));
        results.insert(
            workspace.root.clone(),
            build_package_map(resolver, workspace, &workspace.root, root_deps, root_assets),
        );
    }

    // Build map for external packages in the closure (need to read their pcb.toml)
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

        results.insert(
            pkg_path.clone(),
            build_package_map(
                resolver,
                workspace,
                &pkg_path,
                &config.dependencies,
                &config.assets,
            ),
        );
    }

    // Inject stdlib into all package maps (stdlib is an implicit dependency for all packages)
    // First check if stdlib is a workspace member (e.g., path-patched fork), then fall back to closure
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
    if let Some(stdlib_path) = stdlib_path {
        for map in results.values_mut() {
            map.entry(STDLIB_MODULE_PATH.to_string())
                .or_insert(stdlib_path.clone());
        }
    }

    results
}

/// Path resolver for native CLI that supports vendor, cache, and patches.
///
/// Note: Workspace members are handled directly in `build_resolution_map` before
/// calling the resolver, so they don't need to be tracked here.
pub struct NativePathResolver {
    pub vendor_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub offline: bool,
    pub patches: HashMap<String, PathBuf>,
    pub asset_paths: HashMap<(String, String), PathBuf>,
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

        if !self.offline {
            let cache_path = self.cache_dir.join(module_path).join(version);
            if cache_path.exists() {
                return Some(cache_path);
            }
        }

        None
    }

    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf> {
        let key = (asset_key.to_string(), ref_str.to_string());

        // Exact match
        if let Some(path) = self.asset_paths.get(&key) {
            return Some(path.clone());
        }

        // Try repo root + subpath (e.g., "github.com/org/lib/subdir" -> repo_root + "subdir")
        let (repo_url, subpath) = split_asset_repo_and_subpath(asset_key);
        if !subpath.is_empty() {
            let repo_key = (repo_url.to_string(), ref_str.to_string());
            if let Some(repo_path) = self.asset_paths.get(&repo_key) {
                return Some(repo_path.join(subpath));
            }

            // Find any entry with same repo and derive repo root from it
            for ((k, k_ref), path) in &self.asset_paths {
                if k_ref != ref_str {
                    continue;
                }
                let (k_repo, k_subpath) = split_asset_repo_and_subpath(k);
                if k_repo == repo_url
                    && !k_subpath.is_empty()
                    && let Some(repo_root) = path.parent()
                {
                    return Some(repo_root.join(subpath));
                }
            }
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
    /// Asset dependencies: (module_path, ref) -> resolved_path
    pub assets: HashMap<(String, String), PathBuf>,
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
            assets: HashMap::new(),
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

    /// Resolve a closure package to its root directory, preferring vendor over cache.
    fn resolve_closure_package_root(&self, module_path: &str, version: &str) -> PathBuf {
        let vendor_path = self
            .workspace_info
            .root
            .join("vendor")
            .join(module_path)
            .join(version);
        if vendor_path.exists() {
            vendor_path
        } else {
            self.workspace_info
                .workspace_cache_dir()
                .join(module_path)
                .join(version)
        }
    }

    /// Build the package coordinate → absolute root directory mapping.
    ///
    /// Covers workspace member packages (from `workspace_info.packages`),
    /// external dependency packages (from `closure`, resolved via vendor or cache),
    /// and asset repo roots (from `assets`).
    pub fn package_roots(&self) -> BTreeMap<String, PathBuf> {
        let mut roots = BTreeMap::new();

        for (url, pkg) in &self.workspace_info.packages {
            roots.insert(url.clone(), pkg.dir(&self.workspace_info.root));
        }

        for (module_line, version) in &self.closure {
            let version_str = version.to_string();
            let pkg_root = self.resolve_closure_package_root(&module_line.path, &version_str);
            roots.insert(format!("{}@{}", module_line.path, version_str), pkg_root);
        }

        for ((asset_key, ref_str), resolved_path) in &self.assets {
            let (repo_url, subpath) = split_asset_repo_and_subpath(asset_key);
            let repo_root = if subpath.is_empty() {
                resolved_path.clone()
            } else {
                resolved_path
                    .ancestors()
                    .nth(Path::new(subpath).components().count())
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| resolved_path.clone())
            };
            roots
                .entry(format!("{repo_url}@{ref_str}"))
                .or_insert(repo_root);
        }

        roots
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
                for (asset_url, asset_spec) in &pkg.config.assets {
                    if let Ok(ref_str) = crate::extract_asset_ref_strict(asset_spec) {
                        closure.assets.insert((asset_url.clone(), ref_str));
                    }
                }
            } else if let Some((line, version)) = self.closure.iter().find(|(l, _)| l.path == url) {
                let version_str = version.to_string();
                closure
                    .remote_packages
                    .insert((url.clone(), version_str.clone()));
                let pkg_root = self.resolve_closure_package_root(&line.path, &version_str);
                if let Some(deps) = self.package_resolutions.get(&pkg_root) {
                    for dep_url in deps.keys() {
                        stack.push(dep_url.clone());
                    }
                }
            }
        }

        for (asset_path, asset_ref) in self.assets.keys() {
            closure
                .assets
                .insert((asset_path.clone(), asset_ref.clone()));
        }

        closure
    }
}

/// Transitive dependency closure for a package
#[derive(Debug, Clone, Default)]
pub struct PackageClosure {
    pub local_packages: HashSet<String>,
    pub remote_packages: HashSet<(String, String)>,
    pub assets: HashSet<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;
    use crate::workspace::MemberPackage;

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

        // Dummy resolver that doesn't resolve anything (stdlib should come from workspace member)
        struct NoOpResolver;
        impl PackagePathResolver for NoOpResolver {
            fn resolve_package(&self, _: &str, _: &str) -> Option<PathBuf> {
                None
            }
            fn resolve_asset(&self, _: &str, _: &str) -> Option<PathBuf> {
                None
            }
        }

        let file_provider = crate::DefaultFileProvider::default();
        let results = build_resolution_map(&file_provider, &NoOpResolver, &workspace, &closure);

        // The board's resolution map should include stdlib pointing to the fork
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
    fn test_package_roots_use_workspace_cache_path_for_unvendored_closure_packages() {
        let workspace_root = PathBuf::from("/workspace");
        let workspace = WorkspaceInfo {
            root: workspace_root.clone(),
            cache_dir: PathBuf::from("/Users/test/.pcb/cache"),
            config: None,
            packages: BTreeMap::new(),
            lockfile: None,
            errors: vec![],
        };

        let version = Version::parse("0.5.9").unwrap();
        let line = ModuleLine::new(STDLIB_MODULE_PATH.to_string(), &version);
        let mut closure = HashMap::new();
        closure.insert(line, version);

        let result = ResolutionResult {
            workspace_info: workspace,
            package_resolutions: HashMap::new(),
            closure,
            assets: HashMap::new(),
            lockfile_changed: false,
        };

        let roots = result.package_roots();
        assert_eq!(
            roots.get("github.com/diodeinc/stdlib@0.5.9"),
            Some(&workspace_root.join(".pcb/cache/github.com/diodeinc/stdlib/0.5.9")),
            "package_roots should use workspace-local cache path for unvendored packages"
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

        let result = ResolutionResult {
            workspace_info: workspace,
            package_resolutions: HashMap::new(),
            closure,
            assets: HashMap::new(),
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
