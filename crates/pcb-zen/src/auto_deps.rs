use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::ast_utils::{skip_vendor, visit_string_literals};
use crate::bare_repo::BareRepoCache;
use crate::cache_index::CacheIndex;
use crate::resolve::fetch_package;
use crate::workspace::{WorkspaceInfo, WorkspaceInfoExt};
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::kicad_library::kicad_dependency_aliases;
use pcb_zen_core::load_spec::LoadSpec;

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub versions_corrected: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
    pub unknown_urls: Vec<(PathBuf, Vec<String>)>,
}

#[derive(Debug, Default)]
struct CollectedImports {
    aliases: HashSet<String>,
    urls: HashSet<String>,
    relative_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ResolvedDep {
    module_path: String,
    version: String,
}

/// Scan workspace for .zen files and auto-add missing dependencies to pcb.toml files
pub fn auto_add_zen_deps(workspace_info: &WorkspaceInfo) -> Result<AutoDepsSummary> {
    let workspace_root = &workspace_info.root;
    let packages = &workspace_info.packages;
    let mut package_imports = collect_imports_by_package(workspace_info)?;
    let mut summary = AutoDepsSummary::default();
    let file_provider = DefaultFileProvider::new();
    let kicad_entries = workspace_info.kicad_library_entries();
    let kicad_aliases = kicad_dependency_aliases(&kicad_entries);
    let configured_kicad_versions = workspace_info.asset_dep_versions();

    let index = CacheIndex::open()?;
    let mut bare_repos = BareRepoCache::default();
    let manifests = collect_manifest_paths(workspace_root, packages, &package_imports);

    for pcb_toml_path in manifests {
        let imports = package_imports.remove(&pcb_toml_path).unwrap_or_default();
        let existing_config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
        let mut deps_to_add: Vec<ResolvedDep> = Vec::new();
        let mut unknown_aliases: Vec<String> = Vec::new();
        let mut unknown_urls: Vec<String> = Vec::new();

        // Resolve @aliases to repo URLs via [[workspace.kicad_library]].
        let mut alias_urls: HashSet<String> = HashSet::new();
        for alias in &imports.aliases {
            match kicad_aliases.get(alias) {
                Some(repo_url) => {
                    alias_urls.insert(repo_url.clone());
                }
                None => unknown_aliases.push(alias.clone()),
            }
        }

        // Resolve all URLs (direct file imports + resolved alias repos) uniformly.
        for url in imports.urls.iter().chain(&alias_urls) {
            if is_url_covered_by_manifest(url, &existing_config) {
                continue;
            }
            if let Some(dep) = resolve_kicad_url(url, &configured_kicad_versions) {
                deps_to_add.push(dep);
                continue;
            }
            match resolve_dep_candidate(url, packages, &index) {
                Some(candidate)
                    if can_materialize_dep(workspace_info, &mut bare_repos, &index, &candidate) =>
                {
                    deps_to_add.push(candidate);
                }
                _ => unknown_urls.push(url.clone()),
            }
        }

        push_unknown(
            &mut summary.unknown_aliases,
            &pcb_toml_path,
            unknown_aliases,
        );
        push_unknown(&mut summary.unknown_urls, &pcb_toml_path, unknown_urls);

        let (added, corrected) =
            mutate_manifest_dependencies(&pcb_toml_path, &deps_to_add, packages)?;
        if added > 0 || corrected > 0 {
            summary.total_added += added;
            summary.versions_corrected += corrected;
            summary.packages_updated += 1;
        }
    }

    Ok(summary)
}

fn collect_manifest_paths(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    package_imports: &HashMap<PathBuf, CollectedImports>,
) -> BTreeSet<PathBuf> {
    let mut manifests: BTreeSet<PathBuf> = package_imports.keys().cloned().collect();

    if packages.is_empty() {
        let root_pcb_toml = workspace_root.join("pcb.toml");
        if root_pcb_toml.exists() {
            manifests.insert(root_pcb_toml);
        }
        return manifests;
    }

    for pkg in packages.values() {
        let pcb_toml_path = pkg.dir(workspace_root).join("pcb.toml");
        if pcb_toml_path.exists() {
            manifests.insert(pcb_toml_path);
        }
    }

    manifests
}

fn is_url_covered_by_manifest(url: &str, config: &PcbToml) -> bool {
    config
        .dependencies
        .keys()
        .any(|dep| dep_covers_url(dep, url))
}

fn dep_covers_url(dep: &str, url: &str) -> bool {
    url == dep
        || url
            .strip_prefix(dep)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn push_unknown(summary: &mut Vec<(PathBuf, Vec<String>)>, path: &Path, items: Vec<String>) {
    if items.is_empty() {
        return;
    }
    summary.push((path.to_path_buf(), items));
}

fn can_materialize_dep(
    workspace_info: &WorkspaceInfo,
    bare_repos: &mut BareRepoCache,
    index: &CacheIndex,
    dep: &ResolvedDep,
) -> bool {
    let Some(parsed_version) = crate::tags::parse_relaxed_version(&dep.version) else {
        log::debug!(
            "Skipping auto-dep package {}@{} (invalid version)",
            dep.module_path,
            dep.version
        );
        return false;
    };

    let (repo_url, _) = pcb_zen_core::config::split_repo_and_subpath(&dep.module_path);
    let bare_repo = match bare_repos.ensure_synced(repo_url) {
        Ok(repo) => repo.clone(),
        Err(e) => {
            log::debug!(
                "Skipping auto-dep package {}@{} (repo sync failed): {}",
                dep.module_path,
                dep.version,
                e
            );
            return false;
        }
    };

    if let Err(e) = fetch_package(
        workspace_info,
        &dep.module_path,
        &parsed_version,
        &bare_repo,
        index,
        false,
    ) {
        log::debug!(
            "Skipping auto-dep package {}@{} (materialization failed): {}",
            dep.module_path,
            dep.version,
            e
        );
        return false;
    }

    true
}

fn resolve_dep_candidate(
    url: &str,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    index: &CacheIndex,
) -> Option<ResolvedDep> {
    if let Some(package_url) = find_matching_package_url(url, packages)
        && let Some(pkg) = packages.get(package_url)
    {
        return Some(ResolvedDep {
            module_path: package_url.to_string(),
            version: pkg.version.clone().unwrap_or_else(|| "0.1.0".to_string()),
        });
    }

    // Fall back to remote package discovery (git tags, cached per repo).
    match index.find_remote_package(url) {
        Ok(Some(dep)) => Some(ResolvedDep {
            module_path: dep.module_path,
            version: dep.version,
        }),
        Ok(None) => None,
        Err(e) => {
            eprintln!("  Warning: Failed to discover package for {}: {}", url, e);
            None
        }
    }
}

fn find_matching_package_url<'a>(
    url: &str,
    packages: &'a BTreeMap<String, crate::workspace::MemberPackage>,
) -> Option<&'a str> {
    packages
        .keys()
        .filter(|package_url| dep_covers_url(package_url, url))
        .max_by_key(|package_url| package_url.len())
        .map(|package_url| package_url.as_str())
}

/// Scan .zen files in workspace member packages and group found imports by their nearest pcb.toml
fn collect_imports_by_package(
    workspace_info: &WorkspaceInfo,
) -> Result<HashMap<PathBuf, CollectedImports>> {
    let workspace_root = &workspace_info.root;
    let packages = &workspace_info.packages;
    let mut result: HashMap<PathBuf, CollectedImports> = HashMap::new();

    // Determine directories to scan: member packages if any, otherwise workspace root
    let dirs_to_scan: Vec<PathBuf> = if packages.is_empty() {
        vec![workspace_root.to_path_buf()]
    } else {
        packages.values().map(|m| m.dir(workspace_root)).collect()
    };

    let Some((first, rest)) = dirs_to_scan.split_first() else {
        return Ok(result);
    };
    let mut builder = WalkBuilder::new(first);
    for dir in rest {
        builder.add(dir);
    }
    builder
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .filter_entry(skip_vendor);

    for entry in builder.build().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension() != Some(std::ffi::OsStr::new("zen")) {
            continue;
        }

        let Some(pcb_toml) = find_nearest_pcb_toml(path, workspace_root) else {
            continue;
        };

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let Some(extracted) = extract_imports(&content) else {
            eprintln!("  Warning: Failed to parse {}", path.display());
            continue;
        };

        if let Some(current_package_url) = workspace_info.package_url_for_zen(path)
            && let Some(url) = extracted.urls.iter().find(|url| {
                find_matching_package_url(url, packages) == Some(current_package_url.as_str())
            })
        {
            anyhow::bail!(
                "{} uses package URL '{}' that points into its own package '{}'; use a relative path instead",
                path.display(),
                url,
                current_package_url
            );
        }

        let imports = result.entry(pcb_toml.clone()).or_default();
        imports.aliases.extend(extracted.aliases);
        imports.urls.extend(extracted.urls);

        // Resolve relative paths that escape the package boundary to workspace member URLs
        if !extracted.relative_paths.is_empty() {
            let file_dir = path.parent().unwrap_or(path);
            let pkg_root = pcb_toml
                .parent()
                .unwrap_or(workspace_root)
                .canonicalize()
                .unwrap_or_else(|_| pcb_toml.parent().unwrap_or(workspace_root).to_path_buf());
            for rel_path in &extracted.relative_paths {
                let Ok(resolved) = file_dir.join(rel_path).canonicalize() else {
                    continue;
                };
                if resolved.starts_with(&pkg_root) {
                    continue; // within same package, no dep needed
                }
                if let Some(member_url) = find_owning_member(workspace_root, packages, &resolved) {
                    imports.urls.insert(member_url);
                }
            }
        }
    }

    Ok(result)
}

/// Find the workspace member URL that owns the given canonicalized path.
fn find_owning_member(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    resolved_path: &Path,
) -> Option<String> {
    // Find the longest-matching member directory (most specific package)
    let mut best: Option<(&str, usize)> = None;
    for (url, pkg) in packages {
        let pkg_dir = pkg.dir(workspace_root);
        let canonical = pkg_dir.canonicalize().unwrap_or(pkg_dir);
        if resolved_path.starts_with(&canonical) {
            let depth = canonical.components().count();
            if best.as_ref().is_none_or(|(_, d)| depth > *d) {
                best = Some((url.as_str(), depth));
            }
        }
    }
    best.map(|(url, _)| url.to_string())
}

/// Find nearest pcb.toml by walking up from a file (stopping at workspace root)
fn find_nearest_pcb_toml(from: &Path, workspace_root: &Path) -> Option<PathBuf> {
    let mut dir = from.parent();
    while let Some(d) = dir {
        let pcb_toml = d.join("pcb.toml");
        if pcb_toml.exists() {
            return Some(pcb_toml);
        }
        // Don't walk above workspace root
        if d == workspace_root {
            break;
        }
        dir = d.parent();
    }
    None
}

/// Extract imports from .zen file content
fn extract_imports(content: &str) -> Option<CollectedImports> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut result = CollectedImports::default();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, _| {
            extract_from_str(s, &mut result);
        });
    });

    for stmt in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &stmt.node {
            extract_from_str(&load.module.node, &mut result);
        }
    }

    Some(result)
}

/// Extract alias, URL, or relative path from a string
fn extract_from_str(s: &str, result: &mut CollectedImports) {
    if let Some(spec) = LoadSpec::parse(s) {
        match spec {
            LoadSpec::Stdlib { .. } | LoadSpec::PackageUri { .. } => {}
            LoadSpec::Package { package, .. } => {
                result.aliases.insert(package);
            }
            LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => {
                result.urls.insert(s.to_string());
            }
            LoadSpec::Path { path, .. } => {
                result.relative_paths.push(path);
            }
        }
    }
}

/// Add dependencies to a pcb.toml file and correct workspace member versions
fn mutate_manifest_dependencies(
    pcb_toml_path: &Path,
    deps: &[ResolvedDep],
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<(usize, usize)> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut added = 0usize;
    let mut corrected = 0usize;
    let mut changed = false;

    for dep in deps {
        if is_url_covered_by_manifest(&dep.module_path, &config) {
            continue;
        }

        config.dependencies.insert(
            dep.module_path.clone(),
            DependencySpec::Version(dep.version.clone()),
        );
        added += 1;
        changed = true;
    }

    // Correct workspace member versions (but preserve branch/rev/path overrides)
    for (url, pkg) in packages {
        let version = pkg.version.clone().unwrap_or_else(|| "0.1.0".to_string());
        if let Some(spec) = config.dependencies.get(url)
            && plain_version(spec).is_some_and(|v| v != version)
        {
            config
                .dependencies
                .insert(url.clone(), DependencySpec::Version(version));
            corrected += 1;
            changed = true;
        }
    }

    if changed {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok((added, corrected))
}

fn resolve_kicad_url(
    url: &str,
    configured_kicad_versions: &BTreeMap<String, semver::Version>,
) -> Option<ResolvedDep> {
    configured_kicad_versions
        .iter()
        .find_map(|(repo, version)| {
            dep_covers_url(repo, url).then(|| ResolvedDep {
                module_path: repo.clone(),
                version: version.to_string(),
            })
        })
}

/// Extract the plain version string from a dep spec, ignoring branch/rev/path overrides.
fn plain_version(spec: &DependencySpec) -> Option<&str> {
    match spec {
        DependencySpec::Version(v) => Some(v),
        DependencySpec::Detailed(d)
            if d.branch.is_none() && d.rev.is_none() && d.path.is_none() =>
        {
            d.version.as_deref()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_from_str() {
        let mut result = CollectedImports::default();

        // Aliases are treated generically.
        extract_from_str(
            "@kicad-footprints/Resistor_SMD.pretty/R_0603.kicad_mod",
            &mut result,
        );
        assert!(result.aliases.contains("kicad-footprints"));
        assert!(result.urls.is_empty());

        // stdlib is not considered by auto-deps.
        result = CollectedImports::default();
        extract_from_str("@stdlib/units.zen", &mut result);
        assert!(result.aliases.is_empty());
        assert!(result.urls.is_empty());

        result = CollectedImports::default();
        extract_from_str("github.com/diodeinc/stdlib/units.zen", &mut result);
        assert!(result.aliases.is_empty());
        assert!(result.urls.is_empty());

        // Dynamic alias path still tracks alias.
        result = CollectedImports::default();
        extract_from_str("@kicad-footprints/{}.pretty/{}.kicad_mod", &mut result);
        assert!(result.aliases.contains("kicad-footprints"));
        assert!(result.urls.is_empty());

        // Direct URLs still participate in normal package auto-deps.
        result = CollectedImports::default();
        extract_from_str(
            "github.com/example/components/Resistor/Resistor.zen",
            &mut result,
        );
        assert!(result.aliases.is_empty());
        assert!(
            result
                .urls
                .contains("github.com/example/components/Resistor/Resistor.zen")
        );

        // Relative paths are collected for cross-package resolution.
        result = CollectedImports::default();
        extract_from_str("../../other-pkg/foo.zen", &mut result);
        assert_eq!(result.relative_paths.len(), 1);
        assert_eq!(
            result.relative_paths[0],
            PathBuf::from("../../other-pkg/foo.zen")
        );

        // All LoadSpec::Path values are collected (filtered at resolution time).
        result = CollectedImports::default();
        extract_from_str("VCC", &mut result);
        assert_eq!(result.relative_paths.len(), 1);
    }
}
