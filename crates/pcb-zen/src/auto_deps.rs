use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::HashSet;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::ast_utils::{skip_vendor, visit_string_literals};
use crate::cache_index::CacheIndex;
use crate::resolve::fetch_package;
use crate::workspace::WorkspaceInfo;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::kicad_library::{kicad_dependency_aliases, selected_kicad_repo_versions};

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
}

#[derive(Debug, Clone)]
struct ResolvedDep {
    module_path: String,
    version: String,
}

impl ResolvedDep {
    fn package(module_path: String, version: String) -> Self {
        Self {
            module_path,
            version,
        }
    }
}

/// Scan workspace for .zen files and auto-add missing dependencies to pcb.toml files
///
/// Resolution order for URL imports:
/// 1. Workspace members (local packages)
/// 2. Remote package discovery (git tags) - slow path, cached per repo
pub fn auto_add_zen_deps(workspace_info: &WorkspaceInfo) -> Result<AutoDepsSummary> {
    let workspace_root = &workspace_info.root;
    let packages = &workspace_info.packages;
    let mut package_imports = collect_imports_by_package(workspace_root, packages)?;
    let mut summary = AutoDepsSummary::default();
    let file_provider = DefaultFileProvider::new();
    let pinned_stdlib_version = crate::tags::parse_version(pcb_zen_core::STDLIB_VERSION)
        .ok_or_else(|| anyhow::anyhow!("Invalid pinned stdlib version"))?;
    let kicad_entries = workspace_info.kicad_library_entries();
    let kicad_aliases = kicad_dependency_aliases(kicad_entries);
    let selected_kicad_versions =
        selected_kicad_repo_versions(kicad_entries, workspace_info.manifests()).unwrap_or_default();

    let index = CacheIndex::open()?;
    let manifests = collect_manifest_paths(workspace_root, packages, &package_imports);

    for pcb_toml_path in manifests {
        let imports = package_imports.remove(&pcb_toml_path).unwrap_or_default();
        let existing_config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
        let mut deps_to_add: Vec<ResolvedDep> = Vec::new();
        let mut unknown_aliases: Vec<String> = Vec::new();
        let mut unknown_urls: Vec<String> = Vec::new();

        // Special-case bootstrap for KiCad aliases.
        // These aliases are auto-generated from dependencies, but users often start by writing
        // `@<symbols|footprints-alias>/...` first. Resolve that chicken-and-egg by inferring
        // the repo dependency from [[workspace.kicad_library]] and adding it to pcb.toml.
        for alias in &imports.aliases {
            if alias == "stdlib" {
                continue;
            }
            let Some(module_path) = kicad_aliases.get(alias).map(String::as_str) else {
                unknown_aliases.push(alias.clone());
                continue;
            };
            if is_url_covered_by_manifest(module_path, &existing_config) {
                continue;
            }
            if let Some(version) = selected_kicad_versions.get(module_path) {
                deps_to_add.push(ResolvedDep::package(
                    module_path.to_string(),
                    version.clone(),
                ));
                continue;
            }
            unknown_aliases.push(alias.clone());
        }

        // Process URL imports
        for url in &imports.urls {
            if is_url_covered_by_manifest(url, &existing_config) {
                continue;
            }

            let candidate = resolve_dep_candidate(url, packages, &index);

            let Some(candidate) = candidate else {
                unknown_urls.push(url.clone());
                continue;
            };

            if can_materialize_dep(workspace_info, &index, &candidate) {
                deps_to_add.push(candidate);
            } else {
                unknown_urls.push(url.clone());
            }
        }

        push_unknown(
            &mut summary.unknown_aliases,
            &pcb_toml_path,
            unknown_aliases,
        );
        push_unknown(&mut summary.unknown_urls, &pcb_toml_path, unknown_urls);

        let (added, corrected) = mutate_manifest_dependencies(
            &pcb_toml_path,
            &deps_to_add,
            packages,
            &pinned_stdlib_version,
        )?;
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
    if dep == url {
        return true;
    }

    url.strip_prefix(dep)
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

    if let Err(e) = fetch_package(
        workspace_info,
        &dep.module_path,
        &parsed_version,
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
    find_matching_workspace_member(url, packages)
        .map(|(module_path, version)| ResolvedDep::package(module_path, version))
        .or_else(|| match index.find_remote_package(url) {
            Ok(Some(dep)) => Some(ResolvedDep::package(dep.module_path, dep.version)),
            Ok(None) => None,
            Err(e) => {
                eprintln!("  Warning: Failed to discover package for {}: {}", url, e);
                None
            }
        })
}

/// Find the longest matching workspace member URL for a file URL
fn find_matching_workspace_member(
    file_url: &str,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Option<(String, String)> {
    let without_file = file_url.rsplit_once('/')?.0;
    let mut path = without_file;
    while !path.is_empty() {
        if let Some(pkg) = packages.get(path) {
            let version = pkg.version.clone().unwrap_or_else(|| "0.1.0".to_string());
            return Some((path.to_string(), version));
        }
        path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    }
    None
}

/// Scan .zen files in workspace member packages and group found imports by their nearest pcb.toml
fn collect_imports_by_package(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<HashMap<PathBuf, CollectedImports>> {
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
        let Some((aliases, urls)) = extract_imports(&content) else {
            eprintln!("  Warning: Failed to parse {}", path.display());
            continue;
        };

        let imports = result.entry(pcb_toml).or_default();
        imports.aliases.extend(aliases);
        imports.urls.extend(urls);
    }

    Ok(result)
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
fn extract_imports(content: &str) -> Option<(HashSet<String>, HashSet<String>)> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut aliases = HashSet::new();
    let mut urls = HashSet::new();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, _| {
            extract_from_str(s, &mut aliases, &mut urls);
        });
    });

    for stmt in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &stmt.node {
            extract_from_str(&load.module.node, &mut aliases, &mut urls);
        }
    }

    Some((aliases, urls))
}

/// Extract alias or URL from a string
fn extract_from_str(s: &str, aliases: &mut HashSet<String>, urls: &mut HashSet<String>) {
    // Handle @alias imports
    if let Some(rest) = s.strip_prefix('@') {
        if let Some(name) = rest.split('/').next()
            && !name.is_empty()
        {
            aliases.insert(name.to_string());
        }
        return;
    }

    // Handle direct URLs
    if s.starts_with("github.com/") || s.starts_with("gitlab.com/") {
        urls.insert(s.to_string());
    }
}

/// Add dependencies to a pcb.toml file and correct workspace member versions
fn mutate_manifest_dependencies(
    pcb_toml_path: &Path,
    deps: &[ResolvedDep],
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    pinned_stdlib_version: &semver::Version,
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
        if let Some(current_spec) = config.dependencies.get(url) {
            let should_correct = match current_spec {
                DependencySpec::Version(v) => v != &version,
                DependencySpec::Detailed(d) => {
                    // Don't overwrite branch/rev/path deps
                    if d.branch.is_some() || d.rev.is_some() || d.path.is_some() {
                        false
                    } else {
                        d.version.as_deref() != Some(version.as_str())
                    }
                }
            };
            if should_correct {
                config
                    .dependencies
                    .insert(url.clone(), DependencySpec::Version(version));
                corrected += 1;
                changed = true;
            }
        }
    }

    if let Some(spec) = config.dependencies.get(pcb_zen_core::STDLIB_MODULE_PATH)
        && should_remove_redundant_stdlib(spec, pinned_stdlib_version)
    {
        config.dependencies.remove(pcb_zen_core::STDLIB_MODULE_PATH);
        changed = true;
    }

    if changed {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok((added, corrected))
}

fn should_remove_redundant_stdlib(spec: &DependencySpec, pinned_version: &semver::Version) -> bool {
    use crate::tags::parse_version;

    match spec {
        DependencySpec::Version(v) => parse_version(v).is_some_and(|ver| ver <= *pinned_version),
        DependencySpec::Detailed(d) => {
            if d.branch.is_some() || d.rev.is_some() || d.path.is_some() {
                false
            } else if let Some(v) = &d.version {
                parse_version(v).is_some_and(|ver| ver <= *pinned_version)
            } else {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_from_str() {
        let mut aliases = HashSet::new();
        let mut urls = HashSet::new();

        // Aliases are treated generically.
        extract_from_str(
            "@kicad-footprints/Resistor_SMD.pretty/R_0603.kicad_mod",
            &mut aliases,
            &mut urls,
        );
        assert!(aliases.contains("kicad-footprints"));
        assert!(urls.is_empty());

        // @stdlib -> alias
        aliases.clear();
        urls.clear();
        extract_from_str("@stdlib/units.zen", &mut aliases, &mut urls);
        assert!(aliases.contains("stdlib"));
        assert!(urls.is_empty());

        // Dynamic alias path still tracks alias.
        aliases.clear();
        urls.clear();
        extract_from_str(
            "@kicad-footprints/{}.pretty/{}.kicad_mod",
            &mut aliases,
            &mut urls,
        );
        assert!(aliases.contains("kicad-footprints"));
        assert!(urls.is_empty());

        // Direct URLs still participate in normal package auto-deps.
        aliases.clear();
        urls.clear();
        extract_from_str(
            "github.com/example/components/Resistor/Resistor.zen",
            &mut aliases,
            &mut urls,
        );
        assert!(aliases.is_empty());
        assert!(urls.contains("github.com/example/components/Resistor/Resistor.zen"));
    }
}
