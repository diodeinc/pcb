use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use pcb_zen_core::config::{find_workspace_root, DependencySpec, PcbToml};
use pcb_zen_core::{DefaultFileProvider, FileProvider};
use semver::Version;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

/// Dependency entry before resolution
#[derive(Debug, Clone)]
struct UnresolvedDep {
    url: String,
    spec: DependencySpec,
    source: String, // For error messages
}

/// Resolved dependency with concrete version
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ResolvedDep {
    url: String,
    version: Version,
    source: DependencySource,
}

#[derive(Debug, Clone)]
enum DependencySource {
    Version(Version),
    Branch(String),
    Revision(String),
    Path(PathBuf),
    Patch(PathBuf),
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

    println!("🔍 V2 Dependency Resolution");
    println!("  Workspace root: {}", workspace_root.display());

    // Discover member packages
    let member_patterns = v2
        .workspace
        .as_ref()
        .map(|w| w.members.as_slice())
        .unwrap_or(&[]);
    let packages = discover_packages(file_provider, workspace_root, member_patterns)?;

    // Display workspace type
    if v2.workspace.is_some() {
        println!("  Type: Explicit workspace");
        if !member_patterns.is_empty() {
            println!("  Member patterns: {:?}", member_patterns);
        }
    } else {
        println!("  Type: Standalone package (implicit workspace)");
    }

    // Display discovered packages
    println!("\n📦 Discovered {} package(s):", packages.len());
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

    // Resolve dependencies per-package
    println!("\n🔗 Per-Package Dependency Resolution:");
    let mut all_package_deps = Vec::new();

    for (pcb_toml_path, config) in &packages {
        let PcbToml::V2(v2) = config else { continue };

        let package_name = pcb_toml_path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".into());

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

        println!(
            "    Collected {} unresolved dependencies",
            package_deps.len()
        );
        for dep in &package_deps {
            println!("      - {}", dep.url);
        }

        // Apply MVS per-package
        let resolved = apply_mvs(&package_deps)?;

        println!("    Resolved {} dependencies:", resolved.len());
        for dep in &resolved {
            match &dep.source {
                DependencySource::Version(v) => {
                    println!("      - {}@v{}", dep.url, v);
                }
                DependencySource::Branch(b) => {
                    println!("      - {}@{}", dep.url, b);
                }
                DependencySource::Revision(r) => {
                    println!("      - {}@{}", dep.url, &r[..8.min(r.len())]);
                }
                DependencySource::Path(p) => {
                    println!("      - {} → {}", dep.url, p.display());
                }
                DependencySource::Patch(_) => unreachable!("Patches applied later"),
            }
        }

        all_package_deps.extend(resolved);
    }

    // Merge all package dependency graphs (union)
    let merged_deps = merge_dependency_graphs(all_package_deps);

    // Apply patches at workspace level
    let final_deps = apply_patches(merged_deps, &patches);

    println!("\n✨ Workspace Dependency Graph (merged):");
    println!("  Total unique dependencies: {}", final_deps.len());
    for dep in &final_deps {
        match &dep.source {
            DependencySource::Version(v) => {
                println!("  - {}@v{}", dep.url, v);
            }
            DependencySource::Branch(b) => {
                println!("  - {}@{}", dep.url, b);
            }
            DependencySource::Revision(r) => {
                println!("  - {}@{}", dep.url, &r[..8.min(r.len())]);
            }
            DependencySource::Path(p) => {
                println!("  - {} → {}", dep.url, p.display());
            }
            DependencySource::Patch(p) => {
                println!("  - {} → {} (patched)", dep.url, p.display());
            }
        }
    }

    println!("\n✅ V2 dependency resolution complete (stub - exiting for now)");
    std::process::exit(0);
}

/// Collect dependencies for a single package, resolving workspace inheritance and transitive deps
fn collect_package_dependencies(
    package_root: &Path,
    package_deps: &HashMap<String, DependencySpec>,
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_root: &Path,
) -> Result<Vec<UnresolvedDep>> {
    let mut deps = Vec::new();
    let mut visited = HashMap::new();

    collect_deps_recursive(
        package_root,
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
    current_package_root: &Path,
    current_deps: &HashMap<String, DependencySpec>,
    workspace_deps: &HashMap<String, DependencySpec>,
    workspace_root: &Path,
    deps: &mut Vec<UnresolvedDep>,
    visited: &mut HashMap<String, ()>,
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
                    source: current_package_root.display().to_string(),
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
            source: current_package_root.display().to_string(),
        });

        // Avoid infinite loops
        if visited.contains_key(url) {
            continue;
        }
        visited.insert(url.clone(), ());

        // Recursively resolve transitive dependencies
        let dep_pcb_toml = resolved_path.join("pcb.toml");
        if dep_pcb_toml.exists() {
            let file_provider = DefaultFileProvider::new();
            if let Ok(PcbToml::V2(dep_config)) = PcbToml::from_file(&file_provider, &dep_pcb_toml) {
                collect_deps_recursive(
                    &resolved_path,
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

/// Merge multiple resolved dependency graphs by taking union
fn merge_dependency_graphs(graphs: Vec<ResolvedDep>) -> Vec<ResolvedDep> {
    let mut merged: HashMap<(String, Version), ResolvedDep> = HashMap::new();

    for dep in graphs {
        let key = (dep.url.clone(), dep.version.clone());
        merged.entry(key).or_insert(dep);
    }

    merged.into_values().collect()
}

/// Apply Minimal Version Selection algorithm
///
/// Groups dependencies by (url, semver_family) and selects maximum version per group.
/// Multiple semver families can coexist.
///
/// **Semver 0.x handling**: For 0.x versions, the minor version acts as the major version:
/// - 0.1.x and 0.2.x are incompatible (different families)
/// - 0.3.11 and 0.3.13 are compatible (same 0.3.x family, MVS selects 0.3.13)
/// - For 1.x+, only the major version matters
///
/// Path/branch/rev dependencies are kept separate (don't participate in MVS).
fn apply_mvs(deps: &[UnresolvedDep]) -> Result<Vec<ResolvedDep>> {
    let mut resolved = Vec::new();

    // Separate version deps from non-version deps (path/branch/rev)
    let mut version_deps = Vec::new();
    let mut non_version_deps = Vec::new();

    for dep in deps {
        if is_non_version_dep(&dep.spec) {
            non_version_deps.push(dep);
        } else {
            version_deps.push(dep);
        }
    }

    // Handle non-version deps first (just pass through, no MVS)
    for dep in non_version_deps {
        let source = match &dep.spec {
            DependencySpec::Detailed(detail) => {
                if let Some(path) = &detail.path {
                    DependencySource::Path(PathBuf::from(path))
                } else if let Some(branch) = &detail.branch {
                    DependencySource::Branch(branch.clone())
                } else if let Some(rev) = &detail.rev {
                    DependencySource::Revision(rev.clone())
                } else {
                    unreachable!()
                }
            }
            _ => unreachable!(),
        };

        resolved.push(ResolvedDep {
            url: dep.url.clone(),
            version: Version::new(0, 0, 0), // Placeholder for non-version deps
            source,
        });
    }

    // Group version deps by (url, semver_group)
    // For 0.x versions, minor acts as major (0.1.x and 0.2.x are incompatible)
    // For 1.x+, major is the grouping key
    let mut groups: HashMap<(String, u64, u64), Vec<&UnresolvedDep>> = HashMap::new();

    for dep in version_deps {
        let version = extract_version(&dep.spec)?;
        let group_key = if version.major == 0 {
            (version.major, version.minor) // 0.x: group by minor
        } else {
            (version.major, 0) // 1.x+: group by major only
        };

        groups
            .entry((dep.url.clone(), group_key.0, group_key.1))
            .or_insert_with(Vec::new)
            .push(dep);
    }

    // Select max version per group
    for ((url, _major, _minor), group_deps) in groups {
        // Find max version in this group
        let mut max_version = Version::new(0, 0, 0);
        let mut max_dep = None;

        for dep in group_deps {
            let version = extract_version(&dep.spec)?;
            if version > max_version {
                max_version = version.clone();
                max_dep = Some(dep);
            }
        }

        if let Some(dep) = max_dep {
            let source = match &dep.spec {
                DependencySpec::Version(v) => DependencySource::Version(parse_version_string(v)?),
                DependencySpec::Detailed(detail) => {
                    if let Some(version) = &detail.version {
                        DependencySource::Version(parse_version_string(version)?)
                    } else {
                        unreachable!("Version dep without version field")
                    }
                }
            };

            resolved.push(ResolvedDep {
                url: url.clone(),
                version: max_version,
                source,
            });
        }
    }

    Ok(resolved)
}

/// Check if a dependency spec is non-version (path/branch/rev)
fn is_non_version_dep(spec: &DependencySpec) -> bool {
    match spec {
        DependencySpec::Detailed(detail) => {
            detail.path.is_some() || detail.branch.is_some() || detail.rev.is_some()
        }
        DependencySpec::Version(_) => false,
    }
}

/// Extract version from dependency spec for MVS grouping
fn extract_version(spec: &DependencySpec) -> Result<Version> {
    match spec {
        DependencySpec::Version(v) => parse_version_string(v),
        DependencySpec::Detailed(detail) => {
            if let Some(version) = &detail.version {
                parse_version_string(version)
            } else {
                // Non-version dependencies default to 0.0.0 for grouping
                Ok(Version::new(0, 0, 0))
            }
        }
    }
}

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

/// Apply patch overrides
fn apply_patches(
    mut resolved: Vec<ResolvedDep>,
    patches: &HashMap<String, pcb_zen_core::config::PatchSpec>,
) -> Vec<ResolvedDep> {
    for dep in &mut resolved {
        if let Some(patch) = patches.get(&dep.url) {
            dep.source = DependencySource::Patch(PathBuf::from(&patch.path));
        }
    }
    resolved
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
