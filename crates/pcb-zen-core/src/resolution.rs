//! Shared V2 dependency resolution logic.
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

use crate::config::{
    extract_asset_ref, split_asset_repo_and_subpath, AssetDependencySpec, DependencySpec, Lockfile,
    PcbToml,
};
use crate::workspace::WorkspaceInfo;
use crate::FileProvider;

/// Trait for resolving package and asset paths.
pub trait PackagePathResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf>;
    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf>;
    fn exists(&self, path: &Path) -> bool;
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
    if let DependencySpec::Detailed(d) = spec {
        if let Some(path_str) = &d.path {
            return Some(base_dir.join(path_str));
        }
    }

    // 2. Workspace member
    if let Some(member) = workspace.packages.get(url) {
        return Some(member.dir.clone());
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
fn build_package_map<R: PackagePathResolver>(
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
        if let Some(ref_str) = extract_asset_ref(asset_spec) {
            if let Some(path) = resolver.resolve_asset(asset_key, &ref_str) {
                map.insert(asset_key.clone(), path);
            }
        }
    }

    map
}

/// Path resolver that only looks in the vendor directory.
///
/// Used by WASM where all dependencies must be pre-vendored in the zip.
pub struct VendoredPathResolver<F: FileProvider> {
    file_provider: F,
    /// Pre-computed package paths from lockfile: module_path -> vendored path
    package_paths: HashMap<String, PathBuf>,
    /// Pre-computed asset repo paths: (repo_url, ref) -> vendored repo root
    asset_repos: HashMap<(String, String), PathBuf>,
}

impl<F: FileProvider> VendoredPathResolver<F> {
    /// Create a new vendored path resolver from a lockfile.
    ///
    /// The lockfile contains two types of entries:
    /// - Packages (code dependencies): have a manifest_hash, stored at vendor/{module_path}/{version}
    /// - Assets (data files like KiCad libs): no manifest_hash, stored at vendor/{repo_url}/{version}/{subpath}
    pub fn from_lockfile(file_provider: F, vendor_dir: PathBuf, lockfile: &Lockfile) -> Self {
        let mut package_paths = HashMap::new();
        let mut asset_repos = HashMap::new();

        for entry in lockfile.iter() {
            if entry.manifest_hash.is_some() {
                let path = vendor_dir.join(&entry.module_path).join(&entry.version);
                if file_provider.exists(&path) {
                    package_paths.insert(entry.module_path.clone(), path);
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
            file_provider,
            package_paths,
            asset_repos,
        }
    }
}

impl<F: FileProvider> PackagePathResolver for VendoredPathResolver<F> {
    fn resolve_package(&self, module_path: &str, _version: &str) -> Option<PathBuf> {
        self.package_paths.get(module_path).cloned()
    }

    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf> {
        let (repo_url, subpath) = split_asset_repo_and_subpath(asset_key);
        let key = (repo_url.to_string(), ref_str.to_string());

        self.asset_repos
            .get(&key)
            .map(|repo_path| {
                if subpath.is_empty() {
                    repo_path.clone()
                } else {
                    repo_path.join(subpath)
                }
            })
            .or_else(|| self.package_paths.get(asset_key).cloned())
    }

    fn exists(&self, path: &Path) -> bool {
        self.file_provider.exists(path)
    }
}

/// Build the per-package resolution map.
///
/// Returns a map from package root path to (dependency URL -> resolved path).
pub fn build_resolution_map<R: PackagePathResolver>(
    workspace: &WorkspaceInfo,
    resolver: &R,
) -> HashMap<PathBuf, BTreeMap<String, PathBuf>> {
    let mut results = HashMap::new();

    // Build map for each member package (includes root if it's a package)
    for member in workspace.packages.values() {
        results.insert(
            member.dir.clone(),
            build_package_map(
                resolver,
                workspace,
                &member.dir,
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

    results
}

/// Add resolution maps for transitive (vendored) dependencies.
pub fn add_transitive_resolution_maps<F: FileProvider, R: PackagePathResolver>(
    file_provider: &F,
    resolver: &R,
    workspace: &WorkspaceInfo,
    results: &mut HashMap<PathBuf, BTreeMap<String, PathBuf>>,
) {
    // Collect initial paths to process
    let mut to_process: Vec<PathBuf> = results
        .values()
        .flat_map(|map| map.values())
        .filter(|path| !results.contains_key(*path) && resolver.exists(path))
        .cloned()
        .collect();

    let mut processed = HashSet::new();

    while let Some(pkg_path) = to_process.pop() {
        if !processed.insert(pkg_path.clone()) || results.contains_key(&pkg_path) {
            continue;
        }

        let pcb_toml_path = pkg_path.join("pcb.toml");
        let Ok(content) = file_provider.read_file(&pcb_toml_path) else {
            continue;
        };
        let Ok(config) = PcbToml::parse(&content) else {
            continue;
        };

        let map = build_package_map(
            resolver,
            workspace,
            &pkg_path,
            &config.dependencies,
            &config.assets,
        );

        // Queue newly discovered paths
        for path in map.values() {
            if !results.contains_key(path) && !processed.contains(path) {
                to_process.push(path.clone());
            }
        }

        results.insert(pkg_path, map);
    }
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
                if k_repo == repo_url && !k_subpath.is_empty() {
                    if let Some(repo_root) = path.parent() {
                        return Some(repo_root.join(subpath));
                    }
                }
            }
        }

        None
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryFileProvider;

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
}
