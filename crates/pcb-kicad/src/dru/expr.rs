//! Expressions KiCad's rule compiler rejects, following
//! `common/libeval_compiler/{grammar.lemon,libeval_compiler.cpp}`.
//!
//! Only errors that KiCad 9 through 11 agree on are reported: syntax errors, a
//! function called without an item, and a lone number without units. Item,
//! property and function names change between releases and are not checked.

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Domain {
    Unitless,
    Length,
    Angle,
    Time,
}

const UNITS: [(&str, Domain); 6] = [
    ("mil", Domain::Length),
    ("mm", Domain::Length),
    ("in", Domain::Length),
    ("deg", Domain::Angle),
    ("fs", Domain::Time),
    ("ps", Domain::Time),
];

// Precedences from grammar.lemon, where `||` binds tighter than `&&`.
const COMMA: u8 = 2;
const AND: u8 = 3;
const OR: u8 = 4;
const COMPARE: u8 = 6;
const SUM: u8 = 8;
const PRODUCT: u8 = 9;
const MEMBER: u8 = 10;

// KiCad's parser stack holds 100 entries and does not report overflow. Each
// level here accounts for at most two of them.
const MAX_DEPTH: usize = 24;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Value,
    Unit(Domain),
    Text,
    Name,
    Open,
    Close,
    Not,
    Plus,
    Minus,
    Binary(u8),
}

#[derive(Clone, Copy)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
}

struct Operand<'a> {
    // A function call not yet attached to an item by `.`.
    call: Option<&'a str>,
    domain: Domain,
}

const PLAIN: Operand<'static> = Operand {
    call: None,
    domain: Domain::Unitless,
};

struct Parser<'a> {
    source: &'a str,
    position: usize,
    peeked: Option<Token<'a>>,
    units: bool,
    after_value: bool,
    // Numeric literals, plus the implicit zero KiCad compiles for a unary minus.
    numbers: usize,
    bare_number: Option<&'a str>,
    bare_call: Option<&'a str>,
    arithmetic: bool,
    // KiCad's behavior from here on is not known; nothing may be reported.
    unsure: bool,
}

impl<'a> Parser<'a> {
    // Tokens are read on demand, as KiCad does, so that an error is only
    // reported when KiCad would have reached it.
    fn peek(&mut self) -> Result<Option<Token<'a>>> {
        if self.peeked.is_none() {
            self.peeked = self.lex()?;
        }
        Ok(self.peeked)
    }

    fn next(&mut self) -> Result<Option<Token<'a>>> {
        let token = self.peek()?;
        self.peeked = None;
        Ok(token)
    }

    fn lex(&mut self) -> Result<Option<Token<'a>>> {
        let bytes = self.source.as_bytes();
        while bytes.get(self.position) == Some(&b' ') {
            self.position += 1;
        }
        let start = self.position;
        let Some(&byte) = bytes.get(start) else {
            return Ok(None);
        };
        let rest = &self.source[start..];
        let after_value = std::mem::take(&mut self.after_value);
        let unit = |&&(name, _): &&(&str, Domain)| {
            rest.starts_with(name)
                && !bytes
                    .get(start + name.len())
                    .is_some_and(u8::is_ascii_alphanumeric)
        };
        let (kind, len) = if byte.is_ascii_digit() {
            // One decimal separator, either '.' or ','.
            let mut len = 1;
            let mut separator = false;
            while let Some(&next) = bytes.get(start + len) {
                let is_separator = matches!(next, b'.' | b',');
                if !(next.is_ascii_digit() || is_separator && !separator) {
                    break;
                }
                separator |= is_separator;
                len += 1;
            }
            self.after_value = true;
            (Kind::Value, len)
        } else if let Some(&(name, domain)) = UNITS
            .iter()
            .find(unit)
            // A unitless constraint has no unit names, so this is an identifier
            // unless it follows a number, where either reading is a syntax error.
            .filter(|_| self.units || after_value)
        {
            (Kind::Unit(domain), name.len())
        } else if byte == b'\'' {
            // An unterminated string runs to the end of the expression.
            let mut len = 1;
            while let Some(&next) = bytes.get(start + len) {
                if next == b'\'' {
                    break;
                }
                if next == b'\\' && bytes.get(start + len + 1) == Some(&b'\'') {
                    len += 1;
                }
                len += 1;
            }
            (Kind::Text, (len + 1).min(rest.len()))
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            let len = rest
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count();
            (Kind::Name, len)
        } else {
            // A two-character operator is not recognized when its last
            // character repeats, as in "===".
            let pair = bytes.get(start + 1).filter(|next| {
                matches!(
                    (byte, **next),
                    (b'=' | b'!' | b'<' | b'>', b'=') | (b'&', b'&') | (b'|', b'|')
                ) && bytes.get(start + 2) != Some(*next)
            });
            match (byte, pair) {
                (b'&', Some(_)) => (Kind::Binary(AND), 2),
                (b'|', Some(_)) => (Kind::Binary(OR), 2),
                (_, Some(_)) => (Kind::Binary(COMPARE), 2),
                (b'<' | b'>', None) => (Kind::Binary(COMPARE), 1),
                (b',', None) => (Kind::Binary(COMMA), 1),
                (b'*' | b'/', None) => (Kind::Binary(PRODUCT), 1),
                (b'.', None) => (Kind::Binary(MEMBER), 1),
                (b'+', None) => (Kind::Plus, 1),
                (b'-', None) => (Kind::Minus, 1),
                (b'!', None) => (Kind::Not, 1),
                (b'(', None) => (Kind::Open, 1),
                (b')', None) => (Kind::Close, 1),
                _ => bail!("unexpected character {:?}", byte as char),
            }
        };
        self.position = start + len;
        Ok(Some(Token {
            kind,
            text: &rest[..len],
        }))
    }

    fn eat(&mut self, kind: Kind) -> Result<bool> {
        let found = self.peek()?.is_some_and(|token| token.kind == kind);
        if found {
            self.peeked = None;
        }
        Ok(found)
    }

    fn unexpected<T>(&self, token: Option<Token<'a>>) -> Result<T> {
        match token {
            Some(token) => bail!("unexpected {:?}", token.text),
            None => bail!("unexpected end"),
        }
    }

    fn close(&mut self) -> Result<()> {
        if self.eat(Kind::Close)? {
            return Ok(());
        }
        self.unexpected(self.peeked)
    }

    // Any use other than as the right side of `.` leaves a call without an item.
    fn used(&mut self, operand: &Operand<'a>) {
        self.bare_call = self.bare_call.or(operand.call);
    }

    fn primary(&mut self, depth: usize) -> Result<Operand<'a>> {
        let token = self.next()?;
        let Some(Token { kind, text }) = token else {
            return self.unexpected(token);
        };
        self.arithmetic &= matches!(kind, Kind::Value | Kind::Open | Kind::Plus | Kind::Minus);
        Ok(match kind {
            Kind::Value => {
                self.numbers += 1;
                match self.peek()? {
                    Some(Token {
                        kind: Kind::Unit(domain),
                        ..
                    }) => {
                        if !self.units {
                            bail!("unexpected units in a unitless constraint");
                        }
                        self.peeked = None;
                        Operand { call: None, domain }
                    }
                    _ => {
                        self.bare_number = Some(text);
                        PLAIN
                    }
                }
            }
            Kind::Text => PLAIN,
            Kind::Name => {
                if !self.eat(Kind::Open)? {
                    return Ok(PLAIN);
                }
                if !self.eat(Kind::Close)? {
                    let arguments = self.expression(0, depth + 1)?;
                    self.used(&arguments);
                    self.close()?;
                }
                Operand {
                    call: Some(text),
                    domain: Domain::Unitless,
                }
            }
            // Parentheses and unary plus leave their operand as it is.
            Kind::Open => {
                let inner = self.expression(0, depth + 1)?;
                self.close()?;
                inner
            }
            Kind::Plus => self.expression(PRODUCT, depth + 1)?,
            Kind::Minus => {
                self.numbers += 1;
                let operand = self.expression(PRODUCT, depth + 1)?;
                self.used(&operand);
                Operand {
                    call: None,
                    domain: operand.domain,
                }
            }
            Kind::Not => {
                let operand = self.expression(SUM, depth + 1)?;
                self.used(&operand);
                PLAIN
            }
            _ => return self.unexpected(token),
        })
    }

    fn expression(&mut self, min: u8, depth: usize) -> Result<Operand<'a>> {
        if depth > MAX_DEPTH {
            self.unsure = true;
            bail!("nesting too deep");
        }
        let mut left = self.primary(depth)?;
        while let Some(token) = self.peek()? {
            let precedence = match token.kind {
                Kind::Binary(precedence) => precedence,
                Kind::Plus | Kind::Minus => SUM,
                _ => break,
            };
            if precedence < min {
                break;
            }
            self.peeked = None;
            let right = self.expression(precedence + 1, depth + 1)?;
            self.used(&left);
            if precedence != MEMBER {
                self.used(&right);
            }
            self.arithmetic &= matches!(precedence, SUM | PRODUCT);
            if precedence == COMPARE && self.eat(Kind::Binary(COMPARE))? {
                bail!("comparisons cannot be chained");
            }
            // KiCad's unit propagation, not dimensional analysis: a right
            // operand with units wins, otherwise the left operand's are kept.
            left = Operand {
                call: None,
                domain: if right.domain == Domain::Unitless {
                    left.domain
                } else {
                    right.domain
                },
            };
        }
        Ok(left)
    }
}

struct Expression<'a> {
    // A lone number written without units.
    missing_units: Option<&'a str>,
    // Unit domain of pure arithmetic.
    domain: Option<Domain>,
}

// `None` when KiCad's verdict cannot be predicted.
fn parse(source: &str, units: bool) -> Result<Option<Expression<'_>>> {
    // KiCad hands the text to its compiler as a C string in the process
    // locale, which can truncate or drop anything else.
    if !source.is_ascii() || source.contains('\0') {
        return Ok(None);
    }
    let mut parser = Parser {
        source,
        position: 0,
        peeked: None,
        units,
        after_value: false,
        numbers: 0,
        bare_number: None,
        bare_call: None,
        arithmetic: true,
        unsure: false,
    };
    let parsed = (|| {
        if parser.peek()?.is_none() {
            return Ok(None);
        }
        let root = parser.expression(0, 0)?;
        parser.used(&root);
        match parser.next()? {
            None => Ok(Some(root)),
            token => parser.unexpected(token),
        }
    })();
    let root = match parsed {
        Err(_) if parser.unsure => return Ok(None),
        Err(error) => bail!("{error} in {source:?}"),
        Ok(root) => root,
    };
    if let Some(name) = parser.bare_call {
        bail!("{name}() needs an item, as in A.{name}()");
    }
    Ok(Some(Expression {
        missing_units: parser.bare_number.filter(|_| parser.numbers == 1),
        domain: root.filter(|_| parser.arithmetic).map(|root| root.domain),
    }))
}

const MISSING_UNITS: &str = "use mm, in, mil, deg, fs, or ps";

pub(super) fn condition(source: &str) -> Result<()> {
    if let Some(Expression {
        missing_units: Some(number),
        ..
    }) = parse(source, true)?
    {
        bail!("missing units for {number:?}; {MISSING_UNITS}");
    }
    Ok(())
}

/// The unit domain of a constraint value, or `None` when the value is not
/// plain arithmetic.
pub(super) fn arithmetic(source: &str, unitless: bool) -> Result<Option<Domain>> {
    if source.is_empty() {
        bail!("missing value");
    }
    let Some(expression) = parse(source, !unitless)? else {
        return Ok(None);
    };
    if !unitless && let Some(number) = expression.missing_units {
        bail!("missing units for {number:?}; {MISSING_UNITS}");
    }
    Ok(expression.domain)
}
