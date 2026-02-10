//! KiCad schematic (`.kicad_sch`) helpers.

use crate::Sexpr;
use std::collections::BTreeMap;

use super::props::child_list;

/// Extract `(instances (project ... (path "/...") ...))` paths for a placed schematic symbol.
pub fn schematic_instance_paths(symbol: &[Sexpr]) -> Vec<String> {
    let Some(instances) = child_list(symbol, "instances") else {
        return Vec::new();
    };

    let mut out: Vec<String> = Vec::new();
    for child in instances.iter().skip(1) {
        let Some(project) = child.as_list() else {
            continue;
        };
        if project.first().and_then(Sexpr::as_sym) != Some("project") {
            continue;
        }
        for project_child in project.iter().skip(1) {
            let Some(items) = project_child.as_list() else {
                continue;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("path") {
                continue;
            }
            if let Some(path) = items.get(1).and_then(Sexpr::as_str) {
                out.push(path.to_string());
            }
        }
    }

    out
}

/// Extract the first `(instances (project ... (path "/...") ...))` path for a placed schematic
/// symbol.
///
/// Note: schematic symbols can contain *multiple* project instances. Prefer using
/// [`schematic_instance_paths`] and selecting the correct one at the callsite when possible.
pub fn schematic_instance_path(symbol: &[Sexpr]) -> Option<String> {
    schematic_instance_paths(symbol).into_iter().next()
}

/// Extract all `(property "NAME" "VALUE" ...)` pairs from a placed schematic symbol.
pub fn schematic_properties(symbol: &[Sexpr]) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for node in symbol.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("property") {
            continue;
        }
        let Some(name) = items.get(1).and_then(Sexpr::as_str) else {
            continue;
        };
        let value = items
            .get(2)
            .and_then(Sexpr::as_str)
            .unwrap_or_default()
            .to_string();
        out.insert(name.to_string(), value);
    }
    out
}

/// Extract `(pin "<num>" (uuid "..."))` pin UUIDs from a placed schematic symbol.
pub fn schematic_pins(symbol: &[Sexpr]) -> Option<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for node in symbol.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("pin") {
            continue;
        }
        let Some(pin_number) = items.get(1).and_then(Sexpr::as_str) else {
            continue;
        };
        let uuid = items
            .iter()
            .find_map(|child| {
                let list = child.as_list()?;
                (list.first()?.as_sym() == Some("uuid"))
                    .then(|| list.get(1).and_then(Sexpr::as_str))
                    .flatten()
            })
            .map(|s| s.to_string());
        if let Some(uuid) = uuid {
            out.insert(pin_number.to_string(), uuid);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Extract schematic placement `(at x y [rot])` for a placed symbol.
pub fn schematic_at(symbol: &[Sexpr]) -> Option<(f64, f64, Option<f64>)> {
    let at = child_list(symbol, "at")?;
    let x = crate::number_as_f64(at.get(1)?)?;
    let y = crate::number_as_f64(at.get(2)?)?;
    let rot = at.get(3).and_then(crate::number_as_f64);
    Some((x, y, rot))
}
