//! Structural scans of S-expression text that never build a tree, for
//! callers that need a little from files dominated by large atoms.

use memchr::{memchr2, memchr3};

/// The parentheses of `source` in order, as `(byte offset, is opening)`.
/// String contents are skipped; `;` comments are not recognized.
pub fn parens(source: &str) -> impl Iterator<Item = (usize, bool)> + '_ {
    let bytes = source.as_bytes();
    let mut pos = 0;
    std::iter::from_fn(move || {
        loop {
            let at = pos + memchr3(b'(', b')', b'"', bytes.get(pos..)?)?;
            pos = at + 1;
            match bytes[at] {
                b'"' => pos = string_end(bytes, pos)?,
                paren => return Some((at, paren == b'(')),
            }
        }
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

/// Offset of a parenthesis that has no partner: the first stray `)`, else the
/// outermost `(` left open.
pub fn unbalanced_paren(source: &str) -> Option<usize> {
    let mut open = Vec::new();
    for (offset, opening) in parens(source) {
        if opening {
            open.push(offset);
        } else if open.pop().is_none() {
            return Some(offset);
        }
    }
    open.first().copied()
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
    fn unbalanced_paren_names_the_stray_one() {
        assert_eq!(unbalanced_paren("(a (b) c)"), None);
        assert_eq!(unbalanced_paren("(a (b c)"), Some(0));
        assert_eq!(unbalanced_paren("(a) b)"), Some(5));
    }
}
