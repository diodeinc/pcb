use std::{collections::BTreeMap, path::PathBuf};

use starlark::{
    codemap::{CodeMap, Pos, ResolvedSpan, Span as StarlarkSpan},
    errors::EvalSeverity,
};

use crate::{Diagnostic, FileProviderError};

use super::eval::{EvalContextConfig, EvalSession};
use super::module::FrozenModuleValue;
use super::module::ModulePath;

pub(crate) type FootprintCacheKey = (Option<crate::resolution::PackageScopeKey>, PathBuf);

pub(crate) fn validate_footprints(
    module_tree: &BTreeMap<ModulePath, FrozenModuleValue>,
    config: &EvalContextConfig,
    session: &EvalSession,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    for module in module_tree.values() {
        for component in module.components() {
            let Some(path) = resolve_file_backed_footprint(
                component.footprint(),
                std::path::Path::new(component.source_path()),
                config,
            ) else {
                continue;
            };
            let key = footprint_cache_key(&path, config);
            let cached = if let Some(cached) = session.footprint_cache.get(&key) {
                cached
            } else {
                let cached = validate_footprint_file(&path, config);
                session.footprint_cache.insert(key, cached.clone());
                cached
            };
            diagnostics.extend(cached.into_iter().map(|diagnostic| {
                Diagnostic::categorized(
                    component.source_path(),
                    &format!(
                        "Component `{}` uses invalid footprint `{}`",
                        component.name(),
                        component.footprint()
                    ),
                    "footprint.component",
                    EvalSeverity::Error,
                )
                .with_span(component.declaration_span())
                .with_child(Some(diagnostic.boxed()))
            }));
        }
    }

    diagnostics
}

fn resolve_file_backed_footprint(
    footprint: &str,
    source_path: &std::path::Path,
    config: &EvalContextConfig,
) -> Option<PathBuf> {
    if !footprint.ends_with(".kicad_mod") {
        return None;
    }
    if footprint.starts_with(pcb_sch::PACKAGE_URI_PREFIX) {
        return config.resolution.resolve_package_uri(footprint).ok();
    }

    let path = PathBuf::from(footprint);
    if path.is_absolute() {
        return Some(path);
    }
    source_path.parent().map(|parent| parent.join(path))
}

pub(crate) fn footprint_cache_key(
    path: &std::path::Path,
    config: &EvalContextConfig,
) -> FootprintCacheKey {
    let canonical = config
        .file_provider()
        .canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf());
    let scope = config
        .resolution
        .load_cache_scope_key_for_file(&canonical, config.active_root_package.as_deref());
    (scope, canonical)
}

fn validate_footprint_file(path: &std::path::Path, config: &EvalContextConfig) -> Vec<Diagnostic> {
    let path_str = path.to_string_lossy().to_string();
    match config.file_provider().read_file(path) {
        Ok(source) => match pcb_sexpr::kicad::footprint::validate_footprint_source(&source) {
            Ok(()) => Vec::new(),
            Err(err) => err
                .issues
                .into_iter()
                .map(|issue| {
                    let span = issue
                        .span
                        .map(|span| resolved_span_from_byte_span(&path_str, &source, span));
                    Diagnostic::categorized(
                        &path_str,
                        &format!("Invalid KiCad footprint: {}", issue.message),
                        "footprint.invalid",
                        EvalSeverity::Error,
                    )
                    .with_span(span)
                })
                .collect(),
        },
        Err(FileProviderError::NotFound(_)) => Vec::new(),
        Err(err) => vec![Diagnostic::categorized(
            &path_str,
            &format!("Failed to read KiCad footprint for validation: {err}"),
            "footprint.read",
            EvalSeverity::Error,
        )],
    }
}

fn resolved_span_from_byte_span(path: &str, source: &str, span: pcb_sexpr::Span) -> ResolvedSpan {
    let codemap = CodeMap::new(path.to_string(), source.to_string());
    let start = Pos::new(span.start as u32);
    let end = Pos::new(span.end as u32);
    codemap
        .file_span(StarlarkSpan::new(start, end))
        .resolve_span()
}
