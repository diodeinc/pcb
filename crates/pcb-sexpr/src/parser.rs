//! The S-expression scanner shared by [`crate::Doc`] and [`crate::Sexpr`].
//!
//! It reads the source by byte without recursion. A [`Sink`] makes the nodes;
//! the scanner keeps the nodes of open lists on one stack and hands each list
//! its children as a slice of that stack when the list closes.

use crate::ParseError;

/// Makes nodes for [`scan`]. Offsets are byte offsets into the source.
pub(crate) trait Sink<'s> {
    type Node;
    fn symbol(&mut self, start: u32, text: &'s str) -> Self::Node;
    /// `canonical` tells whether formatting the number reproduces `text`.
    fn number(&mut self, start: u32, text: &'s str, number: Number, canonical: bool) -> Self::Node;
    /// `text` is the unescaped string; it is a slice of the source when
    /// `escaped` is false.
    fn string(&mut self, start: u32, end: u32, text: &str, escaped: bool) -> Self::Node;
    /// Make a list from the children `pending[first..]`, removing them.
    fn list(
        &mut self,
        start: u32,
        end: u32,
        pending: &mut Vec<Self::Node>,
        first: usize,
    ) -> Self::Node;
}

#[derive(Clone, Copy)]
pub(crate) enum Number {
    Int(i64),
    F64(f64),
}

/// Scan the top-level nodes of `source`, or only the first when `first_only` is set.
pub(crate) fn scan<'s, S: Sink<'s>>(
    source: &'s str,
    sink: &mut S,
    first_only: bool,
) -> Result<Vec<S::Node>, ParseError> {
    if u32::try_from(source.len()).is_err() {
        return Err(ParseError::InputTooLarge);
    }
    let bytes = source.as_bytes();
    // Nodes of the open lists, then the finished top-level nodes.
    let mut pending = Vec::with_capacity(256);
    // For each open list: where its children start in `pending`, and its offset.
    let mut open: Vec<(usize, u32)> = Vec::with_capacity(32);
    let mut unescaped = String::new();
    let mut pos = 0;
    loop {
        pos = skip_whitespace(bytes, pos);
        let Some(&b) = bytes.get(pos) else {
            break;
        };
        let start = pos as u32;
        let node = match b {
            b'(' => {
                open.push((pending.len(), start));
                pos += 1;
                continue;
            }
            b')' => {
                let (first, start) = open.pop().ok_or(ParseError::UnexpectedChar(')', '('))?;
                pos += 1;
                sink.list(start, pos as u32, &mut pending, first)
            }
            b'"' => {
                let (text, escaped);
                (pos, text, escaped) = string(source, pos + 1, &mut unescaped)?;
                sink.string(start, pos as u32, text, escaped)
            }
            _ => {
                let len = bytes[pos..]
                    .iter()
                    .position(|&b| is_delimiter(b))
                    .unwrap_or(bytes.len() - pos);
                let text = &source[pos..pos + len];
                pos += len;
                match parse_number(text) {
                    None => sink.symbol(start, text),
                    Some((number, canonical)) => sink.number(start, text, number, canonical),
                }
            }
        };
        pending.push(node);
        if first_only && open.is_empty() {
            break;
        }
    }
    if !open.is_empty() {
        return Err(ParseError::UnclosedList);
    }
    Ok(pending)
}

fn skip_whitespace(bytes: &[u8], mut pos: usize) -> usize {
    while let Some(&b) = bytes.get(pos) {
        if b.is_ascii_whitespace() {
            pos += 1;
        } else if b == b';' {
            pos = memchr::memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |i| pos + i + 1);
        } else {
            break;
        }
    }
    pos
}

/// Read the string whose text starts at `pos`. Returns the offset past the
/// closing quote, the text, and whether it had escapes (and so is in `buffer`).
fn string<'a>(
    source: &'a str,
    mut pos: usize,
    buffer: &'a mut String,
) -> Result<(usize, &'a str, bool), ParseError> {
    let bytes = source.as_bytes();
    let run = |pos: usize| {
        memchr::memchr2(b'"', b'\\', &bytes[pos..]).ok_or(ParseError::UnterminatedString)
    };
    let start = pos;
    pos += run(pos)?;
    if bytes[pos] == b'"' {
        return Ok((pos + 1, &source[start..pos], false));
    }
    buffer.clear();
    buffer.push_str(&source[start..pos]);
    while bytes[pos] == b'\\' {
        pos += 1;
        let escaped = source[pos..]
            .chars()
            .next()
            .ok_or(ParseError::UnterminatedString)?;
        pos += escaped.len_utf8();
        buffer.push(match escaped {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            other => other,
        });
        let len = run(pos)?;
        buffer.push_str(&source[pos..pos + len]);
        pos += len;
    }
    Ok((pos + 1, buffer, true))
}

/// Delimiters are ASCII, so scanning bytes never splits a UTF-8 character.
fn is_delimiter(b: u8) -> bool {
    const TABLE: [bool; 256] = {
        let mut table = [false; 256];
        let mut i = 0;
        while i < 256 {
            let b = i as u8;
            table[i] = b.is_ascii_whitespace() || b == b'(' || b == b')';
            i += 1;
        }
        table
    };
    TABLE[b as usize]
}

/// Parse `atom` as `i64` if it is one, else as `f64`, as `str::parse` does.
/// Also returns whether formatting the number reproduces `atom`.
pub(crate) fn parse_number(atom: &str) -> Option<(Number, bool)> {
    const POW10: [f64; 20] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17, 1e18, 1e19,
    ];
    // Fast path for `-?\d*\.?\d*` with at most 19 digits.
    let (negative, digits) = match atom.as_bytes() {
        [b'-', rest @ ..] => (true, rest),
        bytes => (false, bytes),
    };
    let mut mantissa = 0u64;
    let mut count = 0usize;
    let mut int_len = None;
    for &b in digits {
        match b {
            b'0'..=b'9' if count < 19 => {
                mantissa = mantissa * 10 + u64::from(b - b'0');
                count += 1;
            }
            b'.' if int_len.is_none() => int_len = Some(count),
            _ => return parse_number_slow(atom),
        }
    }
    if count == 0 {
        return parse_number_slow(atom);
    }
    // A number is canonical if `i64::to_string` or `formatter::format_float`
    // gives back its text. Only the fast path can see one.
    let leading_zero = digits[0] == b'0';
    let (number, canonical) = match int_len {
        // `i64::MIN` has no positive counterpart, hence the wrapping negation.
        None if mantissa <= i64::MAX as u64 + u64::from(negative) => {
            let int = mantissa as i64;
            let canonical = !(leading_zero && (count > 1 || negative));
            (
                Number::Int(if negative { int.wrapping_neg() } else { int }),
                canonical,
            )
        }
        // With both the mantissa and the power of ten exact in f64, a single
        // division rounds correctly (Clinger's fast path).
        Some(int_len) if mantissa <= 1 << 53 => {
            let decimals = count - int_len;
            let float = mantissa as f64 / POW10[decimals];
            let canonical = decimals > 0
                && digits[digits.len() - 1] != b'0'
                && match (int_len, leading_zero) {
                    (0, _) => false,
                    (1, true) => decimals <= 10,
                    (_, true) => false,
                    (_, false) => int_len + decimals <= 10,
                };
            (
                Number::F64(if negative { -float } else { float }),
                canonical,
            )
        }
        _ => return parse_number_slow(atom),
    };
    Some((number, canonical))
}

/// Numbers with a sign, exponent, more than 19 digits or a name such as
/// `inf`. None of them formats back to its text.
fn parse_number_slow(atom: &str) -> Option<(Number, bool)> {
    // Rule out most symbols before the slower standard parsers.
    let unsigned = atom.strip_prefix(['+', '-']).unwrap_or(atom);
    if !matches!(
        unsigned.as_bytes().first(),
        Some(b'0'..=b'9' | b'.' | b'i' | b'I' | b'n' | b'N')
    ) {
        return None;
    }
    let number = if let Ok(int) = atom.parse() {
        Number::Int(int)
    } else {
        Number::F64(atom.parse().ok()?)
    };
    Some((number, false))
}
