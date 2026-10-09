//! KiCad symbol library (`.kicad_sym`) helpers.

use std::collections::{BTreeMap, BTreeSet};

use crate::Sexpr;

/// Expand a KiCad native pin stack into physical pin numbers.
/// Plain numbers remain literal. Malformed stacks and unsupported escaping or
/// nested brackets are errors, never literal bracketed pad names.
pub fn expand_stacked_pin_number(number: &str) -> Result<BTreeSet<String>, String> {
    const LIMIT: usize = 4096;
    if !number.contains(['[', ']']) {
        return Ok(BTreeSet::from([number.to_string()]));
    }
    let invalid = || format!("invalid or unsupported stacked pin number {number:?}");
    let inner = number
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(invalid)?;
    if inner.contains(['[', ']', '\\']) {
        return Err(invalid());
    }

    let mut expanded = BTreeSet::new();
    for part in inner.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some((start, end)) = part.split_once('-') {
            let (start_prefix, start_value) =
                alpha_numeric_pin(start.trim()).ok_or_else(invalid)?;
            let (end_prefix, end_value) = alpha_numeric_pin(end.trim()).ok_or_else(invalid)?;
            if start_prefix != end_prefix || start_value > end_value {
                return Err(invalid());
            }
            if end_value - start_value >= LIMIT as u64 {
                return Err(format!(
                    "stacked pin number {number:?} exceeds the limit of {LIMIT} pins"
                ));
            }
            for value in start_value..=end_value {
                expanded.insert(format!("{start_prefix}{value}"));
            }
        } else {
            expanded.insert(part.to_string());
        }
        if expanded.len() > LIMIT {
            return Err(format!(
                "stacked pin number {number:?} exceeds the limit of {LIMIT} pins"
            ));
        }
    }
    if expanded.is_empty() {
        return Err(invalid());
    }
    Ok(expanded)
}

fn alpha_numeric_pin(value: &str) -> Option<(&str, u64)> {
    let prefix = value.trim_end_matches(|c: char| c.is_ascii_digit());
    // A second dash is not an endpoint prefix: it denotes unsupported syntax.
    if prefix.contains('-') {
        return None;
    }
    Some((prefix, value[prefix.len()..].parse().ok()?))
}

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

/// Find the full top-level `(symbol ...)` node by name.
pub fn find_symbol_node<'a>(kicad_symbol_lib: &'a [Sexpr], name: &str) -> Option<&'a Sexpr> {
    kicad_symbol_lib
        .iter()
        .find(|node| node.as_list().and_then(symbol_name).as_deref() == Some(name))
}

/// Find the index of a top-level symbol by name.
pub fn find_symbol_index(kicad_symbol_lib: &[Sexpr], name: &str) -> Option<usize> {
    kicad_symbol_lib.iter().enumerate().find_map(|(idx, node)| {
        let list = node.as_list()?;
        (symbol_name(list).as_deref() == Some(name)).then_some(idx)
    })
}

/// The name and value of a `(property [private] "<name>" "<value>" ...)`.
pub fn property_name_value(property: &[Sexpr]) -> Option<(&Sexpr, &Sexpr)> {
    let fields = &property[1 + private_offset(property)..];
    Some((fields.first()?, fields.get(1)?))
}

/// One when a property starts with KiCad's `private` flag.
fn private_offset(property: &[Sexpr]) -> usize {
    usize::from(property.get(1).and_then(Sexpr::as_sym) == Some("private"))
}

/// Extract direct `(property "<name>" "<value>" ...)` pairs from a symbol.
pub fn symbol_properties(symbol: &[Sexpr]) -> BTreeMap<String, String> {
    symbol
        .iter()
        .skip(2)
        .filter_map(|child| {
            let items = child.as_list()?;
            (items.first().and_then(Sexpr::as_sym) == Some("property")).then_some(items)
        })
        .filter_map(property_name_value)
        .filter_map(|(name, value)| Some((atom_to_string(name)?, atom_to_string(value)?)))
        .collect()
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
    items
        .get(1 + private_offset(items))
        .and_then(atom_to_string)
}

fn set_property_value(mut node: Sexpr, value: &str) -> Sexpr {
    if let Some(items) = node.as_list_mut() {
        let at = 2 + private_offset(items);
        items.resize_with(items.len().max(at + 1), || Sexpr::string(""));
        items[at] = Sexpr::string(value);
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
            Sexpr::list(vec![
                Sexpr::symbol("justify"),
                Sexpr::symbol("left"),
                Sexpr::symbol("top"),
            ]),
            Sexpr::list(vec![Sexpr::symbol("hide"), Sexpr::symbol("yes")]),
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
    fn expands_native_stacks() {
        for (number, expected) in [
            ("[AD12-AD14,7,AD13]", vec!["AD12", "AD13", "AD14", "7"]),
            ("[A01-A3,007]", vec!["A1", "A2", "A3", "007"]),
            ("[A{comma}B,2]", vec!["A{comma}B", "2"]),
            ("[ P_2 - P_4 ,9,,]", vec!["P_2", "P_3", "P_4", "9"]),
            ("[0-0]", vec!["0"]),
            ("AD12-AD22", vec!["AD12-AD22"]),
            ("1,2", vec!["1,2"]),
        ] {
            assert_eq!(
                expand_stacked_pin_number(number).unwrap(),
                expected.into_iter().map(str::to_owned).collect(),
                "{number}"
            );
        }
        assert_eq!(
            expand_stacked_pin_number("[1-4096,1-4096]").unwrap().len(),
            4096
        );
    }

    #[test]
    fn rejects_malformed_or_unsupported_stacks() {
        for number in [
            "[]",
            "[ , ]",
            "[1",
            "1]",
            "x[1]",
            "[[1]]",
            "[3-1]",
            "[AD12-22]",
            "[AD12-ad22]",
            "[A-A3]",
            "[1-]",
            "[-3]",
            "[1-2-3]",
            "[11,19,BAD-RANGE]",
            r"[A\,B]",
            "[1-18446744073709551616]",
            "[0-18446744073709551615]",
            "[1-4097]",
            "[1-4096,5000]",
        ] {
            assert!(expand_stacked_pin_number(number).is_err(), "{number}");
        }
    }

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
        let sym_b_node = find_symbol_node(root, "B").unwrap();
        let props = symbol_properties(sym_a);
        assert_eq!(props.get("Reference"), Some(&"U".to_string()));
        assert_eq!(props.get("Value"), Some(&"A".to_string()));
        assert_eq!(
            sym_b_node
                .as_list()
                .and_then(|items| items.get(1))
                .and_then(Sexpr::as_str),
            Some("B")
        );
    }

    #[test]
    fn rewrites_properties() {
        let source = r#"(kicad_symbol_lib
            (symbol "A"
                (property "Reference" "U" (at 0 0 0))
                (property "Obsolete" "x" (at 0 0 0))
                (property private "Private" "p" (at 0 0 0))
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
                ("Private".to_string(), "p".to_string()),
            ]),
        );

        let props = symbol_properties(sym);
        assert_eq!(props.get("Reference"), Some(&"Q".to_string()));
        assert_eq!(props.get("Private"), Some(&"p".to_string()));
        assert_eq!(props.get("Value"), Some(&"A".to_string()));
        assert!(!props.contains_key("Obsolete"));

        let rendered = crate::formatter::format_tree(&parsed, crate::formatter::FormatMode::Normal);
        assert!(rendered.contains("(property \"Value\" \"A\""));
        assert!(rendered.contains("(justify left top)"));
        assert!(rendered.contains("(hide yes)"));
    }
}
