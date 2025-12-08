use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use pcb_ui::{Colorize, Style, StyledText};
use pcb_zen_core::config::{DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml};
use pcb_zen_core::resolution::{
    add_transitive_resolution_maps, build_resolution_map as shared_build_resolution_map,
    NativePathResolver, PackagePathResolver,
};
use pcb_zen_core::DefaultFileProvider;
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use std::time::Instant;

use crate::cache_index::{cache_base, ensure_bare_repo, CacheIndex};
use crate::canonical::{compute_content_hash_from_dir, compute_manifest_hash};
use crate::git;
use crate::workspace::{WorkspaceInfo, WorkspaceInfoExt};

/// Compute the semver family for a version
///
/// For 0.x versions, the minor version determines the family (0.2.x and 0.3.x are different families)
/// For 1.x+ versions, the major version determines the family
pub fn semver_family(v: &Version) -> String {
    if v.major == 0 {
        format!("v0.{}", v.minor)
    } else {
        format!("v{}", v.major)
    }
}

/// Module line identifier for MVS grouping
///
/// A module line represents a semver family:
/// - For v0.x: family is "v0.<minor>" (e.g., v0.2, v0.3 are different families)
/// - For v1.x+: family is "v<major>" (e.g., v1, v2, v3)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleLine {
    path: String,   // e.g., "github.com/diodeinc/stdlib"
    family: String, // e.g., "v0.3" or "v1"
}

impl ModuleLine {
    fn new(path: String, version: &Version) -> Self {
        ModuleLine {
            path,
            family: semver_family(version),
        }
    }
}

/// Dependency entry before resolution
#[derive(Debug, Clone)]
struct UnresolvedDep {
    url: String,
    spec: DependencySpec,
}

/// Path resolver that uses MVS family matching for package resolution.
///
/// This wraps a precomputed map of `url -> family -> path` and delegates
/// asset resolution to a base `NativePathResolver`.
struct MvsFamilyResolver {
    /// Precomputed package paths: url -> family -> absolute path
    families: HashMap<String, HashMap<String, PathBuf>>,
    /// Base resolver for assets and exists()
    base: NativePathResolver,
}

impl PackagePathResolver for MvsFamilyResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
        let families = self.families.get(module_path)?;
        let req_ver = parse_version_string(version).ok()?;
        let req_family = semver_family(&req_ver);

        families.get(&req_family).cloned().or_else(|| {
            // Fallback: if exactly one family exists, use it
            if families.len() == 1 {
                families.values().next().cloned()
            } else {
                None
            }
        })
    }

    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf> {
        self.base.resolve_asset(asset_key, ref_str)
    }

    fn exists(&self, path: &Path) -> bool {
        self.base.exists(path)
    }
}

/// Package manifest for a code package (dependencies + declared assets)
///
/// Only constructed from V2 pcb.toml files. Asset repositories themselves never have manifests.
#[derive(Clone, Debug)]
pub struct PackageManifest {
    dependencies: BTreeMap<String, DependencySpec>,
    assets: BTreeMap<String, pcb_zen_core::AssetDependencySpec>,
}

impl PackageManifest {
    fn from_v2(v2: &pcb_zen_core::config::PcbToml) -> Self {
        PackageManifest {
            dependencies: v2.dependencies.clone(),
            assets: v2.assets.clone(),
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct ResolutionResult {
    /// Map from Package Root (Absolute Path) -> Import URL -> Resolved Absolute Path
    pub package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    /// Package dependencies in the build closure: (module_path, version)
    pub closure: HashSet<(String, String)>,
    /// Asset dependencies: (module_path, ref) -> resolved_path
    pub assets: HashMap<(String, String), PathBuf>,
}

impl ResolutionResult {
    /// Print the dependency tree to stdout
    pub fn print_tree(&self, workspace_info: &WorkspaceInfo) {
        let workspace_name = workspace_info
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace");

        // Build version map from closure: module_path -> version
        let version_map: HashMap<&str, &str> = self
            .closure
            .iter()
            .map(|(path, version)| (path.as_str(), version.as_str()))
            .collect();

        // Collect root deps (direct deps from workspace packages)
        let mut root_deps: Vec<&str> = Vec::new();
        for pkg in workspace_info.packages.values() {
            for url in pkg.config.dependencies.keys() {
                if version_map.contains_key(url.as_str()) && !root_deps.contains(&url.as_str()) {
                    root_deps.push(url.as_str());
                }
            }
        }
        root_deps.sort();

        if root_deps.is_empty() {
            return;
        }

        // Build dep graph: url -> Vec<dep_urls> by reading pcb.toml from resolved paths
        let mut dep_graph: HashMap<String, Vec<String>> = HashMap::new();

        // Build graph from closure packages by reading their pcb.toml files
        for (path, _version) in &self.closure {
            if dep_graph.contains_key(path) {
                continue;
            }
            // Find resolved path for this package
            for deps in self.package_resolutions.values() {
                if let Some(resolved) = deps.get(path) {
                    let pcb_toml = resolved.join("pcb.toml");
                    if pcb_toml.exists() {
                        if let Ok(content) = std::fs::read_to_string(&pcb_toml) {
                            if let Ok(config) = PcbToml::parse(&content) {
                                let transitive: Vec<String> = config
                                    .dependencies
                                    .keys()
                                    .filter(|dep_url| version_map.contains_key(dep_url.as_str()))
                                    .cloned()
                                    .collect();
                                dep_graph.insert(path.clone(), transitive);
                            }
                        }
                    }
                    break;
                }
            }
        }

        // Track what we've printed to show (*)
        let mut printed = HashSet::new();

        // Helper to format package name: drop host (first segment), show rest
        let format_name = |url: &str| -> String {
            let parts: Vec<_> = url.split('/').collect();
            if parts.len() > 1 {
                parts[1..].join("/")
            } else {
                url.to_string()
            }
        };

        // Recursive print helper
        fn print_dep(
            url: &str,
            version_map: &HashMap<&str, &str>,
            dep_graph: &HashMap<String, Vec<String>>,
            printed: &mut HashSet<String>,
            prefix: &str,
            is_last: bool,
            format_name: &impl Fn(&str) -> String,
        ) {
            let branch = if is_last { "└── " } else { "├── " };
            let version = version_map.get(url).copied().unwrap_or("?");
            let already_printed = !printed.insert(url.to_string());

            println!(
                "{}{}{} v{}{}",
                prefix,
                branch,
                format_name(url),
                version,
                if already_printed { " (*)" } else { "" }
            );

            if already_printed {
                return;
            }

            if let Some(deps) = dep_graph.get(url) {
                let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                let mut sorted_deps: Vec<_> = deps.iter().map(|s| s.as_str()).collect();
                sorted_deps.sort();

                for (i, dep_url) in sorted_deps.iter().enumerate() {
                    let is_last_child = i == sorted_deps.len() - 1;
                    print_dep(
                        dep_url,
                        version_map,
                        dep_graph,
                        printed,
                        &child_prefix,
                        is_last_child,
                        format_name,
                    );
                }
            }
        }

        println!("{}", workspace_name);
        for (i, url) in root_deps.iter().enumerate() {
            let is_last = i == root_deps.len() - 1;
            print_dep(
                url,
                &version_map,
                &dep_graph,
                &mut printed,
                "",
                is_last,
                &format_name,
            );
        }
    }
}

/// Result of V2 vendoring operation
pub struct VendorResult {
    /// Number of packages vendored
    pub package_count: usize,
    /// Number of assets vendored
    pub asset_count: usize,
    /// Path to vendor directory
    pub vendor_dir: PathBuf,
}

/// V2 dependency resolution
///
/// Builds dependency graph using MVS, fetches dependencies,
/// and generates/updates the lockfile.
pub fn resolve_dependencies(
    workspace_info: &mut WorkspaceInfo,
    offline: bool,
) -> Result<ResolutionResult> {
    let workspace_root = workspace_info.root.clone();
    log::debug!(
        "V2 Dependency Resolution{}",
        if offline { " (offline)" } else { "" }
    );
    log::debug!("Workspace root: {}", workspace_root.display());

    // Phase -1: Auto-add missing dependencies from .zen files
    log::debug!("Phase -1: Auto-detecting dependencies from .zen files");
    let auto_deps = crate::auto_deps::auto_add_zen_deps(
        &workspace_root,
        &workspace_info.packages,
        workspace_info.lockfile.as_ref(),
        offline,
    )?;
    if auto_deps.total_added > 0 {
        log::debug!(
            "Auto-added {} dependencies across {} package(s)",
            auto_deps.total_added,
            auto_deps.packages_updated
        );
    }
    if auto_deps.discovered_remote > 0 {
        log::debug!(
            "Discovered {} remote package(s) via git tags",
            auto_deps.discovered_remote
        );
    }
    if auto_deps.versions_corrected > 0 {
        log::debug!(
            "Corrected {} workspace member version(s)",
            auto_deps.versions_corrected
        );
    }
    for (path, aliases) in &auto_deps.unknown_aliases {
        eprintln!(
            "{} {} has unknown aliases:",
            "warning:".with_style(Style::Yellow),
            path.display().to_string().bold()
        );
        for alias in aliases {
            eprintln!("    @{}", alias);
        }
    }
    for (path, urls) in &auto_deps.unknown_urls {
        eprintln!(
            "{} {} has unknown remote URLs:",
            "warning:".with_style(Style::Yellow),
            path.display().to_string().bold()
        );
        for url in urls {
            eprintln!("    {}", url);
        }
    }

    // Reload configs (auto-deps may have modified them)
    workspace_info.reload()?;

    // Validate patches are only at workspace root
    if let Some(config) = &workspace_info.config {
        if !config.patch.is_empty() && config.workspace.is_none() {
            anyhow::bail!(
                "[patch] section is only allowed at workspace root\n  \
                Found in non-workspace pcb.toml at: {}/pcb.toml\n  \
                Move [patch] to workspace root or remove it.",
                workspace_root.display()
            );
        }
    }

    log::debug!(
        "Workspace members: {} (for local resolution)",
        workspace_info.packages.len()
    );

    let patches = workspace_info
        .config
        .as_ref()
        .map(|c| c.patch.clone())
        .unwrap_or_default();

    // MVS state
    let mut selected: HashMap<ModuleLine, Version> = HashMap::new();
    let mut work_queue: VecDeque<ModuleLine> = VecDeque::new();
    let mut manifest_cache: HashMap<(ModuleLine, Version), PackageManifest> = HashMap::new();

    // Preseed from lockfile (opportunistic frontloading)
    // This allows Wave 1 to start fetching known deps immediately
    if let Some(lockfile) = &workspace_info.lockfile {
        for entry in lockfile.iter() {
            // Skip assets (no manifest_hash = asset)
            if entry.manifest_hash.is_none() {
                continue;
            }

            // Skip workspace members (resolved locally)
            if workspace_info.packages.contains_key(&entry.module_path) {
                continue;
            }

            // Parse version (skip invalid entries)
            let Ok(version) = Version::parse(&entry.version) else {
                continue;
            };

            let line = ModuleLine::new(entry.module_path.clone(), &version);

            // Only insert if not already selected (shouldn't happen, but defensive)
            if !selected.contains_key(&line) {
                log::debug!("Adding {}@v{} (from pcb.sum)", entry.module_path, version);
                selected.insert(line.clone(), version);
                work_queue.push_back(line);
            }
        }
    }

    log::debug!("Phase 0: Seed from workspace dependencies");

    // Resolve dependencies per-package
    let mut packages_with_deps = Vec::new();
    let mut packages_without_deps = 0;

    for pkg in workspace_info.packages.values() {
        let package_name = pkg
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".into());
        let pcb_toml_path = pkg.dir.join("pcb.toml");

        // Validate no patches in member packages (except root)
        if !pkg.config.patch.is_empty() && !pkg.rel_path.as_os_str().is_empty() {
            anyhow::bail!(
                "[patch] section is only allowed at workspace root\n  \
                Found in package: {}\n  \
                Location: {}\n  \
                Move [patch] to workspace root.",
                package_name,
                pcb_toml_path.display()
            );
        }

        // Collect this package's dependencies
        let package_deps = collect_package_dependencies(&pcb_toml_path, &pkg.config)
            .with_context(|| format!("in package '{}'", package_name))?;

        if package_deps.is_empty() {
            packages_without_deps += 1;
            continue;
        }

        packages_with_deps.push((package_name, package_deps));
    }

    // Print summary
    if packages_without_deps > 0 {
        log::debug!("  {} packages with no dependencies", packages_without_deps);
    }
    if !packages_with_deps.is_empty() {
        log::debug!("  {} packages with dependencies:", packages_with_deps.len());
        for (package_name, package_deps) in &packages_with_deps {
            log::debug!("    {} ({} deps)", package_name, package_deps.len());
        }
    }

    // Seed MVS state from direct dependencies
    for (_package_name, package_deps) in &packages_with_deps {
        for dep in package_deps {
            if let DependencySpec::Detailed(detail) = &dep.spec {
                if detail.path.is_some() {
                    continue;
                }
            }

            match resolve_to_version(
                &dep.url,
                &dep.spec,
                workspace_info.lockfile.as_ref(),
                offline,
            ) {
                Ok(version) => {
                    add_requirement(
                        dep.url.clone(),
                        version,
                        &mut selected,
                        &mut work_queue,
                        &patches,
                    );
                }
                Err(e) => {
                    eprintln!(
                        "{} failed to resolve {}: {}",
                        "warning:".with_style(Style::Yellow),
                        dep.url.as_str().bold(),
                        e
                    );
                }
            }
        }
    }

    log::debug!("Phase 1: Parallel dependency resolution");

    // Wave-based parallel fetching with MVS
    let phase1_start = Instant::now();
    let mut wave_num = 0;
    let mut total_fetched = 0;

    loop {
        // Collect current wave: packages in queue that haven't been fetched yet
        // Use a HashSet to dedupe - the same ModuleLine can appear multiple times
        // in the queue (e.g., added from lockfile then upgraded in Phase 0)
        let wave: Vec<_> = work_queue
            .drain(..)
            .filter_map(|line| {
                let version = selected.get(&line)?.clone();
                if manifest_cache.contains_key(&(line.clone(), version.clone())) {
                    None
                } else {
                    Some((line, version))
                }
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if wave.is_empty() {
            break;
        }

        wave_num += 1;
        let wave_start = Instant::now();
        log::debug!("  Wave {}: {} packages", wave_num, wave.len());

        // Parallel fetch all packages in this wave
        let results: Vec<_> = wave
            .par_iter()
            .map(|(line, version)| {
                let result = fetch_package(workspace_info, &line.path, version, offline);
                (line.clone(), version.clone(), result)
            })
            .collect();

        // Process results sequentially (MVS requires single-threaded updates)
        let mut new_deps = 0;
        for (line, version, result) in results {
            total_fetched += 1;
            match result {
                Ok(manifest) => {
                    manifest_cache.insert((line.clone(), version.clone()), manifest.clone());

                    for (dep_path, dep_spec) in &manifest.dependencies {
                        if is_non_version_dep(dep_spec) {
                            continue;
                        }

                        match resolve_to_version(
                            dep_path,
                            dep_spec,
                            workspace_info.lockfile.as_ref(),
                            offline,
                        ) {
                            Ok(dep_version) => {
                                let before = work_queue.len();
                                add_requirement(
                                    dep_path.clone(),
                                    dep_version,
                                    &mut selected,
                                    &mut work_queue,
                                    &patches,
                                );
                                if work_queue.len() > before {
                                    new_deps += 1;
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "{} failed to resolve {}: {}",
                                    "warning:".with_style(Style::Yellow),
                                    dep_path.as_str().bold(),
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{} failed to fetch {}@v{}: {}",
                        "warning:".with_style(Style::Yellow),
                        line.path.as_str().bold(),
                        version,
                        e
                    );
                }
            }
        }

        let wave_elapsed = wave_start.elapsed();
        if new_deps > 0 {
            log::debug!(
                "    Fetched in {:.1}s, discovered {} new dependencies",
                wave_elapsed.as_secs_f64(),
                new_deps
            );
        } else {
            log::debug!("    Fetched in {:.1}s", wave_elapsed.as_secs_f64());
        }
    }

    let phase1_elapsed = phase1_start.elapsed();
    log::debug!(
        "  Resolved {} packages in {} waves ({:.1}s)",
        total_fetched,
        wave_num,
        phase1_elapsed.as_secs_f64()
    );

    log::debug!("Phase 2: Build closure");

    // Phase 2: Build the final dependency set using only selected versions
    let closure = build_closure(&workspace_info.packages, &selected, &manifest_cache);

    log::debug!("Build set: {} dependencies", closure.len());

    // Phase 2.5: Collect and fetch assets
    log::debug!("Phase 2.5: Fetching assets");
    let asset_paths =
        collect_and_fetch_assets(workspace_info, &manifest_cache, &selected, offline)?;
    if !asset_paths.is_empty() {
        log::debug!("Fetched {} assets", asset_paths.len());
    } else {
        log::debug!("No assets");
    }

    // Phase 3: (Removed - sparse checkout and hashing now done in Phase 1)

    // Phase 4: Update lockfile with cryptographic hashes
    log::debug!("Phase 4: Lockfile");
    let (lockfile, added_count) = update_lockfile(workspace_info, &closure, &asset_paths)?;

    // Only write lockfile to disk if new entries were added
    if added_count > 0 {
        let lockfile_path = workspace_root.join("pcb.sum");
        std::fs::write(&lockfile_path, lockfile.to_string())?;
        log::debug!("  Updated {}", lockfile_path.display());
    }

    log::debug!("V2 dependency resolution complete");

    let package_resolutions =
        build_resolution_map(workspace_info, &selected, &patches, &asset_paths, offline)?;

    // Convert closure to (module_path, version) pairs
    let closure_set: HashSet<_> = closure
        .iter()
        .map(|(line, version)| (line.path.clone(), version.to_string()))
        .collect();

    Ok(ResolutionResult {
        package_resolutions,
        closure: closure_set,
        assets: asset_paths,
    })
}

/// Vendor dependencies from cache to vendor directory
///
/// Vendors entries matching workspace.vendor patterns plus any additional_patterns.
/// No-op if combined patterns is empty. Incremental - skips existing entries.
///
/// If `target_vendor_dir` is provided, vendors to that directory instead of
/// `workspace_info.root/vendor`. This is used by `pcb release` to vendor into
/// the staging directory.
pub fn vendor_deps(
    workspace_info: &WorkspaceInfo,
    resolution: &ResolutionResult,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
) -> Result<VendorResult> {
    let vendor_dir = target_vendor_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_info.root.join("vendor"));

    // Combine workspace.vendor patterns with additional patterns
    let mut patterns: Vec<&str> = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.workspace.as_ref())
        .map(|w| w.vendor.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    patterns.extend(additional_patterns.iter().map(|s| s.as_str()));

    // No patterns = no-op
    if patterns.is_empty() {
        return Ok(VendorResult {
            package_count: 0,
            asset_count: 0,
            vendor_dir,
        });
    }

    let cache = cache_base();
    let workspace_vendor = workspace_info.root.join("vendor");

    // Build glob matcher
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        builder.add(Glob::new(pattern)?);
    }
    let glob_set = builder.build()?;

    // Create vendor directory if needed
    fs::create_dir_all(&vendor_dir)?;

    // Copy matching packages from workspace vendor or cache (vendor takes precedence)
    let mut package_count = 0;
    for (module_path, version) in &resolution.closure {
        if !glob_set.is_match(module_path) {
            continue;
        }
        let vendor_src = workspace_vendor.join(module_path).join(version);
        let cache_src = cache.join(module_path).join(version);
        let src = if vendor_src.exists() {
            vendor_src
        } else {
            cache_src
        };
        let dst = vendor_dir.join(module_path).join(version);
        if src.exists() && !dst.exists() {
            copy_dir_all(&src, &dst)?;
            package_count += 1;
        }
    }

    // Copy matching assets from workspace vendor or cache (handling subpaths)
    let mut asset_count = 0;
    for (asset_key, ref_str) in resolution.assets.keys() {
        if !glob_set.is_match(asset_key) {
            continue;
        }

        // Split asset_key into (repo_url, subpath) for proper cache/vendor paths
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);

        // Source: check workspace vendor first, then cache
        let vendor_src = if subpath.is_empty() {
            workspace_vendor.join(repo_url).join(ref_str)
        } else {
            workspace_vendor.join(repo_url).join(ref_str).join(subpath)
        };
        let cache_src = if subpath.is_empty() {
            cache.join(repo_url).join(ref_str)
        } else {
            cache.join(repo_url).join(ref_str).join(subpath)
        };
        let src = if vendor_src.exists() {
            vendor_src
        } else {
            cache_src
        };

        // Destination: vendor/{repo}/{ref}/{subpath}
        let dst = if subpath.is_empty() {
            vendor_dir.join(repo_url).join(ref_str)
        } else {
            vendor_dir.join(repo_url).join(ref_str).join(subpath)
        };

        if src.exists() && !dst.exists() {
            // Create parent directory
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }

            // Copy file or directory as appropriate
            if src.is_file() {
                fs::copy(&src, &dst)?;
            } else {
                copy_dir_all(&src, &dst)?;
            }
            asset_count += 1;
        }
    }

    Ok(VendorResult {
        package_count,
        asset_count,
        vendor_dir,
    })
}

/// Recursively copy a directory, excluding .git
fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        if entry.file_type()?.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Build the per-package resolution map
///
/// When offline=true, only includes paths from workspace members, patches, and vendor/
/// (never ~/.pcb/cache). This ensures offline builds fail if dependencies aren't vendored.
///
/// Uses MVS family matching for package resolution and delegates to shared resolution
/// logic for the actual map building.
fn build_resolution_map(
    workspace_info: &WorkspaceInfo,
    selected: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
    asset_paths: &HashMap<(String, String), PathBuf>,
    offline: bool,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let cache = cache_base();
    let vendor = workspace_info.root.join("vendor");

    // Build patch map (patches override remote deps with local paths)
    let patches: HashMap<String, PathBuf> = patches
        .iter()
        .map(|(url, patch)| (url.clone(), workspace_info.root.join(&patch.path)))
        .collect();

    // Create base resolver for package path lookups
    // Note: workspace members are handled directly in shared_build_resolution_map
    let base_resolver = NativePathResolver {
        vendor_dir: vendor.clone(),
        cache_dir: cache.clone(),
        offline,
        patches,
        asset_paths: asset_paths.clone(),
    };

    // Build the families map for MVS family matching
    let mut families: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    for (line, version) in selected {
        let version_str = version.to_string();
        if let Some(abs_path) = base_resolver.resolve_package(&line.path, &version_str) {
            families
                .entry(line.path.clone())
                .or_default()
                .insert(line.family.clone(), abs_path);
        }
    }

    // Create the MVS family resolver that wraps the base for assets
    let resolver = MvsFamilyResolver {
        families,
        base: base_resolver,
    };

    // Use shared resolution logic for root + workspace members
    let mut results = shared_build_resolution_map(workspace_info, &resolver);

    // Add transitive dependencies using shared logic
    let file_provider = DefaultFileProvider::default();
    add_transitive_resolution_maps(&file_provider, &resolver, workspace_info, &mut results);

    Ok(results)
}

/// Collect dependencies for a package and transitive local deps
fn collect_package_dependencies(
    pcb_toml_path: &Path,
    v2_config: &pcb_zen_core::config::PcbToml,
) -> Result<Vec<UnresolvedDep>> {
    let package_dir = pcb_toml_path.parent().unwrap();
    let mut deps = HashMap::new();

    collect_deps_recursive(&v2_config.dependencies, package_dir, &mut deps)
        .with_context(|| format!("in {}", pcb_toml_path.display()))?;

    Ok(deps.into_values().collect())
}

/// Recursively collect dependencies, handling transitive local path dependencies
fn collect_deps_recursive(
    current_deps: &BTreeMap<String, DependencySpec>,
    package_dir: &Path,
    deps: &mut HashMap<String, UnresolvedDep>,
) -> Result<()> {
    for (url, spec) in current_deps {
        // Skip if already seen
        if deps.contains_key(url) {
            continue;
        }

        // Check if it's a local path dependency
        let (local_path, _expected_version) = match spec {
            DependencySpec::Detailed(detail) if detail.path.is_some() => {
                let path = detail.path.as_ref().unwrap();
                if detail.version.is_none() {
                    anyhow::bail!(
                        "Path dependency '{}' must specify a version\n  Example: {{ path = \"{}\", version = \"1.0.0\" }}",
                        url,
                        path
                    );
                }
                (path, detail.version.as_ref())
            }
            _ => {
                // Not a local path dep, just add it
                deps.insert(
                    url.clone(),
                    UnresolvedDep {
                        url: url.clone(),
                        spec: spec.clone(),
                    },
                );
                continue;
            }
        };

        // Resolve local path relative to package directory
        let resolved_path = package_dir.join(local_path);

        if !resolved_path.exists() {
            anyhow::bail!(
                "Local path dependency '{}' not found at {}",
                url,
                resolved_path.display()
            );
        }

        // Add this local path dep
        deps.insert(
            url.clone(),
            UnresolvedDep {
                url: url.clone(),
                spec: spec.clone(),
            },
        );

        // Recursively resolve transitive dependencies
        let dep_pcb_toml = resolved_path.join("pcb.toml");
        if !dep_pcb_toml.exists() {
            continue;
        }

        let file_provider = DefaultFileProvider::new();
        let dep_config = PcbToml::from_file(&file_provider, &dep_pcb_toml)?;
        collect_deps_recursive(&dep_config.dependencies, &resolved_path, deps)
            .with_context(|| format!("in {}", dep_pcb_toml.display()))?;
    }

    Ok(())
}

/// Check if a dependency spec is a local path dependency
///
/// Branch/rev deps are resolved to concrete versions in Phase 0/1 and stored in `selected`,
/// so they should participate in the build closure like regular version deps.
/// Only true local path deps should be excluded from remote fetching.
fn is_non_version_dep(spec: &DependencySpec) -> bool {
    match spec {
        DependencySpec::Detailed(detail) => {
            // Only skip true local path deps. Branch/rev are resolved to
            // concrete versions in `selected`, so they should participate
            // in the build closure.
            detail.path.is_some()
        }
        DependencySpec::Version(_) => false,
    }
}

/// Extract version from dependency spec (simple parser, doesn't resolve branches)
///
/// This is used in Phase 2 build closure to reconstruct ModuleLines.
/// For branches/revs, returns a placeholder - the actual version comes from the selected map.
/// Parse version string, handling different formats
fn parse_version_string(s: &str) -> Result<Version> {
    let s = s.trim_start_matches('^').trim_start_matches('v');

    // Try parsing as full semver
    if let Ok(v) = Version::parse(s) {
        return Ok(v);
    }

    // Try parsing as major.minor (e.g., "0.3" → "0.3.0")
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        1 => Ok(Version::new(parts[0].parse()?, 0, 0)),
        2 => Ok(Version::new(parts[0].parse()?, parts[1].parse()?, 0)),
        _ => anyhow::bail!("Invalid version string: {}", s),
    }
}

/// Collect and fetch all assets from workspace packages and transitive manifests
///
/// Returns map of (module_path, ref) -> resolved_path for all fetched assets
fn collect_and_fetch_assets(
    workspace_info: &WorkspaceInfo,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
    selected: &HashMap<ModuleLine, Version>,
    offline: bool,
) -> Result<HashMap<(String, String), PathBuf>> {
    // Collect all (asset_key, ref) pairs from workspace + transitive deps
    let mut all_assets: Vec<(String, String)> = Vec::new();

    for pkg in workspace_info.packages.values() {
        for (path, spec) in &pkg.config.assets {
            if let Ok(ref_str) = pcb_zen_core::extract_asset_ref_strict(spec) {
                all_assets.push((path.clone(), ref_str));
            }
        }
    }

    for (line, version) in selected {
        if let Some(manifest) = manifest_cache.get(&(line.clone(), version.clone())) {
            for (path, spec) in &manifest.assets {
                if let Ok(ref_str) = pcb_zen_core::extract_asset_ref_strict(spec) {
                    all_assets.push((path.clone(), ref_str));
                }
            }
        }
    }

    if all_assets.is_empty() {
        return Ok(HashMap::new());
    }

    // Dedupe assets by (asset_key, ref)
    let unique_assets: HashSet<_> = all_assets.into_iter().collect();

    // Dedupe repos by (repo_url, ref) - multiple subpaths share the same repo
    let mut repos_to_fetch: HashSet<(String, String)> = HashSet::new();
    for (asset_key, ref_str) in &unique_assets {
        let (repo_url, _) = git::split_asset_repo_and_subpath(asset_key);
        repos_to_fetch.insert((repo_url.to_string(), ref_str.clone()));
    }

    // Print repos we're fetching
    for (repo_url, ref_str) in &repos_to_fetch {
        log::debug!("  {}@{}", repo_url, ref_str);
    }

    // Fetch repos in parallel
    let errors: Vec<_> = repos_to_fetch
        .par_iter()
        .filter_map(|(repo_url, ref_str)| {
            fetch_asset_repo(workspace_info, repo_url, ref_str, offline)
                .err()
                .map(|e| format!("{}@{}: {}", repo_url, ref_str, e))
        })
        .collect();

    if !errors.is_empty() {
        anyhow::bail!("Failed to fetch assets:\n  {}", errors.join("\n  "));
    }

    // Hash and index each subpath in parallel, build result map
    let cache = cache_base();
    let results: Vec<_> = unique_assets
        .par_iter()
        .filter_map(|(asset_key, ref_str)| {
            let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);
            let target_path = cache.join(repo_url).join(ref_str).join(subpath);

            if !target_path.exists() {
                log::warn!("Asset subpath not found: {}", asset_key);
                return None;
            }

            // Index if not already indexed
            match CacheIndex::open() {
                Ok(index) => {
                    if index.get_asset(repo_url, subpath, ref_str).is_none() {
                        match compute_content_hash_from_dir(&target_path) {
                            Ok(hash) => {
                                if let Err(e) = index.set_asset(repo_url, subpath, ref_str, &hash) {
                                    log::warn!("Failed to index {}: {}", asset_key, e);
                                }
                            }
                            Err(e) => log::warn!("Failed to hash {}: {}", asset_key, e),
                        }
                    }
                }
                Err(e) => log::warn!("Failed to open cache index: {}", e),
            }

            Some(((asset_key.clone(), ref_str.clone()), target_path))
        })
        .collect();

    Ok(results.into_iter().collect())
}

/// Fetch a package from Git using sparse checkout
///
/// Fetches all package files, computes content/manifest hashes, and caches locally.
/// Returns the package manifest for dependency resolution.
///
/// Resolution order:
/// 1. Workspace members (always)
/// 2. Patches (always)
/// 3. Vendor directory (always)
/// 4. Cache (only if !offline)
/// 5. Network fetch (only if !offline)
fn fetch_package(
    workspace_info: &WorkspaceInfo,
    module_path: &str,
    version: &Version,
    offline: bool,
) -> Result<PackageManifest> {
    // 1. Workspace member override (highest priority)
    if let Some(member_pkg) = workspace_info.packages.get(module_path) {
        let member_toml = member_pkg.dir.join("pcb.toml");
        return read_manifest_from_path(&member_toml);
    }

    // 2. Check if this module is patched with a local path
    if let Some(patch) = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.patch.get(module_path))
    {
        let patched_path = workspace_info.root.join(&patch.path);
        let patched_toml = patched_path.join("pcb.toml");

        if !patched_toml.exists() {
            anyhow::bail!("Patch path {} has no pcb.toml", patched_path.display());
        }

        return read_manifest_from_path(&patched_toml);
    }

    // 3. Check vendor directory (before cache - vendor is the committed source of truth)
    let vendor_dir = workspace_info
        .root
        .join("vendor")
        .join(module_path)
        .join(version.to_string());
    let vendor_toml = vendor_dir.join("pcb.toml");
    if vendor_toml.exists() {
        return read_manifest_from_path(&vendor_toml);
    }

    // 4. If offline, fail here - vendor is the only allowed source for offline builds
    if offline {
        anyhow::bail!(
            "Package {} v{} not vendored (offline mode)\n  \
            Run `pcb vendor` to vendor dependencies for offline builds",
            module_path,
            version
        );
    }

    // 5. Check cache directory: ~/.pcb/cache/{module_path}/{version}/
    let cache = cache_base();
    let checkout_dir = cache.join(module_path).join(version.to_string());
    let version_str = version.to_string();

    // Open cache index for this thread
    let index = CacheIndex::open()?;

    // Fast path: index entry exists AND directory exists = valid cache
    if index.get_package(module_path, &version_str).is_some() && checkout_dir.exists() {
        let pcb_toml_path = checkout_dir.join("pcb.toml");
        return read_manifest_from_path(&pcb_toml_path);
    }

    // Slow path: fetch via sparse checkout (network)
    let package_root = ensure_sparse_checkout(&checkout_dir, module_path, &version_str, true)?;
    let pcb_toml_path = package_root.join("pcb.toml");

    // Compute hashes
    let content_hash = compute_content_hash_from_dir(&package_root)?;
    let manifest_content = std::fs::read_to_string(&pcb_toml_path)?;
    let manifest_hash = compute_manifest_hash(&manifest_content);

    // Verify against expected hashes from git tag
    verify_tag_hashes(
        &checkout_dir,
        module_path,
        version,
        &content_hash,
        &manifest_hash,
    )?;

    // Store hashes in index
    index.set_package(module_path, &version_str, &content_hash, &manifest_hash)?;

    // Read the manifest
    read_manifest_from_path(&pcb_toml_path)
}

/// Read and parse a pcb.toml manifest (both dependencies and assets)
fn read_manifest_from_path(pcb_toml_path: &Path) -> Result<PackageManifest> {
    let content = std::fs::read_to_string(pcb_toml_path)?;
    let config = PcbToml::parse(&content)?;

    match config {
        config if config.is_v2() => Ok(PackageManifest::from_v2(&config)),
        _ => {
            // V1 packages = empty manifest for V2 resolution
            Ok(PackageManifest {
                dependencies: BTreeMap::new(),
                assets: BTreeMap::new(),
            })
        }
    }
}

/// Fetch an asset repository (no pcb.toml, leaf node, no transitive deps)
///
/// Assets:
/// - Must NOT have a pcb.toml manifest
/// - Are leaf nodes (no transitive dependencies)
/// - Don't participate in MVS (each ref is isolated)
/// - Version/ref used literally as git tag (no v-prefix logic)
///
/// The asset_key may include a subpath (e.g., "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty").
/// The full repo is cached at ~/.pcb/cache/{repo}/{ref}/, but only the subpath is vendored and returned.
///
/// Resolution order:
/// 1. Patches (use asset_key for lookup)
/// 2. Vendor directory: vendor/{repo}/{ref}/{subpath}/
/// 3. Cache: cache/{repo}/{ref}/, return subpath within it
/// 4. Network fetch (only if !offline)
fn fetch_asset_repo(
    workspace_info: &WorkspaceInfo,
    asset_key: &str,
    ref_str: &str,
    offline: bool,
) -> Result<PathBuf> {
    let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);

    // 1. Check if this asset is patched with a local path (use full asset_key for lookup)
    if let Some(patch) = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.patch.get(asset_key))
    {
        let patched_path = workspace_info.root.join(&patch.path);

        log::debug!("Asset {} using patched source: {}", asset_key, patch.path);

        if !patched_path.exists() {
            anyhow::bail!(
                "Asset '{}' is patched to a non-existent path\n  \
                Patch path: {}",
                asset_key,
                patched_path.display()
            );
        }

        return Ok(patched_path);
    }

    // Open cache index
    let index = CacheIndex::open()?;

    // 2. Check vendor directory: vendor/{repo}/{ref}/{subpath}/
    let vendor_base = workspace_info
        .root
        .join("vendor")
        .join(repo_url)
        .join(ref_str);
    let vendor_dir = if subpath.is_empty() {
        vendor_base.clone()
    } else {
        vendor_base.join(subpath)
    };

    if vendor_dir.exists() && index.get_asset(repo_url, subpath, ref_str).is_some() {
        log::debug!("Asset {}@{} vendored", asset_key, ref_str);
        return Ok(vendor_dir);
    }

    // 3. If offline, fail here - vendor is the only allowed source for offline builds
    if offline {
        anyhow::bail!(
            "Asset {} @ {} not vendored (offline mode)\n  \
            Run `pcb vendor` to vendor dependencies for offline builds",
            asset_key,
            ref_str
        );
    }

    // 4. Check cache: full repo at cache/{repo}/{ref}/, target is subpath within it
    let cache = cache_base();
    let repo_cache_dir = cache.join(repo_url).join(ref_str);
    let target_path = if subpath.is_empty() {
        repo_cache_dir.clone()
    } else {
        repo_cache_dir.join(subpath)
    };

    // Check if subpath already indexed and exists in cache
    if index.get_asset(repo_url, subpath, ref_str).is_some() && target_path.exists() {
        log::debug!("Asset {}@{} cached", asset_key, ref_str);
        return Ok(target_path);
    }

    // 5. Ensure base repo is fetched (archive download or sparse checkout)
    // Check for .git (git source) or any content like pcb.toml (archive source)
    let repo_exists = repo_cache_dir.join(".git").exists()
        || (repo_cache_dir.exists()
            && std::fs::read_dir(&repo_cache_dir).is_ok_and(|mut d| d.next().is_some()));
    if !repo_exists {
        log::debug!("Asset {}@{} fetching", asset_key, ref_str);
        ensure_sparse_checkout(&repo_cache_dir, repo_url, ref_str, false)?;
    }

    // Verify subpath exists in the cloned repo
    if !subpath.is_empty() && !target_path.exists() {
        anyhow::bail!(
            "Subpath '{}' not found in {}@{}",
            subpath,
            repo_url,
            ref_str
        );
    }

    // Compute and store content hash on subpath only
    let content_hash = compute_content_hash_from_dir(&target_path)?;
    index.set_asset(repo_url, subpath, ref_str, &content_hash)?;
    log::debug!("Asset {}@{} hashed: {}", asset_key, ref_str, content_hash);

    Ok(target_path)
}

/// Build the final dependency closure using selected versions
///
/// DFS from workspace package dependencies using selected versions.
/// Returns the set of (ModuleLine, Version) pairs in the build closure.
fn build_closure(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> HashSet<(ModuleLine, Version)> {
    let mut build_set = HashSet::new();
    let mut stack = Vec::new();

    // Build index: module_path → ModuleLine for fast lookups (excluding workspace members)
    let mut line_by_path: HashMap<String, Vec<ModuleLine>> = HashMap::new();
    for line in selected.keys() {
        // Skip workspace members - they don't need to be fetched
        if packages.contains_key(&line.path) {
            continue;
        }
        line_by_path
            .entry(line.path.clone())
            .or_default()
            .push(line.clone());
    }

    // Seed DFS from all package dependencies
    for pkg in packages.values() {
        for (url, spec) in &pkg.config.dependencies {
            if !is_non_version_dep(spec) {
                if let Some(lines) = line_by_path.get(url) {
                    stack.extend(lines.iter().cloned());
                }
            }
        }
    }

    // DFS using final selected versions
    while let Some(line) = stack.pop() {
        let version = match selected.get(&line) {
            Some(v) => v.clone(),
            None => continue,
        };

        if build_set.contains(&(line.clone(), version.clone())) {
            continue;
        }

        build_set.insert((line.clone(), version.clone()));

        // Follow transitive dependencies via selected versions
        if let Some(manifest) = manifest_cache.get(&(line.clone(), version)) {
            for (dep_path, dep_spec) in &manifest.dependencies {
                if !is_non_version_dep(dep_spec) {
                    if let Some(lines) = line_by_path.get(dep_path) {
                        stack.extend(lines.iter().cloned());
                    }
                }
            }
        }
    }

    build_set
}

/// Resolve a dependency spec to a concrete version
///
/// Handles:
/// - Exact versions: "0.3.2" → v0.3.2
/// - Branches: { branch = "main" } → pseudo-version (uses lockfile if available)
/// - Revisions: { rev = "abcd1234" } → pseudo-version (uses lockfile if available)
///
/// When offline=true, branch/rev specs MUST have a locked version in pcb.sum.
/// Network access (git ls-remote) is not allowed in offline mode.
fn resolve_to_version(
    module_path: &str,
    spec: &DependencySpec,
    lockfile: Option<&Lockfile>,
    offline: bool,
) -> Result<Version> {
    match spec {
        DependencySpec::Version(v) => parse_version_string(v),
        DependencySpec::Detailed(detail) => {
            if let Some(version) = &detail.version {
                parse_version_string(version)
            } else if let Some(branch) = &detail.branch {
                // Use locked pseudo-version if available (skip git ls-remote)
                if let Some(entry) = lockfile.and_then(|lf| lf.find_by_path(module_path)) {
                    if let Ok(locked_version) = Version::parse(&entry.version) {
                        if locked_version.pre.starts_with("0.") {
                            // It's a pseudo-version, use it
                            log::debug!("        Using locked v{} (from pcb.sum)", locked_version);
                            return Ok(locked_version);
                        }
                    }
                }
                // No lockfile entry - need network access
                if offline {
                    anyhow::bail!(
                        "Branch '{}' for {} requires network access (offline mode)\n  \
                        Add to pcb.sum first by running online, then use --offline",
                        branch,
                        module_path
                    );
                }
                resolve_branch_to_pseudo_version(module_path, branch)
            } else if let Some(rev) = &detail.rev {
                // Use locked pseudo-version if available (skip git ls-remote)
                if let Some(entry) = lockfile.and_then(|lf| lf.find_by_path(module_path)) {
                    if let Ok(locked_version) = Version::parse(&entry.version) {
                        if locked_version.pre.starts_with("0.") {
                            // It's a pseudo-version, use it
                            log::debug!("        Using locked v{} (from pcb.sum)", locked_version);
                            return Ok(locked_version);
                        }
                    }
                }
                // No lockfile entry - need network access
                if offline {
                    anyhow::bail!(
                        "Rev '{}' for {} requires network access (offline mode)\n  \
                        Add to pcb.sum first by running online, then use --offline",
                        &rev[..8.min(rev.len())],
                        module_path
                    );
                }
                resolve_rev_to_pseudo_version(module_path, rev)
            } else {
                anyhow::bail!("Dependency has no version, branch, or rev")
            }
        }
    }
}

/// Resolve a Git branch to a pseudo-version
fn resolve_branch_to_pseudo_version(module_path: &str, branch: &str) -> Result<Version> {
    let (repo_url, _) = git::split_repo_and_subpath(module_path);
    let index = CacheIndex::open()?;

    // Check branch cache first
    let commit = if let Some(cached_commit) = index.get_branch_commit(repo_url, branch) {
        cached_commit
    } else {
        log::debug!(
            "        Resolving branch '{}' for {}...",
            branch,
            module_path
        );
        let refspec = format!("refs/heads/{}", branch);
        let (commit, _) = git::ls_remote_with_fallback(module_path, &refspec)?;
        let _ = index.set_branch_commit(repo_url, branch, &commit);
        commit
    };

    generate_pseudo_version_for_commit(module_path, &commit)
}

/// Resolve a Git revision to a pseudo-version
fn resolve_rev_to_pseudo_version(module_path: &str, rev: &str) -> Result<Version> {
    log::debug!(
        "        Resolving rev '{}' for {}...",
        &rev[..8.min(rev.len())],
        module_path
    );

    generate_pseudo_version_for_commit(module_path, rev)
}

/// Generate a pseudo-version for a Git commit
///
/// Format: v<base>-0.<timestamp>-<commit_short>
/// Base version is derived from latest reachable tag, or v0.0.0 if none
///
/// Uses bare repos (shared with remote package discovery) and caches
/// commit metadata in SQLite to avoid redundant git operations.
fn generate_pseudo_version_for_commit(module_path: &str, commit: &str) -> Result<Version> {
    let (repo_url, _) = git::split_repo_and_subpath(module_path);
    let index = CacheIndex::open()?;

    // Get from cache or compute and insert
    let (timestamp, base_tag) = match index.get_commit_metadata(repo_url, commit) {
        Some(cached) => cached,
        None => {
            let bare_dir = ensure_bare_repo(repo_url)?;
            let base_tag = git::describe_tags(&bare_dir, commit);
            let timestamp = git::show_commit_timestamp(&bare_dir, commit).unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64
            });
            let _ = index.set_commit_metadata(repo_url, commit, timestamp, base_tag.as_deref());
            (timestamp, base_tag)
        }
    };

    let base_version = base_tag
        .and_then(|tag| parse_version_string(&tag).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0));

    build_pseudo_version(&base_version, timestamp, commit)
}

fn build_pseudo_version(base_version: &Version, timestamp: i64, commit: &str) -> Result<Version> {
    // Increment patch version
    let pseudo_base = Version::new(
        base_version.major,
        base_version.minor,
        base_version.patch + 1,
    );

    // Format timestamp as YYYYMMDDhhmmss
    let dt = jiff::Timestamp::from_second(timestamp)?;
    let timestamp_str = dt.strftime("%Y%m%d%H%M%S").to_string();

    // Use full commit hash (40 chars) in pseudo-version for reliable fetching
    let commit_hash = &commit[..commit.len().min(40)];

    // Build pseudo-version string: v<major>.<minor>.<patch+1>-0.<timestamp>-<commit>
    let pseudo_str = format!(
        "{}.{}.{}-0.{}-{}",
        pseudo_base.major, pseudo_base.minor, pseudo_base.patch, timestamp_str, commit_hash
    );

    Version::parse(&pseudo_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse pseudo-version {}: {}", pseudo_str, e))
}

/// Add a requirement to the MVS state (monotonic upgrade)
///
/// Patches are checked here - they override version selection with ultimate authority.
fn add_requirement(
    path: String,
    version: Version,
    selected: &mut HashMap<ModuleLine, Version>,
    work_queue: &mut VecDeque<ModuleLine>,
    patches: &BTreeMap<String, pcb_zen_core::config::PatchSpec>,
) {
    // Check if this module is patched
    let (final_version, is_patched) = if patches.contains_key(&path) {
        // Patch overrides version selection
        // For local path patches, use the requested version as identity
        // (the path is just where we get the code from)
        (version, true)
    } else {
        (version, false)
    };

    let line = ModuleLine::new(path.clone(), &final_version);

    let needs_update = match selected.get(&line) {
        None => true,
        Some(current) => final_version > *current,
    };

    if needs_update {
        let action = if selected.contains_key(&line) {
            "Upgrading"
        } else {
            "Adding"
        };
        let suffix = if is_patched { " (patched)" } else { "" };
        log::debug!("  → {} {}@v{}{}", action, path, final_version, suffix);

        selected.insert(line.clone(), final_version);
        work_queue.push_back(line);
    }
}

/// Verify computed hashes match the expected hashes from the git tag annotation
fn verify_tag_hashes(
    checkout_dir: &Path,
    module_path: &str,
    version: &Version,
    content_hash: &str,
    manifest_hash: &str,
) -> Result<()> {
    // Read tag annotation from FETCH_HEAD (the fetched tag object)
    let Some(tag_body) = git::cat_file_fetch_head(checkout_dir) else {
        return Ok(());
    };

    let Some((expected_content, expected_manifest)) = parse_hashes_from_tag_body(&tag_body) else {
        return Ok(());
    };

    fn check_hash(
        kind: &str,
        computed: &str,
        expected: &str,
        module_path: &str,
        version: &Version,
    ) -> Result<()> {
        if computed != expected {
            anyhow::bail!(
                "{} hash mismatch for {}@v{}\n  \
                Expected (from tag): {}\n  \
                Computed:            {}\n\n\
                This may indicate a bug in the packaging toolchain.",
                kind,
                module_path,
                version,
                expected,
                computed
            );
        }
        Ok(())
    }

    check_hash(
        "Content",
        content_hash,
        &expected_content,
        module_path,
        version,
    )?;
    check_hash(
        "Manifest",
        manifest_hash,
        &expected_manifest,
        module_path,
        version,
    )?;

    Ok(())
}

/// Parse content and manifest hashes from tag annotation body
fn parse_hashes_from_tag_body(body: &str) -> Option<(String, String)> {
    let mut content_hash = None;
    let mut manifest_hash = None;

    for line in body.lines() {
        let line = line.trim();
        if let Some(hash_start) = line.find(" h1:") {
            let hash = line[hash_start + 1..].to_string();
            if line[..hash_start].ends_with("/pcb.toml") {
                manifest_hash = Some(hash);
            } else {
                content_hash = Some(hash);
            }
        }
    }

    content_hash.zip(manifest_hash)
}

/// Ensure sparse-checkout working tree for a module at specific version
///
/// Uses Git sparse-checkout to only materialize the subdirectory for nested packages.
///
/// Cache structure:
/// - Root packages: `~/.pcb/cache/github.com/user/repo/{version}/` (package root = cache dir)
/// - Nested packages: `~/.pcb/cache/github.com/user/repo/components/part/{version}/` (package root = cache dir, contents moved up)
///
/// Returns the package root path (where pcb.toml lives)
fn ensure_sparse_checkout(
    checkout_dir: &Path,
    module_path: &str,
    version_str: &str,
    add_v_prefix: bool,
) -> Result<PathBuf> {
    let (repo_url, subpath) = git::split_repo_and_subpath(module_path);
    let is_pseudo_version = version_str.contains("-0.");

    // Construct ref_spec (tag name or commit hash)
    let ref_spec = if is_pseudo_version {
        // Extract commit hash from pseudo-version (last segment after final -)
        version_str.rsplit('-').next().unwrap().to_string()
    } else if add_v_prefix {
        // Code deps: add v-prefix for semver tags
        if subpath.is_empty() {
            format!("v{}", version_str)
        } else {
            format!("{}/v{}", subpath, version_str)
        }
    } else {
        // Assets: use ref literally
        if subpath.is_empty() {
            version_str.to_string()
        } else {
            format!("{}/{}", subpath, version_str)
        }
    };

    // If directory already exists with content, assume checkout is done (cache hit)
    // Check for pcb.toml or any file as marker (works for both git and archive sources)
    if checkout_dir.exists()
        && (checkout_dir.join(".git").exists() || checkout_dir.join("pcb.toml").exists())
    {
        return Ok(checkout_dir.to_path_buf());
    }

    // Try HTTP archive download first for supported hosts (faster than git)
    // Skip for pseudo-versions (commit hashes) and nested packages with subpaths
    if !is_pseudo_version && subpath.is_empty() {
        let host = repo_url.split('/').next().unwrap_or("");
        if let Some((url_pattern, root_pattern)) = crate::archive::get_archive_pattern(host) {
            match crate::archive::fetch_archive(
                url_pattern,
                root_pattern,
                repo_url,
                &ref_spec,
                checkout_dir,
            ) {
                Ok(path) => {
                    log::info!("Downloaded {} via HTTP archive", module_path);
                    return Ok(path);
                }
                Err(e) => {
                    log::debug!(
                        "Archive download failed for {}: {}, falling back to git",
                        module_path,
                        e
                    );
                    // Clean up any partial download
                    let _ = std::fs::remove_dir_all(checkout_dir);
                }
            }
        }
    }

    // Fallback: Git sparse checkout
    // Initialize Git repo
    std::fs::create_dir_all(checkout_dir)?;
    git::run_in(checkout_dir, &["init", "--template="])?;

    // Disable line ending conversion - critical for cross-platform hash consistency
    git::run_in(checkout_dir, &["config", "core.autocrlf", "false"])?;

    // Add remote (ignore errors if already exists)
    let https_url = format!("https://{}.git", repo_url);
    let _ = git::run_in(checkout_dir, &["remote", "add", "origin", &https_url]);

    // Configure as promisor remote for partial clone (required for --filter=blob:none to work)
    git::run_in(checkout_dir, &["config", "remote.origin.promisor", "true"])?;
    git::run_in(
        checkout_dir,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    )?;

    // Try HTTPS fetch, fallback to SSH if needed, fallback to no-v-prefix
    let ssh_url = git::format_ssh_url(repo_url);
    let tag_ref = if is_pseudo_version {
        ref_spec.clone()
    } else {
        format!("refs/tags/{}", ref_spec)
    };

    let fetch_args = vec![
        "fetch",
        "--depth=1",
        "--filter=blob:none",
        "origin",
        &tag_ref,
    ];

    // Try HTTPS first
    let fetch_succeeded = git::run_in(checkout_dir, &fetch_args).is_ok();

    // Try SSH if HTTPS failed
    if !fetch_succeeded {
        git::run_in(checkout_dir, &["remote", "set-url", "origin", &ssh_url])?;
        git::run_in(checkout_dir, &fetch_args)?;
    }

    // Configure sparse-checkout for nested packages (fetch only the subpath)
    if !subpath.is_empty() {
        git::run_in(checkout_dir, &["sparse-checkout", "init", "--cone"])?;
        git::run_in(checkout_dir, &["sparse-checkout", "set", subpath])?;
    }

    // Checkout and materialize the fetched ref
    git::run_in(checkout_dir, &["reset", "--hard", "FETCH_HEAD"])?;

    // For nested packages: move subpath contents to cache root to eliminate path redundancy
    if !subpath.is_empty() {
        let subpath_dir = checkout_dir.join(subpath);
        if !subpath_dir.exists() {
            anyhow::bail!(
                "Subpath '{}' not found in {} at {}",
                subpath,
                repo_url,
                version_str
            );
        }

        // Cone mode includes root files (/*) - delete all except .git*, subpath
        let subpath_root = subpath.split('/').next().unwrap();
        for entry in std::fs::read_dir(checkout_dir)?.flatten() {
            let name = entry.file_name();
            let keep = name == ".git"
                || name.to_string_lossy().starts_with(".git")
                || name == subpath_root;
            if !keep {
                let _ = std::fs::remove_dir_all(entry.path())
                    .or_else(|_| std::fs::remove_file(entry.path()));
            }
        }

        // Move all contents from subpath_dir to checkout_dir
        for entry in std::fs::read_dir(&subpath_dir)? {
            let entry = entry?;
            std::fs::rename(entry.path(), checkout_dir.join(entry.file_name()))?;
        }

        // Remove now-empty subpath directories
        std::fs::remove_dir_all(subpath_dir.parent().unwrap_or(&subpath_dir))?;
    }

    Ok(checkout_dir.to_path_buf())
}

/// Update lockfile from build set
///
/// Merges with existing lockfile (Go's model): keeps old entries and adds/updates new ones.
/// This allows switching branches without losing checksums and enables historical verification.
/// Use `pcb tidy` (future) to remove unused entries.
///
/// Returns (lockfile, added_count) - only write to disk if added_count > 0
fn update_lockfile(
    workspace_info: &mut WorkspaceInfo,
    build_set: &HashSet<(ModuleLine, Version)>,
    asset_paths: &HashMap<(String, String), PathBuf>,
) -> Result<(Lockfile, usize)> {
    let workspace_root = &workspace_info.root;
    let mut lockfile = workspace_info.lockfile.take().unwrap_or_default();

    let total_count = build_set.len() + asset_paths.len();
    if total_count > 0 {
        log::debug!("  Verifying {} entries...", total_count);
    }

    // Open cache index to read hashes
    let index = CacheIndex::open()?;

    let mut verified_count = 0;
    let mut added_count = 0;

    // Process dependencies
    for (line, version) in build_set {
        let version_str = version.to_string();

        // Check if vendored (trust lockfile, integrity via git)
        let vendor_dir = workspace_root
            .join("vendor")
            .join(&line.path)
            .join(&version_str);
        if vendor_dir.exists() {
            verified_count += 1;
            continue;
        }

        // Not vendored - must be in cache
        let (content_hash, manifest_hash) = index
            .get_package(&line.path, &version_str)
            .ok_or_else(|| anyhow::anyhow!("Missing cache entry for {}@{}", line.path, version))?;

        // Check against existing lockfile entry
        if let Some(existing) = lockfile.get(&line.path, &version_str) {
            if existing.content_hash != content_hash {
                anyhow::bail!(
                    "Cache tampered: {}@v{} content hash mismatch\n  \
                    Expected: {}\n  \
                    Got: {}\n  \
                    Run: rm -rf ~/.pcb/cache/{}",
                    line.path,
                    version,
                    existing.content_hash,
                    content_hash,
                    line.path
                );
            }
            verified_count += 1;
        } else {
            added_count += 1;
            lockfile.insert(LockEntry {
                module_path: line.path.clone(),
                version: version_str,
                content_hash,
                manifest_hash: Some(manifest_hash),
            });
        }
    }

    // Process assets (asset_key includes subpath)
    for (asset_key, ref_str) in asset_paths.keys() {
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);

        // Check if vendored: vendor/{repo}/{ref}/{subpath}
        let vendor_base = workspace_root.join("vendor").join(repo_url).join(ref_str);
        let vendor_dir = if subpath.is_empty() {
            vendor_base
        } else {
            vendor_base.join(subpath)
        };
        if vendor_dir.exists() {
            verified_count += 1;
            continue;
        }

        // Not vendored - must be in cache
        let content_hash = index
            .get_asset(repo_url, subpath, ref_str)
            .ok_or_else(|| anyhow::anyhow!("Missing cache entry for {}@{}", asset_key, ref_str))?;

        // Lockfile entry uses full asset_key (includes subpath)
        if lockfile.get(asset_key, ref_str).is_none() {
            added_count += 1;
            lockfile.insert(LockEntry {
                module_path: asset_key.clone(),
                version: ref_str.clone(),
                content_hash,
                manifest_hash: None,
            });
        } else {
            verified_count += 1;
        }
    }

    if added_count > 0 {
        log::debug!("  {} new, {} verified", added_count, verified_count);
    } else if verified_count > 0 {
        log::debug!("  {} verified", verified_count);
    }

    Ok((lockfile, added_count))
}

// PackageClosure and package_closure() method are now in workspace.rs
