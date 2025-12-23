use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::ast_utils::{skip_vendor, visit_string_literals};
use crate::cache_index::{cache_base, find_lockfile_entry, CacheIndex};
use crate::git;
use pcb_zen_core::config::{AssetDependencySpec, DependencySpec, Lockfile, PcbToml, KICAD_ASSETS};
use pcb_zen_core::DefaultFileProvider;

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub versions_corrected: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
    pub unknown_urls: Vec<(PathBuf, Vec<String>)>,
    pub discovered_remote: usize,
    pub stdlib_removed: usize,
}

#[derive(Debug, Default)]
struct CollectedImports {
    aliases: HashSet<String>,
    urls: HashSet<String>,
}

/// Scan workspace for .zen files and auto-add missing dependencies to pcb.toml files
///
/// Resolution order for URL imports:
/// 1. Workspace members (local packages)
/// 2. Lockfile entries (pcb.sum) - fast path, no git operations
/// 3. Remote package discovery (git tags) - slow path, cached per repo (skipped when offline)
pub fn auto_add_zen_deps(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    lockfile: Option<&Lockfile>,
    offline: bool,
) -> Result<AutoDepsSummary> {
    let package_imports = collect_imports_by_package(workspace_root, packages)?;
    let mut summary = AutoDepsSummary::default();

    let index = if !offline {
        CacheIndex::open().ok()
    } else {
        None
    };

    for (pcb_toml_path, imports) in package_imports {
        let mut deps_to_add: Vec<(String, String, bool)> = Vec::new();
        let mut unknown_aliases: Vec<String> = Vec::new();
        let mut unknown_urls: Vec<String> = Vec::new();

        // Process @alias imports
        // Note: @stdlib is handled implicitly by the toolchain, no need to add to [dependencies]
        for alias in &imports.aliases {
            if alias != "stdlib" {
                unknown_aliases.push(alias.clone());
            }
        }

        // Process URL imports
        let cache = cache_base();
        for url in &imports.urls {
            // Check if this is a known KiCad asset with subpath
            if let Some(version) = get_kicad_asset_version(url) {
                // Opportunistically verify path exists if we have the repo cached
                let (repo_url, subpath) = git::split_asset_repo_and_subpath(url);
                let repo_cache_dir = cache.join(repo_url).join(&version);
                if repo_cache_dir.exists() && !subpath.is_empty() {
                    let target_path = repo_cache_dir.join(subpath);
                    if !target_path.exists() {
                        // Path doesn't exist in cached repo, skip auto-dep
                        unknown_urls.push(url.clone());
                        continue;
                    }
                }
                deps_to_add.push((url.clone(), version, true));
                continue;
            }

            // Try workspace members first
            if let Some((package_url, version)) = find_matching_workspace_member(url, packages) {
                deps_to_add.push((package_url, version, false));
                continue;
            }

            // Try lockfile
            if let Some(lf) = lockfile {
                if let Some((module_path, version)) = find_lockfile_entry(url, lf) {
                    deps_to_add.push((module_path, version, false));
                    continue;
                }
            }

            // Try sqlite cache
            if let Some(ref idx) = index {
                if let Some((module_path, version)) = idx.find_remote_package(url) {
                    deps_to_add.push((module_path, version, false));
                    continue;
                }
            }

            // Fetch and populate cache (only if online)
            if offline {
                unknown_urls.push(url.clone());
                continue;
            }

            if let Some(ref idx) = index {
                match idx.find_or_discover_remote_package(url) {
                    Ok(Some((module_path, version))) => {
                        deps_to_add.push((module_path, version, false));
                        summary.discovered_remote += 1;
                    }
                    Ok(None) => unknown_urls.push(url.clone()),
                    Err(e) => {
                        eprintln!("  Warning: Failed to discover package for {}: {}", url, e);
                        unknown_urls.push(url.clone());
                    }
                }
            } else {
                unknown_urls.push(url.clone());
            }
        }

        if !unknown_aliases.is_empty() {
            summary
                .unknown_aliases
                .push((pcb_toml_path.clone(), unknown_aliases));
        }
        if !unknown_urls.is_empty() {
            summary
                .unknown_urls
                .push((pcb_toml_path.clone(), unknown_urls));
        }

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

    for dir in dirs_to_scan {
        let walker = WalkBuilder::new(&dir)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(skip_vendor)
            .build();

        for entry in walker.filter_map(|e| e.ok()) {
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
    deps: &[(String, String, bool)],
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Result<(usize, usize)> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut added = 0;
    let mut corrected = 0;

    for (url, version, is_asset) in deps {
        if config.dependencies.contains_key(url) || config.assets.contains_key(url) {
            continue;
        }

        // For assets, check if already satisfied by a whole-repo entry
        if *is_asset {
            let (repo_url, subpath) = git::split_asset_repo_and_subpath(url);
            if !subpath.is_empty() && config.assets.contains_key(repo_url) {
                continue;
            }
        }

        if *is_asset {
            config
                .assets
                .insert(url.clone(), AssetDependencySpec::Ref(version.clone()));
        } else {
            config
                .dependencies
                .insert(url.clone(), DependencySpec::Version(version.clone()));
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
