use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use pcb_eda::kicad::symbol_check::{FootprintPads, Severity, check_symbol};
use starlark::{codemap::CodeMap, errors::EvalSeverity, values::ValueLike};

use crate::{Diagnostic, FileProvider, resolution::ResolutionResult};

use super::footprint::{resolve_file_backed_footprint, resolved_span};
use super::module::{FrozenModuleValue, ModulePath};
use super::symbol::{SymbolValue, symbol_source_files};

/// Check the KiCad symbol of every component whose symbol file belongs to a
/// workspace package; a dependency's symbols are not the workspace's to fix.
/// Diagnostics point into the symbol file, the file that has to change.
pub(crate) fn check_symbols(
    module_tree: &BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &ResolutionResult,
    file_provider: &dyn FileProvider,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut checked = HashSet::new();
    // Symbols that extend one parent repeat the findings in its pins.
    let mut reported = HashSet::new();
    let mut pads: HashMap<PathBuf, Option<BTreeSet<String>>> = HashMap::new();

    for component in module_tree.values().flat_map(|module| module.components()) {
        let Some(symbol) = component.symbol().downcast_ref::<SymbolValue>() else {
            continue;
        };
        let (Some(uri), Some(name)) = (symbol.source_uri(), symbol.name()) else {
            continue;
        };
        if !resolution.is_workspace_uri(uri) {
            continue;
        }
        let Ok(path) = resolution.resolve_package_uri(uri) else {
            continue;
        };
        let footprint = resolve_file_backed_footprint(
            component.footprint(),
            Path::new(component.source_path()),
            resolution,
        );
        if !checked.insert((path.clone(), name.to_string(), footprint.clone())) {
            continue;
        }
        let Ok(sources) = symbol_source_files(&path, name, file_provider) else {
            continue;
        };
        let footprint_pads = footprint.as_deref().and_then(|footprint| {
            let numbers = pads.entry(footprint.to_path_buf()).or_insert_with(|| {
                let text = file_provider.read_file(footprint).ok()?;
                let parsed = pcb_sexpr::parse(&text).ok()?;
                Some(pcb_sexpr::kicad::footprint::pad_numbers(&parsed))
            });
            Some(FootprintPads {
                name: footprint.file_name()?.to_str()?,
                numbers: numbers.as_ref()?,
            })
        });

        let texts: Vec<&str> = sources.iter().map(|(_, text)| text.as_str()).collect();
        let issues = check_symbol(&texts, name, footprint_pads);
        if issues.is_empty() {
            continue;
        }
        let codemaps: Vec<(String, CodeMap)> = sources
            .iter()
            .map(|(path, text)| {
                let path = path.to_string_lossy().into_owned();
                (path.clone(), CodeMap::new(path, text.clone()))
            })
            .collect();
        for issue in issues {
            let (path, codemap) = &codemaps[issue.source];
            let body = format!("[{}] {}\nhelp: {}", issue.kind, issue.message, issue.help);
            if !reported.insert((path.clone(), issue.span.start, body.clone())) {
                continue;
            }
            let severity = match issue.severity {
                Severity::Error => EvalSeverity::Error,
                Severity::Warning => EvalSeverity::Warning,
                Severity::Advice => EvalSeverity::Advice,
            };
            diagnostics.push(
                Diagnostic::categorized(path, &body, issue.kind, severity)
                    .with_span(Some(resolved_span(codemap, issue.span))),
            );
        }
    }

    diagnostics
}
