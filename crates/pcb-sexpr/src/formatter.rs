//! Formatting helpers for S-expressions.
//!
//! This module contains:
//! - A KiCad-style text prettifier (`prettify`) that mirrors KiCad's `Prettify()`
//! - A tree entrypoint (`format_tree`) that always formats via `prettify`

use crate::{Sexpr, SexprKind};

/// KiCad-compatible formatting modes from `KICAD_FORMAT::FORMAT_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormatMode {
    /// Standard KiCad formatting.
    #[default]
    Normal,
    /// Keep selected text-property lists on a single line.
    CompactTextProperties,
    /// Keep `(lib ...)` rows on a single line.
    LibraryTable,
}

impl FormatMode {
    #[inline]
    fn uses_compact_text_properties(self) -> bool {
        self == Self::CompactTextProperties
    }

    #[inline]
    fn uses_library_table_rows(self) -> bool {
        self == Self::LibraryTable
    }
}

/// Pretty-print raw S-expression text using KiCad's `Prettify()` logic.
///
/// This intentionally mirrors KiCad's character-stream formatter behavior:
/// it normalizes whitespace, uses tab indentation, and applies KiCad's
/// XY/short-form/library-table special cases.
pub fn prettify(source: &str, mode: FormatMode) -> String {
    const QUOTE_CHAR: u8 = b'"';
    const INDENT_CHAR: u8 = b'\t';
    const INDENT_SIZE: usize = 1;
    const XY_SPECIAL_CASE_COLUMN_LIMIT: usize = 99;
    const CONSECUTIVE_TOKEN_WRAP_THRESHOLD: usize = 72;

    let text_special_case = mode.uses_compact_text_properties();
    let lib_special_case = mode.uses_library_table_rows();

    let bytes = source.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());

    let mut list_depth = 0usize;
    let mut lib_depth = 0usize;
    let mut last_non_whitespace = 0u8;
    let mut in_quote = false;
    let mut has_inserted_space = false;
    let mut in_multi_line_list = false;
    let mut in_xy = false;
    let mut in_short_form = false;
    let mut in_lib_row = false;
    let mut short_form_depth = 0usize;
    let mut column = 0usize;
    let mut backslash_count = 0usize;

    for (i, &current) in bytes.iter().enumerate() {
        let next = next_non_whitespace(bytes, i + 1);

        if is_whitespace(current) && !in_quote {
            if !has_inserted_space
                && list_depth > 0
                && last_non_whitespace != b'('
                && next != b')'
                && next != b'('
            {
                if in_xy || column < CONSECUTIVE_TOKEN_WRAP_THRESHOLD {
                    out.push(b' ');
                    column += 1;
                } else if in_short_form || in_lib_row {
                    out.push(b' ');
                } else {
                    out.push(b'\n');
                    push_indent(&mut out, list_depth * INDENT_SIZE, INDENT_CHAR);
                    column = list_depth * INDENT_SIZE;
                    in_multi_line_list = true;
                }

                has_inserted_space = true;
            }
        } else {
            has_inserted_space = false;

            if current == b'(' && !in_quote {
                let current_is_xy = is_xy(bytes, i);
                let current_is_short_form = text_special_case && is_short_form(bytes, i);
                let current_is_lib = lib_special_case && is_lib(bytes, i);

                if out.is_empty() {
                    out.push(b'(');
                    column += 1;
                } else if (in_xy && current_is_xy && column < XY_SPECIAL_CASE_COLUMN_LIMIT)
                    || in_short_form
                    || in_lib_row
                {
                    out.extend_from_slice(b" (");
                    column += 2;
                } else {
                    out.push(b'\n');
                    push_indent(&mut out, list_depth * INDENT_SIZE, INDENT_CHAR);
                    out.push(b'(');
                    column = list_depth * INDENT_SIZE + 1;
                }

                in_xy = current_is_xy;

                if current_is_short_form {
                    in_short_form = true;
                    short_form_depth = list_depth;
                } else if current_is_lib {
                    in_lib_row = true;
                    lib_depth = list_depth;
                }

                list_depth += 1;
            } else if current == b')' && !in_quote {
                list_depth = list_depth.saturating_sub(1);

                if in_short_form {
                    out.push(b')');
                    column += 1;
                } else if in_lib_row && list_depth == lib_depth {
                    out.push(b')');
                    in_lib_row = false;
                } else if last_non_whitespace == b')' || in_multi_line_list {
                    out.push(b'\n');
                    push_indent(&mut out, list_depth * INDENT_SIZE, INDENT_CHAR);
                    out.push(b')');
                    column = list_depth * INDENT_SIZE + 1;
                    in_multi_line_list = false;
                } else {
                    out.push(b')');
                    column += 1;
                }

                if short_form_depth == list_depth {
                    in_short_form = false;
                    short_form_depth = 0;
                }
            } else {
                if current == b'\\' {
                    backslash_count += 1;
                } else if current == QUOTE_CHAR && (backslash_count & 1) == 0 {
                    in_quote = !in_quote;
                }

                if current != b'\\' {
                    backslash_count = 0;
                }

                out.push(current);
                column += 1;
            }

            last_non_whitespace = current;
        }
    }

    // POSIX newline at EOF.
    out.push(b'\n');

    String::from_utf8(out).expect("formatter emitted non-UTF-8 output")
}

/// Format an S-expression tree through the KiCad-style prettifier.
///
/// The returned string includes a trailing newline.
pub fn format_tree(sexpr: &Sexpr, mode: FormatMode) -> String {
    let raw = serialize_compact(sexpr);
    prettify(&raw, mode)
}

fn serialize_compact(sexpr: &Sexpr) -> String {
    let mut out = String::new();
    write_compact(sexpr, &mut out);
    out
}

fn write_compact(sexpr: &Sexpr, out: &mut String) {
    match &sexpr.kind {
        SexprKind::Symbol(s) => out.push_str(s),
        SexprKind::String(s) => {
            out.push('"');
            out.push_str(&escape_string(s));
            out.push('"');
        }
        SexprKind::Int(n) => {
            if let Some(raw) = sexpr.raw_atom.as_deref() {
                out.push_str(raw);
            } else {
                out.push_str(&n.to_string());
            }
        }
        SexprKind::F64(f) => {
            if let Some(raw) = sexpr.raw_atom.as_deref() {
                out.push_str(raw);
            } else {
                out.push_str(&trim_float(f.to_string()));
            }
        }
        SexprKind::List(items) => {
            out.push('(');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(' ');
                }
                write_compact(item, out);
            }
            out.push(')');
        }
    }
}

/// Quote a string value, escaping special characters.
pub(crate) fn quote_string(value: &str) -> String {
    let escaped = escape_string(value);
    let mut quoted = String::with_capacity(escaped.len() + 2);
    quoted.push('"');
    quoted.push_str(&escaped);
    quoted.push('"');
    quoted
}

pub(crate) fn escape_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            _ => result.push(ch),
        }
    }
    result
}

fn is_whitespace(ch: u8) -> bool {
    matches!(ch, b' ' | b'\t' | b'\n' | b'\r')
}

fn next_non_whitespace(bytes: &[u8], idx: usize) -> u8 {
    bytes[idx..]
        .iter()
        .copied()
        .find(|&ch| !is_whitespace(ch))
        .unwrap_or(0)
}

fn token_after_lparen(bytes: &[u8], pos: usize) -> &[u8] {
    let mut idx = pos.saturating_add(1);
    let start = idx;
    while idx < bytes.len() && bytes[idx].is_ascii_alphabetic() {
        idx += 1;
    }
    &bytes[start..idx]
}

fn is_xy(bytes: &[u8], pos: usize) -> bool {
    pos + 3 < bytes.len()
        && bytes[pos + 1] == b'x'
        && bytes[pos + 2] == b'y'
        && bytes[pos + 3] == b' '
}

fn is_short_form(bytes: &[u8], pos: usize) -> bool {
    let token = token_after_lparen(bytes, pos);
    const SHORT_FORM_TOKENS: &[&[u8]] = &[
        b"font",
        b"stroke",
        b"fill",
        b"teardrop",
        b"offset",
        b"rotate",
        b"scale",
    ];
    SHORT_FORM_TOKENS.contains(&token)
}

fn is_lib(bytes: &[u8], pos: usize) -> bool {
    token_after_lparen(bytes, pos) == b"lib"
}

fn push_indent(out: &mut Vec<u8>, depth: usize, ch: u8) {
    out.reserve(depth);
    out.extend(std::iter::repeat_n(ch, depth));
}

fn trim_float(mut s: String) -> String {
    if !s.contains('.') {
        return s;
    }

    while let Some(stripped) = s.strip_suffix('0') {
        s = stripped.to_string();
    }
    if let Some(stripped) = s.strip_suffix('.') {
        s = stripped.to_string();
    }

    if s.is_empty() { "0".to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::{FormatMode, format_tree, prettify};
    use crate::{Sexpr, parse};

    #[test]
    fn prettify_basic_board() {
        let input = "(kicad_pcb (version 20240101) (generator pcbnew) (general (thickness 1.6)))";
        let expected = "(kicad_pcb\n\t(version 20240101)\n\t(generator pcbnew)\n\t(general\n\t\t(thickness 1.6)\n\t)\n)\n";

        assert_eq!(prettify(input, FormatMode::Normal), expected);
    }

    #[test]
    fn prettify_xy_stays_on_line_until_threshold() {
        let input = "(pts (xy 1 2) (xy 3 4) (xy 5 6) (xy 7 8))";
        let expected = "(pts\n\t(xy 1 2) (xy 3 4) (xy 5 6) (xy 7 8)\n)\n";

        assert_eq!(prettify(input, FormatMode::Normal), expected);
    }

    #[test]
    fn prettify_compact_text_properties_short_form_tokens() {
        let input =
            "(effects (font (size 1 1) (thickness 0.15)) (stroke (width 0.12) (type solid)))";
        let expected = "(effects\n\t(font (size 1 1) (thickness 0.15))\n\t(stroke (width 0.12) (type solid))\n)\n";

        assert_eq!(prettify(input, FormatMode::CompactTextProperties), expected);
    }

    #[test]
    fn prettify_library_table_rows() {
        let input = "(fp_lib_table (lib (name A) (type KiCad) (uri /x)) (lib (name B) (type KiCad) (uri /y)))";
        let expected = "(fp_lib_table\n\t(lib (name A) (type KiCad) (uri /x))\n\t(lib (name B) (type KiCad) (uri /y))\n)\n";

        assert_eq!(prettify(input, FormatMode::LibraryTable), expected);
    }

    #[test]
    fn prettify_ignores_parens_inside_quoted_strings() {
        let input = "(root (field \"a (b) \\\"c\\\"\") (x 1))";
        let expected = "(root\n\t(field \"a (b) \\\"c\\\"\")\n\t(x 1)\n)\n";

        assert_eq!(prettify(input, FormatMode::Normal), expected);
    }

    #[test]
    fn format_tree_uses_prettify_pipeline() {
        let sexpr = Sexpr::list(vec![
            Sexpr::symbol("kicad_pcb"),
            Sexpr::list(vec![Sexpr::symbol("version"), Sexpr::int(20240101)]),
            Sexpr::list(vec![Sexpr::symbol("generator"), Sexpr::symbol("pcbnew")]),
        ]);

        let expected = "(kicad_pcb\n\t(version 20240101)\n\t(generator pcbnew)\n)\n";
        assert_eq!(format_tree(&sexpr, FormatMode::Normal), expected);
    }

    #[test]
    fn format_tree_has_trailing_newline() {
        let sexpr = Sexpr::list(vec![Sexpr::symbol("at"), Sexpr::int(10), Sexpr::int(20)]);
        assert_eq!(format_tree(&sexpr, FormatMode::Normal), "(at 10 20)\n");
    }

    #[test]
    fn format_tree_preserves_parsed_numeric_lexemes() {
        let sexpr = parse(
            r#"(kicad_pcb
                (setup
                    (pcbplotparams
                        (dashed_line_dash_ratio 12.000000)
                        (dashed_line_gap_ratio 3.000000)
                        (hpglpendiameter 15.000000)
                    )
                )
            )"#,
        )
        .unwrap();

        let out = format_tree(&sexpr, FormatMode::Normal);
        assert!(out.contains("(dashed_line_dash_ratio 12.000000)"));
        assert!(out.contains("(dashed_line_gap_ratio 3.000000)"));
        assert!(out.contains("(hpglpendiameter 15.000000)"));
    }
}
