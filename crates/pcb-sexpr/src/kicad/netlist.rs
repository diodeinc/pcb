//! KiCad netlist (`kicadsexpr`) helpers.

use crate::Sexpr;

use super::props::child_list;

/// Parse a KiCad netlist `(sheetpath (names "...") (tstamps "..."))` child.
pub fn sheetpath(list: &[Sexpr]) -> Option<(Option<String>, String)> {
    let sheetpath = child_list(list, "sheetpath")?;

    let mut names: Option<String> = None;
    let mut tstamps: Option<String> = None;

    for item in sheetpath.iter().skip(1) {
        let Some(items) = item.as_list() else {
            continue;
        };
        match items.first().and_then(Sexpr::as_sym) {
            Some("names") => {
                names = items.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
            }
            Some("tstamps") => {
                tstamps = items.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
            }
            _ => {}
        }
    }

    tstamps.map(|t| (names, t))
}
