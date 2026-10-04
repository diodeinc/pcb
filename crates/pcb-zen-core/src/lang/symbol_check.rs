use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pcb_eda::kicad::symbol_check::{
    FootprintPads, Severity, SymbolIssue, check_library, check_symbol,
};
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use starlark::{codemap::CodeMap, errors::EvalSeverity, values::ValueLike};
use tracing::{info_span, instrument};

use crate::{Diagnostic, FileProvider, resolution::ResolutionResult};

use super::footprint::{resolve_file_backed_footprint, resolved_span};
use super::module::{FrozenModuleValue, ModulePath};
use super::symbol::{SymbolValue, loaded_symbol_library};

/// A symbol library some component loaded from, with what it takes to turn
/// its issues into diagnostics.
struct Library {
    library: Arc<KicadSymbolLibrary>,
    paths: Vec<String>,
    /// One per source, built when the library has its first issue.
    codemaps: Vec<CodeMap>,
    parses: bool,
}

impl Library {
    fn diagnostic(&mut self, issue: SymbolIssue) -> Diagnostic {
        if self.codemaps.is_empty() {
            let sources = self.paths.iter().zip(self.library.sources());
            self.codemaps = sources
                .map(|(path, text)| CodeMap::new(path.clone(), text.clone()))
                .collect();
        }
        let body = format!("[{}] {}\nhelp: {}", issue.kind, issue.message, issue.help);
        let severity = match issue.severity {
            Severity::Error => EvalSeverity::Error,
            Severity::Warning => EvalSeverity::Warning,
            Severity::Advice => EvalSeverity::Advice,
        };
        Diagnostic::categorized(&self.paths[issue.source], &body, issue.kind, severity).with_span(
            Some(resolved_span(&self.codemaps[issue.source], issue.span)),
        )
    }
}

/// Check the KiCad symbol of every component whose symbol file belongs to a
/// workspace package; a dependency's symbols are not the workspace's to fix.
/// Diagnostics point into the symbol file, the file that has to change.
#[instrument(name = "check_symbols", skip_all)]
pub(crate) fn check_symbols(
    module_tree: &BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &ResolutionResult,
    file_provider: &dyn FileProvider,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut checked = HashSet::new();
    // Symbols that extend one parent repeat the findings in its pins.
    let mut reported = HashSet::new();
    let mut libraries: HashMap<PathBuf, Option<Library>> = HashMap::new();
    let mut pads: HashMap<PathBuf, Option<BTreeSet<String>>> = HashMap::new();

    for component in module_tree.values().flat_map(|module| module.components()) {
        let Some(symbol) = component.symbol().downcast_ref::<SymbolValue>() else {
            continue;
        };
        let (Some(uri), Some(name)) = (symbol.source_uri(), symbol.name()) else {
            continue;
        };
        // Instances of one component repeat; resolving paths is the costly part.
        if !checked.insert((uri, name, component.footprint(), component.source_path()))
            || !resolution.is_workspace_uri(uri)
        {
            continue;
        }
        let Ok(path) = resolution.resolve_package_uri(uri) else {
            continue;
        };
        // The library is the one the symbol was loaded from, already parsed.
        let library = libraries.entry(path).or_insert_with_key(|path| {
            let (library, paths) = loaded_symbol_library(path, name, file_provider).ok()?;
            let mut library = Library {
                paths: paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                codemaps: Vec::new(),
                parses: true,
                library,
            };
            if let Some(issue) = check_library(&library.library) {
                library.parses = false;
                diagnostics.push(library.diagnostic(issue));
            }
            Some(library)
        });
        let Some(library) = library.as_mut().filter(|library| library.parses) else {
            continue;
        };

        let footprint = resolve_file_backed_footprint(
            component.footprint(),
            Path::new(component.source_path()),
            resolution,
        );
        let footprint_pads = footprint.as_deref().and_then(|footprint| {
            let numbers = pads.entry(footprint.to_path_buf()).or_insert_with(|| {
                let _span = info_span!("footprint_pads").entered();
                let text = file_provider.read_file(footprint).ok()?;
                Some(pcb_sexpr::kicad::footprint::pad_numbers(&text))
            });
            Some(FootprintPads {
                name: footprint.file_name()?.to_str()?,
                numbers: numbers.as_ref()?,
            })
        });

        for issue in check_symbol(&library.library, name, footprint_pads) {
            let file = library.paths[issue.source].clone();
            if reported.insert((file, issue.span.start, issue.message.clone())) {
                diagnostics.push(library.diagnostic(issue));
            }
        }
    }

    diagnostics
}
