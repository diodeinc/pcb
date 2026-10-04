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

/// Why `source` is not one well-formed list, with the offset of the fault.
///
/// A `;` outside a string is a fault: this crate's parser reads it as the
/// start of a comment, but KiCad has no comments and fails on what follows.
pub fn malformed(source: &str) -> Option<(usize, &'static str)> {
    let bytes = source.as_bytes();
    let comment = |gap: Range<usize>| {
        let at = gap.start + memchr(b';', &bytes[gap])?;
        Some((at, "`;` starts a comment, which KiCad does not have"))
    };
    let (mut depth, mut root, mut scanned) = (0usize, None, 0);
    for (range, token) in tokens(source) {
        if let Some(fault) = comment(scanned..range.start) {
            return Some(fault);
        }
        scanned = range.end;
        match token {
            Token::Open if depth == 0 && root.is_some() => {
                return Some((range.start, "a second top-level form"));
            }
            Token::Open => {
                root.get_or_insert(range.start);
                depth += 1;
            }
            Token::Close if depth == 0 => return Some((range.start, "unmatched `)`")),
            Token::Close => depth -= 1,
            Token::String => {}
            Token::UnterminatedString => return Some((range.start, "unterminated string")),
        }
    }
    comment(scanned..bytes.len()).or_else(|| Some((root.filter(|_| depth > 0)?, "unclosed `(`")))
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
    fn malformed_names_the_first_fault() {
        for (source, fault) in [
            (r#"(a (b ";") c)"#, None),
            ("(a (b c)", Some((0, "unclosed `(`"))),
            ("(a) b)", Some((5, "unmatched `)`"))),
            ("(a) (b)", Some((4, "a second top-level form"))),
            (r#"(a) "b"#, Some((4, "unterminated string"))),
            (
                "(a ; (\n b)",
                Some((3, "`;` starts a comment, which KiCad does not have")),
            ),
        ] {
            assert_eq!(malformed(source), fault, "{source}");
        }
    }
}
