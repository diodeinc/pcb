use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pcb_eda::kicad::symbol_check::{
    FootprintPads, Severity, SymbolIssue, check_library, check_symbol, fix,
};
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use starlark::{codemap::CodeMap, errors::EvalSeverity, values::ValueLike};
use tracing::{info_span, instrument};

use crate::{Diagnostic, FileProvider, resolution::ResolutionResult};

use super::component::FrozenComponentValue;
use super::footprint::{resolve_file_backed_footprint, resolved_span};
use super::module::{FrozenModuleValue, ModulePath};
use super::symbol::{SymbolValue, loaded_symbol_library};

/// A symbol library some component loaded from, with what it takes to turn
/// its issues into diagnostics.
struct Library {
    library: Arc<KicadSymbolLibrary>,
    /// The path and line index of each source of the library.
    sources: Vec<(String, CodeMap)>,
}

impl Library {
    fn load(path: &Path, symbol: &str, file_provider: &dyn FileProvider) -> Option<Self> {
        let (library, paths) = loaded_symbol_library(path, symbol, file_provider).ok()?;
        let sources = paths
            .iter()
            .zip(library.sources())
            .map(|(path, text)| {
                let path = path.to_string_lossy().into_owned();
                (path.clone(), CodeMap::new(path, text.clone()))
            })
            .collect();
        Some(Self { library, sources })
    }

    fn diagnostic(&self, issue: &SymbolIssue) -> Diagnostic {
        let (path, codemap) = &self.sources[issue.source];
        let fixable = !issue.fix.is_empty();
        let fix = if fixable { "; `pcb fix` does this" } else { "" };
        let body = format!(
            "[{}] {}\nhelp: {}{fix}",
            issue.kind, issue.message, issue.help
        );
        let severity = match issue.severity {
            Severity::Error => EvalSeverity::Error,
            Severity::Warning => EvalSeverity::Warning,
            Severity::Advice => EvalSeverity::Advice,
        };
        Diagnostic::categorized(path, &body, issue.kind, severity)
            .with_span(Some(resolved_span(codemap, issue.span)))
            .with_fixable(fixable)
    }
}

/// Each component whose symbol file belongs to a workspace package, with that
/// file and the symbol's name; a dependency's symbols are not the workspace's
/// to fix.
fn workspace_symbols<'a>(
    module_tree: &'a BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &'a ResolutionResult,
) -> impl Iterator<Item = (&'a FrozenComponentValue, PathBuf, &'a str)> {
    let mut seen = HashSet::new();
    let components = module_tree.values().flat_map(|module| module.components());
    components.filter_map(move |component| {
        let symbol = component.symbol().downcast_ref::<SymbolValue>()?;
        let (uri, name) = (symbol.source_uri()?, symbol.name()?);
        // Instances of one component repeat; resolving paths is the costly part.
        let fresh = seen.insert((uri, name, component.footprint(), component.source_path()));
        let path = (fresh && resolution.is_workspace_uri(uri))
            .then(|| resolution.resolve_package_uri(uri).ok())??;
        Some((component, path, name))
    })
}

/// Fix what can be fixed in the symbols [`check_symbols`] checks. `fixed`
/// holds the new text of each file that changes, and is read back so that
/// fixes from several evaluations build on each other.
pub(crate) fn fix_symbols(
    module_tree: &BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &ResolutionResult,
    file_provider: &dyn FileProvider,
    fixed: &mut BTreeMap<PathBuf, String>,
) {
    for (_, path, name) in workspace_symbols(module_tree, resolution) {
        let Ok((library, paths)) = loaded_symbol_library(&path, name, file_provider) else {
            continue;
        };
        let texts = paths.iter().zip(library.sources());
        let sources = texts
            .map(|(path, text)| fixed.get(path).unwrap_or(text).clone())
            .collect();
        let after = fix(sources, name);
        for ((path, before), after) in paths.into_iter().zip(library.sources()).zip(after) {
            if after != *before {
                fixed.insert(path, after);
            }
        }
    }
}

/// Check the KiCad symbol of every component whose symbol file belongs to a
/// workspace package. Diagnostics point into the symbol file, the file that
/// has to change.
#[instrument(name = "check_symbols", skip_all)]
pub(crate) fn check_symbols(
    module_tree: &BTreeMap<ModulePath, &FrozenModuleValue>,
    resolution: &ResolutionResult,
    file_provider: &dyn FileProvider,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    // Symbols that extend one parent repeat the findings in its pins.
    let mut reported = HashSet::new();
    let mut libraries: HashMap<PathBuf, Option<Library>> = HashMap::new();
    let mut pads: HashMap<PathBuf, Option<BTreeSet<String>>> = HashMap::new();

    for (component, path, name) in workspace_symbols(module_tree, resolution) {
        // The library is the one the symbol was loaded from, already parsed.
        // One KiCad cannot read is reported once and its symbols left alone.
        let library = libraries.entry(path).or_insert_with_key(|path| {
            let library = Library::load(path, name, file_provider)?;
            match check_library(&library.library) {
                Some(unreadable) => {
                    diagnostics.push(library.diagnostic(&unreadable));
                    None
                }
                None => Some(library),
            }
        });
        let Some(library) = library else {
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
            let file = library.sources[issue.source].0.clone();
            if reported.insert((file, issue.span.start, issue.message.clone())) {
                diagnostics.push(library.diagnostic(&issue));
            }
        }
    }

    diagnostics
}
