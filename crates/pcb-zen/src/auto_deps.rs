use anyhow::Result;
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast_utils::visit_string_literals;
use pcb_zen_core::config::{AssetDependencySpec, DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;

/// Known alias mappings: (alias, url, version, is_asset)
const KNOWN_ALIASES: &[(&str, &str, &str, bool)] = &[
    ("stdlib", "github.com/akhilles/stdlib", "0.4.5", false),
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

/// Default version for workspace member dependencies
const DEFAULT_VERSION: &str = "0.1.0";

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
}

#[derive(Debug, Default)]
struct CollectedImports {
    aliases: HashSet<String>,
    urls: HashSet<String>,
}

/// Scan workspace for .zen files and auto-add missing dependencies to pcb.toml files
pub fn auto_add_zen_deps(
    workspace_root: &Path,
    workspace_members: &HashSet<String>,
) -> Result<AutoDepsSummary> {
    let package_imports = collect_imports_by_package(workspace_root)?;
    let mut summary = AutoDepsSummary::default();

    for (pcb_toml_path, imports) in package_imports {
        let mut deps_to_add: Vec<(String, &str, bool)> = Vec::new();
        let mut unknown_aliases: Vec<String> = Vec::new();

        // Process @alias imports
        for alias in &imports.aliases {
            if let Some((_, url, ver, is_asset)) =
                KNOWN_ALIASES.iter().find(|(a, _, _, _)| a == alias)
            {
                deps_to_add.push((url.to_string(), ver, *is_asset));
            } else {
                unknown_aliases.push(alias.clone());
            }
        }

        // Process URL imports - add workspace member deps with default version
        for url in &imports.urls {
            if let Some(package_url) = find_matching_workspace_member(url, workspace_members) {
                deps_to_add.push((package_url, DEFAULT_VERSION, false));
            }
        }

        if !unknown_aliases.is_empty() {
            summary
                .unknown_aliases
                .push((pcb_toml_path.clone(), unknown_aliases));
        }

        let added = add_dependencies(&pcb_toml_path, &deps_to_add)?;
        if added > 0 {
            summary.total_added += added;
            summary.packages_updated += 1;
        }
    }

    Ok(summary)
}

/// Find the longest matching workspace member URL for a file URL
/// e.g., "github.com/diodeinc/registry/modules/basic/CastellatedHoles/CastellatedHoles.zen"
///    -> Some("github.com/diodeinc/registry/modules/basic") if that's a workspace member
fn find_matching_workspace_member(
    file_url: &str,
    workspace_members: &HashSet<String>,
) -> Option<String> {
    // Strip the .zen filename first
    let without_file = file_url.rsplit_once('/')?.0;

    // Try progressively shorter prefixes to find a matching workspace member
    let mut path = without_file;
    while !path.is_empty() {
        if workspace_members.contains(path) {
            return Some(path.to_string());
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
        .build();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension() != Some(std::ffi::OsStr::new("zen")) {
            continue;
        }

        let Some(pcb_toml) = find_nearest_pcb_toml(path) else {
            continue;
        };

        let content = std::fs::read_to_string(path)?;
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

/// Add dependencies to a pcb.toml file, returns count added
fn add_dependencies(pcb_toml_path: &Path, deps: &[(String, &str, bool)]) -> Result<usize> {
    if deps.is_empty() {
        return Ok(0);
    }

    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let mut added = 0;

    for (url, version, is_asset) in deps {
        if config.dependencies.contains_key(url) || config.assets.contains_key(url) {
            continue;
        }

        if *is_asset {
            config
                .assets
                .insert(url.clone(), AssetDependencySpec::Ref(version.to_string()));
        } else {
            config
                .dependencies
                .insert(url.clone(), DependencySpec::Version(version.to_string()));
        }
        added += 1;
    }

    if added > 0 {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok(added)
}
