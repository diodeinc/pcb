//! KiCad symbol library (`.kicad_sym`) helpers.

use std::collections::BTreeMap;

use crate::Sexpr;

/// Return root items for a KiCad symbol library `(kicad_symbol_lib ...)`.
pub fn kicad_symbol_lib_items(sexpr: &Sexpr) -> Option<&[Sexpr]> {
    let items = sexpr.as_list()?;
    (items.first().and_then(Sexpr::as_sym) == Some("kicad_symbol_lib")).then_some(items)
}

/// Return mutable root items for a KiCad symbol library `(kicad_symbol_lib ...)`.
pub fn kicad_symbol_lib_items_mut(sexpr: &mut Sexpr) -> Option<&mut Vec<Sexpr>> {
    let items = sexpr.as_list_mut()?;
    (items.first().and_then(Sexpr::as_sym) == Some("kicad_symbol_lib")).then_some(items)
}

/// Return the symbol name from a `(symbol "<name>" ...)` list.
pub fn symbol_name(symbol: &[Sexpr]) -> Option<String> {
    if symbol.first().and_then(Sexpr::as_sym) != Some("symbol") {
        return None;
    }
    symbol.get(1).and_then(atom_to_string)
}

/// Return names of all top-level symbols in a KiCad symbol library.
pub fn symbol_names(kicad_symbol_lib: &[Sexpr]) -> Vec<String> {
    kicad_symbol_lib
        .iter()
        .filter_map(|node| node.as_list())
        .filter_map(symbol_name)
        .collect()
}

/// Find a top-level symbol by name.
pub fn find_symbol<'a>(kicad_symbol_lib: &'a [Sexpr], name: &str) -> Option<&'a [Sexpr]> {
    kicad_symbol_lib.iter().find_map(|node| {
        let list = node.as_list()?;
        (symbol_name(list).as_deref() == Some(name)).then_some(list)
    })
}

/// Find the index of a top-level symbol by name.
pub fn find_symbol_index(kicad_symbol_lib: &[Sexpr], name: &str) -> Option<usize> {
    kicad_symbol_lib.iter().enumerate().find_map(|(idx, node)| {
        let list = node.as_list()?;
        (symbol_name(list).as_deref() == Some(name)).then_some(idx)
    })
}

/// Check whether a symbol directly declares `(extends "...")`.
pub fn symbol_declares_extends(symbol: &[Sexpr]) -> bool {
    symbol.iter().skip(2).any(|child| {
        child
            .as_list()
            .and_then(|items| items.first().and_then(Sexpr::as_sym))
            == Some("extends")
    })
}

/// Extract direct `(property "<name>" "<value>" ...)` pairs from a symbol.
pub fn symbol_properties(symbol: &[Sexpr]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for child in symbol.iter().skip(2) {
        let Some(items) = child.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("property") {
            continue;
        }
        let Some(name) = items.get(1).and_then(atom_to_string) else {
            continue;
        };
        let Some(value) = items.get(2).and_then(atom_to_string) else {
            continue;
        };
        out.insert(name, value);
    }
    out
}

/// Rewrite a symbol's direct `(property ...)` nodes to match `next`.
///
/// Existing property nodes are updated or removed, and missing nodes are created.
/// New properties are inserted before nested `(symbol ...)` unit/style blocks.
pub fn rewrite_symbol_properties(symbol_items: &mut Vec<Sexpr>, next: &BTreeMap<String, String>) {
    let mut remaining = next.clone();
    let mut rewritten = Vec::with_capacity(symbol_items.len() + next.len());

    for item in symbol_items.drain(..) {
        let Some(name) = property_name(&item) else {
            rewritten.push(item);
            continue;
        };

        if let Some(new_value) = remaining.remove(&name) {
            rewritten.push(set_property_value(item, &new_value));
        }
    }

    let insert_idx = rewritten
        .iter()
        .enumerate()
        .skip(2)
        .find_map(|(idx, node)| is_nested_symbol(node).then_some(idx))
        .unwrap_or(rewritten.len());

    let additions = remaining
        .into_iter()
        .map(|(key, value)| default_property_node(&key, &value));
    rewritten.splice(insert_idx..insert_idx, additions);

    *symbol_items = rewritten;
}

fn atom_to_string(node: &Sexpr) -> Option<String> {
    if let Some(s) = node.as_str() {
        return Some(s.to_string());
    }
    if let Some(s) = node.as_sym() {
        return Some(s.to_string());
    }
    if let Some(i) = node.as_int() {
        return Some(i.to_string());
    }
    node.as_float().map(|f| f.to_string())
}

fn property_name(node: &Sexpr) -> Option<String> {
    let items = node.as_list()?;
    if items.first().and_then(Sexpr::as_sym) != Some("property") {
        return None;
    }
    items.get(1).and_then(atom_to_string)
}

fn set_property_value(mut node: Sexpr, value: &str) -> Sexpr {
    if let Some(items) = node.as_list_mut() {
        if items.len() <= 2 {
            while items.len() < 2 {
                items.push(Sexpr::string(""));
            }
            items.push(Sexpr::string(value));
        } else {
            items[2] = Sexpr::string(value);
        }
    }
    node
}

fn default_property_node(name: &str, value: &str) -> Sexpr {
    Sexpr::list(vec![
        Sexpr::symbol("property"),
        Sexpr::string(name),
        Sexpr::string(value),
        Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::float(0.0),
            Sexpr::float(0.0),
            Sexpr::int(0),
        ]),
        Sexpr::list(vec![
            Sexpr::symbol("effects"),
            Sexpr::list(vec![
                Sexpr::symbol("font"),
                Sexpr::list(vec![
                    Sexpr::symbol("size"),
                    Sexpr::float(1.27),
                    Sexpr::float(1.27),
                ]),
            ]),
        ]),
    ])
}

fn is_nested_symbol(node: &Sexpr) -> bool {
    node.as_list()
        .and_then(|items| items.first().and_then(Sexpr::as_sym))
        == Some("symbol")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_symbols_and_properties() {
        let source = r#"(kicad_symbol_lib
            (symbol "A"
                (property "Reference" "U" (at 0 0 0))
                (property "Value" "A" (at 0 0 0))
            )
            (symbol "B"
                (property "Reference" "R" (at 0 0 0))
            )
        )"#;
        let parsed = crate::parse(source).unwrap();
        let root = kicad_symbol_lib_items(&parsed).unwrap();
        assert_eq!(symbol_names(root), vec!["A".to_string(), "B".to_string()]);
        let sym_a = find_symbol(root, "A").unwrap();
        let props = symbol_properties(sym_a);
        assert_eq!(props.get("Reference"), Some(&"U".to_string()));
        assert_eq!(props.get("Value"), Some(&"A".to_string()));
    }

    #[test]
    fn rewrites_properties() {
        let source = r#"(kicad_symbol_lib
            (symbol "A"
                (property "Reference" "U" (at 0 0 0))
                (property "Obsolete" "x" (at 0 0 0))
                (symbol "A_0_1")
            )
        )"#;
        let mut parsed = crate::parse(source).unwrap();
        let root = kicad_symbol_lib_items_mut(&mut parsed).unwrap();
        let idx = find_symbol_index(root, "A").unwrap();
        let sym = root.get_mut(idx).and_then(Sexpr::as_list_mut).unwrap();
        rewrite_symbol_properties(
            sym,
            &BTreeMap::from([
                ("Reference".to_string(), "Q".to_string()),
                ("Value".to_string(), "A".to_string()),
            ]),
        );

        let props = symbol_properties(sym);
        assert_eq!(props.get("Reference"), Some(&"Q".to_string()));
        assert_eq!(props.get("Value"), Some(&"A".to_string()));
        assert!(!props.contains_key("Obsolete"));
    }
}
