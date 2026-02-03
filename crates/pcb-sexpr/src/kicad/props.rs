//! Common KiCad-ish S-expression query helpers.
//!
//! Many KiCad formats use small list nodes that behave like key/value properties:
//! `(tag "value")`, `(tag 123)`, `(tag yes)`, etc. These helpers standardize querying.

use crate::find_child_list;
use crate::Sexpr;

/// Find a direct child list `(tag ...)` within `list`.
pub fn child_list<'a>(list: &'a [Sexpr], tag: &str) -> Option<&'a [Sexpr]> {
    find_child_list(list, tag)
}

/// Find a string property `(tag "VALUE")` within `list`.
pub fn string_prop(list: &[Sexpr], tag: &str) -> Option<String> {
    child_list(list, tag)?
        .get(1)?
        .as_str()
        .map(|s| s.to_string())
}

/// Find a symbol atom property `(tag VALUE)` within `list`.
pub fn sym_prop(list: &[Sexpr], tag: &str) -> Option<String> {
    child_list(list, tag)?
        .get(1)?
        .as_sym()
        .map(|s| s.to_string())
}

/// Find a boolean property that is represented as `(tag yes)` or `(tag no)`.
pub fn yes_no_prop(list: &[Sexpr], tag: &str) -> Option<bool> {
    match sym_prop(list, tag)?.as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Find an integer property `(tag 123)` within `list`.
pub fn int_prop(list: &[Sexpr], tag: &str) -> Option<i64> {
    child_list(list, tag)?.get(1)?.as_int()
}

/// Find a list-of-strings property `(tag "A" "B" ...)` within `list`.
pub fn string_list_prop(list: &[Sexpr], tag: &str) -> Option<Vec<String>> {
    let items = child_list(list, tag)?;
    let out: Vec<String> = items
        .iter()
        .skip(1)
        .filter_map(|s| s.as_str().map(|v| v.to_string()))
        .collect();
    (!out.is_empty()).then_some(out)
}
