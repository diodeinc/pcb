use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use pcb_zen_core::config::{find_workspace_root, DependencySpec, LockEntry, Lockfile, PcbToml};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

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

/// Check if the input paths are in a V2 workspace and run dependency resolution if needed
///
/// This is called once per `pcb build` invocation (workspace-first architecture).
/// For V2 workspaces, it runs dependency resolution before any .zen file discovery.
pub fn maybe_resolve_v2_workspace(paths: &[PathBuf]) -> Result<()> {
    let input_path = if paths.is_empty() {
        std::env::current_dir()?
    } else {
        paths[0].clone()
    };

    let file_provider = Arc::new(DefaultFileProvider::new());
    let workspace_root = find_workspace_root(&*file_provider, &input_path);

    let pcb_toml_path = workspace_root.join("pcb.toml");
    if !file_provider.exists(&pcb_toml_path) {
        return Ok(());
    }

    let config = PcbToml::from_file(&*file_provider, &pcb_toml_path)?;
    if let PcbToml::V2(_) = config {
        resolve_dependencies(&*file_provider, &workspace_root, &config)?;
    }

    Ok(())
}

/// V2 dependency resolution
///
/// Discovers member packages and builds dependency graph.
/// TODO: Implement MVS algorithm and lockfile verification.
fn resolve_dependencies(
    file_provider: &dyn FileProvider,
    workspace_root: &Path,
    config: &PcbToml,
) -> Result<()> {
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
    let packages = discover_packages(file_provider, workspace_root, member_patterns)?;

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

    let workspace_deps = v2
        .workspace
        .as_ref()
        .map(|w| &w.dependencies)
        .cloned()
        .unwrap_or_default();

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
        let package_deps = collect_package_dependencies(
            pcb_toml_path.parent().unwrap(),
            &v2.dependencies,
            &workspace_deps,
            workspace_root,
        )?;

        if package_deps.is_empty() {
            println!("    No dependencies");
            continue;
        }

        println!("    Seeding {} dependencies into MVS:", package_deps.len());

        // Seed MVS state from this package's dependencies
        for dep in &package_deps {
            // Skip local path dependencies (handled separately)
            if let DependencySpec::Detailed(detail) = &dep.spec {
                if detail.path.is_some() {
                    println!(
                        "      - {} → {} (local path)",
                        dep.url,
                        detail.path.as_ref().unwrap()
                    );
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
    let build_set = build_closure(
        &packages,
        &workspace_deps,
        workspace_root,
        &selected,
        &manifest_cache,
    )?;

    println!("Build set: {} dependencies", build_set.len());

    println!("\nFinal Resolved Dependencies:");
    for (line, version) in &selected {
        if build_set.contains(&(line.clone(), version.clone())) {
            println!("  {}@v{} ({})", line.path, version, line.family);
        }
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
    std::process::exit(0);
}

/// Collect dependencies for a single package, resolving workspace inheritance and transitive deps
fn collect_package_dependencies(
    _package_root: &Path,
    package_deps: &HashMap<String, DependencySpec>,
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_root: &Path,
) -> Result<Vec<UnresolvedDep>> {
    let mut deps = Vec::new();
    let mut visited = HashSet::new();

    collect_deps_recursive(
        package_deps,
        workspace_deps,
        workspace_root,
        &mut deps,
        &mut visited,
    )?;

    Ok(deps)
}

/// Recursively collect dependencies, handling transitive local path dependencies
fn collect_deps_recursive(
    current_deps: &HashMap<String, DependencySpec>,
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_root: &Path,
    deps: &mut Vec<UnresolvedDep>,
    visited: &mut HashSet<String>,
) -> Result<()> {
    for (url, spec) in current_deps {
        // Resolve workspace inheritance
        let resolved_spec = match spec {
            DependencySpec::Detailed(detail) if detail.workspace => {
                workspace_deps
                    .get(url)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Package inherits dependency {} from workspace, but it's not defined in [workspace.dependencies]",
                            url
                        )
                    })?
                    .clone()
            }
            other => other.clone(),
        };

        // Check if it's a local path dependency
        let local_path = match &resolved_spec {
            DependencySpec::Detailed(detail) if detail.path.is_some() => {
                detail.path.as_ref().unwrap()
            }
            _ => {
                // Not a local path dep, just add it
                deps.push(UnresolvedDep {
                    url: url.clone(),
                    spec: resolved_spec,
                });
                continue;
            }
        };

        // Resolve local path relative to workspace root (like Cargo)
        let resolved_path = workspace_root.join(local_path);

        // Validate path exists
        if !resolved_path.exists() {
            anyhow::bail!(
                "Local path dependency '{}' not found at {} (path '{}' is relative to workspace root {})",
                url,
                resolved_path.display(),
                local_path,
                workspace_root.display()
            );
        }

        // Add this dep
        deps.push(UnresolvedDep {
            url: url.clone(),
            spec: resolved_spec,
        });

        // Avoid infinite loops
        if !visited.insert(url.clone()) {
            continue;
        }

        // Recursively resolve transitive dependencies
        let dep_pcb_toml = resolved_path.join("pcb.toml");
        if dep_pcb_toml.exists() {
            let file_provider = DefaultFileProvider::new();
            if let Ok(PcbToml::V2(dep_config)) = PcbToml::from_file(&file_provider, &dep_pcb_toml) {
                collect_deps_recursive(
                    &dep_config.dependencies,
                    workspace_deps,
                    workspace_root,
                    deps,
                    visited,
                )?;
            }
        }
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
    use std::process::Command;

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

    // Determine if this is a pseudo-version or regular tag
    let is_pseudo_version = version.pre.starts_with("0.");
    let ref_spec = if is_pseudo_version {
        // Extract commit hash from pseudo-version (last segment after final -)
        let version_str = version.to_string();
        version_str
            .split('-')
            .next_back()
            .unwrap_or("HEAD")
            .to_string()
    } else {
        format!("v{}", version)
    };

    // Cache directory: ~/.pcb/cache/{module_path}/{version}/
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    let cache_base = home.join(".pcb").join("cache");
    let checkout_dir = cache_base.join(module_path).join(version.to_string());

    // Check if we've already processed this version
    let pcb_toml_path = checkout_dir.join("pcb.toml");
    let marker_path = checkout_dir.join(".no-manifest");
    let cache_marker = checkout_dir.join(".pcbcache");

    // Prefer using cached manifest if available (skip git operations)
    if pcb_toml_path.exists() && (cache_marker.exists() || marker_path.exists()) {
        // Successfully cached with manifest OR asset package
        println!("        Using cached manifest");
        return read_manifest_from_path(&pcb_toml_path, module_path);
    }

    if marker_path.exists() {
        // Already tried, no V2 manifest (asset package or V1)
        println!("        Cached (no manifest)");
        return Ok(HashMap::new());
    }

    // Clone with --filter=blob:none, then extract just pcb.toml using git show
    println!("        Cloning (blob-filtered)");

    if git_clone_with_fallback(
        module_path,
        &["--filter=blob:none", "--no-checkout"],
        &checkout_dir,
    )
    .is_err()
    {
        let marker_path = checkout_dir.join(".no-manifest");
        fs::create_dir_all(&checkout_dir)?;
        let _ = fs::write(&marker_path, "Failed to clone (tried HTTPS and SSH)\n");
        return Ok(HashMap::new());
    }

    // For pseudo-versions, fetch the specific commit
    if is_pseudo_version {
        let fetch_status = Command::new("git")
            .arg("fetch")
            .arg("--depth=1")
            .arg("origin")
            .arg(&ref_spec)
            .current_dir(&checkout_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !fetch_status.success() {
            let marker_path = checkout_dir.join(".no-manifest");
            let _ = fs::write(&marker_path, "Failed to fetch commit\n");
            return Ok(HashMap::new());
        }
    }

    // Extract just pcb.toml using git show (doesn't require full checkout)
    let show_output = Command::new("git")
        .arg("show")
        .arg(format!("{}:pcb.toml", ref_spec))
        .current_dir(&checkout_dir)
        .output();

    match show_output {
        Ok(output) if output.status.success() => {
            // Write pcb.toml to disk
            fs::write(&pcb_toml_path, &output.stdout)?;
        }
        _ => {
            // No pcb.toml at this ref (asset package)
            let marker_path = checkout_dir.join(".no-manifest");
            let _ = fs::write(&marker_path, "No pcb.toml in repository\n");
            return Ok(HashMap::new());
        }
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

    match config {
        PcbToml::V2(v2) => Ok(v2.dependencies),
        PcbToml::V1(_) => {
            // V1 manifest - treat as asset package for now (no transitive deps)
            // Create marker to avoid re-parsing
            let marker_path = pcb_toml_path.parent().unwrap().join(".no-manifest");
            let _ = fs::write(&marker_path, "V1 manifest - no transitive dependencies\n");
            Ok(HashMap::new())
        }
    }
}

/// Build the final dependency closure using selected versions
///
/// DFS from workspace package dependencies using selected versions.
fn build_closure(
    packages: &[(PathBuf, PcbToml)],
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_root: &Path,
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

        let package_deps = collect_package_dependencies(
            pcb_toml_path.parent().unwrap(),
            &v2.dependencies,
            workspace_deps,
            workspace_root,
        )?;

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

/// Create a canonical, deterministic tar archive from a directory
///
/// Rules from packaging.md:
/// - Regular files and directories only (no symlinks, devices)
/// - Relative paths, forward slashes, lexicographic order
/// - Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
/// - File mode: 0644, directory mode: 0755
/// - End with two 512-byte zero blocks
/// - Respect .gitignore and filter internal marker files
fn create_canonical_tar<W: std::io::Write>(dir: &Path, writer: W) -> Result<()> {
    use std::fs;
    use tar::{Builder, Header};

    let mut builder = Builder::new(writer);

    // Use PAX format for long path support (KiCad footprints have very long paths)
    builder.mode(tar::HeaderMode::Deterministic);

    // Collect all files and directories, sorted lexicographically
    // Use ignore crate to respect .gitignore
    let mut entries = Vec::new();
    for result in WalkBuilder::new(dir)
        .hidden(false) // Don't skip hidden files (we want .zen files if hidden)
        .git_ignore(true) // Respect .gitignore
        .git_global(false) // Don't use global gitignore
        .git_exclude(true) // Respect .git/info/exclude
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

        // Remove Phase 1's partial clone and do fresh full shallow clone
        if cache_dir.exists() {
            std::fs::remove_dir_all(&cache_dir)?;
        }
        clone_full_shallow(&cache_dir, &line.path, &version.to_string())?;

        // Compute hashes immediately after fetch
        print!("      Computing hashes... ");
        std::io::Write::flush(&mut std::io::stdout())?;

        let content_hash = compute_content_hash_from_dir(&cache_dir)?;
        let manifest_hash = if cache_dir.join("pcb.toml").exists() {
            let manifest_content = std::fs::read_to_string(cache_dir.join("pcb.toml"))?;
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

/// Clone full repository at specific version using shallow clone
fn clone_full_shallow(cache_dir: &Path, module_path: &str, version_str: &str) -> Result<()> {
    let is_pseudo_version = version_str.contains("-0.");

    if is_pseudo_version {
        // Pseudo-versions: clone repo, fetch commit, checkout
        let commit = version_str.rsplit('-').next().unwrap();

        git_clone_with_fallback(module_path, &["--no-checkout"], cache_dir)?;
        run_git_in(&["fetch", "--depth=1", "origin", commit], cache_dir)?;
        run_git_in(&["checkout", commit], cache_dir)?;
    } else {
        // Regular versions: try v-prefix first, fallback to no prefix
        let v_tag = format!("v{}", version_str);

        if git_clone_with_fallback(module_path, &["--depth=1", "--branch", &v_tag], cache_dir)
            .is_err()
        {
            // Try without v-prefix
            git_clone_with_fallback(
                module_path,
                &["--depth=1", "--branch", version_str],
                cache_dir,
            )?;
        }
    }

    Ok(())
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

    let https_url = format!("https://{}.git", module_path);
    let ssh_url = format_ssh_url(module_path);

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
fn git_clone_with_fallback(module_path: &str, args: &[&str], dest: &Path) -> Result<()> {
    use std::process::Command;

    let https_url = format!("https://{}.git", module_path);
    let ssh_url = format_ssh_url(module_path);
    let dest_str = dest
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid path"))?;

    // Try HTTPS first
    let mut clone_args = vec!["clone"];
    clone_args.extend_from_slice(args);
    clone_args.push(&https_url);
    clone_args.push(dest_str);

    let https_result = Command::new("git")
        .args(&clone_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(status) = https_result {
        if status.success() {
            return Ok(());
        }
    }

    // Fallback to SSH - rebuild args with SSH URL
    let mut ssh_args = vec!["clone"];
    ssh_args.extend_from_slice(args);
    ssh_args.push(&ssh_url);
    ssh_args.push(dest_str);

    let status = Command::new("git")
        .args(&ssh_args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;

    if !status.success() {
        anyhow::bail!("Failed to clone {} (tried HTTPS and SSH)", module_path);
    }

    Ok(())
}

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
