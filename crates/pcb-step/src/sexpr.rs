//! Pull parser for KiCad S-expressions.
//!
//! There is no tree. A caller opens a list, reads the leading atoms it cares
//! about, descends into the child lists it recognises and skips the rest. The
//! whole board is consumed in one pass with nothing allocated per node.

use memchr::memchr;

use crate::Error;

pub(crate) struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    depth: u32,
}

/// Bytes that end a bare atom.
const DELIM: [bool; 256] = {
    let mut t = [false; 256];
    t[b' ' as usize] = true;
    t[b'\t' as usize] = true;
    t[b'\n' as usize] = true;
    t[b'\r' as usize] = true;
    t[b'(' as usize] = true;
    t[b')' as usize] = true;
    t[b'"' as usize] = true;
    t
};

impl<'a> Parser<'a> {
    pub(crate) fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            pos: 0,
            depth: 0,
        }
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if !b.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
    }

    fn bare_atom(&mut self) -> &'a str {
        let start = self.pos;
        while let Some(&b) = self.src.get(self.pos) {
            if DELIM[b as usize] {
                break;
            }
            self.pos += 1;
        }
        // SAFETY: the source is validated UTF-8 and atoms end at ASCII delimiters.
        unsafe { std::str::from_utf8_unchecked(&self.src[start..self.pos]) }
    }

    /// Quoted atom; the returned text keeps its backslash escapes.
    fn quoted_atom(&mut self) -> Result<&'a str, Error> {
        let start = self.pos + 1;
        let mut at = start;
        loop {
            let Some(off) = memchr(b'"', &self.src[at..]) else {
                return Err(Error::Syntax("unterminated string"));
            };
            let end = at + off;
            let backslashes = self.src[start..end]
                .iter()
                .rev()
                .take_while(|&&b| b == b'\\')
                .count();
            if backslashes % 2 == 0 {
                self.pos = end + 1;
                return Ok(unsafe { std::str::from_utf8_unchecked(&self.src[start..end]) });
            }
            at = end + 1;
        }
    }

    fn bar_text(&mut self) -> Result<&'a [u8], Error> {
        let start = self.pos + 1;
        let Some(off) = memchr(b'|', &self.src[start..]) else {
            return Err(Error::Syntax("unterminated bar data"));
        };
        self.pos = start + off + 1;
        Ok(&self.src[start..start + off])
    }

    /// Consume one token of the current list. Returns `None` at the closing
    /// paren, which is consumed.
    fn token(&mut self) -> Result<Option<Tok<'a>>, Error> {
        self.skip_ws();
        let Some(&b) = self.src.get(self.pos) else {
            return if self.depth == 0 {
                Ok(None)
            } else {
                Err(Error::Syntax("unterminated list"))
            };
        };
        match b {
            b'(' => {
                self.pos += 1;
                self.depth += 1;
                self.skip_ws();
                let name = match self.src.get(self.pos) {
                    Some(b'"') => self.quoted_atom()?,
                    _ => self.bare_atom(),
                };
                Ok(Some(Tok::Open(name)))
            }
            b')' => {
                if self.depth == 0 {
                    return Err(Error::Syntax("unbalanced close paren"));
                }
                self.pos += 1;
                self.depth -= 1;
                Ok(None)
            }
            b'"' => {
                self.quoted_atom()?;
                Ok(Some(Tok::Other))
            }
            b'|' => {
                self.bar_text()?;
                Ok(Some(Tok::Other))
            }
            _ => {
                self.bare_atom();
                Ok(Some(Tok::Other))
            }
        }
    }

    /// Advance to the next child list of the current list, skipping atoms.
    /// Returns its name, or `None` once the current list is closed.
    pub(crate) fn open(&mut self) -> Result<Option<&'a str>, Error> {
        loop {
            match self.token()? {
                Some(Tok::Open(name)) => return Ok(Some(name)),
                Some(Tok::Other) => {}
                None => return Ok(None),
            }
        }
    }

    /// Next atom of the current list, without consuming a paren.
    pub(crate) fn atom(&mut self) -> Result<Option<&'a str>, Error> {
        self.skip_ws();
        match self.src.get(self.pos) {
            Some(b'(' | b')') | None => Ok(None),
            Some(b'"') => Ok(Some(self.quoted_atom()?)),
            Some(b'|') => {
                self.bar_text()?;
                Ok(None)
            }
            Some(_) => Ok(Some(self.bare_atom())),
        }
    }

    pub(crate) fn bar(&mut self) -> Result<Option<&'a [u8]>, Error> {
        loop {
            self.skip_ws();
            match self.src.get(self.pos) {
                Some(b'|') => return Ok(Some(self.bar_text()?)),
                Some(b'(' | b')') | None => return Ok(None),
                Some(b'"') => {
                    self.quoted_atom()?;
                }
                Some(_) => {
                    self.bare_atom();
                }
            }
        }
    }

    pub(crate) fn f64(&mut self, what: &'static str) -> Result<f64, Error> {
        self.atom()?
            .and_then(|a| a.parse().ok())
            .ok_or(Error::Number(what))
    }

    pub(crate) fn f64_or(&mut self, default: f64) -> Result<f64, Error> {
        match self.atom()? {
            Some(a) => a.parse().map_err(|_| Error::Number("number")),
            None => Ok(default),
        }
    }

    /// Two numbers, as written by `(xy x y)`, `(at x y ...)` and friends.
    pub(crate) fn xy(&mut self, what: &'static str) -> Result<crate::geom::Vec2, Error> {
        let x = self.f64(what)?;
        let y = self.f64(what)?;
        Ok(crate::geom::Vec2::new(x, y))
    }

    /// Skip the remainder of the current list, including its closing paren.
    pub(crate) fn skip(&mut self) -> Result<(), Error> {
        let target = self.depth - 1;
        while self.depth != target {
            self.token()?;
        }
        Ok(())
    }

    pub(crate) fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.src.len()
    }
}

enum Tok<'a> {
    Open(&'a str),
    Other,
}

/// Resolve KiCad string escapes; the common case has none and borrows.
pub(crate) fn unescape(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\\') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}
