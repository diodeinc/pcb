//! Structural scans of S-expression text that never build a tree, for
//! callers that need a little from files dominated by large atoms.

use std::ops::Range;

use memchr::{memchr, memchr2, memchr3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Open,
    Close,
    String,
    /// A quote that is never closed; the token runs to the end of the text.
    UnterminatedString,
}

/// The parentheses and quoted strings of `source` in order, with their byte
/// ranges. A `;` is not read as the start of a comment: see [`malformed`].
pub fn tokens(source: &str) -> impl Iterator<Item = (Range<usize>, Token)> + '_ {
    let bytes = source.as_bytes();
    let mut pos = 0;
    std::iter::from_fn(move || {
        let at = pos + memchr3(b'(', b')', b'"', bytes.get(pos..)?)?;
        let (end, token) = match bytes[at] {
            b'(' => (at + 1, Token::Open),
            b')' => (at + 1, Token::Close),
            _ => match string_end(bytes, at + 1) {
                Some(end) => (end, Token::String),
                None => (bytes.len(), Token::UnterminatedString),
            },
        };
        pos = end;
        Some((at..end, token))
    })
}

/// Offset just past the closing quote of a string whose contents start at `pos`.
fn string_end(bytes: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        pos += memchr2(b'"', b'\\', bytes.get(pos..)?)?;
        if bytes[pos] == b'"' {
            return Some(pos + 1);
        }
        pos += 2;
    }
}

/// The parentheses of `source` in order, as `(byte offset, is opening)`.
pub fn parens(source: &str) -> impl Iterator<Item = (usize, bool)> + '_ {
    tokens(source).filter_map(|(range, token)| match token {
        Token::Open => Some((range.start, true)),
        Token::Close => Some((range.start, false)),
        Token::String | Token::UnterminatedString => None,
    })
}

/// The `;` comments of `source`, each from a `;` outside a string to the end
/// of its line.
pub fn comments(source: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = source.as_bytes();
    let mut pos = 0;
    std::iter::from_fn(move || {
        loop {
            let at = pos + memchr2(b';', b'"', bytes.get(pos..)?)?;
            if bytes[at] == b'"' {
                pos = string_end(bytes, at + 1)?;
                continue;
            }
            pos = memchr(b'\n', &bytes[at..]).map_or(bytes.len(), |eol| at + eol);
            return Some(at..pos - usize::from(bytes[pos - 1] == b'\r'));
        }
    })
}

/// Why `source` is not one well-formed list, with the offset of the fault.
/// What follows the root list is not read, as KiCad does not read it.
///
/// A `;` outside a string is a fault: this crate's parser reads it as the
/// start of a comment, but KiCad reads it as text and fails on it.
pub fn malformed(source: &str) -> Option<(usize, &'static str)> {
    let bytes = source.as_bytes();
    let comment = |gap: Range<usize>| {
        let at = gap.start + memchr(b';', &bytes[gap])?;
        Some((at, "`;` starts a comment here, but KiCad reads it as text"))
    };
    let (mut depth, mut root, mut scanned) = (0usize, None, 0);
    for (range, token) in tokens(source) {
        if let Some(fault) = comment(scanned..range.start) {
            return Some(fault);
        }
        scanned = range.end;
        match token {
            Token::Open => {
                root.get_or_insert(range.start);
                depth += 1;
            }
            Token::Close if depth == 0 => return Some((range.start, "unmatched `)`")),
            Token::Close if depth == 1 => return None,
            Token::Close => depth -= 1,
            Token::String if depth == 0 => {
                return Some((range.start, "text outside the root list"));
            }
            Token::String => {}
            Token::UnterminatedString => return Some((range.start, "unterminated string")),
        }
    }
    comment(scanned..bytes.len()).or_else(|| Some((root?, "unclosed `(`")))
}

/// The direct child lists of the root list of a well-formed `source`, as
/// byte ranges in order. Nothing after the root list is read.
pub fn children(source: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let (mut depth, mut start) = (0usize, 0);
    parens(source)
        .map(move |(at, open)| {
            depth = if open {
                depth + 1
            } else {
                depth.saturating_sub(1)
            };
            if open && depth == 2 {
                start = at;
            }
            (depth, open, start..at + 1)
        })
        .take_while(|(depth, open, _)| *open || *depth > 0)
        .filter_map(|(depth, open, range)| (!open && depth == 1).then_some(range))
}

/// The head word of the list that opens at `open`.
pub fn head(source: &str, open: usize) -> &str {
    let rest = &source[open + 1..];
    let end = rest
        .find(|c: char| c.is_ascii_whitespace() || matches!(c, '(' | ')' | '"'))
        .unwrap_or(rest.len());
    &rest[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parens_skip_strings() {
        let source = r#"(a ")(" (b "\")") )"#;
        let found: Vec<_> = parens(source).collect();
        assert_eq!(found, [(0, true), (8, true), (16, false), (18, false)]);
    }

    #[test]
    fn comments_run_to_the_end_of_the_line() {
        let source = "(a \";\" ; one \"(\n b) ;two\r\n";
        let found: Vec<_> = comments(source).map(|range| &source[range]).collect();
        assert_eq!(found, ["; one \"(", ";two"]);
    }

    #[test]
    fn malformed_names_the_first_fault() {
        for (source, fault) in [
            (r#"(a (b ";") c)"#, None),
            ("(a (b c)", Some((0, "unclosed `(`"))),
            (") (a)", Some((0, "unmatched `)`"))),
            ("(a) b) (", None),
            (r#""b" (a)"#, Some((0, "text outside the root list"))),
            (r#"(a "b"#, Some((3, "unterminated string"))),
            (
                "(a ; (\n b)",
                Some((3, "`;` starts a comment here, but KiCad reads it as text")),
            ),
        ] {
            assert_eq!(malformed(source), fault, "{source}");
        }
    }

    #[test]
    fn children_are_the_direct_lists_of_the_root() {
        let source = r#"(lib (version 1) "x" (symbol "a" (unit (pin))) (b)) (c (d))"#;
        let found: Vec<_> = children(source)
            .map(|range| (head(source, range.start), &source[range]))
            .collect();
        assert_eq!(
            found,
            [
                ("version", "(version 1)"),
                ("symbol", r#"(symbol "a" (unit (pin)))"#),
                ("b", "(b)"),
            ]
        );
        assert_eq!(head(source, 0), "lib");
        assert_eq!(head("(a)", 0), "a");
        assert_eq!(head("()", 0), "");
    }
}
