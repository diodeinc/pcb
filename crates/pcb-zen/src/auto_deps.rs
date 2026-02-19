use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::ast_utils::{skip_vendor, visit_string_literals};
use crate::cache_index::CacheIndex;
use crate::git;
use crate::resolve::{fetch_asset_repo, fetch_package};
use crate::workspace::WorkspaceInfo;
use pcb_zen_core::config::{AssetDependencySpec, DependencySpec, PcbToml, KICAD_ASSETS};
use pcb_zen_core::DefaultFileProvider;

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub versions_corrected: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
    pub unknown_urls: Vec<(PathBuf, Vec<String>)>,
    pub stdlib_removed: usize,
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
    is_asset: bool,
}

impl ResolvedDep {
    fn package(module_path: String, version: String) -> Self {
        Self {
            module_path,
            version,
            is_asset: false,
        }
    }

    fn asset(module_path: String, version: String) -> Self {
        Self {
            module_path,
            version,
            is_asset: true,
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
    let package_imports = collect_imports_by_package(workspace_root, packages)?;
    let mut summary = AutoDepsSummary::default();
    let file_provider = DefaultFileProvider::new();

    let index = CacheIndex::open()?;

    for (pcb_toml_path, imports) in package_imports {
        let existing_config = PcbToml::from_file(&file_provider, &pcb_toml_path)?;
        let mut deps_to_add: Vec<ResolvedDep> = Vec::new();
        let unknown_aliases: Vec<String> = imports
            .aliases
            .iter()
            .filter(|alias| alias.as_str() != "stdlib")
            .cloned()
            .collect();
        let mut unknown_urls: Vec<String> = Vec::new();

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

        let (added, corrected) =
            add_and_correct_dependencies(&pcb_toml_path, &deps_to_add, packages)?;
        if added > 0 || corrected > 0 {
            summary.total_added += added;
            summary.versions_corrected += corrected;
            summary.packages_updated += 1;
        }
    }

    // Remove redundant stdlib dependencies (version <= pinned toolchain version)
    summary.stdlib_removed = remove_redundant_stdlib(workspace_root, packages)?;

    Ok(summary)
}

fn is_url_covered_by_manifest(url: &str, config: &PcbToml) -> bool {
    config
        .dependencies
        .keys()
        .any(|dep| dep_covers_url(dep, url))
        || config.assets.keys().any(|asset| dep_covers_url(asset, url))
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
    if dep.is_asset {
        let (repo_url, subpath) = git::split_asset_repo_and_subpath(&dep.module_path);
        let asset_key = dep.module_path.clone();
        let result = fetch_asset_repo(workspace_info, repo_url, &dep.version, &[asset_key], false)
            .and_then(|base| {
                let target = if subpath.is_empty() {
                    base
                } else {
                    base.join(subpath)
                };
                anyhow::ensure!(
                    target.exists(),
                    "Asset subpath '{}' not found in {}@{}",
                    subpath,
                    repo_url,
                    dep.version
                );
                Ok(())
            });

        if let Err(e) = result {
            log::debug!(
                "Skipping auto-dep asset {}@{} (materialization failed): {}",
                dep.module_path,
                dep.version,
                e
            );
            return false;
        }
        return true;
    }

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
    get_kicad_asset_version(url)
        .map(|version| ResolvedDep::asset(url.to_string(), version))
        .or_else(|| {
            find_matching_workspace_member(url, packages)
                .map(|(module_path, version)| ResolvedDep::package(module_path, version))
        })
        .or_else(|| match index.find_remote_package(url) {
            Ok(Some(dep)) => Some(ResolvedDep::package(dep.module_path, dep.version)),
            Ok(None) => None,
            Err(e) => {
                eprintln!("  Warning: Failed to discover package for {}: {}", url, e);
                None
            }
        })
}

/// Get the version for a known KiCad asset URL (returns None if not a KiCad asset with subpath)
fn get_kicad_asset_version(url: &str) -> Option<String> {
    for (_, base_url, version) in KICAD_ASSETS {
        if let Some(rest) = url.strip_prefix(base_url) {
            // Must have a subpath (starts with /)
            if rest.starts_with('/') && rest.len() > 1 {
                return Some(version.to_string());
            }
        }
    }
    None
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

/// Extract asset reference with subpath from @kicad-* aliases
///
/// For footprints: extracts .pretty directory, uses full path only if filename is static
/// For symbols: extracts filename, strips :ComponentName suffix
fn extract_kicad_asset(s: &str) -> Option<(String, String)> {
    for (alias, base_url, version) in KICAD_ASSETS {
        let prefix = format!("@{}/", alias);
        let Some(rest) = s.strip_prefix(&prefix) else {
            continue;
        };

        if rest.is_empty() {
            return None;
        }

        // Footprints need .pretty directory
        if *alias == "kicad-footprints" {
            let pretty_idx = rest.find(".pretty")?;
            let pretty_end = pretty_idx + ".pretty".len();
            let dir_part = &rest[..pretty_end];

            // Directory must be static
            if dir_part.contains('{') || dir_part.contains('%') {
                return None;
            }

            // Check for static filename with extension
            let after_dir = &rest[pretty_end..];
            if let Some(filename) = after_dir.strip_prefix('/') {
                if !filename.is_empty()
                    && !filename.contains('{')
                    && !filename.contains('%')
                    && !filename.contains('/')
                    && filename.contains('.')
                {
                    return Some((
                        format!("{}/{}/{}", base_url, dir_part, filename),
                        version.to_string(),
                    ));
                }
            }

            // Fallback to directory only
            return Some((format!("{}/{}", base_url, dir_part), version.to_string()));
        }

        // Symbols: extract filename, strip :ComponentName
        let mut filename = rest.split('/').next().unwrap_or(rest);
        if let Some(colon_idx) = filename.find(':') {
            filename = &filename[..colon_idx];
        }

        if filename.is_empty() || filename.contains('{') || filename.contains('%') {
            return None;
        }

        return Some((format!("{}/{}", base_url, filename), version.to_string()));
    }
    None
}

/// Extract alias or URL from a string
fn extract_from_str(s: &str, aliases: &mut HashSet<String>, urls: &mut HashSet<String>) {
    // Try KiCad asset extraction first
    if let Some((url, _version)) = extract_kicad_asset(s) {
        urls.insert(url);
        return;
    }

    // Handle @alias imports
    if let Some(rest) = s.strip_prefix('@') {
        if let Some(name) = rest.split('/').next() {
            if !name.is_empty() {
                // Skip known KiCad aliases if extraction failed (dynamic paths)
                let is_kicad = KICAD_ASSETS.iter().any(|(alias, _, _)| *alias == name);
                if !is_kicad {
                    aliases.insert(name.to_string());
                }
            }
        }
        return;
    }

    // Handle direct URLs
    if s.starts_with("github.com/") || s.starts_with("gitlab.com/") {
        urls.insert(s.to_string());
    }
}

/// Add dependencies to a pcb.toml file and correct workspace member versions
fn add_and_correct_dependencies(
    pcb_toml_path: &Path,
    deps: &[ResolvedDep],
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<(usize, usize)> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut added = 0;
    let mut corrected = 0;

    for dep in deps {
        if is_url_covered_by_manifest(&dep.module_path, &config) {
            continue;
        }

        if dep.is_asset {
            config.assets.insert(
                dep.module_path.clone(),
                AssetDependencySpec::Ref(dep.version.clone()),
            );
        } else {
            config.dependencies.insert(
                dep.module_path.clone(),
                DependencySpec::Version(dep.version.clone()),
            );
        }
        added += 1;
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
            }
        }
    }

    if added > 0 || corrected > 0 {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok((added, corrected))
}

/// Remove redundant stdlib dependencies from pcb.toml files
///
/// If a stdlib dependency is declared with a version <= the toolchain's pinned version,
/// it's redundant since the toolchain implicitly provides stdlib. This function removes
/// such entries.
///
/// Returns the number of pcb.toml files that were modified.
pub fn remove_redundant_stdlib(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<usize> {
    use crate::tags::parse_version;

    let pinned_version = parse_version(pcb_zen_core::STDLIB_VERSION)
        .ok_or_else(|| anyhow::anyhow!("Invalid pinned stdlib version"))?;

    let mut removed_count = 0;

    for package in packages.values() {
        let pcb_toml_path = package.dir(workspace_root).join("pcb.toml");
        if !pcb_toml_path.exists() {
            continue;
        }

        let mut config = PcbToml::from_file(&DefaultFileProvider::new(), &pcb_toml_path)?;

        if let Some(spec) = config.dependencies.get(pcb_zen_core::STDLIB_MODULE_PATH) {
            let should_remove = match spec {
                DependencySpec::Version(v) => {
                    parse_version(v).is_some_and(|ver| ver <= pinned_version)
                }
                DependencySpec::Detailed(d) => {
                    if d.branch.is_some() || d.rev.is_some() || d.path.is_some() {
                        false
                    } else if let Some(v) = &d.version {
                        parse_version(v).is_some_and(|ver| ver <= pinned_version)
                    } else {
                        false
                    }
                }
            };

            if should_remove {
                config.dependencies.remove(pcb_zen_core::STDLIB_MODULE_PATH);
                std::fs::write(&pcb_toml_path, toml::to_string_pretty(&config)?)?;
                removed_count += 1;
            }
        }
    }

    Ok(removed_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_kicad_asset_footprints() {
        // Static full path
        let (url, ver) = extract_kicad_asset(
            "@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod",
        )
        .unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"
        );
        assert_eq!(ver, "9.0.3");

        // Dynamic filename - fallback to directory
        let (url, _) =
            extract_kicad_asset("@kicad-footprints/TestPoint.pretty/TestPoint_{name}.kicad_mod")
                .unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-footprints/TestPoint.pretty"
        );

        // Truncated string - fallback to directory
        let (url, _) = extract_kicad_asset(
            "@kicad-footprints/Mounting_Wuerth.pretty/Mounting_Wuerth_WA-SMSI-",
        )
        .unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-footprints/Mounting_Wuerth.pretty"
        );

        // Just directory
        let (url, _) = extract_kicad_asset("@kicad-footprints/Mounting_Wuerth.pretty").unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-footprints/Mounting_Wuerth.pretty"
        );

        // No .pretty directory
        assert!(extract_kicad_asset("@kicad-footprints/").is_none());
        assert!(extract_kicad_asset("@kicad-footprints/SomeFile.txt").is_none());
    }

    #[test]
    fn test_extract_kicad_asset_symbols() {
        // Standard symbol file
        let (url, ver) = extract_kicad_asset("@kicad-symbols/Device.kicad_sym").unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym"
        );
        assert_eq!(ver, "9.0.3");

        // Strip :ComponentName suffix
        let (url, _) = extract_kicad_asset("@kicad-symbols/Device.kicad_sym:D_Schottky").unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym"
        );

        // Truncated component name
        let (url, _) = extract_kicad_asset("@kicad-symbols/Device.kicad_sym:D").unwrap();
        assert_eq!(
            url,
            "gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym"
        );

        // Empty after prefix
        assert!(extract_kicad_asset("@kicad-symbols/").is_none());

        // Dynamic filename
        assert!(extract_kicad_asset("@kicad-symbols/{name}.kicad_sym").is_none());
    }

    #[test]
    fn test_get_kicad_asset_version() {
        // Base repos without subpath
        assert!(get_kicad_asset_version("gitlab.com/kicad/libraries/kicad-footprints").is_none());
        assert!(get_kicad_asset_version("gitlab.com/kicad/libraries/kicad-symbols").is_none());

        // With subpath
        assert_eq!(
            get_kicad_asset_version(
                "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty"
            ),
            Some("9.0.3".to_string())
        );
        assert_eq!(
            get_kicad_asset_version("gitlab.com/kicad/libraries/kicad-symbols/Device.kicad_sym"),
            Some("9.0.3".to_string())
        );
    }

    #[test]
    fn test_extract_from_str() {
        let mut aliases = HashSet::new();
        let mut urls = HashSet::new();

        // KiCad asset -> URL
        extract_from_str(
            "@kicad-footprints/Resistor_SMD.pretty/R_0603.kicad_mod",
            &mut aliases,
            &mut urls,
        );
        assert!(aliases.is_empty());
        assert!(urls.contains(
            "gitlab.com/kicad/libraries/kicad-footprints/Resistor_SMD.pretty/R_0603.kicad_mod"
        ));

        // @stdlib -> alias
        aliases.clear();
        urls.clear();
        extract_from_str("@stdlib/units.zen", &mut aliases, &mut urls);
        assert!(aliases.contains("stdlib"));
        assert!(urls.is_empty());

        // Dynamic KiCad path -> silently skipped
        aliases.clear();
        urls.clear();
        extract_from_str(
            "@kicad-footprints/{}.pretty/{}.kicad_mod",
            &mut aliases,
            &mut urls,
        );
        assert!(aliases.is_empty());
        assert!(urls.is_empty());
    }
}
