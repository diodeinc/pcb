//! Edits to source text by byte span.

use crate::Span;

/// A replacement for `span` of a source text; empty `text` deletes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

impl Edit {
    pub fn delete(span: Span) -> Self {
        Self {
            span,
            text: String::new(),
        }
    }
}

/// `text` with `edits` made. Of two edits that overlap, the first wins.
pub fn apply(text: &str, mut edits: Vec<Edit>) -> String {
    edits.sort_by_key(|edit| edit.span.start);
    let mut out = String::with_capacity(text.len());
    let mut done = 0;
    for Edit { span, text: new } in edits {
        let (mut start, mut end) = (span.start, span.end);
        // A deletion takes the blanks before it, and the line it leaves empty.
        if new.is_empty() {
            start = text[..start].trim_end_matches([' ', '\t']).len();
            let line = text[..start].rfind('\n').map_or(0, |eol| eol + 1);
            let rest = text[end..]
                .find('\n')
                .map_or(text.len(), |eol| end + eol + 1);
            if start == line && text[end..rest].trim().is_empty() {
                end = rest;
            }
        }
        if start < done {
            continue;
        }
        out.push_str(&text[done..start]);
        out.push_str(&new);
        done = end;
    }
    out.push_str(&text[done..]);
    out
}
