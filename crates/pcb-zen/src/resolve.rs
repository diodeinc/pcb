use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{
    DependencyDetail, DependencySpec, LockEntry, Lockfile, PatchSpec, PcbToml,
    split_repo_and_subpath,
};
use pcb_zen_core::kicad_library::{
    KicadRepoMatch, kicad_http_mirror_template_for_repo, match_kicad_managed_repo,
};
use pcb_zen_core::resolution::{
    ModuleLine, NativePathResolver, PackagePathResolver, ResolutionResult, build_resolution_map,
    semver_family,
};
use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use semver::Version;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info_span, instrument};

use std::time::Instant;

use crate::cache_index::{
    CacheIndex, cache_base, ensure_bare_repo, ensure_workspace_cache_symlink,
};
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
        if let Ok(glob) = globset::Glob::new(pattern)
            && glob.compile_matcher().is_match(url)
        {
            return Some(patch);
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
/// fallback package resolution to a base `NativePathResolver`.
struct MvsFamilyResolver {
    /// Precomputed package paths: url -> family -> absolute path
    families: HashMap<String, HashMap<String, PathBuf>>,
    /// Base resolver for direct package path lookup.
    base: NativePathResolver,
}

impl PackagePathResolver for MvsFamilyResolver {
    fn resolve_package(&self, module_path: &str, version: &str) -> Option<PathBuf> {
        let Some(families) = self.families.get(module_path) else {
            // Non-package dependencies (no pcb.toml) are not in the closure/family map.
            // Fall back to direct vendor/cache lookup.
            return self.base.resolve_package(module_path, version);
        };

        // Try to match by semver family
        if let Ok(v) = parse_version_string(version)
            && let Some(path) = families.get(&semver_family(&v))
        {
            return Some(path.clone());
        }

        // Fallback for branch/rev specs: use single family if only one exists
        if families.len() == 1 {
            return families.values().next().cloned();
        }

        // Final fallback for non-standard version strings or family misses.
        self.base.resolve_package(module_path, version)
    }
}

fn build_fetch_pool() -> Result<rayon::ThreadPool> {
    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    ThreadPoolBuilder::new()
        .num_threads(jobs)
        .thread_name(|idx| format!("pcb-fetch-{idx}"))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build fetch thread pool: {e}"))
}

/// Print the dependency tree to stdout.
pub fn print_dep_tree(resolution: &ResolutionResult) {
    let workspace_info = &resolution.workspace_info;
    let workspace_name = workspace_info
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    // Index closure by path for fast lookup
    let by_path: HashMap<&str, &Version> = resolution
        .closure
        .iter()
        .map(|(line, version)| (line.path.as_str(), version))
        .collect();

    // Collect root deps (direct deps from workspace packages)
    let mut root_deps: Vec<String> = Vec::new();
    for pkg in workspace_info.packages.values() {
        for url in pkg.config.dependencies.keys() {
            if by_path.contains_key(url.as_str()) && !root_deps.contains(url) {
                root_deps.push(url.clone());
            }
        }
    }
    root_deps.sort();

    if root_deps.is_empty() {
        return;
    }

    // Build dep graph: url -> Vec<dep_urls> by reading pcb.toml from resolved paths
    let mut dep_graph: HashMap<String, Vec<String>> = HashMap::new();
    for line in resolution.closure.keys() {
        if dep_graph.contains_key(&line.path) {
            continue;
        }
        for deps in resolution.package_resolutions.values() {
            if let Some(resolved) = deps.get(&line.path) {
                let pcb_toml = resolved.join("pcb.toml");
                if let Ok(content) = std::fs::read_to_string(&pcb_toml)
                    && let Ok(config) = PcbToml::parse(&content)
                {
                    let mut transitive: Vec<String> = config
                        .dependencies
                        .keys()
                        .filter(|dep_url| by_path.contains_key(dep_url.as_str()))
                        .cloned()
                        .collect();
                    transitive.sort();
                    dep_graph.insert(line.path.clone(), transitive);
                }
                break;
            }
        }
    }

    let mut printed = HashSet::new();
    let _ = crate::tree::print_tree(workspace_name.to_string(), root_deps, |url| {
        let version = by_path
            .get(url.as_str())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".into());
        let name = url.split('/').skip(1).collect::<Vec<_>>().join("/");
        let already = !printed.insert(url.clone());

        let label = format!("{} v{}{}", name, version, if already { " (*)" } else { "" });
        let children = if already {
            vec![]
        } else {
            dep_graph.get(url).cloned().unwrap_or_default()
        };
        (label, children)
    });
}

/// Result of vendoring operation
pub struct VendorResult {
    /// Number of packages vendored
    pub package_count: usize,
    /// Number of stale entries pruned from vendor/
    pub pruned_count: usize,
    /// Path to vendor directory
    pub vendor_dir: PathBuf,
}

/// Run auto-deps phase: detect missing dependencies from .zen files and add to pcb.toml
#[instrument(name = "auto_deps", skip_all)]
fn run_auto_deps(workspace_info: &mut WorkspaceInfo) -> Result<()> {
    log::debug!("Phase -1: Auto-detecting dependencies from .zen files");
    let auto_deps = crate::auto_deps::auto_add_zen_deps(workspace_info)?;

    if auto_deps.total_added > 0 {
        log::debug!(
            "Auto-added {} dependencies across {} package(s)",
            auto_deps.total_added,
            auto_deps.packages_updated
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
    Ok(())
}

fn is_branch_only_dep(detail: &DependencyDetail) -> bool {
    detail.branch.is_some()
        && detail.rev.is_none()
        && detail.version.is_none()
        && detail.path.is_none()
}

fn branch_only_mode(offline: bool, locked: bool) -> Option<&'static str> {
    if offline {
        Some("--offline")
    } else if locked {
        Some("--locked")
    } else {
        None
    }
}

fn branch_without_rev_error(
    dep_url: &str,
    branch: &str,
    mode: &str,
    manifest_path: Option<&Path>,
) -> anyhow::Error {
    let context = manifest_path.map_or_else(
        || format!("Dependency '{}'", dep_url),
        |path| format!("Dependency '{}' in {}", dep_url, path.display()),
    );
    anyhow::anyhow!(
        "{} uses branch='{}' without rev, which is not reproducible in {} mode.\n\
        Run `pcb build` or `pcb update` online to pin it to a commit.",
        context,
        branch,
        mode
    )
}

/// Normalize branch-only deps to branch+rev in online mode, and reject them in
/// locked/offline mode.
#[instrument(name = "normalize_branch_deps", skip_all)]
fn normalize_or_validate_branch_deps(
    workspace_info: &mut WorkspaceInfo,
    offline: bool,
    locked: bool,
) -> Result<usize> {
    let file_provider = DefaultFileProvider::new();
    let mut total_normalized = 0usize;

    for pkg in workspace_info.packages.values() {
        let pcb_toml_path = pkg.dir(&workspace_info.root).join("pcb.toml");
        if !pcb_toml_path.exists() {
            continue;
        }

        let mut config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
        let mut normalized = 0usize;

        for (dep_url, spec) in &mut config.dependencies {
            let DependencySpec::Detailed(detail) = spec else {
                continue;
            };
            if !is_branch_only_dep(detail) {
                continue;
            }

            let branch = detail.branch.as_deref().unwrap_or_default();
            if let Some(mode) = branch_only_mode(offline, locked) {
                return Err(branch_without_rev_error(
                    dep_url,
                    branch,
                    mode,
                    Some(&pcb_toml_path),
                ));
            }

            let commit = git::resolve_branch_head(dep_url, branch).with_context(|| {
                format!("Failed to resolve branch '{}' for {}", branch, dep_url)
            })?;
            detail.rev = Some(commit);
            normalized += 1;
        }

        if normalized > 0 {
            std::fs::write(&pcb_toml_path, toml::to_string_pretty(&config)?)?;
            total_normalized += normalized;
            log::debug!(
                "Pinned {} branch dependency declaration(s) in {}",
                normalized,
                pcb_toml_path.display()
            );
        }
    }

    if total_normalized > 0 {
        workspace_info.reload()?;
    }
    Ok(total_normalized)
}

/// Refresh `{ branch = "...", rev = "..." }` dependencies to current branch tip.
///
/// Returns the number of dependency entries whose `rev` changed.
pub fn refresh_branch_pins_in_manifests(
    pcb_toml_paths: &[PathBuf],
    package_filter: &[String],
) -> Result<usize> {
    let file_provider = DefaultFileProvider::new();
    let mut refreshed = 0usize;

    for pcb_toml_path in pcb_toml_paths {
        if !pcb_toml_path.exists() {
            continue;
        }

        let mut config = PcbToml::from_file(&file_provider, pcb_toml_path)?;
        let mut changed = false;

        for (dep_url, spec) in &mut config.dependencies {
            if !package_filter.is_empty() && !package_filter.iter().any(|p| dep_url.contains(p)) {
                continue;
            }

            let DependencySpec::Detailed(detail) = spec else {
                continue;
            };
            let (Some(branch), Some(current_rev)) =
                (detail.branch.as_deref(), detail.rev.as_deref())
            else {
                continue;
            };
            if detail.version.is_some() || detail.path.is_some() {
                continue;
            }

            match git::resolve_branch_head(dep_url, branch) {
                Ok(latest_rev) if latest_rev != current_rev => {
                    detail.rev = Some(latest_rev);
                    refreshed += 1;
                    changed = true;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!(
                        "  Warning: Failed to refresh branch '{}' for {}: {}",
                        branch, dep_url, e
                    );
                }
            }
        }

        if changed {
            std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
        }
    }

    Ok(refreshed)
}

/// Dependency resolution
///
/// Builds dependency graph using MVS, fetches dependencies,
/// and generates/updates the lockfile.
///
/// When `locked` is true:
/// - Auto-deps phase is skipped (dependencies must be declared in pcb.toml)
/// - Lockfile is verified instead of written (fails if out of date)
/// - Recommended for CI to catch missing dependencies or stale lockfiles
#[instrument(name = "resolve_dependencies", skip_all)]
pub fn resolve_dependencies(
    workspace_info: &mut WorkspaceInfo,
    offline: bool,
    locked: bool,
) -> Result<ResolutionResult> {
    let workspace_root = workspace_info.root.clone();

    // Ensure workspace cache symlink exists (<workspace>/.pcb/cache -> ~/.pcb/cache)
    // This provides stable workspace-relative paths in generated files (e.g., fp-lib-table)
    ensure_workspace_cache_symlink(&workspace_root)?;

    // Standalone mode: .zen file with inline manifest (no pcb.toml)
    // In this mode we skip auto-deps and lockfile writing
    let is_standalone = !workspace_root.join("pcb.toml").exists();

    log::debug!(
        "Dependency Resolution{}{}{}",
        if offline { " (offline)" } else { "" },
        if locked { " (locked)" } else { "" },
        if is_standalone { " (standalone)" } else { "" }
    );
    log::debug!("Workspace root: {}", workspace_root.display());

    // Phase -1: Auto-add missing dependencies from .zen files
    // Skip for standalone mode (no pcb.toml to modify)
    // Skip for locked/offline modes (trust the lockfile)
    if !is_standalone && !locked && !offline {
        run_auto_deps(workspace_info)?;
    }

    // Normalize branch-only dependencies to branch+rev in online mode and
    // reject branch-only deps in locked/offline mode.
    if !is_standalone {
        let normalized = normalize_or_validate_branch_deps(workspace_info, offline, locked)?;
        if normalized > 0 {
            log::debug!(
                "Pinned {} branch dependency declaration(s) to commits",
                normalized
            );
        }
    }

    // Validate patches are only at workspace root
    if let Some(config) = &workspace_info.config
        && !config.patch.is_empty()
        && config.workspace.is_none()
    {
        anyhow::bail!(
            "[patch] section is only allowed at workspace root\n  \
                Found in non-workspace pcb.toml at: {}/pcb.toml\n  \
                Move [patch] to workspace root or remove it.",
            workspace_root.display()
        );
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
    let mut manifest_cache: HashMap<(ModuleLine, Version), PcbToml> = HashMap::new();

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
            // Skip non-package lock entries (manifest hash is package-only).
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
        let pkg_dir = pkg.dir(&workspace_info.root);
        let package_name = pkg_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".into());
        let pcb_toml_path = pkg_dir.join("pcb.toml");

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
        let package_deps =
            collect_package_dependencies(&pcb_toml_path, &pkg.config, workspace_info)
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

            if let DependencySpec::Detailed(detail) = spec
                && detail.path.is_some()
            {
                continue;
            }

            let version = resolve_to_version(
                &mut pseudo_ctx,
                &dep.url,
                spec,
                workspace_info.lockfile.as_ref(),
                locked,
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

    let fetch_pool = build_fetch_pool()?;
    let cache_index = CacheIndex::open()?;
    let kicad_entries = workspace_info.kicad_library_entries();

    log::debug!(
        "Phase 1: Parallel dependency resolution ({} jobs)",
        fetch_pool.current_num_threads()
    );
    let _phase1_span = info_span!("fetch_deps").entered();

    // Wave-based parallel fetching with MVS
    let phase1_start = Instant::now();
    let mut wave_num = 0;
    let mut total_fetched = 0;

    loop {
        // Collect current wave: packages in queue that haven't been fetched yet
        // Use a HashSet to dedupe - the same ModuleLine can appear multiple times
        // in the queue (e.g., added from lockfile then upgraded in Phase 0)
        let candidates: Vec<_> = work_queue
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

        // Separate asset deps (managed by [[workspace.kicad_library]]) from code packages.
        // Asset deps have no manifest and stay out of the closure/lockfile.
        let mut wave = Vec::new();
        for (line, version) in candidates {
            match match_kicad_managed_repo(kicad_entries, &line.path, &version) {
                KicadRepoMatch::SelectorMatched => continue,
                KicadRepoMatch::SelectorMismatch => {
                    anyhow::bail!(
                        "Dependency {}@{} does not match any [[workspace.kicad_library]] major version",
                        line.path,
                        version
                    );
                }
                KicadRepoMatch::NotManaged => wave.push((line, version)),
            }
        }

        if wave.is_empty() {
            break;
        }

        wave_num += 1;
        let wave_start = Instant::now();
        log::debug!("  Wave {}: {} packages", wave_num, wave.len());

        // Parallel fetch all packages in this wave
        let results: Vec<_> = fetch_pool.install(|| {
            wave.par_iter()
                .map(|(line, version)| {
                    let result =
                        fetch_package(workspace_info, &line.path, version, &cache_index, offline);
                    (line.clone(), version.clone(), result)
                })
                .collect()
        });

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
                    locked,
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
    // Path-patched forks are now workspace members, so their deps are included automatically
    let closure = build_closure(&workspace_info.packages, &selected, &manifest_cache);

    log::debug!("Build set: {} dependencies", closure.len());

    // Phase 2.5: Materialize asset dependencies (KiCad symbol/footprint/model repos).
    log::debug!("Phase 2.5: Materialize asset dependencies");
    materialize_asset_deps(workspace_info, offline)?;
    log::debug!("Materialized asset dependencies");

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
        let new_lockfile = update_lockfile(&workspace_root, &old_lockfile, &closure, &patches)?;

        if locked {
            // In locked mode: fail if lockfile additions would be required.
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

    log::debug!("dependency resolution complete");

    let package_resolutions = build_native_resolution_map(workspace_info, &closure, &patches);

    Ok(ResolutionResult {
        workspace_info: workspace_info.clone(),
        package_resolutions,
        closure,
        lockfile_changed,
    })
}

/// Vendor dependencies from cache to vendor directory
///
/// Vendors entries matching workspace.vendor patterns plus any additional_patterns.
/// No-op if combined patterns is empty. Incremental - skips existing entries.
///
/// If `target_vendor_dir` is provided, vendors to that directory instead of
/// `workspace_info.root/vendor`. This is used by `pcb publish` to vendor into
/// the staging directory.
///
/// This function performs an incremental sync:
/// - Adds any packages/KiCad repos from the resolution that are missing in vendor/
/// - When `prune=true`, removes any {url}/{version-or-ref} directories not in the resolution
///
/// Pruning should be disabled when offline (can't re-fetch deleted deps).
#[instrument(name = "vendor_deps", skip_all)]
pub fn vendor_deps(
    resolution: &ResolutionResult,
    additional_patterns: &[String],
    target_vendor_dir: Option<&Path>,
    prune: bool,
) -> Result<VendorResult> {
    let workspace_info = &resolution.workspace_info;
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
            pruned_count: 0,
            vendor_dir,
        });
    }
    log::debug!("Vendor patterns: {:?}", patterns);

    let cache = &workspace_info.cache_dir;
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
            copy_dir_all(&src, &dst, &HashSet::new())?;
            package_count += 1;
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
        pruned_count,
        vendor_dir,
    })
}

/// Recursively copy a directory, excluding hidden directories/files and symlinks.
///
/// Optionally excludes specified directory roots (used when copying workspace
/// packages to exclude nested packages that are separate workspace members).
pub fn copy_dir_all(src: &Path, dst: &Path, excluded_roots: &HashSet<PathBuf>) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        // Skip hidden files/directories (starting with .)
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(name);
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            // Skip if this directory is the root of another workspace package
            if excluded_roots.contains(&src_path) {
                log::debug!(
                    "Skipping nested package dir during staging: {}",
                    src_path.display()
                );
                continue;
            }
            copy_dir_all(&src_path, &dst_path, excluded_roots)?;
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

/// Build the per-package resolution map.
///
/// Uses MVS family matching for package resolution and delegates to shared resolution
/// logic for the actual map building.
fn build_native_resolution_map(
    workspace_info: &WorkspaceInfo,
    closure: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
) -> HashMap<PathBuf, BTreeMap<String, PathBuf>> {
    // Use workspace cache path (symlink) for stable workspace-relative paths in generated files
    let cache = workspace_info.workspace_cache_dir();
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
        patches,
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
    build_resolution_map(&file_provider, &resolver, workspace_info, closure)
}

/// Collect dependencies for a package and transitive local deps
fn collect_package_dependencies(
    pcb_toml_path: &Path,
    config: &pcb_zen_core::config::PcbToml,
    workspace_info: &WorkspaceInfo,
) -> Result<Vec<UnresolvedDep>> {
    let package_dir = pcb_toml_path.parent().unwrap();
    let mut deps = HashMap::new();

    collect_deps_recursive(&config.dependencies, package_dir, &mut deps, workspace_info)
        .with_context(|| format!("in {}", pcb_toml_path.display()))?;

    Ok(deps.into_values().collect())
}

/// Recursively collect dependencies, handling transitive local path dependencies
fn collect_deps_recursive(
    current_deps: &BTreeMap<String, DependencySpec>,
    package_dir: &Path,
    deps: &mut HashMap<String, UnresolvedDep>,
    workspace_info: &WorkspaceInfo,
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

        // Check if this path dependency points to a workspace member
        // This is not allowed - workspace members are resolved automatically by URL
        if let Ok(canonical_path) = resolved_path.canonicalize() {
            for member_pkg in workspace_info.packages.values() {
                let member_dir = member_pkg.dir(&workspace_info.root);
                if let Ok(canonical_member) = member_dir.canonicalize()
                    && canonical_path == canonical_member
                {
                    anyhow::bail!(
                        "dependency '{}' uses path to workspace member; remove the 'path' field",
                        url
                    );
                }
            }
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
        collect_deps_recursive(
            &dep_config.dependencies,
            &resolved_path,
            deps,
            workspace_info,
        )
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
    tags::parse_relaxed_version(s).ok_or_else(|| anyhow::anyhow!("Invalid version string: {}", s))
}

fn pseudo_version_commit(version: &Version) -> Option<&str> {
    if !version.pre.starts_with("0.") {
        return None;
    }
    version
        .pre
        .as_str()
        .rsplit_once('-')
        .map(|(_, commit)| commit)
}

fn pseudo_matches_rev(version: &Version, rev: &str) -> bool {
    pseudo_version_commit(version).is_some_and(|commit| {
        // Accept full or shortened rev forms (e.g. 40-char in manifest vs 12-char in pseudo).
        commit.starts_with(rev) || rev.starts_with(commit)
    })
}

/// Materialize asset dependencies selected by dependency resolution.
fn materialize_asset_deps(workspace_info: &WorkspaceInfo, offline: bool) -> Result<()> {
    let targets = workspace_info.asset_dep_versions();
    if targets.is_empty() {
        return Ok(());
    }

    let workspace_cache = workspace_info.root.join(".pcb/cache");
    let missing: Vec<(String, Version)> = targets
        .iter()
        .filter(|(repo, version)| {
            !workspace_cache
                .join(repo)
                .join(version.to_string())
                .join(".pcb-cached")
                .exists()
        })
        .map(|(repo, version)| (repo.clone(), version.clone()))
        .collect();

    if offline && !missing.is_empty() {
        let first = &missing[0];
        anyhow::bail!(
            "{}@{} is not cached. Run `pcb build` once online to fetch it.",
            first.0,
            first.1
        );
    }

    if missing.is_empty() {
        return Ok(());
    }

    let total = missing.len();
    let spinner = Spinner::builder(format!(
        "Fetching {}",
        missing[0].0.rsplit('/').next().unwrap_or(&missing[0].0)
    ))
    .start();

    for (idx, (repo, version)) in missing.into_iter().enumerate() {
        let version_str = version.to_string();
        let repo_name = repo.rsplit('/').next().unwrap_or(&repo);
        spinner.set_message(format!(
            "Fetching [{}/{}] {}@{}",
            idx + 1,
            total,
            repo_name,
            version_str
        ));

        let http_mirror = kicad_http_mirror_template_for_repo(
            workspace_info.kicad_library_entries(),
            &repo,
            &version,
        )?
        .map(|template| crate::archive::render_http_mirror_url(template, &repo, &version_str))
        .transpose()
        .with_context(|| {
            format!(
                "Failed to render http_mirror URL for {}@{}",
                repo, version_str
            )
        })?;

        let cache_dir = cache_base().join(&repo).join(&version_str);
        let fetch_result = ensure_sparse_checkout(
            &cache_dir,
            &repo,
            &version_str,
            false,
            http_mirror.as_deref(),
        )
        .map(|_| ())
        .with_context(|| format!("Failed to fetch {}@{}", repo, version_str));

        if let Err(err) = fetch_result {
            spinner.error(format!("Failed to fetch {}", repo_name));
            return Err(err);
        }
    }

    spinner.finish();
    Ok(())
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
#[instrument(name = "fetch_package", skip_all, fields(path = %module_path))]
pub(crate) fn fetch_package(
    workspace_info: &WorkspaceInfo,
    module_path: &str,
    version: &Version,
    index: &CacheIndex,
    offline: bool,
) -> Result<PcbToml> {
    // 1. Workspace member override (highest priority)
    if let Some(member_pkg) = workspace_info.packages.get(module_path) {
        let member_toml = member_pkg.dir(&workspace_info.root).join("pcb.toml");
        return PcbToml::from_path(&member_toml);
    }

    // 2. Check if this module is patched with a local path
    // (branch/rev patches fall through to normal fetch - version already resolved)
    if let Some(patch) = workspace_info
        .config
        .as_ref()
        .and_then(|c| c.patch.get(module_path))
        && let Some(path) = &patch.path
    {
        let patched_path = workspace_info.root.join(path);
        let patched_toml = patched_path.join("pcb.toml");

        if !patched_toml.exists() {
            anyhow::bail!("Patch path {} has no pcb.toml", patched_path.display());
        }

        return PcbToml::from_path(&patched_toml);
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
        return PcbToml::from_path(&vendor_toml);
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

    let pcb_toml_path = checkout_dir.join("pcb.toml");

    // Fast path: index entry exists AND pcb.toml exists = valid cache
    if index.get_package(module_path, &version_str).is_some() && pcb_toml_path.exists() {
        return PcbToml::from_path(&pcb_toml_path);
    }

    // Slow path: fetch via sparse checkout (network)
    ensure_sparse_checkout(&checkout_dir, module_path, &version_str, true, None)?;

    // Compute hashes
    let content_hash = compute_content_hash_from_dir(&checkout_dir)?;
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
    PcbToml::from_path(&pcb_toml_path)
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
#[instrument(name = "build_closure", skip_all)]
fn build_closure(
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    selected: &HashMap<ModuleLine, Version>,
    manifest_cache: &HashMap<(ModuleLine, Version), PcbToml>,
) -> HashMap<ModuleLine, Version> {
    let mut closure = HashMap::new();
    let mut stack = Vec::new();

    // Seed DFS from all package dependencies (includes path-patched forks since they're workspace members)
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

        // Non-package dependencies (no pcb.toml) are intentionally excluded from
        // closure/lockfile tracking.
        if !manifest_cache.contains_key(&(line.clone(), version.clone())) {
            continue;
        }

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
/// - Revisions: { rev = "abcd1234" } → pseudo-version (uses lockfile if available)
/// - Branches: { branch = "main" } → pseudo-version
///
/// `rev` takes precedence over `branch` when both are present.
///
/// Branch-only dependencies are rejected in locked/offline mode (validated in
/// normalize_or_validate_branch_deps). Branch/rev pseudo-version resolution still
/// requires lockfile/cache when offline.
fn resolve_to_version(
    ctx: &mut PseudoVersionContext,
    module_path: &str,
    spec: &DependencySpec,
    lockfile: Option<&Lockfile>,
    locked: bool,
    offline: bool,
) -> Result<Version> {
    match spec {
        DependencySpec::Version(v) => parse_version_string(v),
        DependencySpec::Detailed(detail) => {
            if let Some(version) = &detail.version {
                parse_version_string(version)
            } else if let Some(rev) = &detail.rev {
                // Use locked pseudo-version if available (skip git ls-remote)
                if let Some(entry) = lockfile.and_then(|lf| lf.find_by_path(module_path))
                    && let Ok(locked_version) = Version::parse(&entry.version)
                {
                    if pseudo_matches_rev(&locked_version, rev) {
                        // Matching pseudo-version in lockfile, safe to reuse.
                        log::debug!("        Using locked v{} (from pcb.sum)", locked_version);
                        return Ok(locked_version);
                    }
                    if pseudo_version_commit(&locked_version).is_some() {
                        log::debug!(
                            "        Ignoring locked v{} for {} (rev mismatch: wanted {})",
                            locked_version,
                            module_path,
                            &rev[..8.min(rev.len())]
                        );
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
            } else if let Some(branch) = &detail.branch {
                // Branch-only deps should have been normalized earlier.
                if let Some(mode) = branch_only_mode(offline, locked) {
                    return Err(branch_without_rev_error(module_path, branch, mode, None));
                }
                ctx.resolve_branch(module_path, branch)
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
        let (repo_url, _) = split_repo_and_subpath(module_path);
        let commit = match self.index.get_branch_commit(repo_url, branch) {
            Some(c) => c,
            None => {
                log::debug!("        Resolving branch '{}'...", branch);
                let commit = git::resolve_branch_head(module_path, branch)?;
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
        let (repo_url, subpath) = split_repo_and_subpath(module_path);
        let bare_dir = self.ensure_bare_repo(repo_url)?;

        // `git fetch origin <sha>` only works with full 40-char object IDs.
        // Users (and tests) may provide short hashes, so resolve to a full commit first.
        let commit_full = git::rev_parse(&bare_dir, commit).ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to resolve rev '{}' in {} (provide a full commit hash or a ref that exists)",
                commit,
                repo_url
            )
        })?;
        let commit = commit_full.as_str();

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

/// Populate a cache directory with exclusive locking.
///
/// Only one process fetches; others wait for the lock and then see the completed result.
/// If the fetching process crashes, the OS releases the lock and waiters retry.
fn populate_cache<F>(cache_dir: &Path, marker: &str, fetch: F) -> Result<PathBuf>
where
    F: FnOnce(&Path) -> Result<()>,
{
    // Fast path: already complete
    if cache_dir.join(marker).exists() {
        return Ok(cache_dir.to_path_buf());
    }

    // Acquire exclusive lock (blocks until available, auto-releases on crash)
    let _lock = git::lock_dir(cache_dir)?;

    // Double-check after acquiring lock
    if cache_dir.join(marker).exists() {
        return Ok(cache_dir.to_path_buf());
    }

    // Clean up any incomplete cache before fetching
    let _ = std::fs::remove_dir_all(cache_dir);
    std::fs::create_dir_all(cache_dir)?;

    fetch(cache_dir)?;

    Ok(cache_dir.to_path_buf())
}

/// Ensure sparse-checkout working tree for a module at specific version.
///
/// Uses Git sparse-checkout to only materialize the subdirectory for nested packages.
///
/// Returns the package root path (where pcb.toml lives)
pub fn ensure_sparse_checkout(
    checkout_dir: &Path,
    module_path: &str,
    version_str: &str,
    add_v_prefix: bool,
    http_mirror_url: Option<&str>,
) -> Result<PathBuf> {
    let marker = if add_v_prefix {
        "pcb.toml"
    } else {
        ".pcb-cached"
    };

    populate_cache(checkout_dir, marker, |dest| {
        let (repo_url, subpath) = split_repo_and_subpath(module_path);
        let is_pseudo_version = version_str.contains("-0.");

        // Construct ref_spec (tag name or commit hash)
        // For pseudo-versions, use commit hash directly (no subpath prefix)
        // For regular versions, include subpath prefix in tag name
        let ref_spec = if is_pseudo_version {
            version_str.rsplit('-').next().unwrap().to_string()
        } else {
            let version_part = if add_v_prefix {
                format!("v{}", version_str)
            } else {
                version_str.to_string()
            };
            if subpath.is_empty() {
                version_part
            } else {
                format!("{}/{}", subpath, version_part)
            }
        };

        if !add_v_prefix {
            anyhow::ensure!(
                !is_pseudo_version,
                "KiCad library versions must be semver tags, got {} for {}",
                version_str,
                module_path
            );
            anyhow::ensure!(
                subpath.is_empty(),
                "KiCad library must resolve to repo root, got {}",
                module_path
            );

            if let Some(url) = http_mirror_url {
                if let Err(mirror_err) = crate::archive::fetch_http_archive(url, dest) {
                    log::warn!(
                        "HTTP mirror fetch failed for {}@{} ({}); falling back to git sparse checkout",
                        module_path,
                        version_str,
                        mirror_err
                    );
                    let _ = std::fs::remove_dir_all(dest);
                    std::fs::create_dir_all(dest)?;
                    fetch_via_git(dest, repo_url, &ref_spec, subpath, false).with_context(|| {
                        format!(
                            "Failed to fetch {} via git sparse checkout after mirror failure ({})",
                            module_path, url
                        )
                    })?;
                }
            } else {
                fetch_via_git(dest, repo_url, &ref_spec, subpath, false).with_context(|| {
                    format!("Failed to fetch {} via git sparse checkout", module_path)
                })?;
            }
            std::fs::write(dest.join(".pcb-cached"), "")?;
            return Ok(());
        }

        fetch_via_git(dest, repo_url, &ref_spec, subpath, is_pseudo_version)
            .with_context(|| format!("Failed to fetch {} via git sparse checkout", module_path))?;
        Ok(())
    })
}

/// Fetch a repo/ref into staging via git sparse checkout.
fn fetch_via_git(
    staging: &Path,
    repo_url: &str,
    ref_spec: &str,
    subpath: &str,
    is_pseudo: bool,
) -> Result<()> {
    std::fs::create_dir_all(staging)?;
    git::run_in(staging, &["init", "--template="])?;
    git::run_in(staging, &["config", "core.autocrlf", "false"])?;

    let https_url = format!("https://{}.git", repo_url);
    let _ = git::run_in(staging, &["remote", "add", "origin", &https_url]);
    git::run_in(staging, &["config", "remote.origin.promisor", "true"])?;
    git::run_in(
        staging,
        &["config", "remote.origin.partialclonefilter", "blob:none"],
    )?;

    let fetch_ref = if is_pseudo {
        ref_spec.to_string()
    } else {
        format!("refs/tags/{}", ref_spec)
    };
    let fetch_args = [
        "fetch",
        "--depth=1",
        "--filter=blob:none",
        "origin",
        &fetch_ref,
    ];
    if git::run_in(staging, &fetch_args).is_err() {
        git::run_in(
            staging,
            &[
                "remote",
                "set-url",
                "origin",
                &git::format_ssh_url(repo_url),
            ],
        )?;
        git::run_in(staging, &fetch_args)?;
    }

    if !subpath.is_empty() {
        git::run_in(staging, &["sparse-checkout", "init", "--cone"])?;
        git::run_in(staging, &["sparse-checkout", "set", subpath])?;
    }
    git::run_in(staging, &["reset", "--hard", "FETCH_HEAD"])?;

    // For nested packages: move subpath contents to root.
    if !subpath.is_empty() {
        let subpath_dir = staging.join(subpath);
        anyhow::ensure!(subpath_dir.exists(), "Subpath '{}' not found", subpath);

        // Delete all except .git* and subpath root.
        let subpath_root = subpath.split('/').next().unwrap();
        for entry in std::fs::read_dir(staging)?.flatten() {
            let name = entry.file_name();
            if name != ".git" && !name.to_string_lossy().starts_with(".git") && name != subpath_root
            {
                let _ = std::fs::remove_dir_all(entry.path())
                    .or_else(|_| std::fs::remove_file(entry.path()));
            }
        }

        // Move subpath contents to root and clean up the subpath root.
        for entry in std::fs::read_dir(&subpath_dir)? {
            let entry = entry?;
            std::fs::rename(entry.path(), staging.join(entry.file_name()))?;
        }
        std::fs::remove_dir_all(staging.join(subpath_root))?;
    }

    Ok(())
}

/// Build lockfile from current resolution
///
/// Creates a fresh lockfile containing only the entries needed for current resolution.
/// Unused entries from the old lockfile are automatically excluded.
#[instrument(name = "update_lockfile", skip_all)]
fn update_lockfile(
    workspace_root: &Path,
    old_lockfile: &Lockfile,
    closure: &HashMap<ModuleLine, Version>,
    patches: &BTreeMap<String, PatchSpec>,
) -> Result<Lockfile> {
    let mut new_lockfile = Lockfile::default();

    let total_count = closure.len();
    if total_count > 0 {
        log::debug!("  Verifying {} entries...", total_count);
    }

    let index = CacheIndex::open()?;

    for (line, version) in closure {
        // Skip packages with path patches (local overrides are not locked)
        if let Some(patch) = find_matching_patch(&line.path, patches)
            && patch.path.is_some()
        {
            continue;
        }
        let version_str = version.to_string();

        // Check if vendored - if so, reuse existing lockfile entry
        let vendor_dir = workspace_root
            .join("vendor")
            .join(&line.path)
            .join(&version_str);
        if let Some(existing) = old_lockfile.get(&line.path, &version_str)
            && vendor_dir.exists()
        {
            new_lockfile.insert(existing.clone());
            continue;
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

    log::debug!("  {} entries", new_lockfile.entries.len());

    Ok(new_lockfile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::MemberPackage;
    use tempfile::TempDir;

    fn workspace_with_root_config(config: PcbToml) -> WorkspaceInfo {
        let mut packages = BTreeMap::new();
        packages.insert(
            "workspace".to_string(),
            MemberPackage {
                rel_path: PathBuf::new(),
                config: config.clone(),
                version: None,
                dirty: false,
            },
        );

        WorkspaceInfo {
            root: PathBuf::from("/workspace"),
            cache_dir: PathBuf::new(),
            config: Some(config),
            packages,
            lockfile: None,
            errors: vec![],
        }
    }

    #[test]
    fn test_kicad_repos_offline_requires_cache() {
        let temp = TempDir::new().unwrap();
        let mut config = PcbToml::default();
        config.workspace = Some(pcb_zen_core::config::WorkspaceConfig {
            kicad_library: vec![pcb_zen_core::config::KicadLibraryConfig {
                version: Version::new(9, 0, 0),
                symbols: "gitlab.com/kicad/libraries/kicad-symbols".to_string(),
                footprints: "gitlab.com/kicad/libraries/kicad-footprints".to_string(),
                models: BTreeMap::new(),
                http_mirror: None,
            }],
            ..Default::default()
        });

        let mut workspace = workspace_with_root_config(config);
        workspace.root = temp.path().to_path_buf();
        let err = materialize_asset_deps(&workspace, true)
            .expect_err("expected offline mode to require cached asset deps");

        assert!(err.to_string().contains("not cached"));
    }
}
