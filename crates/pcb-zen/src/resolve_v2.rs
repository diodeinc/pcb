use anyhow::Result;
use ignore::WalkBuilder;
use pcb_zen_core::config::{
    find_workspace_root, DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml,
};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

use crate::git;
use crate::workspace::{get_workspace_info, WorkspaceInfo};

/// Get the PCB cache base directory (~/.pcb/cache)
fn cache_base() -> PathBuf {
    let home = dirs::home_dir().expect("Determine home directory");
    home.join(".pcb").join("cache")
}

/// Path dependency validation errors
#[derive(Debug, Error)]
enum PathDepError {
    #[error("Path dependency '{url}' must specify a version\n  Example: {{ path = \"{path}\", version = \"1.0.0\" }}")]
    MissingVersion { url: String, path: String },

    #[error("Path dependency '{url}' points to V1 package\n  Location: {location}\n  Path dependencies require V2 packages")]
    V1Package { url: String, location: PathBuf },
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

#[derive(Debug, Clone)]
pub struct ResolutionResult {
    pub workspace_root: PathBuf,
    /// Map from Package Root (Absolute Path) -> Import URL -> Resolved Absolute Path
    /// Uses BTreeMap for deterministic ordering (enables longest prefix matching)
    pub package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>>,
    pub packages: Vec<PathBuf>,
}

/// Check if the input paths are in a V2 workspace and run dependency resolution if needed
///
/// This is called once per `pcb build` invocation (workspace-first architecture).
/// For V2 workspaces, it runs dependency resolution before any .zen file discovery.
pub fn maybe_resolve_v2_workspace(paths: &[PathBuf]) -> Result<Option<ResolutionResult>> {
    let input_path = if paths.is_empty() {
        std::env::current_dir()?
    } else {
        paths[0].clone()
    };

    let file_provider = Arc::new(DefaultFileProvider::new());
    let workspace_root = find_workspace_root(&*file_provider, &input_path);

    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !file_provider.exists(&pcb_toml_path) {
        return Ok(None);
    }

    let config = PcbToml::from_file(&*file_provider, &pcb_toml_path)?;
    if config.is_v2() {
        return Ok(Some(resolve_dependencies(
            &*file_provider,
            &workspace_root,
        )?));
    }

    Ok(None)
}

/// V2 dependency resolution
///
/// Discovers member packages, builds dependency graph using MVS, fetches dependencies,
/// and generates/updates the lockfile.
fn resolve_dependencies(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
) -> Result<ResolutionResult> {
    println!("V2 Dependency Resolution");
    println!("Workspace root: {}", workspace_root.display());

    // Get workspace info - the single source of truth
    let mut workspace_info = get_workspace_info(file_provider, workspace_root)?;

    // Phase -1: Auto-add missing dependencies from .zen files
    println!("\nPhase -1: Auto-detecting dependencies from .zen files");
    let auto_deps = crate::auto_deps::auto_add_zen_deps(
        workspace_root,
        &workspace_info.packages,
        workspace_info.lockfile.as_ref(),
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

            match resolve_to_version(&dep.url, &dep.spec, workspace_info.lockfile.as_ref()) {
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
                let result = fetch_package(
                    workspace_root,
                    &line.path,
                    version,
                    &patches,
                    &workspace_info.packages,
                );
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
    let build_set = build_closure(&workspace_info.packages, &selected, &manifest_cache)?;

    println!("Build set: {} dependencies", build_set.len());

    // Print dependency tree
    print_dependency_tree(
        &workspace_info.packages,
        workspace_root,
        &build_set,
        &manifest_cache,
    )?;

    // Phase 2.5: Collect and fetch assets
    println!("\nPhase 2.5: Fetching assets");
    let asset_set = collect_and_fetch_assets(
        &workspace_info.packages,
        &manifest_cache,
        &selected,
        workspace_root,
        &patches,
    )?;
    if !asset_set.is_empty() {
        println!("Fetched {} assets", asset_set.len());
    } else {
        println!("No assets");
    }

    // Phase 3: (Removed - sparse checkout and hashing now done in Phase 1)

    // Phase 4: Update lockfile with cryptographic hashes
    println!("\nPhase 4: Updating lockfile");
    let lockfile = update_lockfile(workspace_info.lockfile.take(), &build_set, &asset_set)?;

    // Write lockfile to disk
    let lockfile_path = workspace_root.join("pcb.sum");
    std::fs::write(&lockfile_path, lockfile.to_string())?;
    println!("  Written to {}", lockfile_path.display());

    println!("\nV2 dependency resolution complete");

    let package_paths = workspace_info
        .packages
        .values()
        .map(|pkg| pkg.dir.clone())
        .collect();

    let package_resolutions =
        build_resolution_map(&workspace_info, &selected, &patches, &manifest_cache)?;

    Ok(ResolutionResult {
        workspace_root: workspace_root.to_path_buf(),
        package_resolutions,
        packages: package_paths,
    })
}

/// Build the per-package resolution map
fn build_resolution_map(
    workspace_info: &WorkspaceInfo,
    selected: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let cache = cache_base();
    // Helper to compute absolute path for a module line
    let get_abs_path = |line: &ModuleLine, version: &Version| -> PathBuf {
        // 1. Check workspace members first
        if let Some(member_pkg) = workspace_info.packages.get(&line.path) {
            return member_pkg.dir.clone();
        }
        // 2. Check patches
        if let Some(patch) = patches.get(&line.path) {
            return workspace_info.root.join(&patch.path);
        }
        // 3. Fall back to cache
        cache.join(&line.path).join(version.to_string())
    };

    // Build lookup: url -> family -> path
    let mut url_to_families: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    for (line, version) in selected {
        let abs_path = get_abs_path(line, version);
        url_to_families
            .entry(line.path.clone())
            .or_default()
            .insert(line.family.clone(), abs_path);
    }

    // Resolve dependency -> path (with per-package base directory)
    let resolve = |base_dir: &Path, url: &str, spec: &DependencySpec| -> Option<PathBuf> {
        // Local path - resolve relative to base_dir
        if let DependencySpec::Detailed(d) = spec {
            if let Some(path_str) = &d.path {
                return Some(base_dir.join(path_str));
            }
        }

        // Remote: find matching family
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
            if families.len() == 1 {
                families.values().next().cloned()
            } else {
                None
            }
        })
    };

    // Resolve asset dependency to cache path
    // Must match fetch_asset_repo() cache layout
    let resolve_asset =
        |url: &str, asset_spec: &pcb_zen_core::AssetDependencySpec| -> Result<PathBuf> {
            // Check patch first
            if let Some(patch) = patches.get(url) {
                return Ok(workspace_info.root.join(&patch.path));
            }

            // Cache path: ~/.pcb/cache/{url}/{ref}/
            let ref_str = extract_asset_ref(asset_spec)?;
            Ok(cache.join(url).join(ref_str))
        };

    // Build deps map for a package with base directory for local path resolution
    // V2 does not support aliases - only canonical URL dependencies
    let build_map = |base_dir: &Path,
                     pkg_deps: &BTreeMap<String, DependencySpec>,
                     pkg_assets: &BTreeMap<String, pcb_zen_core::AssetDependencySpec>|
     -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();

        // Package deps - resolve local paths relative to base_dir
        for (url, spec) in pkg_deps {
            if let Some(path) = resolve(base_dir, url, spec) {
                map.insert(url.clone(), path);
            }
        }

        // Asset deps - resolve to cache paths
        for (url, asset_spec) in pkg_assets {
            if let Ok(path) = resolve_asset(url, asset_spec) {
                map.insert(url.clone(), path);
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

    // Transitive deps: each remote package gets its own resolution map
    // This ensures MVS-selected versions are used for all transitive loads
    // Assets come from the cached manifest (no duplicate file I/O)
    for (line, version) in selected {
        let abs_path = get_abs_path(line, version);
        if let Some(manifest) = manifest_cache.get(&(line.clone(), version.clone())) {
            results.insert(
                abs_path.clone(),
                build_map(&abs_path, &manifest.dependencies, &manifest.assets),
            );
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
        let dep_config = match PcbToml::from_file(&file_provider, &dep_pcb_toml) {
            Ok(config) if config.is_v2() => config,
            Ok(_) => {
                return Err(PathDepError::V1Package {
                    url: url.clone(),
                    location: dep_pcb_toml.clone(),
                }
                .into());
            }
            Err(e) => return Err(e),
        };

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
fn extract_asset_ref(spec: &pcb_zen_core::AssetDependencySpec) -> Result<String> {
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
/// Returns set of (module_path, ref) for all fetched assets
fn collect_and_fetch_assets(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
    selected: &HashMap<ModuleLine, Version>,
    workspace_root: &Path,
    patches: &BTreeMap<String, pcb_zen_core::config::PatchSpec>,
) -> Result<HashSet<(String, String)>> {
    let mut asset_set = HashSet::new();
    let mut seen_assets: HashSet<(String, String)> = HashSet::new();

    // Helper to fetch an asset if not already seen
    let mut fetch_if_needed =
        |module_path: &str, asset_spec: &pcb_zen_core::AssetDependencySpec| -> Result<()> {
            let ref_str = extract_asset_ref(asset_spec)?;

            // Skip if already processed
            if !seen_assets.insert((module_path.to_string(), ref_str.clone())) {
                return Ok(());
            }

            println!("      Fetching {}@{}", module_path, ref_str);

            // Fetch the asset repo
            fetch_asset_repo(workspace_root, module_path, &ref_str, patches)?;
            asset_set.insert((module_path.to_string(), ref_str));
            Ok(())
        };

    // 1. Collect assets from workspace and member packages
    for pkg in packages.values() {
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

    Ok(asset_set)
}

/// Fetch a package from Git using sparse checkout
///
/// Fetches all package files, computes content/manifest hashes, and caches locally.
/// Returns the package manifest for dependency resolution.
fn fetch_package(
    workspace_root: &Path,
    module_path: &str,
    version: &Version,
    patches: &BTreeMap<String, pcb_zen_core::config::PatchSpec>,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<PackageManifest> {
    // 1. Workspace member override (highest priority)
    if let Some(member_pkg) = packages.get(module_path) {
        let member_toml = member_pkg.dir.join("pcb.toml");
        return read_manifest_from_path(&member_toml);
    }

    // 2. Check if this module is patched with a local path
    if let Some(patch) = patches.get(module_path) {
        let patched_path = workspace_root.join(&patch.path);
        let patched_toml = patched_path.join("pcb.toml");

        if !patched_toml.exists() {
            anyhow::bail!("Patch path {} has no pcb.toml", patched_path.display());
        }

        return read_manifest_from_path(&patched_toml);
    }

    // Cache directory: ~/.pcb/cache/{module_path}/{version}/
    let cache = cache_base();
    let checkout_dir = cache.join(module_path).join(version.to_string());
    let cache_marker = checkout_dir.join(".pcbcache");

    // Fast path: .pcbcache marker is written AFTER successful checkout + hash computation,
    // so its existence guarantees the package is fully fetched and valid
    if cache_marker.exists() {
        let pcb_toml_path = checkout_dir.join("pcb.toml");
        return read_manifest_from_path(&pcb_toml_path);
    }

    // Slow path: fetch via sparse checkout
    let package_root =
        ensure_sparse_checkout(&checkout_dir, module_path, &version.to_string(), true)?;
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

    let marker_content = format!("{}\n{}\n", content_hash, manifest_hash);
    std::fs::write(&cache_marker, marker_content)?;

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
fn fetch_asset_repo(
    workspace_root: &Path,
    module_path: &str,
    ref_str: &str,
    patches: &BTreeMap<String, pcb_zen_core::config::PatchSpec>,
) -> Result<PathBuf> {
    // 1. Check if this module is patched with a local path
    if let Some(patch) = patches.get(module_path) {
        let patched_path = workspace_root.join(&patch.path);

        println!("      Using patched source: {}", patch.path);

        // Verify patch path exists
        if !patched_path.exists() {
            anyhow::bail!(
                "Asset '{}' is patched to a non-existent path\n  \
                Patch path: {}",
                module_path,
                patched_path.display()
            );
        }

        // Assets: ignore pcb.toml if present (no validation needed)
        return Ok(patched_path);
    }

    // Cache directory: ~/.pcb/cache/{module_path}/{ref}/
    let cache = cache_base();
    let checkout_dir = cache.join(module_path).join(ref_str);

    // Check if already fetched
    if checkout_dir.exists() && checkout_dir.join(".pcbcache").exists() {
        println!("        Cached");
        return Ok(checkout_dir);
    }

    // Use sparse checkout to fetch the asset repo
    println!("        Cloning (sparse checkout)");

    let package_root = ensure_sparse_checkout(&checkout_dir, module_path, ref_str, false)?;

    // Compute and store content hash
    print!("        Computing hashes... ");
    std::io::Write::flush(&mut std::io::stdout())?;

    let content_hash = compute_content_hash_from_dir(&package_root)?;
    let cache_marker = checkout_dir.join(".pcbcache");
    // Assets don't have manifests, so just write content hash
    std::fs::write(&cache_marker, format!("{}\n", content_hash))?;

    println!("done");

    Ok(package_root)
}

/// Print a tree view of the dependency graph (cargo tree style)
fn print_dependency_tree(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    workspace_root: &Path,
    build_set: &HashSet<(ModuleLine, Version)>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> Result<()> {
    println!();

    // Build reverse lookup: path -> (line, version)
    let mut build_map: HashMap<String, (ModuleLine, Version)> = HashMap::new();
    for (line, version) in build_set {
        build_map.insert(line.path.clone(), (line.clone(), version.clone()));
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

    // Collect all root dependencies from packages
    let mut all_roots = Vec::new();

    // Package dependencies
    for pkg in packages.values() {
        let pcb_toml_path = pkg.dir.join("pcb.toml");
        let package_deps = collect_package_dependencies(&pcb_toml_path, &pkg.config)?;

        for dep in package_deps {
            if !is_non_version_dep(&dep.spec)
                && build_map.contains_key(&dep.url)
                && !all_roots.contains(&dep.url)
            {
                all_roots.push(dep.url.clone());
            }
        }
    }

    // Sort for consistent output
    all_roots.sort();
    all_roots.dedup();

    if !all_roots.is_empty() {
        println!("{}", workspace_name);
        for (i, url) in all_roots.iter().enumerate() {
            let is_last = i == all_roots.len() - 1;
            print_dep(
                url,
                &build_map,
                manifest_cache,
                &mut printed,
                "",
                is_last,
                &format_name,
            );
        }
    }

    Ok(())
}

/// Build the final dependency closure using selected versions
///
/// DFS from workspace package dependencies using selected versions.
fn build_closure(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> Result<HashSet<(ModuleLine, Version)>> {
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
        let pcb_toml_path = pkg.dir.join("pcb.toml");
        let package_deps = collect_package_dependencies(&pcb_toml_path, &pkg.config)?;

        for dep in package_deps {
            if !is_non_version_dep(&dep.spec) {
                // Find selected ModuleLine(s) for this path
                if let Some(lines) = line_by_path.get(&dep.url) {
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

    Ok(build_set)
}

/// Resolve a dependency spec to a concrete version
///
/// Handles:
/// - Exact versions: "0.3.2" → v0.3.2
/// - Branches: { branch = "main" } → pseudo-version (uses lockfile if available)
/// - Revisions: { rev = "abcd1234" } → pseudo-version (uses lockfile if available)
fn resolve_to_version(
    module_path: &str,
    spec: &DependencySpec,
    lockfile: Option<&Lockfile>,
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
                resolve_rev_to_pseudo_version(module_path, rev)
            } else {
                anyhow::bail!("Dependency has no version, branch, or rev")
            }
        }
    }
}

/// Resolve a Git branch to a pseudo-version
fn resolve_branch_to_pseudo_version(module_path: &str, branch: &str) -> Result<Version> {
    println!(
        "        Resolving branch '{}' for {}...",
        branch, module_path
    );

    let refspec = format!("refs/heads/{}", branch);
    let (commit, git_url) = git::ls_remote_with_fallback(module_path, &refspec)?;

    generate_pseudo_version_for_commit(module_path, &commit, &git_url)
}

/// Resolve a Git revision to a pseudo-version
fn resolve_rev_to_pseudo_version(module_path: &str, rev: &str) -> Result<Version> {
    println!(
        "        Resolving rev '{}' for {}...",
        &rev[..8.min(rev.len())],
        module_path
    );

    // For revisions, just use HTTPS (SSH wouldn't help for commit lookup)
    let git_url = format!("https://{}.git", module_path);
    generate_pseudo_version_for_commit(module_path, rev, &git_url)
}

/// Generate a pseudo-version for a Git commit
///
/// Format: v<base>-0.<timestamp>-<commit_short>
/// Base version is derived from latest reachable tag, or v0.0.0 if none
fn generate_pseudo_version_for_commit(
    module_path: &str,
    commit: &str,
    git_url: &str,
) -> Result<Version> {
    // Get a minimal clone to inspect the commit
    let cache = cache_base();
    let temp_clone = cache.join("temp").join(module_path);

    std::fs::create_dir_all(temp_clone.parent().unwrap())?;

    // Clone if needed (shallow)
    if !temp_clone.join(".git").exists() {
        git::clone_bare_with_filter(git_url, &temp_clone)?;
    } else {
        // Fetch updates
        let _ = git::run_in(&temp_clone, &["fetch", "origin", "--tags"]);
    }

    // Find latest tag reachable from this commit
    let base_version = git::describe_tags(&temp_clone, commit)
        .and_then(|tag| parse_version_string(&tag).ok())
        .unwrap_or_else(|| Version::new(0, 0, 0));

    // Increment patch version
    let pseudo_base = Version::new(
        base_version.major,
        base_version.minor,
        base_version.patch + 1,
    );

    // Get commit timestamp
    let timestamp = git::show_commit_timestamp(&temp_clone, commit).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    });

    // Format timestamp as YYYYMMDDhhmmss using jiff
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

/// Collect entries for canonical tar (shared between create and list)
fn collect_canonical_entries(dir: &Path) -> Result<Vec<(PathBuf, std::fs::FileType)>> {
    let mut entries = Vec::new();
    let package_root = dir.to_path_buf();
    for result in WalkBuilder::new(dir)
        .filter_entry(move |entry| {
            let path = entry.path();
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && path != package_root
                && path.join("pcb.toml").is_file()
            {
                return false;
            }
            true
        })
        .build()
    {
        let entry = result?;
        let path = entry.path();
        let rel_path = match path.strip_prefix(dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if rel_path == Path::new("") {
            continue;
        }
        let file_type = entry.file_type().unwrap();
        // Only include files - directories are implicit from file paths in tar
        // This avoids issues with empty directories (which git doesn't track anyway)
        if file_type.is_file() {
            entries.push((rel_path.to_path_buf(), file_type));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

/// List entries that would be included in canonical tar (for debugging)
pub fn list_canonical_tar_entries(dir: &Path) -> Result<Vec<String>> {
    let entries = collect_canonical_entries(dir)?;
    Ok(entries
        .into_iter()
        .map(|(p, _ft)| p.display().to_string())
        .collect())
}

/// Create a canonical, deterministic tar archive from a directory
///
/// Rules from packaging.md:
/// - Regular files only (directories are implicit from paths)
/// - Relative paths, forward slashes, lexicographic order
/// - Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
/// - File mode: 0644
/// - End with two 512-byte zero blocks
/// - Respect .gitignore and filter internal marker files
/// - Exclude nested packages (subdirs with pcb.toml + [package])
pub fn create_canonical_tar<W: std::io::Write>(dir: &Path, writer: W) -> Result<()> {
    use std::fs;
    use tar::{Builder, Header};

    let mut builder = Builder::new(writer);
    builder.mode(tar::HeaderMode::Deterministic);

    let entries = collect_canonical_entries(dir)?;

    for (rel_path, _file_type) in entries {
        let full_path = dir.join(&rel_path);
        let path_str = rel_path.to_str().unwrap().replace('\\', "/");

        let file = fs::File::open(&full_path)?;
        let len = file.metadata()?.len();
        let mut header = Header::new_gnu();
        header.set_size(len);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("")?;
        header.set_groupname("")?;
        header.set_entry_type(tar::EntryType::Regular);

        builder.append_data(&mut header, &path_str, file)?;
    }

    builder.finish()?;

    Ok(())
}

/// Compute content hash from a directory
///
/// Creates canonical GNU tarball from directory, streams to BLAKE3 hasher.
/// Format: h1:<base64-encoded-blake3>
pub fn compute_content_hash_from_dir(cache_dir: &Path) -> Result<String> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    // Stream canonical tar directly to BLAKE3 hasher (avoids buffering entire tar in memory)
    let mut hasher = blake3::Hasher::new();
    create_canonical_tar(cache_dir, &mut hasher)?;
    let hash = hasher.finalize();

    Ok(format!("h1:{}", STANDARD.encode(hash.as_bytes())))
}

/// Compute manifest hash for a pcb.toml file
///
/// Format: h1:<base64-encoded-blake3>
pub fn compute_manifest_hash(manifest_content: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let hash = blake3::hash(manifest_content.as_bytes());

    format!("h1:{}", STANDARD.encode(hash.as_bytes()))
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

    if content_hash != expected_content {
        anyhow::bail!(
            "Content hash mismatch for {}@v{}\n  \
            Expected (from tag): {}\n  \
            Computed:            {}\n\n\
            This may indicate a bug in the packaging toolchain.",
            module_path,
            version,
            expected_content,
            content_hash
        );
    }

    if manifest_hash != expected_manifest {
        anyhow::bail!(
            "Manifest hash mismatch for {}@v{}\n  \
            Expected (from tag): {}\n  \
            Computed:            {}\n\n\
            This may indicate a bug in the packaging toolchain.",
            module_path,
            version,
            expected_manifest,
            manifest_hash
        );
    }

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

/// Clone git repository with HTTPS→SSH fallback
/// Update lockfile from build set
///
/// Merges with existing lockfile (Go's model): keeps old entries and adds/updates new ones.
/// This allows switching branches without losing checksums and enables historical verification.
/// Use `pcb tidy` (future) to remove unused entries.
fn update_lockfile(
    existing_lockfile: Option<Lockfile>,
    build_set: &HashSet<(ModuleLine, Version)>,
    asset_set: &HashSet<(String, String)>,
) -> Result<Lockfile> {
    // Start with existing lockfile or create new one
    let mut lockfile = existing_lockfile.unwrap_or_default();

    let total_count = build_set.len() + asset_set.len();
    println!("  Reading hashes for {} entries...", total_count);

    let mut updated_count = 0;
    let mut added_count = 0;

    // Process dependencies
    for (line, version) in build_set {
        let cache_dir = cache_base().join(&line.path).join(version.to_string());
        let cache_marker = cache_dir.join(".pcbcache");

        // Read hashes from cache marker
        let marker_content = std::fs::read_to_string(&cache_marker).map_err(|e| {
            anyhow::anyhow!("Missing cache marker for {}@{}: {}", line.path, version, e)
        })?;

        let mut lines = marker_content.lines();
        let content_hash = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("Invalid cache marker format: missing content hash"))?
            .to_string();
        let manifest_hash = lines.next().map(|s| s.to_string());

        // Check against existing lockfile entry
        if let Some(existing) = lockfile.get(&line.path, &version.to_string()) {
            // Verify hashes match
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
            updated_count += 1;
        } else {
            added_count += 1;
        }

        lockfile.insert(LockEntry {
            module_path: line.path.clone(),
            version: version.to_string(),
            content_hash,
            manifest_hash,
        });
    }

    // Process assets
    for (module_path, ref_str) in asset_set {
        let cache_dir = cache_base().join(module_path).join(ref_str);
        let cache_marker = cache_dir.join(".pcbcache");

        // Read hashes from cache marker
        let marker_content = std::fs::read_to_string(&cache_marker).map_err(|e| {
            anyhow::anyhow!(
                "Missing cache marker for {}@{}: {}",
                module_path,
                ref_str,
                e
            )
        })?;

        let mut lines = marker_content.lines();
        let content_hash = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("Invalid cache marker format: missing content hash"))?
            .to_string();
        let manifest_hash = lines.next().map(|s| s.to_string());

        // Check if this is a new entry or update
        if lockfile.get(module_path, ref_str).is_none() {
            added_count += 1;
        } else {
            updated_count += 1;
        }

        lockfile.insert(LockEntry {
            module_path: module_path.clone(),
            version: ref_str.clone(),
            content_hash,
            manifest_hash,
        });
    }

    if added_count > 0 || updated_count > 0 {
        println!(
            "  Summary: {} added, {} verified",
            added_count, updated_count
        );
    }

    Ok(lockfile)
}
