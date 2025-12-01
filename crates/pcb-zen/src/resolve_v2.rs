use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use std::time::Instant;
use thiserror::Error;

use crate::cache_index::{cache_base, ensure_bare_repo, CacheIndex};
use crate::canonical::{compute_content_hash_from_dir, compute_manifest_hash};
use crate::git;
use crate::workspace::WorkspaceInfo;

/// Path dependency validation errors
#[derive(Debug, Error)]
enum PathDepError {
    #[error("Path dependency '{url}' must specify a version\n  Example: {{ path = \"{path}\", version = \"1.0.0\" }}")]
    MissingVersion { url: String, path: String },
}

/// Module line identifier for MVS grouping
///
/// A module line represents a semver family:
/// - For v0.x: family is "v0.<minor>" (e.g., v0.2, v0.3 are different families)
/// - For v1.x+: family is "v<major>" (e.g., v1, v2, v3)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ModuleLine {
    path: String,   // e.g., "github.com/diodeinc/stdlib"
    family: String, // e.g., "v0.3" or "v1"
}

impl ModuleLine {
    fn new(path: String, version: &Version) -> Self {
        let family = if version.major == 0 {
            format!("v0.{}", version.minor)
        } else {
            format!("v{}", version.major)
        };

        ModuleLine { path, family }
    }
}

/// Dependency entry before resolution
#[derive(Debug, Clone)]
struct UnresolvedDep {
    url: String,
    spec: DependencySpec,
}

/// Package manifest for a code package (dependencies + declared assets)
///
/// Only constructed from V2 pcb.toml files. Asset repositories themselves never have manifests.
#[derive(Clone, Debug)]
struct PackageManifest {
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
    println!(
        "V2 Dependency Resolution{}",
        if offline { " (offline)" } else { "" }
    );
    println!("Workspace root: {}", workspace_root.display());

    // Phase -1: Auto-add missing dependencies from .zen files
    println!("\nPhase -1: Auto-detecting dependencies from .zen files");
    let auto_deps = crate::auto_deps::auto_add_zen_deps(
        &workspace_root,
        &workspace_info.packages,
        workspace_info.lockfile.as_ref(),
        offline,
    )?;
    if auto_deps.total_added > 0
        || auto_deps.versions_corrected > 0
        || auto_deps.discovered_remote > 0
    {
        if auto_deps.total_added > 0 {
            println!(
                "  Auto-added {} dependencies across {} package(s)",
                auto_deps.total_added, auto_deps.packages_updated
            );
        }
        if auto_deps.discovered_remote > 0 {
            println!(
                "  Discovered {} remote package(s) via git tags",
                auto_deps.discovered_remote
            );
        }
        if auto_deps.versions_corrected > 0 {
            println!(
                "  Corrected {} workspace member version(s)",
                auto_deps.versions_corrected
            );
        }
    } else {
        println!("  No missing dependencies or version corrections");
    }
    for (path, aliases) in &auto_deps.unknown_aliases {
        eprintln!("  ⊙ {} has unknown aliases:", path.display());
        for alias in aliases {
            eprintln!("      @{}", alias);
        }
    }
    for (path, urls) in &auto_deps.unknown_urls {
        eprintln!("  ⊙ {} has unknown remote URLs:", path.display());
        for url in urls {
            eprintln!("      {}", url);
        }
    }

    // Reload configs (auto-deps may have modified them)
    workspace_info.reload()?;

    // Validate patches are only at workspace root
    if !workspace_info.config.patch.is_empty() && workspace_info.config.workspace.is_none() {
        anyhow::bail!(
            "[patch] section is only allowed at workspace root\n  \
            Found in non-workspace pcb.toml at: {}/pcb.toml\n  \
            Move [patch] to workspace root or remove it.",
            workspace_root.display()
        );
    }

    // Display workspace info
    if let Some(ws) = &workspace_info.config.workspace {
        println!("Type: Explicit workspace");
        if !ws.members.is_empty() {
            println!("Member patterns: {:?}", ws.members);
        }
    } else {
        println!("Type: Standalone package (implicit workspace)");
    }

    println!("\nDiscovered {} package(s):", workspace_info.packages.len());
    for pkg in workspace_info.packages.values() {
        let package_name = pkg
            .dir
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| "root".into());

        if let Some(board) = &pkg.config.board {
            println!(
                "  - {} (board: {}) → {}",
                package_name,
                board.name,
                pkg.dir.display()
            );
        } else {
            println!("  - {} → {}", package_name, pkg.dir.display());
        }
    }

    println!(
        "\nWorkspace members: {} (for local resolution)",
        workspace_info.packages.len()
    );

    let patches = workspace_info.config.patch.clone();

    // MVS state
    let mut selected: HashMap<ModuleLine, Version> = HashMap::new();
    let mut work_queue: VecDeque<ModuleLine> = VecDeque::new();
    let mut manifest_cache: HashMap<(ModuleLine, Version), PackageManifest> = HashMap::new();

    println!("\nPhase 0: Seed from workspace dependencies");

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
        let package_deps = collect_package_dependencies(&pcb_toml_path, &pkg.config)?;

        if package_deps.is_empty() {
            packages_without_deps += 1;
            continue;
        }

        packages_with_deps.push((package_name, package_deps));
    }

    // Print summary
    if packages_without_deps > 0 {
        println!("  {} packages with no dependencies", packages_without_deps);
    }
    if !packages_with_deps.is_empty() {
        println!("  {} packages with dependencies:", packages_with_deps.len());
        for (package_name, package_deps) in &packages_with_deps {
            println!("    {} ({} deps)", package_name, package_deps.len());
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
                    eprintln!("  Warning: Failed to resolve {}: {}", dep.url, e);
                }
            }
        }
    }

    println!("\nPhase 1: Parallel dependency resolution");

    // Wave-based parallel fetching with MVS
    let phase1_start = Instant::now();
    let mut wave_num = 0;
    let mut total_fetched = 0;

    loop {
        // Collect current wave: packages in queue that haven't been fetched yet
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
            .collect();

        if wave.is_empty() {
            break;
        }

        wave_num += 1;
        let wave_start = Instant::now();
        println!("  Wave {}: {} packages", wave_num, wave.len());

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
                                eprintln!("    Warning: Failed to resolve {}: {}", dep_path, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "    Warning: Failed to fetch {}@v{}: {}",
                        line.path, version, e
                    );
                }
            }
        }

        let wave_elapsed = wave_start.elapsed();
        if new_deps > 0 {
            println!(
                "    Fetched in {:.1}s, discovered {} new dependencies",
                wave_elapsed.as_secs_f64(),
                new_deps
            );
        } else {
            println!("    Fetched in {:.1}s", wave_elapsed.as_secs_f64());
        }
    }

    let phase1_elapsed = phase1_start.elapsed();
    println!(
        "\n  Resolved {} packages in {} waves ({:.1}s)",
        total_fetched,
        wave_num,
        phase1_elapsed.as_secs_f64()
    );

    println!("\nPhase 2: Build closure");
    println!();

    // Phase 2: Build the final dependency set using only selected versions
    let closure = build_closure(&workspace_info.packages, &selected, &manifest_cache)?;

    println!("Build set: {} dependencies", closure.build_set.len());

    // Print dependency tree
    print_dependency_tree(&workspace_root, &closure, &manifest_cache);

    // Phase 2.5: Collect and fetch assets
    println!("\nPhase 2.5: Fetching assets");
    let asset_paths =
        collect_and_fetch_assets(workspace_info, &manifest_cache, &selected, offline)?;
    if !asset_paths.is_empty() {
        println!("Fetched {} assets", asset_paths.len());
    } else {
        println!("No assets");
    }

    // Phase 3: (Removed - sparse checkout and hashing now done in Phase 1)

    // Phase 4: Update lockfile with cryptographic hashes
    println!("\nPhase 4: Lockfile");
    let (lockfile, added_count) =
        update_lockfile(workspace_info, &closure.build_set, &asset_paths)?;

    // Only write lockfile to disk if new entries were added
    if added_count > 0 {
        let lockfile_path = workspace_root.join("pcb.sum");
        std::fs::write(&lockfile_path, lockfile.to_string())?;
        println!("  Updated {}", lockfile_path.display());
    }

    println!("\nV2 dependency resolution complete");

    let package_resolutions = build_resolution_map(
        workspace_info,
        &selected,
        &patches,
        &manifest_cache,
        &asset_paths,
        offline,
    )?;

    // Convert closure to (module_path, version) pairs
    let closure: HashSet<_> = closure
        .build_set
        .iter()
        .map(|(line, version)| (line.path.clone(), version.to_string()))
        .collect();

    Ok(ResolutionResult {
        package_resolutions,
        closure,
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
        .workspace
        .as_ref()
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

    // Build glob matcher
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        builder.add(Glob::new(pattern)?);
    }
    let glob_set = builder.build()?;

    // Create vendor directory if needed
    fs::create_dir_all(&vendor_dir)?;

    // Copy matching packages from cache
    let mut package_count = 0;
    for (module_path, version) in &resolution.closure {
        if !glob_set.is_match(module_path) {
            continue;
        }
        let src = cache.join(module_path).join(version);
        let dst = vendor_dir.join(module_path).join(version);
        if src.exists() && !dst.exists() {
            copy_dir_all(&src, &dst)?;
            package_count += 1;
        }
    }

    // Copy matching assets from cache (handling subpaths)
    let mut asset_count = 0;
    for (asset_key, ref_str) in resolution.assets.keys() {
        if !glob_set.is_match(asset_key) {
            continue;
        }

        // Split asset_key into (repo_url, subpath) for proper cache/vendor paths
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);

        // Source: cache/{repo}/{ref}/{subpath}
        let src = if subpath.is_empty() {
            cache.join(repo_url).join(ref_str)
        } else {
            cache.join(repo_url).join(ref_str).join(subpath)
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
fn build_resolution_map(
    workspace_info: &WorkspaceInfo,
    selected: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
    asset_paths: &HashMap<(String, String), PathBuf>,
    offline: bool,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let cache = cache_base();
    let vendor = workspace_info.root.join("vendor");

    // Helper to compute absolute path for a module line
    let get_abs_path = |line: &ModuleLine, version: &Version| -> Option<PathBuf> {
        if let Some(member_pkg) = workspace_info.packages.get(&line.path) {
            return Some(member_pkg.dir.clone());
        }
        if let Some(patch) = patches.get(&line.path) {
            return Some(workspace_info.root.join(&patch.path));
        }
        let vendor_path = vendor.join(&line.path).join(version.to_string());
        if vendor_path.exists() {
            return Some(vendor_path);
        }
        if !offline {
            let cache_path = cache.join(&line.path).join(version.to_string());
            if cache_path.exists() {
                return Some(cache_path);
            }
        }
        None
    };

    // Build lookup: url -> family -> path
    let mut url_to_families: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    for (line, version) in selected {
        if let Some(abs_path) = get_abs_path(line, version) {
            url_to_families
                .entry(line.path.clone())
                .or_default()
                .insert(line.family.clone(), abs_path);
        }
    }

    // Resolve dependency -> path
    let resolve = |base_dir: &Path, url: &str, spec: &DependencySpec| -> Option<PathBuf> {
        if let DependencySpec::Detailed(d) = spec {
            if let Some(path_str) = &d.path {
                return Some(base_dir.join(path_str));
            }
        }
        let families = url_to_families.get(url)?;
        let version_str = match spec {
            DependencySpec::Version(v) => v.as_str(),
            DependencySpec::Detailed(d) => d
                .version
                .as_deref()
                .or(d.rev.as_deref())
                .or(d.branch.as_deref())?,
        };
        let req_version = parse_version_string(version_str).ok()?;
        let req_family = ModuleLine::new(url.to_string(), &req_version).family;
        families.get(&req_family).cloned().or_else(|| {
            (families.len() == 1)
                .then(|| families.values().next().cloned())
                .flatten()
        })
    };

    // Helper to get asset path with LPM fallback
    // If exact asset_key not found, try to find any entry from the same repo
    let get_asset_path = |asset_key: &str, ref_str: &str| -> Option<PathBuf> {
        // 1. Try exact match first
        if let Some(path) = asset_paths.get(&(asset_key.to_string(), ref_str.to_string())) {
            return Some(path.clone());
        }

        // 2. LPM: try without subpath (whole-repo dependency)
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);
        if !subpath.is_empty() {
            if let Some(path) = asset_paths.get(&(repo_url.to_string(), ref_str.to_string())) {
                return Some(path.join(subpath));
            }
        }

        // 3. Find any entry from the same repo (for subpath-only dependencies)
        // e.g., pcb.toml has "gitlab.com/.../Device.kicad_sym" but we need "power.kicad_sym"
        for ((key, key_ref), path) in asset_paths.iter() {
            if key_ref != ref_str {
                continue;
            }
            let (key_repo, key_subpath) = git::split_asset_repo_and_subpath(key);
            if key_repo == repo_url && !key_subpath.is_empty() {
                // Found another subpath from same repo - use repo root + our subpath
                // path points to key_subpath, so we need to go up to repo root
                if let Some(repo_root) = path.parent() {
                    return Some(repo_root.join(subpath));
                }
            }
        }

        None
    };

    // Build deps map for a package
    let build_map = |base_dir: &Path,
                     pkg_deps: &BTreeMap<String, DependencySpec>,
                     pkg_assets: &BTreeMap<String, pcb_zen_core::AssetDependencySpec>|
     -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();
        for (url, spec) in pkg_deps {
            if let Some(path) = resolve(base_dir, url, spec) {
                map.insert(url.clone(), path);
            }
        }
        for (asset_key, asset_spec) in pkg_assets {
            if let Ok(ref_str) = extract_asset_ref(asset_spec) {
                if let Some(path) = get_asset_path(asset_key, &ref_str) {
                    map.insert(asset_key.clone(), path);
                }
            }
        }
        map
    };

    let mut results = HashMap::new();

    // Workspace root
    results.insert(
        workspace_info.root.clone(),
        build_map(
            &workspace_info.root,
            &workspace_info.config.dependencies,
            &workspace_info.config.assets,
        ),
    );

    // Member packages
    for pkg in workspace_info.packages.values() {
        results.insert(
            pkg.dir.clone(),
            build_map(&pkg.dir, &pkg.config.dependencies, &pkg.config.assets),
        );
    }

    // Transitive deps
    for (line, version) in selected {
        if let Some(abs_path) = get_abs_path(line, version) {
            if let Some(manifest) = manifest_cache.get(&(line.clone(), version.clone())) {
                results.insert(
                    abs_path.clone(),
                    build_map(&abs_path, &manifest.dependencies, &manifest.assets),
                );
            }
        }
    }

    Ok(results)
}

/// Collect dependencies for a package and transitive local deps
fn collect_package_dependencies(
    pcb_toml_path: &Path,
    v2_config: &pcb_zen_core::config::PcbToml,
) -> Result<Vec<UnresolvedDep>> {
    let package_dir = pcb_toml_path.parent().unwrap();
    let mut deps = HashMap::new();

    collect_deps_recursive(&v2_config.dependencies, package_dir, &mut deps)?;

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
                    return Err(PathDepError::MissingVersion {
                        url: url.clone(),
                        path: path.clone(),
                    }
                    .into());
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
        collect_deps_recursive(&dep_config.dependencies, &resolved_path, deps)?;
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

/// Extract ref string from AssetDependencySpec
///
/// Returns an error if the spec doesn't specify a version, branch, or rev (including HEAD)
pub fn extract_asset_ref(spec: &pcb_zen_core::AssetDependencySpec) -> Result<String> {
    use pcb_zen_core::AssetDependencySpec;

    match spec {
        AssetDependencySpec::Ref(r) => {
            if r == "HEAD" {
                anyhow::bail!(
                    "Asset ref 'HEAD' is not allowed; use an explicit version, branch, or rev"
                );
            }
            Ok(r.clone())
        }
        AssetDependencySpec::Detailed(detail) => {
            let ref_str = detail
                .version
                .clone()
                .or_else(|| detail.branch.clone())
                .or_else(|| detail.rev.clone())
                .ok_or_else(|| anyhow::anyhow!("Asset must specify version, branch, or rev"))?;

            if ref_str == "HEAD" {
                anyhow::bail!(
                    "Asset ref 'HEAD' is not allowed; use an explicit version, branch, or rev"
                );
            }
            Ok(ref_str)
        }
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
    let mut asset_paths: HashMap<(String, String), PathBuf> = HashMap::new();

    // Helper to fetch an asset if not already seen
    let mut fetch_if_needed =
        |module_path: &str, asset_spec: &pcb_zen_core::AssetDependencySpec| -> Result<()> {
            let ref_str = extract_asset_ref(asset_spec)?;
            let key = (module_path.to_string(), ref_str.clone());

            // Skip if already processed
            if asset_paths.contains_key(&key) {
                return Ok(());
            }

            println!("      Fetching {}@{}", module_path, ref_str);

            // Fetch the asset repo and store the resolved path
            let path = fetch_asset_repo(workspace_info, module_path, &ref_str, offline)?;
            asset_paths.insert(key, path);
            Ok(())
        };

    // 1. Collect assets from workspace and member packages
    for pkg in workspace_info.packages.values() {
        if pkg.config.assets.is_empty() {
            continue;
        }
        let package_name = pkg
            .dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".into());

        println!("\n  Package: {}", package_name);
        println!("    {} assets", pkg.config.assets.len());

        for (module_path, asset_spec) in &pkg.config.assets {
            fetch_if_needed(module_path, asset_spec)?;
        }
    }

    // 2. Collect assets from transitive packages (via manifest cache)
    for (line, version) in selected {
        if let Some(manifest) = manifest_cache.get(&(line.clone(), version.clone())) {
            if !manifest.assets.is_empty() {
                println!("\n  Transitive: {}", line.path);
                println!("    {} assets", manifest.assets.len());

                for (module_path, asset_spec) in &manifest.assets {
                    fetch_if_needed(module_path, asset_spec)?;
                }
            }
        }
    }

    Ok(asset_paths)
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
    if let Some(patch) = workspace_info.config.patch.get(module_path) {
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
    if let Some(patch) = workspace_info.config.patch.get(asset_key) {
        let patched_path = workspace_info.root.join(&patch.path);

        println!("      Using patched source: {}", patch.path);

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
        println!("        Vendored");
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
        println!("        Cached");
        return Ok(target_path);
    }

    // 5. Ensure base repo is fetched (sparse checkout the full repo once)
    if !repo_cache_dir.join(".git").exists() {
        println!("        Cloning (sparse checkout)");
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
    print!("        Computing hashes... ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let content_hash = compute_content_hash_from_dir(&target_path)?;
    index.set_asset(repo_url, subpath, ref_str, &content_hash)?;

    println!("done");

    Ok(target_path)
}

/// Print a tree view of the dependency graph (cargo tree style)
fn print_dependency_tree(
    workspace_root: &Path,
    closure: &ClosureResult,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) {
    println!();

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

    // Helper to print a dependency with tree formatting (cargo tree style)
    fn print_dep(
        url: &str,
        build_map: &HashMap<String, (ModuleLine, Version)>,
        manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
        printed: &mut HashSet<String>,
        prefix: &str,
        is_last: bool,
        format_name: &impl Fn(&str) -> String,
    ) {
        let branch = if is_last { "└── " } else { "├── " };

        if let Some((line, version)) = build_map.get(url) {
            let already_printed = !printed.insert(url.to_string());

            println!(
                "{}{}{} v{}{}",
                prefix,
                branch,
                format_name(url),
                version,
                if already_printed { " (*)" } else { "" }
            );

            // Don't recurse if already shown
            if already_printed {
                return;
            }

            // Print transitive dependencies
            if let Some(manifest) = manifest_cache.get(&(line.clone(), version.clone())) {
                let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                let mut dep_list: Vec<_> = manifest
                    .dependencies
                    .iter()
                    .filter(|(dep_url, spec)| {
                        !is_non_version_dep(spec) && build_map.contains_key(*dep_url)
                    })
                    .collect();

                // Sort for consistent output
                dep_list.sort_by_key(|(url, _)| *url);

                for (i, (dep_url, _)) in dep_list.iter().enumerate() {
                    let is_last_child = i == dep_list.len() - 1;
                    print_dep(
                        dep_url,
                        build_map,
                        manifest_cache,
                        printed,
                        &child_prefix,
                        is_last_child,
                        format_name,
                    );
                }
            }
        }
    }

    // Workspace root header
    let workspace_name = workspace_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    if !closure.root_deps.is_empty() {
        println!("{}", workspace_name);
        for (i, url) in closure.root_deps.iter().enumerate() {
            let is_last = i == closure.root_deps.len() - 1;
            print_dep(
                url,
                &closure.build_map,
                manifest_cache,
                &mut printed,
                "",
                is_last,
                &format_name,
            );
        }
    }
}

/// Result of building the dependency closure
struct ClosureResult {
    /// Set of (ModuleLine, Version) pairs in the build closure
    build_set: HashSet<(ModuleLine, Version)>,
    /// Lookup: module_path -> (ModuleLine, Version) for the build set
    build_map: HashMap<String, (ModuleLine, Version)>,
    /// Direct dependency URLs from workspace packages (for tree roots)
    root_deps: Vec<String>,
}

/// Build the final dependency closure using selected versions
///
/// DFS from workspace package dependencies using selected versions.
/// Returns the build set, a lookup map, and root dependency URLs.
fn build_closure(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> Result<ClosureResult> {
    let mut build_set = HashSet::new();
    let mut stack = Vec::new();
    let mut root_deps = Vec::new();

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

    // Seed DFS from all package dependencies, collecting root deps
    for pkg in packages.values() {
        for (url, spec) in &pkg.config.dependencies {
            if !is_non_version_dep(spec) {
                // Track root deps for tree printing
                if line_by_path.contains_key(url) && !root_deps.contains(url) {
                    root_deps.push(url.clone());
                }
                // Find selected ModuleLine(s) for this path
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

    // Build reverse lookup: path -> (line, version) from build_set
    let mut build_map: HashMap<String, (ModuleLine, Version)> = HashMap::new();
    for (line, version) in &build_set {
        build_map.insert(line.path.clone(), (line.clone(), version.clone()));
    }

    // Sort root deps for consistent output
    root_deps.sort();
    root_deps.dedup();

    Ok(ClosureResult {
        build_set,
        build_map,
        root_deps,
    })
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
                            println!("        Using locked v{} (from pcb.sum)", locked_version);
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
                            println!("        Using locked v{} (from pcb.sum)", locked_version);
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
        println!(
            "        Resolving branch '{}' for {}...",
            branch, module_path
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
    println!(
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
        println!("  → {} {}@v{}{}", action, path, final_version, suffix);

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

    // If .git already exists, assume checkout is done (cache hit)
    // Package root is always checkout_dir (nested packages already moved up)
    if checkout_dir.join(".git").exists() {
        return Ok(checkout_dir.to_path_buf());
    }

    // Initialize Git repo
    std::fs::create_dir_all(checkout_dir)?;
    git::run_in(checkout_dir, &["init"])?;

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
        println!("  Verifying {} entries...", total_count);
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
        println!("  {} new, {} verified", added_count, verified_count);
    } else if verified_count > 0 {
        println!("  {} verified", verified_count);
    }

    Ok((lockfile, added_count))
}

// PackageClosure and package_closure() method are now in workspace.rs
