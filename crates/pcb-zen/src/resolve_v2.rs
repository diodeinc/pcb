use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use pcb_ui::{Colorize, Style, StyledText};
use pcb_zen_core::config::{
    DependencyDetail, DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml,
};
use pcb_zen_core::resolution::{
    build_resolution_map, semver_family, ModuleLine, NativePathResolver, PackagePathResolver,
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
use crate::tags;
use crate::workspace::{WorkspaceInfo, WorkspaceInfoExt};

/// Find matching patch for a module path, supporting glob patterns.
/// Exact matches take priority, then glob patterns in sorted order.
fn find_matching_patch<'a>(
    url: &str,
    patches: &'a BTreeMap<String, PatchSpec>,
) -> Option<&'a PatchSpec> {
    if let Some(patch) = patches.get(url) {
        return Some(patch);
    }
    for (pattern, patch) in patches {
        if let Ok(glob) = globset::Glob::new(pattern) {
            if glob.compile_matcher().is_match(url) {
                return Some(patch);
            }
        }
    }
    None
}

/// Returns a patch override for the dependency spec if a branch/rev patch applies.
///
/// Patches act as a "dumb rewrite" of dependency specs before MVS runs.
/// Path patches don't affect version resolution (they just change fetch location).
fn get_patch_override(url: &str, patches: &BTreeMap<String, PatchSpec>) -> Option<DependencySpec> {
    let patch = find_matching_patch(url, patches)?;

    if let Some(branch) = &patch.branch {
        return Some(DependencySpec::Detailed(DependencyDetail {
            branch: Some(branch.clone()),
            version: None,
            rev: None,
            path: None,
        }));
    }

    if let Some(rev) = &patch.rev {
        return Some(DependencySpec::Detailed(DependencyDetail {
            rev: Some(rev.clone()),
            version: None,
            branch: None,
            path: None,
        }));
    }

    None
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

        // Try to match by semver family
        if let Ok(v) = parse_version_string(version) {
            if let Some(path) = families.get(&semver_family(&v)) {
                return Some(path.clone());
            }
        }

        // Fallback for branch/rev specs: use single family if only one exists
        if families.len() == 1 {
            return families.values().next().cloned();
        }

        None
    }

    fn resolve_asset(&self, asset_key: &str, ref_str: &str) -> Option<PathBuf> {
        self.base.resolve_asset(asset_key, ref_str)
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
    /// Package dependencies in the build closure: ModuleLine -> Version
    pub closure: HashMap<ModuleLine, Version>,
    /// Asset dependencies: (module_path, ref) -> resolved_path
    pub assets: HashMap<(String, String), PathBuf>,
    /// Whether the lockfile (pcb.sum) was updated during resolution
    pub lockfile_changed: bool,
}

impl ResolutionResult {
    /// Print the dependency tree to stdout
    pub fn print_tree(&self, workspace_info: &WorkspaceInfo) {
        let workspace_name = workspace_info
            .root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace");

        // Index closure by path for fast lookup
        let by_path: HashMap<&str, &Version> = self
            .closure
            .iter()
            .map(|(line, version)| (line.path.as_str(), version))
            .collect();

        // Collect root deps (direct deps from workspace packages)
        let mut root_deps: Vec<&str> = Vec::new();
        for pkg in workspace_info.packages.values() {
            for url in pkg.config.dependencies.keys() {
                if by_path.contains_key(url.as_str()) && !root_deps.contains(&url.as_str()) {
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
        for line in self.closure.keys() {
            if dep_graph.contains_key(&line.path) {
                continue;
            }
            // Find resolved path for this package
            for deps in self.package_resolutions.values() {
                if let Some(resolved) = deps.get(&line.path) {
                    let pcb_toml = resolved.join("pcb.toml");
                    if pcb_toml.exists() {
                        if let Ok(content) = std::fs::read_to_string(&pcb_toml) {
                            if let Ok(config) = PcbToml::parse(&content) {
                                let transitive: Vec<String> = config
                                    .dependencies
                                    .keys()
                                    .filter(|dep_url| by_path.contains_key(dep_url.as_str()))
                                    .cloned()
                                    .collect();
                                dep_graph.insert(line.path.clone(), transitive);
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
            by_path: &HashMap<&str, &Version>,
            dep_graph: &HashMap<String, Vec<String>>,
            printed: &mut HashSet<String>,
            prefix: &str,
            is_last: bool,
            format_name: &impl Fn(&str) -> String,
        ) {
            let branch = if is_last { "└── " } else { "├── " };
            let version = by_path
                .get(url)
                .map(|v| v.to_string())
                .unwrap_or("?".to_string());
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
                        by_path,
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
                &by_path,
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
    /// Number of stale entries pruned from vendor/
    pub pruned_count: usize,
    /// Path to vendor directory
    pub vendor_dir: PathBuf,
}

/// Run auto-deps phase: detect missing dependencies from .zen files and add to pcb.toml
fn run_auto_deps(
    workspace_info: &mut WorkspaceInfo,
    workspace_root: &Path,
    offline: bool,
) -> Result<()> {
    log::debug!("Phase -1: Auto-detecting dependencies from .zen files");
    let auto_deps = crate::auto_deps::auto_add_zen_deps(
        workspace_root,
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
    if auto_deps.stdlib_removed > 0 {
        log::debug!(
            "Removed {} redundant stdlib dependency declaration(s)",
            auto_deps.stdlib_removed
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
    Ok(())
}

/// V2 dependency resolution
///
/// Builds dependency graph using MVS, fetches dependencies,
/// and generates/updates the lockfile.
///
/// When `locked` is true:
/// - Auto-deps phase is skipped (dependencies must be declared in pcb.toml)
/// - Lockfile is verified instead of written (fails if out of date)
/// - Recommended for CI to catch missing dependencies or stale lockfiles
pub fn resolve_dependencies(
    workspace_info: &mut WorkspaceInfo,
    offline: bool,
    locked: bool,
) -> Result<ResolutionResult> {
    let workspace_root = workspace_info.root.clone();

    // Standalone mode: .zen file with inline manifest (no pcb.toml)
    // In this mode we skip auto-deps and lockfile writing
    let is_standalone = !workspace_root.join("pcb.toml").exists();

    log::debug!(
        "V2 Dependency Resolution{}{}{}",
        if offline { " (offline)" } else { "" },
        if locked { " (locked)" } else { "" },
        if is_standalone { " (standalone)" } else { "" }
    );
    log::debug!("Workspace root: {}", workspace_root.display());

    // Phase -1: Auto-add missing dependencies from .zen files
    // Skip for standalone mode (no pcb.toml to modify)
    // Skip for locked/offline modes (trust the lockfile)
    if !is_standalone && !locked && !offline {
        run_auto_deps(workspace_info, &workspace_root, offline)?;
    }

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

    // Inject implicit stdlib dependency (toolchain-pinned minimum version)
    // This ensures stdlib is always available without explicit declaration,
    // and acts as a minimum version floor (MVS will pick higher if user specifies)
    // Skip in locked/offline modes - trust the lockfile instead
    if !locked && !offline {
        let stdlib_version = Version::parse(pcb_zen_core::STDLIB_VERSION)
            .expect("STDLIB_VERSION must be valid semver");
        let stdlib_line = ModuleLine::new(
            pcb_zen_core::STDLIB_MODULE_PATH.to_string(),
            &stdlib_version,
        );
        if find_matching_patch(pcb_zen_core::STDLIB_MODULE_PATH, &patches).is_none() {
            selected.insert(stdlib_line.clone(), stdlib_version.clone());
            work_queue.push_back(stdlib_line);
            log::debug!(
                "Injected implicit stdlib dependency: {}@v{}",
                pcb_zen_core::STDLIB_MODULE_PATH,
                pcb_zen_core::STDLIB_VERSION
            );
        }
    }

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

            // Skip patched packages (let patch version take precedence)
            if find_matching_patch(&entry.module_path, &patches).is_some() {
                continue;
            }

            // Parse version (skip invalid entries)
            let Ok(version) = Version::parse(&entry.version) else {
                continue;
            };

            // Skip pseudo-versions (branch/rev deps) - let them resolve fresh from cache
            // This ensures `pcb update` picks up new commits
            if !version.pre.is_empty() {
                continue;
            }

            let line = ModuleLine::new(entry.module_path.clone(), &version);

            // Insert if not already selected, or replace if this version is higher.
            // This ensures deterministic selection of the highest version within a family,
            // regardless of HashMap iteration order in the lockfile.
            if let Some(existing) = selected.get(&line) {
                if version > *existing {
                    log::debug!(
                        "Upgrading {}@v{} -> v{} (from pcb.sum)",
                        entry.module_path,
                        existing,
                        version
                    );
                    selected.insert(line, version);
                }
            } else {
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

    // Create pseudo-version context to cache expensive operations across all resolutions
    let mut pseudo_ctx = PseudoVersionContext::new()?;

    // Seed MVS state from direct dependencies
    for (_package_name, package_deps) in &packages_with_deps {
        for dep in package_deps {
            // Apply branch/rev patch overrides before resolution
            let patch_override = get_patch_override(&dep.url, &patches);
            let spec = patch_override.as_ref().unwrap_or(&dep.spec);

            if let DependencySpec::Detailed(detail) = spec {
                if detail.path.is_some() {
                    continue;
                }
            }

            let version = resolve_to_version(
                &mut pseudo_ctx,
                &dep.url,
                spec,
                workspace_info.lockfile.as_ref(),
                offline,
            )
            .with_context(|| format!("Failed to resolve {}", dep.url))?;

            add_requirement(
                dep.url.clone(),
                version,
                &mut selected,
                &mut work_queue,
                &patches,
            );
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
            let manifest =
                result.with_context(|| format!("Failed to fetch {}@v{}", line.path, version))?;

            manifest_cache.insert((line.clone(), version.clone()), manifest.clone());

            for (dep_path, dep_spec) in &manifest.dependencies {
                // Apply branch/rev patch overrides before resolution
                let patch_override = get_patch_override(dep_path, &patches);
                let spec = patch_override.as_ref().unwrap_or(dep_spec);

                if is_non_version_dep(spec) {
                    continue;
                }

                let dep_version = resolve_to_version(
                    &mut pseudo_ctx,
                    dep_path,
                    spec,
                    workspace_info.lockfile.as_ref(),
                    offline,
                )
                .with_context(|| format!("Failed to resolve {}", dep_path))?;

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
    // Skip for standalone mode (no pcb.sum to write)
    let mut lockfile_changed = false;
    if !is_standalone {
        log::debug!("Phase 4: Lockfile");
        let lockfile_path = workspace_root.join("pcb.sum");
        let old_lockfile = workspace_info
            .lockfile
            .as_ref()
            .cloned()
            .unwrap_or_default();
        let new_lockfile = update_lockfile(&workspace_root, &old_lockfile, &closure, &asset_paths)?;

        if locked {
            // In locked mode: fail if new entries would be added (deletions are safe)
            let mut missing: Vec<_> = new_lockfile
                .entries
                .keys()
                .filter(|k| !old_lockfile.entries.contains_key(*k))
                .map(|(path, ver)| format!("{}@{}", path, ver))
                .collect();

            if !missing.is_empty() {
                missing.sort();
                let list = missing
                    .iter()
                    .take(10)
                    .map(|k| format!("    - {}", k))
                    .collect::<Vec<_>>()
                    .join("\n");
                let more = missing
                    .len()
                    .checked_sub(10)
                    .filter(|&n| n > 0)
                    .map(|n| format!("\n    ... and {} more", n))
                    .unwrap_or_default();
                anyhow::bail!(
                    "Lockfile is out of date (--locked mode)\n\
                    Missing entries in pcb.sum:\n{list}{more}\n\n\
                    Run `pcb build` without --locked to update pcb.sum"
                );
            }
        } else {
            let old_content = std::fs::read_to_string(&lockfile_path).unwrap_or_default();
            let new_content = new_lockfile.to_string();
            if new_content != old_content {
                std::fs::write(&lockfile_path, &new_content)?;
                log::debug!("  Updated {}", lockfile_path.display());
                lockfile_changed = true;
            }
            // Keep workspace_info.lockfile in sync
            workspace_info.lockfile = Some(new_lockfile);
        }
    }

    log::debug!("V2 dependency resolution complete");

    let package_resolutions =
        build_native_resolution_map(workspace_info, &closure, &patches, &asset_paths, offline)?;

    Ok(ResolutionResult {
        package_resolutions,
        closure,
        assets: asset_paths,
        lockfile_changed,
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
///
/// This function performs an incremental sync:
/// - Adds any packages/assets from the resolution that are missing in vendor/
/// - When `prune=true`, removes any {url}/{version-or-ref} directories not in the resolution
///
/// Pruning should be disabled when offline (can't re-fetch deleted deps).
pub fn vendor_deps(
    workspace_info: &WorkspaceInfo,
    resolution: &ResolutionResult,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
    prune: bool,
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
        log::debug!("No vendor patterns configured, skipping vendoring");
        return Ok(VendorResult {
            package_count: 0,
            asset_count: 0,
            pruned_count: 0,
            vendor_dir,
        });
    }
    log::debug!("Vendor patterns: {:?}", patterns);

    let cache = cache_base();
    let workspace_vendor = workspace_info.root.join("vendor");

    // Build glob matcher
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        builder.add(Glob::new(pattern)?);
    }
    let glob_set = builder.build()?;

    fs::create_dir_all(&vendor_dir)?;

    // Track all desired {url}/{version-or-ref} roots for pruning stale entries
    let mut desired_roots: HashSet<PathBuf> = HashSet::new();

    // Copy matching packages from workspace vendor or cache (vendor takes precedence)
    let mut package_count = 0;
    for (line, version) in &resolution.closure {
        if !glob_set.is_match(&line.path) {
            continue;
        }
        let version_str = version.to_string();

        // Track this package root for pruning
        let rel_root = PathBuf::from(&line.path).join(&version_str);
        desired_roots.insert(rel_root);

        let vendor_src = workspace_vendor.join(&line.path).join(&version_str);
        let cache_src = cache.join(&line.path).join(&version_str);
        let src = if vendor_src.exists() {
            vendor_src
        } else {
            cache_src
        };
        let dst = vendor_dir.join(&line.path).join(&version_str);
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

        // Track the repo/ref root for pruning (assets share repo/ref roots)
        let rel_root = PathBuf::from(repo_url).join(ref_str);
        desired_roots.insert(rel_root);

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

    // Prune stale {url}/{version-or-ref} directories not in the resolution
    let pruned_count = if prune {
        prune_stale_vendor_roots(&vendor_dir, &desired_roots)?
    } else {
        0
    };

    Ok(VendorResult {
        package_count,
        asset_count,
        pruned_count,
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

/// Prune stale {path}/{version} directories from vendor/
///
/// Walks vendor/ recursively and removes directories not in desired_roots
/// or on the path to a desired root. Returns the number of roots pruned.
fn prune_stale_vendor_roots(vendor_dir: &Path, desired_roots: &HashSet<PathBuf>) -> Result<usize> {
    if !vendor_dir.exists() {
        return Ok(0);
    }

    // Build set of ancestor paths (paths we must traverse to reach desired roots)
    let mut ancestors: HashSet<PathBuf> = HashSet::new();
    for root in desired_roots {
        let mut ancestor = PathBuf::new();
        for component in root.components() {
            ancestors.insert(ancestor.clone());
            ancestor.push(component);
        }
    }

    let mut pruned = 0;
    prune_dir(
        vendor_dir,
        &PathBuf::new(),
        desired_roots,
        &ancestors,
        &mut pruned,
    )?;
    Ok(pruned)
}

fn prune_dir(
    base: &Path,
    rel: &Path,
    desired_roots: &HashSet<PathBuf>,
    ancestors: &HashSet<PathBuf>,
    pruned: &mut usize,
) -> Result<()> {
    for entry in fs::read_dir(base.join(rel))? {
        let entry = entry?;
        let name = entry.file_name();
        let child_rel = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };

        if entry.file_type()?.is_dir() {
            if desired_roots.contains(&child_rel) {
                // This is a desired root - keep everything inside it
                continue;
            } else if ancestors.contains(&child_rel) {
                // On path to a desired root - recurse to find what to prune
                prune_dir(base, &child_rel, desired_roots, ancestors, pruned)?;
                // Clean up if now empty
                if entry.path().read_dir()?.next().is_none() {
                    fs::remove_dir(entry.path())?;
                }
            } else {
                // Not needed - prune entire subtree
                log::debug!("Pruning stale vendor path: {}", child_rel.display());
                fs::remove_dir_all(entry.path())?;
                *pruned += 1;
            }
        }
        // Files at the root level of vendor/ shouldn't exist, ignore them
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
fn build_native_resolution_map(
    workspace_info: &WorkspaceInfo,
    closure: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
    asset_paths: &HashMap<(String, String), PathBuf>,
    offline: bool,
) -> Result<HashMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let cache = cache_base();
    let vendor = workspace_info.root.join("vendor");

    // Build patch map (only path patches - branch/rev patches use normal fetch)
    // Expand glob patterns against closure URLs
    let mut path_patches: HashMap<String, PathBuf> = HashMap::new();
    for (pattern, patch) in patches {
        if let Some(path_str) = &patch.path {
            let abs_path = workspace_info.root.join(path_str);
            if let Ok(glob) = globset::Glob::new(pattern) {
                let matcher = glob.compile_matcher();
                for line in closure.keys() {
                    if matcher.is_match(&line.path) {
                        path_patches.insert(line.path.clone(), abs_path.clone());
                    }
                }
            }
        }
    }
    let patches = path_patches;

    // Create base resolver for package path lookups
    // Note: workspace members are handled directly in build_resolution_map
    let base_resolver = NativePathResolver {
        vendor_dir: vendor.clone(),
        cache_dir: cache.clone(),
        offline,
        patches,
        asset_paths: asset_paths.clone(),
    };

    // Build the families map for MVS family matching
    let mut families: HashMap<String, HashMap<String, PathBuf>> = HashMap::new();
    for (line, version) in closure {
        let version_str = version.to_string();
        if let Some(abs_path) = base_resolver.resolve_package(&line.path, &version_str) {
            families
                .entry(line.path.clone())
                .or_default()
                .insert(line.family.clone(), abs_path);
        }
    }

    let resolver = MvsFamilyResolver {
        families,
        base: base_resolver,
    };

    let file_provider = DefaultFileProvider::default();
    let results = build_resolution_map(&file_provider, &resolver, workspace_info, closure);

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
    // Track which asset_keys belong to each repo for offline validation
    let mut repos_to_fetch: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (asset_key, ref_str) in &unique_assets {
        let (repo_url, _) = git::split_asset_repo_and_subpath(asset_key);
        repos_to_fetch
            .entry((repo_url.to_string(), ref_str.clone()))
            .or_default()
            .push(asset_key.clone());
    }

    // Print repos we're fetching
    for (repo_url, ref_str) in repos_to_fetch.keys() {
        log::debug!("  {}@{}", repo_url, ref_str);
    }

    // Fetch repos in parallel
    let errors: Vec<_> = repos_to_fetch
        .par_iter()
        .filter_map(|((repo_url, ref_str), asset_keys)| {
            fetch_asset_repo(workspace_info, repo_url, ref_str, asset_keys, offline)
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
    // (branch/rev patches fall through to normal fetch - version already resolved)
    if let Some(patch) = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.patch.get(module_path))
    {
        if let Some(path) = &patch.path {
            let patched_path = workspace_info.root.join(path);
            let patched_toml = patched_path.join("pcb.toml");

            if !patched_toml.exists() {
                anyhow::bail!("Patch path {} has no pcb.toml", patched_path.display());
            }

            return read_manifest_from_path(&patched_toml);
        }
    }

    // 3. Check vendor directory (only if also in lockfile for consistency)
    let version_str = version.to_string();
    let vendor_dir = workspace_info
        .root
        .join("vendor")
        .join(module_path)
        .join(&version_str);
    let vendor_toml = vendor_dir.join("pcb.toml");
    let in_lockfile = workspace_info
        .lockfile
        .as_ref()
        .map(|lf| lf.get(module_path, &version_str).is_some())
        .unwrap_or(false);
    if vendor_toml.exists() && in_lockfile {
        return read_manifest_from_path(&vendor_toml);
    }

    // 4. If offline, fail here - vendor is the only allowed source for offline builds
    if offline {
        anyhow::bail!(
            "Package not vendored (offline mode)\n  \
            Run `pcb vendor` to vendor dependencies for offline builds"
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
    repo_url: &str,
    ref_str: &str,
    asset_keys: &[String],
    offline: bool,
) -> Result<PathBuf> {
    // Check if asset has a local path patch (branch/rev patches fall through to normal fetch)
    let get_path_patch = |key: &str| {
        workspace_info
            .config
            .as_ref()
            .and_then(|c| c.patch.get(key))
            .and_then(|p| p.path.as_ref())
    };

    // For offline mode, verify all non-patched asset_keys are vendored and in lockfile
    if offline {
        let vendor_base = workspace_info
            .root
            .join("vendor")
            .join(repo_url)
            .join(ref_str);

        for asset_key in asset_keys {
            if get_path_patch(asset_key).is_some() {
                continue;
            }

            let (_, subpath) = git::split_asset_repo_and_subpath(asset_key);
            let vendor_dir = if subpath.is_empty() {
                vendor_base.clone()
            } else {
                vendor_base.join(subpath)
            };
            let in_lockfile = workspace_info
                .lockfile
                .as_ref()
                .map(|lf| lf.get(asset_key, ref_str).is_some())
                .unwrap_or(false);

            if !vendor_dir.exists() || !in_lockfile {
                anyhow::bail!(
                    "Asset {} @ {} not vendored (offline mode)\n  \
                    Run `pcb vendor` to vendor dependencies for offline builds",
                    asset_key,
                    ref_str
                );
            }
            log::debug!("Asset {}@{} vendored", asset_key, ref_str);
        }

        return Ok(vendor_base);
    }

    // Online mode: fetch repo and index subpaths
    let index = CacheIndex::open()?;
    let repo_cache_dir = cache_base().join(repo_url).join(ref_str);

    // Ensure base repo is fetched (check for .git or any content)
    let repo_exists = repo_cache_dir.join(".git").exists()
        || (repo_cache_dir.exists()
            && std::fs::read_dir(&repo_cache_dir).is_ok_and(|mut d| d.next().is_some()));
    if !repo_exists {
        log::debug!("Asset {}@{} fetching", repo_url, ref_str);
        ensure_sparse_checkout(&repo_cache_dir, repo_url, ref_str, false)?;
    }

    // Verify and index each subpath
    for asset_key in asset_keys {
        if get_path_patch(asset_key).is_some() {
            continue;
        }

        let (_, subpath) = git::split_asset_repo_and_subpath(asset_key);
        let target_path = if subpath.is_empty() {
            repo_cache_dir.clone()
        } else {
            repo_cache_dir.join(subpath)
        };

        if !subpath.is_empty() && !target_path.exists() {
            anyhow::bail!(
                "Subpath '{}' not found in {}@{}",
                subpath,
                repo_url,
                ref_str
            );
        }

        if index.get_asset(repo_url, subpath, ref_str).is_none() {
            let content_hash = compute_content_hash_from_dir(&target_path)?;
            index.set_asset(repo_url, subpath, ref_str, &content_hash)?;
            log::debug!("Asset {}@{} hashed: {}", asset_key, ref_str, content_hash);
        } else {
            log::debug!("Asset {}@{} cached", asset_key, ref_str);
        }
    }

    Ok(repo_cache_dir)
}

/// Get the ModuleLine for a dependency spec.
///
/// For version specs, computes the family from the version.
/// For branch/rev specs, finds the resolved line in selected.
fn get_line_for_dep(
    url: &str,
    spec: &DependencySpec,
    selected: &HashMap<ModuleLine, Version>,
) -> Option<ModuleLine> {
    // Extract version string from spec
    let version_str = match spec {
        DependencySpec::Version(v) => Some(v.as_str()),
        DependencySpec::Detailed(d) => d.version.as_deref(),
    };

    if let Some(v) = version_str {
        // Version spec: compute family and look up
        let ver = parse_version_string(v).ok()?;
        let line = ModuleLine::new(url.to_string(), &ver);
        selected.contains_key(&line).then_some(line)
    } else {
        // Branch/rev dep: find the line in selected (pseudo-versions aren't preseeded)
        selected.keys().find(|line| line.path == url).cloned()
    }
}

/// Build the final dependency closure using selected versions
///
/// DFS from workspace package dependencies using selected versions.
/// Returns map of ModuleLine -> Version for packages in the build closure.
///
/// IMPORTANT: Only includes ModuleLines that are actually reachable from workspace
/// dependencies. Stale entries preseeded from the lockfile are excluded if they
/// don't match any dependency's resolved family. Workspace members are excluded.
fn build_closure(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PackageManifest>,
) -> HashMap<ModuleLine, Version> {
    let mut closure = HashMap::new();
    let mut stack = Vec::new();

    // Seed DFS from all package dependencies
    // Use get_line_for_dep to find the specific ModuleLine matching each dependency's family
    // Skip workspace members (resolved locally, not part of closure)
    for pkg in packages.values() {
        for (url, spec) in &pkg.config.dependencies {
            if is_non_version_dep(spec) || packages.contains_key(url) {
                continue;
            }
            if let Some(line) = get_line_for_dep(url, spec, selected) {
                stack.push(line);
            }
        }
    }

    // Always include stdlib (implicitly available to all packages)
    // Find the highest version stdlib line in selected
    let stdlib_line = selected
        .iter()
        .filter(|(line, _)| line.path == pcb_zen_core::STDLIB_MODULE_PATH)
        .max_by_key(|(_, v)| (*v).clone())
        .map(|(line, _)| line.clone());
    if let Some(line) = stdlib_line {
        stack.push(line);
    }

    // DFS using final selected versions
    while let Some(line) = stack.pop() {
        // Skip workspace members
        if packages.contains_key(&line.path) {
            continue;
        }

        let version = match selected.get(&line) {
            Some(v) => v.clone(),
            None => continue,
        };

        if closure.contains_key(&line) {
            continue;
        }

        closure.insert(line.clone(), version.clone());

        // Follow transitive dependencies via selected versions
        if let Some(manifest) = manifest_cache.get(&(line.clone(), version)) {
            for (dep_path, dep_spec) in &manifest.dependencies {
                if is_non_version_dep(dep_spec) || packages.contains_key(dep_path) {
                    continue;
                }
                if let Some(dep_line) = get_line_for_dep(dep_path, dep_spec, selected) {
                    stack.push(dep_line);
                }
            }
        }
    }

    closure
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
    ctx: &mut PseudoVersionContext,
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
                // Branch deps always resolve fresh from cache (updated by `pcb update`)
                if offline {
                    // In offline mode, use locked pseudo-version if available
                    if let Some(entry) = lockfile.and_then(|lf| lf.find_by_path(module_path)) {
                        if let Ok(locked_version) = Version::parse(&entry.version) {
                            if !locked_version.pre.is_empty() {
                                log::debug!("        Using locked v{} (offline)", locked_version);
                                return Ok(locked_version);
                            }
                        }
                    }
                    anyhow::bail!(
                        "Branch '{}' for {} requires network access (offline mode)\n  \
                        Add to pcb.sum first by running online, then use --offline",
                        branch,
                        module_path
                    );
                }
                ctx.resolve_branch(module_path, branch)
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
                ctx.resolve_rev(module_path, rev)
            } else {
                anyhow::bail!("Dependency has no version, branch, or rev")
            }
        }
    }
}

/// Context for pseudo-version generation, caching expensive operations.
struct PseudoVersionContext {
    index: CacheIndex,
    bare_repos: HashMap<String, PathBuf>,
    base_versions: HashMap<String, HashMap<String, Version>>,
}

impl PseudoVersionContext {
    fn new() -> Result<Self> {
        Ok(Self {
            index: CacheIndex::open()?,
            bare_repos: HashMap::new(),
            base_versions: HashMap::new(),
        })
    }

    fn ensure_bare_repo(&mut self, repo_url: &str) -> Result<PathBuf> {
        if let Some(path) = self.bare_repos.get(repo_url) {
            return Ok(path.clone());
        }
        let path = ensure_bare_repo(repo_url)?;
        self.bare_repos.insert(repo_url.to_string(), path.clone());
        Ok(path)
    }

    fn resolve_branch(&mut self, module_path: &str, branch: &str) -> Result<Version> {
        let (repo_url, _) = git::split_repo_and_subpath(module_path);
        let commit = match self.index.get_branch_commit(repo_url, branch) {
            Some(c) => c,
            None => {
                log::debug!("        Resolving branch '{}'...", branch);
                let refspec = format!("refs/heads/{}", branch);
                let (commit, _) = git::ls_remote_with_fallback(module_path, &refspec)?;
                let _ = self.index.set_branch_commit(repo_url, branch, &commit);
                commit
            }
        };
        self.generate_pseudo_version(module_path, &commit)
    }

    fn resolve_rev(&mut self, module_path: &str, rev: &str) -> Result<Version> {
        log::debug!("        Resolving rev '{}'...", &rev[..8.min(rev.len())]);
        self.generate_pseudo_version(module_path, rev)
    }

    fn generate_pseudo_version(&mut self, module_path: &str, commit: &str) -> Result<Version> {
        let (repo_url, subpath) = git::split_repo_and_subpath(module_path);
        let bare_dir = self.ensure_bare_repo(repo_url)?;

        let timestamp = match self.index.get_commit_metadata(repo_url, commit) {
            Some((ts, _)) => ts,
            None => {
                let ts = git::show_commit_timestamp(&bare_dir, commit).unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                });
                let _ = self.index.set_commit_metadata(repo_url, commit, ts, None);
                ts
            }
        };

        let base_version = self.get_base_version(&bare_dir, repo_url, subpath);

        // Build pseudo-version: <base+1>-0.<timestamp>-<commit>
        let dt = jiff::Timestamp::from_second(timestamp)?;
        let pseudo_str = format!(
            "{}.{}.{}-0.{}-{}",
            base_version.major,
            base_version.minor,
            base_version.patch + 1,
            dt.strftime("%Y%m%d%H%M%S"),
            &commit[..commit.len().min(40)]
        );
        Version::parse(&pseudo_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse pseudo-version {}: {}", pseudo_str, e))
    }

    fn get_base_version(&mut self, bare_dir: &Path, repo_url: &str, subpath: &str) -> Version {
        if !self.base_versions.contains_key(repo_url) {
            let mut versions: HashMap<String, Version> = HashMap::new();
            if let Ok(tags) = git::list_all_tags(bare_dir) {
                for tag in tags {
                    if let Some((pkg_path, version)) = tags::parse_tag(&tag) {
                        versions
                            .entry(pkg_path)
                            .and_modify(|v| {
                                if version > *v {
                                    *v = version.clone()
                                }
                            })
                            .or_insert(version);
                    }
                }
            }
            self.base_versions.insert(repo_url.to_string(), versions);
        }
        self.base_versions
            .get(repo_url)
            .and_then(|v| v.get(subpath))
            .cloned()
            .unwrap_or_else(|| Version::new(0, 0, 0))
    }
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
    // Check if this module is patched (supports glob patterns)
    let (final_version, is_patched) = if find_matching_patch(&path, patches).is_some() {
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

/// Build lockfile from current resolution
///
/// Creates a fresh lockfile containing only the entries needed for current resolution.
/// Unused entries from the old lockfile are automatically excluded.
fn update_lockfile(
    workspace_root: &Path,
    old_lockfile: &Lockfile,
    closure: &HashMap<ModuleLine, Version>,
    asset_paths: &HashMap<(String, String), PathBuf>,
) -> Result<Lockfile> {
    let mut new_lockfile = Lockfile::default();

    let total_count = closure.len() + asset_paths.len();
    if total_count > 0 {
        log::debug!("  Verifying {} entries...", total_count);
    }

    let index = CacheIndex::open()?;

    for (line, version) in closure {
        let version_str = version.to_string();

        // Check if vendored - if so, reuse existing lockfile entry
        let vendor_dir = workspace_root
            .join("vendor")
            .join(&line.path)
            .join(&version_str);
        if let Some(existing) = old_lockfile.get(&line.path, &version_str) {
            if vendor_dir.exists() {
                new_lockfile.insert(existing.clone());
                continue;
            }
        }

        // Not vendored or not in lockfile - must be in cache
        let (content_hash, manifest_hash) = index
            .get_package(&line.path, &version_str)
            .ok_or_else(|| anyhow::anyhow!("Missing cache entry for {}@{}", line.path, version))?;

        // Check against existing lockfile entry for tampering
        if let Some(existing) = old_lockfile.get(&line.path, &version_str) {
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
            new_lockfile.insert(existing.clone());
        } else {
            new_lockfile.insert(LockEntry {
                module_path: line.path.clone(),
                version: version_str,
                content_hash,
                manifest_hash: Some(manifest_hash),
            });
        }
    }

    for (asset_key, ref_str) in asset_paths.keys() {
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(asset_key);

        // Check if vendored - if so, reuse existing lockfile entry
        let vendor_base = workspace_root.join("vendor").join(repo_url).join(ref_str);
        let vendor_dir = if subpath.is_empty() {
            vendor_base
        } else {
            vendor_base.join(subpath)
        };
        if let Some(existing) = old_lockfile.get(asset_key, ref_str) {
            if vendor_dir.exists() {
                new_lockfile.insert(existing.clone());
                continue;
            }
        }

        // Not vendored or not in lockfile - must be in cache
        let content_hash = index
            .get_asset(repo_url, subpath, ref_str)
            .ok_or_else(|| anyhow::anyhow!("Missing cache entry for {}@{}", asset_key, ref_str))?;

        if let Some(existing) = old_lockfile.get(asset_key, ref_str) {
            new_lockfile.insert(existing.clone());
        } else {
            new_lockfile.insert(LockEntry {
                module_path: asset_key.clone(),
                version: ref_str.clone(),
                content_hash,
                manifest_hash: None,
            });
        }
    }

    log::debug!("  {} entries", new_lockfile.entries.len());

    Ok(new_lockfile)
}

// PackageClosure and package_closure() method are now in workspace.rs
