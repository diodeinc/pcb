use anyhow::{Context, Result};
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::ast_utils::{skip_vendor, visit_string_literals};
use crate::cache_index::{find_lockfile_entry, CacheIndex};
use pcb_zen_core::config::{AssetDependencySpec, DependencySpec, Lockfile, PcbToml};
use pcb_zen_core::DefaultFileProvider;

/// Known alias mappings: (alias, url, version, is_asset)
const KNOWN_ALIASES: &[(&str, &str, &str, bool)] = &[
    ("stdlib", "github.com/diodeinc/stdlib", "0.4.0", false),
    (
        "kicad-symbols",
        "gitlab.com/kicad/libraries/kicad-symbols",
        "9.0.3",
        true,
    ),
    (
        "kicad-footprints",
        "gitlab.com/kicad/libraries/kicad-footprints",
        "9.0.3",
        true,
    ),
];

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub versions_corrected: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
    pub unknown_urls: Vec<(PathBuf, Vec<String>)>,
    pub discovered_remote: usize,
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
///
/// `packages` maps module_path -> MemberPackage (contains version info)
pub fn auto_add_zen_deps(
    workspace_root: &Path,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
    lockfile: Option<&Lockfile>,
    offline: bool,
) -> Result<AutoDepsSummary> {
    let package_imports = collect_imports_by_package(workspace_root)?;
    let mut summary = AutoDepsSummary::default();

    // Open cache index for remote package discovery
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
        for alias in &imports.aliases {
            if let Some((_, url, ver, is_asset)) =
                KNOWN_ALIASES.iter().find(|(a, _, _, _)| a == alias)
            {
                deps_to_add.push((url.to_string(), ver.to_string(), *is_asset));
            } else {
                unknown_aliases.push(alias.clone());
            }
        }

        // Process URL imports with 3-tier resolution (tier 3 skipped when offline)
        for url in &imports.urls {
            // 1. Try workspace members first (local packages)
            if let Some((package_url, version)) = find_matching_workspace_member(url, packages) {
                deps_to_add.push((package_url, version, false));
                continue;
            }

            // 2. Try lockfile (fast path - no git operations)
            if let Some(lf) = lockfile {
                if let Some((module_path, version)) = find_lockfile_entry(url, lf) {
                    deps_to_add.push((module_path, version, false));
                    continue;
                }
            }

            // 3. Try sqlite cache (fast path - no git)
            if let Some(ref idx) = index {
                if let Some((module_path, version)) = idx.find_remote_package(url) {
                    deps_to_add.push((module_path, version, false));
                    continue;
                }
            }

            // 4. Fetch and populate cache (slow path - git, only if online)
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
                    Ok(None) => {
                        unknown_urls.push(url.clone());
                    }
                    Err(e) => {
                        // Network/git error - report but don't fail the build
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

    Ok(summary)
}

/// Find the longest matching workspace member URL for a file URL
/// e.g., "github.com/diodeinc/registry/modules/basic/CastellatedHoles/CastellatedHoles.zen"
///    -> Some(("github.com/diodeinc/registry/modules/basic", "0.2.0")) if that's a workspace member
///
/// Returns (module_path, version) tuple
fn find_matching_workspace_member(
    file_url: &str,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> Option<(String, String)> {
    // Strip the filename first
    let without_file = file_url.rsplit_once('/')?.0;

    // Try progressively shorter prefixes to find a matching workspace member
    let mut path = without_file;
    while !path.is_empty() {
        if let Some(pkg) = packages.get(path) {
            let version = pkg.version.clone().unwrap_or_else(|| "0.1.0".to_string());
            return Some((path.to_string(), version));
        }
        // Strip the last path component
        path = match path.rsplit_once('/') {
            Some((prefix, _)) => prefix,
            None => break,
        };
    }
    None
}

/// Scan .zen files and group found imports by their nearest pcb.toml
fn collect_imports_by_package(workspace_root: &Path) -> Result<HashMap<PathBuf, CollectedImports>> {
    let mut result: HashMap<PathBuf, CollectedImports> = HashMap::new();

    let walker = WalkBuilder::new(workspace_root)
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

        let Some(pcb_toml) = find_nearest_pcb_toml(path) else {
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

/// Find nearest pcb.toml by walking up from a file
fn find_nearest_pcb_toml(from: &Path) -> Option<PathBuf> {
    let mut dir = from.parent();
    while let Some(d) = dir {
        let pcb_toml = d.join("pcb.toml");
        if pcb_toml.exists() {
            return Some(pcb_toml);
        }
        dir = d.parent();
    }
    None
}

/// Extract imports from .zen file content
/// Returns (aliases, urls) where aliases are @foo patterns and urls are github.com/... patterns
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
    if let Some(rest) = s.strip_prefix('@') {
        if let Some(name) = rest.split('/').next() {
            if !name.is_empty() {
                aliases.insert(name.to_string());
            }
        }
        return;
    }

    if s.starts_with("github.com/") || s.starts_with("gitlab.com/") {
        urls.insert(s.to_string());
    }
}

/// Add dependencies to a pcb.toml file and correct workspace member versions
/// Returns (added_count, corrected_count)
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

    corrected += correct_workspace_member_versions(&mut config, packages);

    if added > 0 || corrected > 0 {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok((added, corrected))
}

/// Correct versions of existing workspace member dependencies
/// Returns count of versions corrected
fn correct_workspace_member_versions(
    config: &mut PcbToml,
    packages: &BTreeMap<String, crate::workspace::MemberPackage>,
) -> usize {
    let mut corrected = 0;

    for (url, pkg) in packages {
        let version = pkg.version.clone().unwrap_or_else(|| "0.1.0".to_string());
        if let Some(current_spec) = config.dependencies.get(url) {
            let current_version = extract_version_string(current_spec);
            if current_version.as_deref() != Some(version.as_str()) {
                config
                    .dependencies
                    .insert(url.clone(), DependencySpec::Version(version));
                corrected += 1;
            }
        }
    }

    corrected
}

/// Extract version string from a DependencySpec
fn extract_version_string(spec: &DependencySpec) -> Option<String> {
    match spec {
        DependencySpec::Version(v) => Some(v.clone()),
        DependencySpec::Detailed(detail) => detail.version.clone(),
    }
}
