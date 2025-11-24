use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use pcb_zen_core::config::{
    find_workspace_root, DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml,
};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use walkdir::WalkDir;

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
    dependencies: HashMap<String, DependencySpec>,
    assets: HashMap<String, pcb_zen_core::AssetDependencySpec>,
}

impl PackageManifest {
    fn from_v2(v2: &pcb_zen_core::config::PcbTomlV2) -> Self {
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
    if let PcbToml::V2(_) = config {
        return Ok(Some(resolve_dependencies(
            &*file_provider,
            &workspace_root,
            &config,
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
    config: &PcbToml,
) -> Result<ResolutionResult> {
    let PcbToml::V2(v2) = config else {
        unreachable!("resolve_dependencies called on non-V2 config");
    };

    // Validate that patches are only at workspace root
    if !v2.patch.is_empty() && v2.workspace.is_none() {
        anyhow::bail!(
            "[patch] section is only allowed at workspace root\n  \
            Found in non-workspace pcb.toml at: {}\n  \
            Move [patch] to workspace root or remove it.",
            workspace_root.join("pcb.toml").display()
        );
    }

    println!("V2 Dependency Resolution");
    println!("Workspace root: {}", workspace_root.display());

    // Load existing lockfile if present - used for preseeding and verification
    let lockfile_path = workspace_root.join("pcb.sum");
    let existing_lockfile = if lockfile_path.exists() {
        println!("Loading pcb.sum...");
        let content = std::fs::read_to_string(&lockfile_path)?;
        let lockfile = Lockfile::parse(&content)?;
        println!("  Loaded lockfile");
        Some(lockfile)
    } else {
        println!("No pcb.sum found (will be created)");
        None
    };

    // Discover member packages
    let member_patterns = v2
        .workspace
        .as_ref()
        .map(|w| w.members.as_slice())
        .unwrap_or(&[]);
    let mut packages = discover_packages(file_provider, workspace_root, member_patterns)?;

    // Check if workspace root itself is also a package
    if v2.package.is_some() {
        let root_pcb_toml = workspace_root.join("pcb.toml");
        packages.insert(0, (root_pcb_toml, config.clone()));
    }

    // Display workspace type
    if v2.workspace.is_some() {
        println!("Type: Explicit workspace");
        if !member_patterns.is_empty() {
            println!("Member patterns: {:?}", member_patterns);
        }
    } else {
        println!("Type: Standalone package (implicit workspace)");
    }

    // Display discovered packages
    println!("\nDiscovered {} package(s):", packages.len());
    for (pcb_toml_path, config) in &packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_name = pcb_toml_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| "unknown".into());

        if let Some(board) = &v2.board {
            println!(
                "  - {} (board: {}) → {}",
                package_name,
                board.name,
                pcb_toml_path.display()
            );
        } else {
            println!("  - {} → {}", package_name, pcb_toml_path.display());
        }
    }

    // Build workspace members index: inferred_package_path -> (dir, config)
    // Package paths are inferred from workspace.path + relative directory
    let mut workspace_members: HashMap<String, (PathBuf, pcb_zen_core::config::PcbTomlV2)> =
        HashMap::new();
    
    if let Some(workspace_path) = v2.workspace.as_ref().and_then(|w| w.path.as_ref()) {
        for (pcb_toml_path, config) in &packages {
            let PcbToml::V2(v2) = config else { continue };
            
            // Skip if no package section
            if v2.package.is_none() {
                continue;
            }
            
            let package_dir = pcb_toml_path.parent().unwrap().to_path_buf();
            
            // Infer package path from workspace.path + relative directory
            if let Ok(relative_path) = package_dir.strip_prefix(workspace_root) {
                let relative_str = relative_path.to_string_lossy();
                let inferred_path = if relative_str.is_empty() {
                    // Root package at workspace root
                    workspace_path.clone()
                } else {
                    // Member package: workspace.path + relative directory
                    format!("{}/{}", workspace_path, relative_str)
                };
                
                workspace_members.insert(inferred_path, (package_dir, v2.clone()));
            }
        }
    }

    let patches = v2.patch.clone();

    // MVS state
    let mut selected: HashMap<ModuleLine, Version> = HashMap::new();
    let mut work_queue: VecDeque<ModuleLine> = VecDeque::new();
    let mut manifest_cache: HashMap<(ModuleLine, Version), PackageManifest> = HashMap::new();

    println!("\nPhase 0: Seed from workspace dependencies");

    // Preseed from pcb.sum to skip sequential discovery
    if let Some(ref lockfile) = existing_lockfile {
        println!("  Preseeding from pcb.sum...");
        let mut preseed_count = 0;
        // Collect all unique module paths from lockfile (skip assets)
        let mut seen_modules = HashSet::new();
        for entry in lockfile.iter() {
            // Skip asset entries (no manifest hash)
            if entry.manifest_hash.is_none() {
                continue;
            }

            if seen_modules.insert(entry.module_path.clone()) {
                if let Ok(version) = Version::parse(&entry.version) {
                    let line = ModuleLine::new(entry.module_path.clone(), &version);
                    // Tentatively select this version (MVS may upgrade later)
                    if !selected.contains_key(&line) {
                        selected.insert(line.clone(), version);
                        work_queue.push_back(line);
                        preseed_count += 1;
                    }
                }
            }
        }
        println!("  Preseeded {} modules from lockfile", preseed_count);
    }
    println!();

    // Resolve dependencies per-package
    println!("Per-Package Dependency Resolution:");

    for (pcb_toml_path, config) in &packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_name = pcb_toml_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

        // Validate no patches in member packages
        if !v2.patch.is_empty() {
            anyhow::bail!(
                "[patch] section is only allowed at workspace root\n  \
                Found in package: {}\n  \
                Location: {}\n  \
                Move [patch] to workspace root.",
                package_name,
                pcb_toml_path.display()
            );
        }

        println!("\n  Package: {}", package_name);

        // Collect this package's dependencies
        let package_deps = collect_package_dependencies(pcb_toml_path, v2, workspace_root)?;

        if package_deps.is_empty() {
            println!("    No dependencies");
            continue;
        }

        println!("    Seeding {} dependencies into MVS:", package_deps.len());

        // Seed MVS state from this package's dependencies
        for dep in &package_deps {
            // Skip local path dependencies (resolved per-package in build_resolution_map)
            if let DependencySpec::Detailed(detail) = &dep.spec {
                if detail.path.is_some() {
                    println!("      - {} (local path)", dep.url);
                    continue;
                }
            }

            // Resolve to concrete version (handles branches/revs)
            match resolve_to_version(&dep.url, &dep.spec, existing_lockfile.as_ref()) {
                Ok(version) => {
                    println!("      - {}@v{}", dep.url, version);
                    add_requirement(
                        dep.url.clone(),
                        version,
                        &mut selected,
                        &mut work_queue,
                        &patches,
                    );
                }
                Err(e) => {
                    eprintln!("      Warning: Failed to resolve {}: {}", dep.url, e);
                }
            }
        }
    }

    println!("\nPhase 1: Discovery + MVS fixed point");
    println!("Initial work queue: {} modules", work_queue.len());
    println!();

    // Phase 1: Iteratively fetch manifests and discover transitive dependencies
    let mut iterations = 0;
    while let Some(line) = work_queue.pop_front() {
        iterations += 1;
        let version = selected[&line].clone();

        // Check if we already fetched this exact version
        if manifest_cache.contains_key(&(line.clone(), version.clone())) {
            continue;
        }

        println!(
            "  [{}] Fetching {}@v{} ({})",
            iterations, line.path, version, line.family
        );

        // Fetch the manifest for this version (workspace members + patches applied inside)
        match fetch_manifest(
            workspace_root,
            &line.path,
            &version,
            &patches,
            &workspace_members,
        ) {
            Ok(manifest) => {
                if !manifest.dependencies.is_empty() {
                    println!("      Found {} dependencies", manifest.dependencies.len());
                }

                // Cache the manifest
                manifest_cache.insert((line.clone(), version.clone()), manifest.clone());

                // Add requirements from this manifest
                for (dep_path, dep_spec) in &manifest.dependencies {
                    // Skip local path dependencies
                    if is_non_version_dep(dep_spec) {
                        continue;
                    }

                    match resolve_to_version(dep_path, dep_spec, existing_lockfile.as_ref()) {
                        Ok(dep_version) => {
                            println!("        requires {}@v{}", dep_path, dep_version);
                            add_requirement(
                                dep_path.clone(),
                                dep_version,
                                &mut selected,
                                &mut work_queue,
                                &patches,
                            );
                        }
                        Err(e) => {
                            eprintln!("        Warning: Failed to resolve {}: {}", dep_path, e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("      Warning: {}", e);
            }
        }
    }

    println!("\nFixed point reached after {} iterations", iterations);

    println!("\nPhase 2: Build closure");
    println!();

    // Phase 2: Build the final dependency set using only selected versions
    let build_set = build_closure(
        &packages,
        &selected,
        &manifest_cache,
        &workspace_members,
        workspace_root,
    )?;

    println!("Build set: {} dependencies", build_set.len());

    // Print dependency tree
    print_dependency_tree(&packages, workspace_root, &build_set, &manifest_cache)?;

    // Phase 2.5: Collect and fetch assets
    println!("\nPhase 2.5: Fetching assets");
    let asset_set = collect_and_fetch_assets(
        &packages,
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

    // Phase 3: Fetch full repository contents for the build set
    println!("\nPhase 3: Fetching full repository contents");
    fetch_full_contents(workspace_root, &build_set, existing_lockfile.as_ref())?;

    // Phase 4: Update lockfile with cryptographic hashes
    println!("\nPhase 4: Updating lockfile");
    let lockfile = update_lockfile(existing_lockfile, &build_set)?;

    // Write lockfile to disk
    let lockfile_path = workspace_root.join("pcb.sum");
    std::fs::write(&lockfile_path, lockfile.to_string())?;
    println!("  Written to {}", lockfile_path.display());

    println!("\nV2 dependency resolution complete");

    let package_paths = packages.iter().map(|(path, _)| path.clone()).collect();

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    let package_resolutions = build_resolution_map(&ResolutionContext {
        workspace_root,
        home_dir: &home,
        root_config: config,
        packages: &packages,
        selected: &selected,
        patches: &patches,
        manifest_cache: &manifest_cache,
        workspace_members: &workspace_members,
    })?;

    Ok(ResolutionResult {
        workspace_root: workspace_root.to_path_buf(),
        package_resolutions,
        packages: package_paths,
    })
}

/// Resolution context for building load maps
struct ResolutionContext<'a> {
    workspace_root: &'a Path,
    home_dir: &'a Path,
    root_config: &'a PcbToml,
    packages: &'a [(PathBuf, PcbToml)],
    selected: &'a HashMap<ModuleLine, Version>,
    patches: &'a HashMap<String, PatchSpec>,
    manifest_cache: &'a HashMap<(ModuleLine, Version), PackageManifest>,
    workspace_members: &'a HashMap<String, (PathBuf, pcb_zen_core::config::PcbTomlV2)>,
}

/// Build the per-package resolution map
fn build_resolution_map(
    ctx: &ResolutionContext,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    // Helper to compute absolute path for a module line
    let get_abs_path = |line: &ModuleLine, _version: &Version| -> PathBuf {
        // 1. Check workspace members first
        if let Some((member_dir, _)) = ctx.workspace_members.get(&line.path) {
            return member_dir.clone();
        }
        // 2. Check patches
        if let Some(patch) = ctx.patches.get(&line.path) {
            return ctx.workspace_root.join(&patch.path);
        }
        // 3. Fall back to cache
        ctx.home_dir
            .join(".pcb")
            .join("cache")
            .join(&line.path)
            .join(_version.to_string())
    };

    // Build lookup: url -> family -> path
    let mut url_to_families: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    for (line, version) in ctx.selected {
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
            if let Some(patch) = ctx.patches.get(url) {
                return Ok(ctx.workspace_root.join(&patch.path));
            }

            // Cache path: ~/.pcb/cache/{url}/{ref}/
            let ref_str = extract_asset_ref(asset_spec)?;
            Ok(ctx
                .home_dir
                .join(".pcb")
                .join("cache")
                .join(url)
                .join(ref_str))
        };

    // Build deps map for a package with base directory for local path resolution
    // V2 does not support aliases - only canonical URL dependencies
    let build_map = |base_dir: &Path,
                     pkg_deps: &HashMap<String, DependencySpec>,
                     pkg_assets: &HashMap<String, pcb_zen_core::AssetDependencySpec>|
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
    if let PcbToml::V2(v2) = ctx.root_config {
        results.insert(
            ctx.workspace_root.to_path_buf(),
            build_map(ctx.workspace_root, &v2.dependencies, &v2.assets),
        );
    }

    // Member packages
    for (pkg_path, config) in ctx.packages {
        if let PcbToml::V2(v2) = config {
            let pkg_dir = pkg_path.parent().unwrap();
            results.insert(
                pkg_dir.to_path_buf(),
                build_map(pkg_dir, &v2.dependencies, &v2.assets),
            );
        }
    }

    // Transitive deps: each remote package gets its own resolution map
    // This ensures MVS-selected versions are used for all transitive loads
    // Assets come from the cached manifest (no duplicate file I/O)
    for (line, version) in ctx.selected {
        let abs_path = get_abs_path(line, version);
        if let Some(manifest) = ctx.manifest_cache.get(&(line.clone(), version.clone())) {
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
    v2_config: &pcb_zen_core::config::PcbTomlV2,
    workspace_root: &Path,
) -> Result<Vec<UnresolvedDep>> {
    let package_dir = pcb_toml_path.parent().unwrap();
    let mut deps = HashMap::new();

    collect_deps_recursive(
        &v2_config.dependencies,
        package_dir,
        workspace_root,
        &mut deps,
    )?;

    Ok(deps.into_values().collect())
}

/// Recursively collect dependencies, handling transitive local path dependencies
fn collect_deps_recursive(
    current_deps: &HashMap<String, DependencySpec>,
    package_dir: &Path,
    workspace_root: &Path,
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

        // Check if path points inside workspace
        if let Ok(rel_path) = resolved_path.strip_prefix(workspace_root) {
            anyhow::bail!(
                "Path dependency '{}' points inside workspace\n  \
                Path: {}\n  \
                Workspace members are automatically resolved - remove the 'path' field.\n  \
                Use: \"{}\" = \"{}\"",
                url,
                rel_path.display(),
                url,
                _expected_version.unwrap_or(&"0.1".to_string())
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
            Ok(PcbToml::V2(config)) => config,
            Ok(PcbToml::V1(_)) => {
                return Err(PathDepError::V1Package {
                    url: url.clone(),
                    location: dep_pcb_toml.clone(),
                }
                .into());
            }
            Err(e) => return Err(e),
        };

        collect_deps_recursive(
            &dep_config.dependencies,
            &resolved_path,
            workspace_root,
            deps,
        )?;
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

/// Cache decision for dependency fetching
enum CacheDecision {
    Fetch,
    Use,
}

/// Decide whether to fetch or use cached dependency
fn decide_cache_usage(
    cache_dir: &Path,
    cache_marker: &Path,
    lockfile: Option<&Lockfile>,
    module_path: &str,
    version: &Version,
) -> Result<CacheDecision> {
    // No cache marker -> fetch
    if !cache_marker.exists() {
        return Ok(CacheDecision::Fetch);
    }

    // No lockfile -> trust cache
    let Some(lockfile) = lockfile else {
        println!("    {}@v{} (cached)", module_path, version);
        return Ok(CacheDecision::Use);
    };

    // Lockfile present but no entry -> trust cache
    let Some(locked_entry) = lockfile.get(module_path, &version.to_string()) else {
        println!("    {}@v{} (cached)", module_path, version);
        return Ok(CacheDecision::Use);
    };

    // Verify hashes against lockfile
    let marker_content = std::fs::read_to_string(cache_marker)?;
    let mut lines = marker_content.lines();
    let cached_content = lines.next().unwrap_or("");
    let cached_manifest = lines.next();

    if cached_content != locked_entry.content_hash {
        println!(
            "    {}@v{} (cache mismatch, re-fetching)",
            module_path, version
        );
        std::fs::remove_dir_all(cache_dir)?;
        return Ok(CacheDecision::Fetch);
    }

    if let (Some(cm), Some(lm)) = (cached_manifest, &locked_entry.manifest_hash) {
        if cm != lm {
            println!(
                "    {}@v{} (manifest mismatch, re-fetching)",
                module_path, version
            );
            std::fs::remove_dir_all(cache_dir)?;
            return Ok(CacheDecision::Fetch);
        }
    }

    println!("    {}@v{} (cached, verified)", module_path, version);
    Ok(CacheDecision::Use)
}

/// Validate that a code dependency has pcb.toml manifest
fn validate_code_dep_has_manifest(cache_dir: &Path, module_path: &str) -> Result<()> {
    let pcb_toml_path = cache_dir.join("pcb.toml");
    if !pcb_toml_path.exists() {
        anyhow::bail!(
            "Code dependency '{}' missing pcb.toml\n  \
            If this is an asset repository, declare it under [assets] instead.",
            module_path
        );
    }
    Ok(())
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
    packages: &[(PathBuf, PcbToml)],
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
    selected: &HashMap<ModuleLine, Version>,
    workspace_root: &Path,
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
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
    for (pcb_toml_path, config) in packages {
        let PcbToml::V2(v2) = config else { continue };

        if v2.assets.is_empty() {
            continue;
        }

        let package_name = pcb_toml_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

        println!("\n  Package: {}", package_name);
        println!("    {} assets", v2.assets.len());

        for (module_path, asset_spec) in &v2.assets {
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

/// Fetch a manifest (both dependencies and assets) from Git using sparse checkout
fn fetch_manifest(
    workspace_root: &Path,
    module_path: &str,
    version: &Version,
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
    workspace_members: &HashMap<String, (PathBuf, pcb_zen_core::config::PcbTomlV2)>,
) -> Result<PackageManifest> {
    // 1. Workspace member override (highest priority)
    if let Some((_member_dir, member_v2)) = workspace_members.get(module_path) {
        println!("      Using workspace member: {}", module_path);
        return Ok(PackageManifest::from_v2(member_v2));
    }

    // 2. Check if this module is patched with a local path
    if let Some(patch) = patches.get(module_path) {
        let patched_path = workspace_root.join(&patch.path);
        let patched_toml = patched_path.join("pcb.toml");

        println!("      Using patched source: {}", patch.path);

        if !patched_toml.exists() {
            anyhow::bail!("Patch path {} has no pcb.toml", patched_path.display());
        }

        return read_manifest_from_path(&patched_toml, module_path);
    }

    // Cache directory: ~/.pcb/cache/{module_path}/{version}/
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let cache_base = home.join(".pcb").join("cache");
    let checkout_dir = cache_base.join(module_path).join(version.to_string());

    // Use sparse checkout to fetch the module (shared with Phase 3)
    println!("        Cloning (sparse checkout)");

    let package_root =
        ensure_sparse_checkout(&checkout_dir, module_path, &version.to_string(), true)?;
    let pcb_toml_path = package_root.join("pcb.toml");

    // Read the manifest
    read_manifest_from_path(&pcb_toml_path, module_path)
}

/// Read and parse a pcb.toml manifest (both dependencies and assets)
fn read_manifest_from_path(pcb_toml_path: &Path, _module_path: &str) -> Result<PackageManifest> {
    // Check if pcb.toml exists (code packages must have manifests in V2)
    if !pcb_toml_path.exists() {
        anyhow::bail!(
            "Code dependency missing pcb.toml manifest\n  \
            If this is an asset repository (e.g., KiCad libraries), declare it under [assets] instead."
        );
    }

    let file_provider = DefaultFileProvider::new();
    let config = PcbToml::from_file(&file_provider, pcb_toml_path)?;

    match config {
        PcbToml::V2(v2) => Ok(PackageManifest::from_v2(&v2)),
        PcbToml::V1(_) => {
            // V1 packages don't have assets
            anyhow::bail!(
                "Failed to parse pcb.toml as V2 manifest\n  \
                If this is an asset repository, declare it under [assets] instead."
            );
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
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
) -> Result<PathBuf> {
    use std::fs;

    // 1. Check if this module is patched with a local path
    if let Some(patch) = patches.get(module_path) {
        let patched_path = workspace_root.join(&patch.path);
        let patched_toml = patched_path.join("pcb.toml");

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

        // Verify patch target does NOT have pcb.toml (asset constraint)
        if patched_toml.exists() {
            anyhow::bail!(
                "Asset '{}' is patched to a path with pcb.toml\n  \
                Patch path: {}\n  \
                Assets must not have manifests. Declare this under [dependencies] instead.",
                module_path,
                patched_path.display()
            );
        }

        return Ok(patched_path);
    }

    // Cache directory: ~/.pcb/cache/{module_path}/{ref}/
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let cache_base = home.join(".pcb").join("cache");
    let checkout_dir = cache_base.join(module_path).join(ref_str);

    // Check if already fetched
    if checkout_dir.exists() && checkout_dir.join(".pcbcache").exists() {
        println!("        Cached");
        return Ok(checkout_dir);
    }

    // Use sparse checkout to fetch the asset repo
    println!("        Cloning (sparse checkout)");

    let package_root = ensure_sparse_checkout(&checkout_dir, module_path, ref_str, false)?;

    // Verify no pcb.toml exists (strict asset constraint)
    let pcb_toml_path = package_root.join("pcb.toml");
    if pcb_toml_path.exists() {
        // Clean up the checkout
        let _ = fs::remove_dir_all(&checkout_dir);
        anyhow::bail!(
            "Asset '{}' has a pcb.toml manifest\n  \
            Location: {}\n  \
            Assets must not have manifests. Declare this under [dependencies] instead.",
            module_path,
            pcb_toml_path.display()
        );
    }

    Ok(package_root)
}

/// Print a tree view of the dependency graph (cargo tree style)
fn print_dependency_tree(
    packages: &[(PathBuf, PcbToml)],
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
    for (pcb_toml_path, config) in packages {
        let PcbToml::V2(v2) = config else { continue };
        let package_deps = collect_package_dependencies(pcb_toml_path, v2, workspace_root)?;

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
    packages: &[(PathBuf, PcbToml)],
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
    workspace_members: &HashMap<String, (PathBuf, pcb_zen_core::config::PcbTomlV2)>,
    workspace_root: &Path,
) -> Result<HashSet<(ModuleLine, Version)>> {
    let mut build_set = HashSet::new();
    let mut stack = Vec::new();

    // Build index: module_path → ModuleLine for fast lookups (excluding workspace members)
    let mut line_by_path: HashMap<String, Vec<ModuleLine>> = HashMap::new();
    for line in selected.keys() {
        // Skip workspace members - they don't need to be fetched
        if workspace_members.contains_key(&line.path) {
            continue;
        }
        line_by_path
            .entry(line.path.clone())
            .or_default()
            .push(line.clone());
    }

    // Seed DFS from all package dependencies
    for (pcb_toml_path, config) in packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_deps = collect_package_dependencies(pcb_toml_path, v2, workspace_root)?;

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
    let (commit, git_url) = git_ls_remote_with_fallback(module_path, &refspec)?;

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
    use std::process::Command;

    // Get a minimal clone to inspect the commit
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let temp_clone = home
        .join(".pcb")
        .join("cache")
        .join("temp")
        .join(module_path);

    std::fs::create_dir_all(temp_clone.parent().unwrap())?;

    // Clone if needed (shallow)
    if !temp_clone.join(".git").exists() {
        Command::new("git")
            .arg("clone")
            .arg("--bare")
            .arg("--filter=blob:none")
            .arg(git_url)
            .arg(&temp_clone)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    } else {
        // Fetch updates
        Command::new("git")
            .arg("-C")
            .arg(&temp_clone)
            .arg("fetch")
            .arg("origin")
            .arg("--tags")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
    }

    // Find latest tag reachable from this commit
    let describe_output = Command::new("git")
        .arg("-C")
        .arg(&temp_clone)
        .arg("describe")
        .arg("--tags")
        .arg("--abbrev=0")
        .arg(commit)
        .output();

    let base_version = if let Ok(output) = describe_output {
        if output.status.success() {
            let tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Parse tag (remove leading v)
            parse_version_string(&tag).unwrap_or_else(|_| Version::new(0, 0, 0))
        } else {
            Version::new(0, 0, 0)
        }
    } else {
        Version::new(0, 0, 0)
    };

    // Increment patch version
    let pseudo_base = Version::new(
        base_version.major,
        base_version.minor,
        base_version.patch + 1,
    );

    // Get commit timestamp
    let timestamp_output = Command::new("git")
        .arg("-C")
        .arg(&temp_clone)
        .arg("show")
        .arg("-s")
        .arg("--format=%ct")
        .arg(commit)
        .output()?;

    let timestamp = if timestamp_output.status.success() {
        String::from_utf8_lossy(&timestamp_output.stdout)
            .trim()
            .parse::<i64>()
            .unwrap_or_else(|_| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64
            })
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    };

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
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
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

/// Discover V2 packages matching glob patterns
fn discover_packages(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    patterns: &[String],
) -> Result<Vec<(PathBuf, PcbToml)>> {
    if patterns.is_empty() {
        return Ok(vec![]);
    }

    // Build glob matchers
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern)?);
        // Match pattern without /* suffix for exact directory matches
        if let Some(exact) = pattern.strip_suffix("/*") {
            builder.add(Glob::new(exact)?);
        }
    }
    let glob_set = builder.build()?;

    // Walk workspace and collect matching V2 packages
    let mut packages = Vec::new();
    for entry in WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.path().is_dir() {
            continue;
        }

        if let Ok(rel_path) = entry.path().strip_prefix(workspace_root) {
            if glob_set.is_match(rel_path) {
                let pcb_toml = entry.path().join("pcb.toml");
                if file_provider.exists(&pcb_toml) {
                    if let Ok(config) = PcbToml::from_file(file_provider, &pcb_toml) {
                        if matches!(config, PcbToml::V2(_)) {
                            packages.push((pcb_toml, config));
                        }
                    }
                }
            }
        }
    }

    Ok(packages)
}

/// Check if a directory is a package root (has pcb.toml with [package])
fn is_package_dir(dir: &Path) -> bool {
    let pcb_toml = dir.join("pcb.toml");
    if !pcb_toml.is_file() {
        return false;
    }

    let file_provider = DefaultFileProvider::new();
    if let Ok(config) = PcbToml::from_file(&file_provider, &pcb_toml) {
        matches!(config, PcbToml::V2(ref v2) if v2.package.is_some())
    } else {
        false
    }
}

/// Create a canonical, deterministic tar archive from a directory
///
/// Rules from packaging.md:
/// - Regular files and directories only (no symlinks, devices)
/// - Relative paths, forward slashes, lexicographic order
/// - Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
/// - File mode: 0644, directory mode: 0755
/// - End with two 512-byte zero blocks
/// - Respect .gitignore and filter internal marker files
/// - Exclude nested packages (subdirs with pcb.toml + [package])
pub fn create_canonical_tar<W: std::io::Write>(dir: &Path, writer: W) -> Result<()> {
    use std::fs;
    use tar::{Builder, Header};

    let mut builder = Builder::new(writer);

    // Use PAX format for long path support (KiCad footprints have very long paths)
    builder.mode(tar::HeaderMode::Deterministic);

    // Collect all files and directories, sorted lexicographically
    // Use ignore crate to respect .gitignore
    let mut entries = Vec::new();
    let package_root = dir.to_path_buf(); // Clone for closure
    for result in WalkBuilder::new(dir)
        .hidden(false) // Don't skip hidden files (we want .zen files if hidden)
        .git_ignore(true) // Respect .gitignore
        .git_global(false) // Don't use global gitignore
        .git_exclude(true) // Respect .git/info/exclude
        .filter_entry(move |entry| {
            let path = entry.path();

            // Filter out .git directory entirely (don't descend into it)
            if let Some(file_name) = entry.file_name().to_str() {
                if file_name == ".git" {
                    return false;
                }
            }

            // Prune nested packages: if this is a directory with pcb.toml + [package],
            // and it's not the root we're packaging, exclude it and its entire subtree
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && path != package_root
                && is_package_dir(path)
            {
                return false; // Prune this entire subtree
            }

            true
        })
        .build()
    {
        let entry = result?;
        let path = entry.path();

        // Skip internal marker files (.full-checkout, .pcbcache)
        if let Some(file_name) = path.file_name() {
            let name = file_name.to_str().unwrap_or("");
            if name == ".full-checkout" || name == ".pcbcache" {
                continue;
            }
        }

        // Get relative path
        let rel_path = match path.strip_prefix(dir) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if rel_path == Path::new("") {
            continue; // Skip root
        }

        let file_type = entry.file_type().unwrap();
        entries.push((rel_path.to_path_buf(), file_type));
    }

    // Sort lexicographically
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Add entries to tar
    for (rel_path, file_type) in entries {
        let full_path = dir.join(&rel_path);
        let path_str = rel_path.to_str().unwrap().replace('\\', "/");

        if file_type.is_dir() {
            // Directory entry - use append_path_with_contents for automatic long path handling
            let mut header = Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o755);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_username("")?;
            header.set_groupname("")?;
            header.set_entry_type(tar::EntryType::Directory);

            // Use append_data which handles long paths via PAX extensions
            builder.append_data(&mut header, &path_str, &[][..])?;
        } else if file_type.is_file() {
            // Regular file - use append_data for automatic long path handling
            let content = fs::read(&full_path)?;
            let mut header = Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_username("")?;
            header.set_groupname("")?;
            header.set_entry_type(tar::EntryType::Regular);

            // Use append_data which handles long paths via PAX extensions
            builder.append_data(&mut header, &path_str, &content[..])?;
        }
        // Skip symlinks and other special files
    }

    // Finish tar (adds two 512-byte zero blocks)
    builder.finish()?;

    Ok(())
}

/// Compute content hash from a directory
///
/// Creates canonical USTAR tarball from directory, streams to BLAKE3 hasher.
/// Format: h1:<base64-encoded-blake3>
fn compute_content_hash_from_dir(cache_dir: &Path) -> Result<String> {
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
fn compute_manifest_hash(manifest_content: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let hash = blake3::hash(manifest_content.as_bytes());

    format!("h1:{}", STANDARD.encode(hash.as_bytes()))
}

/// Fetch full repository contents for all dependencies in the build set
///
/// Computes content and manifest hashes and stores them in .pcbcache marker.
/// If lockfile is provided, verifies cached content against locked hashes.
fn fetch_full_contents(
    _workspace_root: &Path,
    build_set: &HashSet<(ModuleLine, Version)>,
    lockfile: Option<&Lockfile>,
) -> Result<()> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    println!("  Fetching {} dependencies...", build_set.len());

    for (line, version) in build_set {
        let cache_dir = home
            .join(".pcb")
            .join("cache")
            .join(&line.path)
            .join(version.to_string());

        let cache_marker = cache_dir.join(".pcbcache");

        let decision =
            decide_cache_usage(&cache_dir, &cache_marker, lockfile, &line.path, version)?;

        if let CacheDecision::Use = decision {
            validate_code_dep_has_manifest(&cache_dir, &line.path)?;
            continue;
        } else {
            // Cache miss - fetch and hash
            println!("    {}@v{}...", line.path, version);

            let package_root =
                ensure_sparse_checkout(&cache_dir, &line.path, &version.to_string(), true)?;

            print!("      Computing hashes... ");
            std::io::Write::flush(&mut std::io::stdout())?;

            let content_hash = compute_content_hash_from_dir(&package_root)?;

            validate_code_dep_has_manifest(&package_root, &line.path)?;

            let pcb_toml_path = package_root.join("pcb.toml");
            let manifest_content = std::fs::read_to_string(&pcb_toml_path)?;
            let manifest_hash = compute_manifest_hash(&manifest_content);

            let marker_content = format!("{}\n{}\n", content_hash, manifest_hash);
            std::fs::write(&cache_marker, marker_content)?;

            println!("done");
        }
    }

    Ok(())
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
    let (repo_url, subpath) = split_repo_and_subpath(module_path);
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
    run_git_in(&["init"], checkout_dir)?;

    // Add remote (ignore errors if already exists)
    let https_url = format!("https://{}.git", repo_url);
    let _ = run_git_in(&["remote", "add", "origin", &https_url], checkout_dir);

    // Configure as promisor remote for partial clone (required for --filter=blob:none to work)
    run_git_in(&["config", "remote.origin.promisor", "true"], checkout_dir)?;
    run_git_in(
        &["config", "remote.origin.partialclonefilter", "blob:none"],
        checkout_dir,
    )?;

    // Try HTTPS fetch, fallback to SSH if needed, fallback to no-v-prefix
    let ssh_url = format_ssh_url(repo_url);
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

    let mut fetch_succeeded = false;

    // Try HTTPS first
    let fetch_result = std::process::Command::new("git")
        .args(&fetch_args)
        .current_dir(checkout_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(status) = fetch_result {
        if status.success() {
            fetch_succeeded = true;
        }
    }

    // Try SSH if HTTPS failed
    if !fetch_succeeded {
        run_git_in(&["remote", "set-url", "origin", &ssh_url], checkout_dir)?;
        run_git_in(&fetch_args, checkout_dir)?;
    }

    // Configure sparse-checkout for nested packages (fetch only the subpath)
    if !subpath.is_empty() {
        run_git_in(&["sparse-checkout", "init", "--cone"], checkout_dir)?;
        run_git_in(&["sparse-checkout", "set", subpath], checkout_dir)?;
    }

    // Checkout and materialize the fetched ref
    run_git_in(&["reset", "--hard", "FETCH_HEAD"], checkout_dir)?;

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

/// Extract repository boundary and subpath from module path
///
/// Go convention: host/user/repo is the repository boundary
/// For GitHub: github.com/user/repo
/// For GitLab: gitlab.com/user/project (or nested groups: gitlab.com/group/subgroup/project)
///
/// Returns (repo_url, subpath)
/// Examples:
/// - github.com/diodeinc/registry → ("github.com/diodeinc/registry", "")
/// - github.com/diodeinc/registry/components/2N7002 → ("github.com/diodeinc/registry", "components/2N7002")
/// - gitlab.com/kicad/libraries/kicad-symbols → ("gitlab.com/kicad/libraries/kicad-symbols", "")
fn split_repo_and_subpath(module_path: &str) -> (&str, &str) {
    let parts: Vec<&str> = module_path.split('/').collect();

    if parts.is_empty() {
        return (module_path, "");
    }

    let host = parts[0];

    // For GitHub only: repository is host/user/repo (first 3 segments), everything after is subpath
    // For GitLab: support nested groups, so treat entire path as repo (no subpath support for now)
    if host == "github.com" {
        if parts.len() <= 3 {
            // No subpath (just host/user/repo or less)
            return (module_path, "");
        }

        // Split at the 3rd slash: host/user/repo | subpath
        let repo_boundary = parts[..3].join("/");

        // Return borrowed slices by finding the boundary position in the original string
        let boundary_len = repo_boundary.len();
        let repo_url = &module_path[..boundary_len];
        let subpath_str = if boundary_len < module_path.len() {
            &module_path[boundary_len + 1..] // Skip the '/' separator
        } else {
            ""
        };

        (repo_url, subpath_str)
    } else {
        // GitLab and other hosts: treat entire path as repo (GitLab supports nested groups)
        (module_path, "")
    }
}

/// Convert module path to SSH URL format
///
/// Examples:
/// - github.com/user/repo → git@github.com:user/repo.git
/// - gitlab.com/user/repo → git@gitlab.com:user/repo.git
fn format_ssh_url(module_path: &str) -> String {
    let parts: Vec<&str> = module_path.splitn(2, '/').collect();
    if parts.len() == 2 {
        let host = parts[0];
        let path = parts[1];
        format!("git@{}:{}.git", host, path)
    } else {
        // Fallback to HTTPS format if parsing fails
        format!("https://{}.git", module_path)
    }
}

fn run_git_in(args: &[&str], dir: &Path) -> Result<()> {
    use std::process::Command;
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("git command failed: git {}", args.join(" "));
    }
    Ok(())
}

/// Execute git ls-remote with HTTPS→SSH fallback
///
/// Returns (commit_hash, git_url_used)
fn git_ls_remote_with_fallback(module_path: &str, refspec: &str) -> Result<(String, String)> {
    use std::process::Command;

    let (repo_url, _) = split_repo_and_subpath(module_path);
    let https_url = format!("https://{}.git", repo_url);
    let ssh_url = format_ssh_url(repo_url);

    // Try HTTPS first
    let output = Command::new("git")
        .arg("ls-remote")
        .arg(&https_url)
        .arg(refspec)
        .output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let commit = stdout
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .ok_or_else(|| anyhow::anyhow!("Ref {} not found in {}", refspec, module_path))?;
        return Ok((commit.to_string(), https_url));
    }

    // Fallback to SSH
    let output = Command::new("git")
        .arg("ls-remote")
        .arg(&ssh_url)
        .arg(refspec)
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to ls-remote {} for {} (tried HTTPS and SSH)",
            refspec,
            module_path
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commit = stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| anyhow::anyhow!("Ref {} not found in {}", refspec, module_path))?;
    Ok((commit.to_string(), ssh_url))
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
) -> Result<Lockfile> {
    // Start with existing lockfile or create new one
    let mut lockfile = existing_lockfile.unwrap_or_default();

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;

    println!("  Reading hashes for {} dependencies...", build_set.len());

    let mut updated_count = 0;
    let mut added_count = 0;

    for (line, version) in build_set {
        let cache_dir = home
            .join(".pcb")
            .join("cache")
            .join(&line.path)
            .join(version.to_string());

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

        // Check if this is a new entry or update
        let is_new = lockfile.get(&line.path, &version.to_string()).is_none();

        lockfile.insert(LockEntry {
            module_path: line.path.clone(),
            version: version.to_string(),
            content_hash,
            manifest_hash,
        });

        if is_new {
            added_count += 1;
            println!("    {}@v{} (added)", line.path, version);
        } else {
            updated_count += 1;
            println!("    {}@v{} (verified)", line.path, version);
        }
    }

    if added_count > 0 || updated_count > 0 {
        println!(
            "  Summary: {} added, {} verified",
            added_count, updated_count
        );
    }

    Ok(lockfile)
}
