use anyhow::Result;
use ignore::WalkBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::{ExprP, StmtP};
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use pcb_zen_core::config::{AssetDependencySpec, DependencySpec, PcbToml};
use pcb_zen_core::DefaultFileProvider;

/// Known alias mappings: (url, version, is_asset)
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

#[derive(Debug, Default)]
pub struct AutoDepsSummary {
    pub total_added: usize,
    pub packages_updated: usize,
    pub unknown_aliases: Vec<(PathBuf, Vec<String>)>,
}

/// Scan workspace for .zen files and auto-add missing dependencies to pcb.toml files
pub fn auto_add_zen_deps(workspace_root: &Path) -> Result<AutoDepsSummary> {
    // Group aliases by package (nearest pcb.toml)
    let package_aliases = collect_aliases_by_package(workspace_root)?;

    let mut summary = AutoDepsSummary::default();

    for (pcb_toml_path, aliases) in package_aliases {
        // Partition into known and unknown aliases
        let mut known: Vec<(&str, &str, bool)> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();

        for alias in &aliases {
            if let Some((_, url, ver, is_asset)) =
                KNOWN_ALIASES.iter().find(|(a, _, _, _)| a == alias)
            {
                known.push((url, ver, *is_asset));
            } else {
                unknown.push(alias.clone());
            }
        }

        if !unknown.is_empty() {
            summary
                .unknown_aliases
                .push((pcb_toml_path.clone(), unknown));
        }

        if known.is_empty() {
            continue;
        }

        // Add known dependencies to pcb.toml
        let added = add_dependencies(&pcb_toml_path, &known)?;
        if added > 0 {
            summary.total_added += added;
            summary.packages_updated += 1;
        }
    }

    Ok(summary)
}

/// Scan .zen files and group found aliases by their nearest pcb.toml
fn collect_aliases_by_package(workspace_root: &Path) -> Result<HashMap<PathBuf, HashSet<String>>> {
    let mut result: HashMap<PathBuf, HashSet<String>> = HashMap::new();

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

        // Find nearest pcb.toml by walking up
        let Some(pcb_toml) = find_nearest_pcb_toml(path) else {
            continue;
        };

        // Extract aliases from file
        let content = std::fs::read_to_string(path)?;
        let aliases = match extract_aliases(&content) {
            Some(a) => a,
            None => {
                eprintln!("  Warning: Failed to parse {}", path.display());
                continue;
            }
        };

        result.entry(pcb_toml).or_default().extend(aliases);
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

/// Extract @alias patterns from .zen file content
fn extract_aliases(content: &str) -> Option<HashSet<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut aliases = HashSet::new();

    // Check all string literals in expressions
    ast.statement().visit_expr(|expr| {
        extract_from_expr(expr, &mut aliases);
    });

    // Check load() statements
    for stmt in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &stmt.node {
            extract_alias_from_str(&load.module.node, &mut aliases);
        }
    }

    Some(aliases)
}

/// Recursively extract aliases from an expression
fn extract_from_expr(expr: &starlark_syntax::syntax::ast::AstExpr, aliases: &mut HashSet<String>) {
    // Check string literals
    if let ExprP::Literal(lit) = &expr.node {
        let s = lit.to_string();
        if s.len() > 2 && (s.starts_with('"') || s.starts_with('\'')) {
            extract_alias_from_str(&s[1..s.len() - 1], aliases);
        }
    }

    // Recurse into subexpressions
    match &expr.node {
        ExprP::Call(_, args) => {
            for arg in &args.args {
                if let starlark_syntax::syntax::ast::ArgumentP::Positional(e)
                | starlark_syntax::syntax::ast::ArgumentP::Named(_, e) = &arg.node
                {
                    extract_from_expr(e, aliases);
                }
            }
        }
        ExprP::If(if_box) => {
            let (a, b, c) = &**if_box;
            extract_from_expr(a, aliases);
            extract_from_expr(b, aliases);
            extract_from_expr(c, aliases);
        }
        ExprP::List(exprs) | ExprP::Tuple(exprs) => {
            for e in exprs {
                extract_from_expr(e, aliases);
            }
        }
        ExprP::Dict(pairs) => {
            for (k, v) in pairs {
                extract_from_expr(k, aliases);
                extract_from_expr(v, aliases);
            }
        }
        _ => {}
    }
}

/// Extract alias name from a string like "@stdlib/foo.zen"
fn extract_alias_from_str(s: &str, aliases: &mut HashSet<String>) {
    if let Some(rest) = s.strip_prefix('@') {
        if let Some(name) = rest.split('/').next() {
            if !name.is_empty() {
                aliases.insert(name.to_string());
            }
        }
    }
}

/// Add dependencies to a pcb.toml file, returns count added
fn add_dependencies(pcb_toml_path: &Path, deps: &[(&str, &str, bool)]) -> Result<usize> {
    let mut config = PcbToml::from_file(&DefaultFileProvider::new(), pcb_toml_path)?;
    let existing = config.auto_generated_aliases();

    let mut added = 0;
    for (url, version, is_asset) in deps {
        // Skip if already exists
        if existing.values().any(|v| v == url) {
            continue;
        }

        if *is_asset {
            if config
                .assets
                .insert(
                    url.to_string(),
                    AssetDependencySpec::Ref(version.to_string()),
                )
                .is_none()
            {
                added += 1;
            }
        } else if config
            .dependencies
            .insert(
                url.to_string(),
                DependencySpec::Version(version.to_string()),
            )
            .is_none()
        {
            added += 1;
        }
    }

    if added > 0 {
        std::fs::write(pcb_toml_path, toml::to_string_pretty(&config)?)?;
    }

    Ok(added)
}
