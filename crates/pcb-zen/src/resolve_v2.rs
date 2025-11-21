use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use pcb_zen_core::config::{
    find_workspace_root, DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml,
};
use pcb_zen_core::{DefaultFileProvider, FileProvider, LoadSpec};
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

    #[error("Path dependency '{url}' missing [package] section\n  Location: {location}")]
    MissingPackageSection { url: String, location: PathBuf },

    #[error("Path dependency '{url}' missing package.path field\n  Location: {location}\n  Add: path = \"{url}\"")]
    MissingPackagePath { url: String, location: PathBuf },

    #[error("Path dependency key mismatch:\n  Dependency: '{dep_key}'\n  Declared:   '{pkg_path}'\n  Location:   {location}")]
    PathMismatch {
        dep_key: String,
        pkg_path: String,
        location: PathBuf,
    },
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

    let patches = v2.patch.clone();

    // MVS state
    let mut selected: HashMap<ModuleLine, Version> = HashMap::new();
    let mut work_queue: VecDeque<ModuleLine> = VecDeque::new();
    let mut manifest_cache: HashMap<(ModuleLine, Version), HashMap<String, DependencySpec>> =
        HashMap::new();

    println!("\nPhase 0: Seed from workspace dependencies");

    // Preseed from pcb.sum to skip sequential discovery
    if let Some(ref lockfile) = existing_lockfile {
        println!("  Preseeding from pcb.sum...");
        let mut preseed_count = 0;
        // Collect all unique module paths from lockfile
        let mut seen_modules = HashSet::new();
        for entry in lockfile.iter() {
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
        let package_deps = collect_package_dependencies(pcb_toml_path, v2)?;

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

        // Fetch the manifest for this version (patches applied inside)
        match fetch_manifest(workspace_root, &line.path, &version, &patches) {
            Ok(deps) => {
                if !deps.is_empty() {
                    println!("      Found {} dependencies", deps.len());
                }

                // Cache the manifest
                manifest_cache.insert((line.clone(), version.clone()), deps.clone());

                // Add requirements from this manifest
                for (dep_path, dep_spec) in &deps {
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
    let build_set = build_closure(&packages, &selected, &manifest_cache)?;

    println!("Build set: {} dependencies", build_set.len());

    // Print dependency tree
    print_dependency_tree(&packages, workspace_root, &build_set, &manifest_cache)?;

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

    let package_resolutions = build_resolution_map(
        workspace_root,
        &home,
        config,
        &packages,
        &selected,
        &patches,
        &manifest_cache,
    )?;

    Ok(ResolutionResult {
        workspace_root: workspace_root.to_path_buf(),
        package_resolutions,
        packages: package_paths,
    })
}

/// Build the per-package resolution map
fn build_resolution_map(
    workspace_root: &Path,
    home_dir: &Path,
    root_config: &PcbToml,
    packages: &[(PathBuf, PcbToml)],
    selected: &HashMap<ModuleLine, Version>,
    patches: &HashMap<String, PatchSpec>,
    manifest_cache: &HashMap<(ModuleLine, Version), HashMap<String, DependencySpec>>,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    // Helper to compute absolute path for a module line
    let get_abs_path = |line: &ModuleLine, version: &Version| -> PathBuf {
        if let Some(patch) = patches.get(&line.path) {
            workspace_root.join(&patch.path)
        } else {
            home_dir
                .join(".pcb")
                .join("cache")
                .join(&line.path)
                .join(version.to_string())
        }
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

    // Resolve alias -> path
    let resolve_alias = |target: &str| -> Option<PathBuf> {
        let spec = LoadSpec::parse(target)?;
        match spec {
            LoadSpec::Path { path, .. } => Some(workspace_root.join(path)),
            LoadSpec::Github { user, repo, .. } => {
                let url = format!("github.com/{}/{}", user, repo);
                url_to_families
                    .get(&url)
                    .and_then(|f| f.values().next().cloned())
            }
            LoadSpec::Gitlab { project_path, .. } => {
                let url = format!("gitlab.com/{}", project_path);
                url_to_families
                    .get(&url)
                    .and_then(|f| f.values().next().cloned())
            }
            _ => None,
        }
    };

    // Build deps map for a package with base directory for local path resolution
    let build_map = |base_dir: &Path,
                     pkg_deps: &HashMap<String, DependencySpec>,
                     ws_aliases: Option<&HashMap<String, String>>|
     -> BTreeMap<String, PathBuf> {
        let mut map = BTreeMap::new();

        // Aliases
        if let Some(aliases) = ws_aliases {
            for (alias, target) in aliases {
                if let Some(path) = resolve_alias(target) {
                    map.insert(alias.clone(), path);
                }
            }
        }

        // Package deps - resolve local paths relative to base_dir
        for (url, spec) in pkg_deps {
            if let Some(path) = resolve(base_dir, url, spec) {
                map.insert(url.clone(), path);
            }
        }

        map
    };

    let mut results = HashMap::new();

    // Workspace root
    if let PcbToml::V2(v2) = root_config {
        let aliases = v2.workspace.as_ref().map(|w| &w.aliases);
        results.insert(
            workspace_root.to_path_buf(),
            build_map(workspace_root, &v2.dependencies, aliases),
        );
    }

    // Member packages
    for (pkg_path, config) in packages {
        if let PcbToml::V2(v2) = config {
            if let PcbToml::V2(root) = root_config {
                let aliases = root.workspace.as_ref().map(|w| &w.aliases);
                let pkg_dir = pkg_path.parent().unwrap();
                results.insert(
                    pkg_dir.to_path_buf(),
                    build_map(pkg_dir, &v2.dependencies, aliases),
                );
            }
        }
    }

    // Transitive deps: each remote package gets its own resolution map
    // This ensures MVS-selected versions are used for all transitive loads
    for (line, version) in selected {
        let abs_path = get_abs_path(line, version);
        if let Some(deps) = manifest_cache.get(&(line.clone(), version.clone())) {
            results.insert(abs_path.clone(), build_map(&abs_path, deps, None));
        }
    }

    Ok(results)
}

/// Collect dependencies for a package and transitive local deps
fn collect_package_dependencies(
    pcb_toml_path: &Path,
    v2_config: &pcb_zen_core::config::PcbTomlV2,
) -> Result<Vec<UnresolvedDep>> {
    let package_dir = pcb_toml_path.parent().unwrap();
    let mut deps = HashMap::new();

    collect_deps_recursive(&v2_config.dependencies, package_dir, &mut deps)?;

    Ok(deps.into_values().collect())
}

/// Recursively collect dependencies, handling transitive local path dependencies
fn collect_deps_recursive(
    current_deps: &HashMap<String, DependencySpec>,
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

        // Validate package.path is present and matches dependency key
        let package_config =
            dep_config
                .package
                .as_ref()
                .ok_or_else(|| PathDepError::MissingPackageSection {
                    url: url.clone(),
                    location: dep_pcb_toml.clone(),
                })?;

        let declared_path =
            package_config
                .path
                .as_ref()
                .ok_or_else(|| PathDepError::MissingPackagePath {
                    url: url.clone(),
                    location: dep_pcb_toml.clone(),
                })?;

        if declared_path != url {
            return Err(PathDepError::PathMismatch {
                dep_key: url.clone(),
                pkg_path: declared_path.clone(),
                location: dep_pcb_toml.clone(),
            }
            .into());
        }

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

/// Fetch a manifest (pcb.toml dependencies) from Git using sparse checkout
fn fetch_manifest(
    workspace_root: &Path,
    module_path: &str,
    version: &Version,
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
) -> Result<HashMap<String, DependencySpec>> {
    use std::fs;

    // Check if this module is patched with a local path
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

    // Check if we've already processed this version
    let marker_path = checkout_dir.join(".no-manifest");

    if marker_path.exists() {
        // Already tried, no V2 manifest (asset package or V1)
        println!("        Cached (no manifest)");
        return Ok(HashMap::new());
    }

    // Use sparse checkout to fetch the module (shared with Phase 3)
    println!("        Cloning (sparse checkout)");

    let package_root =
        match ensure_sparse_checkout(&checkout_dir, module_path, &version.to_string()) {
            Ok(root) => root,
            Err(e) => {
                // Failed to fetch - mark as asset package
                let _ = fs::create_dir_all(&checkout_dir);
                let _ = fs::write(&marker_path, format!("Failed to fetch: {}\n", e));
                return Ok(HashMap::new());
            }
        };

    let pcb_toml_path = package_root.join("pcb.toml");

    // Check if manifest exists
    if !pcb_toml_path.exists() {
        // Asset package (no pcb.toml)
        let _ = fs::write(&marker_path, "No pcb.toml in repository\n");
        return Ok(HashMap::new());
    }

    // Read the manifest
    read_manifest_from_path(&pcb_toml_path, module_path)
}

/// Read and parse a pcb.toml manifest
fn read_manifest_from_path(
    pcb_toml_path: &Path,
    _module_path: &str,
) -> Result<HashMap<String, DependencySpec>> {
    use std::fs;

    // Check if pcb.toml exists (asset packages don't have manifests)
    if !pcb_toml_path.exists() {
        // Asset package (e.g., KiCad symbols/footprints) - no dependencies
        // Create marker to avoid re-fetching
        let marker_path = pcb_toml_path.parent().unwrap().join(".no-manifest");
        let _ = fs::write(&marker_path, "Asset package - no V2 manifest\n");
        return Ok(HashMap::new());
    }

    let file_provider = DefaultFileProvider::new();
    let config = PcbToml::from_file(&file_provider, pcb_toml_path)?;

    match config.to_v2() {
        Ok(v2) => Ok(v2.dependencies),
        Err(e) => {
            // Failed to convert to V2 - treat as asset package
            // Create marker to avoid re-parsing
            let marker_path = pcb_toml_path.parent().unwrap().join(".no-manifest");
            let _ = fs::write(
                &marker_path,
                format!("Failed to parse/convert manifest: {}\n", e),
            );
            Ok(HashMap::new())
        }
    }
}

/// Print a tree view of the dependency graph (cargo tree style)
fn print_dependency_tree(
    packages: &[(PathBuf, PcbToml)],
    workspace_root: &Path,
    build_set: &HashSet<(ModuleLine, Version)>,
    manifest_cache: &HashMap<(ModuleLine, Version), HashMap<String, DependencySpec>>,
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
        manifest_cache: &HashMap<(ModuleLine, Version), HashMap<String, DependencySpec>>,
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
            if let Some(deps) = manifest_cache.get(&(line.clone(), version.clone())) {
                let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
                let mut dep_list: Vec<_> = deps
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
        let package_deps = collect_package_dependencies(pcb_toml_path, v2)?;

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
    manifest_cache: &HashMap<(ModuleLine, Version), HashMap<String, DependencySpec>>,
) -> Result<HashSet<(ModuleLine, Version)>> {
    let mut build_set = HashSet::new();
    let mut stack = Vec::new();

    // Build index: module_path → ModuleLine for fast lookups
    let mut line_by_path: HashMap<String, Vec<ModuleLine>> = HashMap::new();
    for line in selected.keys() {
        line_by_path
            .entry(line.path.clone())
            .or_default()
            .push(line.clone());
    }

    // Seed DFS from all package dependencies
    for (pcb_toml_path, config) in packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_deps = collect_package_dependencies(pcb_toml_path, v2)?;

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
        if let Some(deps) = manifest_cache.get(&(line.clone(), version)) {
            for (dep_path, dep_spec) in deps {
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

        // Skip internal marker files (.no-manifest, .full-checkout, .pcbcache)
        if let Some(file_name) = path.file_name() {
            let name = file_name.to_str().unwrap_or("");
            if name == ".no-manifest" || name == ".full-checkout" || name == ".pcbcache" {
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

        // Check if already cached
        if cache_marker.exists() {
            // Verify cache integrity against lockfile if available
            if let Some(lf) = lockfile {
                if let Some(locked_entry) = lf.get(&line.path, &version.to_string()) {
                    // Read cached hashes
                    let marker_content = std::fs::read_to_string(&cache_marker)?;
                    let mut lines_iter = marker_content.lines();
                    let cached_content_hash = lines_iter.next().unwrap_or("");
                    let cached_manifest_hash = lines_iter.next();

                    // Verify content hash
                    if cached_content_hash != locked_entry.content_hash {
                        println!(
                            "    {}@v{} (cache mismatch, re-fetching)",
                            line.path, version
                        );
                        std::fs::remove_dir_all(&cache_dir)?;
                        // Will re-fetch below
                    } else if let (Some(cached_mh), Some(locked_mh)) =
                        (cached_manifest_hash, &locked_entry.manifest_hash)
                    {
                        // Verify manifest hash if both exist
                        if cached_mh != locked_mh {
                            println!(
                                "    {}@v{} (manifest mismatch, re-fetching)",
                                line.path, version
                            );
                            std::fs::remove_dir_all(&cache_dir)?;
                            // Will re-fetch below
                        } else {
                            println!("    {}@v{} (cached, verified)", line.path, version);
                            continue;
                        }
                    } else {
                        println!("    {}@v{} (cached, verified)", line.path, version);
                        continue;
                    }
                } else {
                    // Not in lockfile, trust cache
                    println!("    {}@v{} (cached)", line.path, version);
                    continue;
                }
            } else {
                // No lockfile, trust cache
                println!("    {}@v{} (cached)", line.path, version);
                continue;
            }
        }

        // Re-check if marker still doesn't exist (might have been deleted above)
        if cache_marker.exists() {
            continue;
        }

        println!("    {}@v{}...", line.path, version);

        let is_asset_package = cache_dir.join(".no-manifest").exists();

        // Ensure sparse-checkout working tree (reuses Phase 1 clone if already done)
        let package_root = ensure_sparse_checkout(&cache_dir, &line.path, &version.to_string())?;

        // Compute hashes immediately after fetch
        print!("      Computing hashes... ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let content_hash = compute_content_hash_from_dir(&package_root)?;
        let manifest_hash = if package_root.join("pcb.toml").exists() {
            let manifest_content = std::fs::read_to_string(package_root.join("pcb.toml"))?;
            Some(compute_manifest_hash(&manifest_content))
        } else {
            None
        };

        // Write cache marker with hashes
        // Format: content_hash\nmanifest_hash (or just content_hash if no manifest)
        let marker_content = if let Some(mh) = &manifest_hash {
            format!("{}\n{}\n", content_hash, mh)
        } else {
            format!("{}\n", content_hash)
        };
        std::fs::write(&cache_marker, marker_content)?;

        // Restore asset package marker if needed
        if is_asset_package {
            std::fs::write(cache_dir.join(".no-manifest"), "")?;
        }

        println!("done");
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
) -> Result<PathBuf> {
    let (repo_url, subpath) = split_repo_and_subpath(module_path);
    let is_pseudo_version = version_str.contains("-0.");

    // Construct ref_spec (tag name or commit hash)
    let ref_spec = if is_pseudo_version {
        // Extract commit hash from pseudo-version (last segment after final -)
        version_str.rsplit('-').next().unwrap().to_string()
    } else {
        // Regular version: construct tag with subpath prefix
        if subpath.is_empty() {
            format!("v{}", version_str)
        } else {
            format!("{}/v{}", subpath, version_str)
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
        if run_git_in(&fetch_args, checkout_dir).is_ok() {
            fetch_succeeded = true;
        }
    }

    // Try without v-prefix if both HTTPS and SSH with v-prefix failed
    if !fetch_succeeded && !is_pseudo_version {
        let tag_ref_no_v = if subpath.is_empty() {
            format!("refs/tags/{}", version_str)
        } else {
            format!("refs/tags/{}/{}", subpath, version_str)
        };
        let fetch_args_no_v = vec![
            "fetch",
            "--depth=1",
            "--filter=blob:none",
            "origin",
            &tag_ref_no_v,
        ];
        run_git_in(&fetch_args_no_v, checkout_dir)?;
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
