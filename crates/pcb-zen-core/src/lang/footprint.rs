use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use starlark::{
    codemap::{CodeMap, Pos, ResolvedSpan, Span as StarlarkSpan},
    errors::EvalSeverity,
};

use crate::{Diagnostic, FileProvider, FileProviderError, resolution::ResolutionResult};

use super::module::FrozenModuleValue;
use super::module::ModulePath;

pub(crate) fn validate_footprints(
    module_tree: &BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &ResolutionResult,
    file_provider: &dyn FileProvider,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut validated = HashMap::new();

    for module in module_tree.values() {
        for component in module.components() {
            let Some(path) = resolve_file_backed_footprint(
                component.footprint(),
                std::path::Path::new(component.source_path()),
                resolution,
            ) else {
                continue;
            };
            let path = file_provider.canonicalize(&path).unwrap_or(path);
            let cached = validated
                .entry(path.clone())
                .or_insert_with(|| validate_footprint_file(&path, file_provider));
            diagnostics.extend(cached.iter().cloned().map(|diagnostic| {
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
    resolution: &ResolutionResult,
) -> Option<PathBuf> {
    if !footprint.ends_with(".kicad_mod") {
        return None;
    }
    if footprint.starts_with(pcb_sch::PACKAGE_URI_PREFIX) {
        return resolution.resolve_package_uri(footprint).ok();
    }

    let path = PathBuf::from(footprint);
    if path.is_absolute() {
        return Some(path);
    }
    source_path.parent().map(|parent| parent.join(path))
}

fn validate_footprint_file(
    path: &std::path::Path,
    file_provider: &dyn FileProvider,
) -> Vec<Diagnostic> {
    let path_str = path.to_string_lossy().to_string();
    match file_provider.read_file(path) {
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
