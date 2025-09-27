use std::fmt;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Unexpected end of input")]
    UnexpectedEof,

    #[error("Expected '(' but found '{0}'")]
    ExpectedOpenParen(char),

    #[error("Expected ')' but found end of input")]
    UnexpectedEofInList,

    #[error("Invalid number format: {0}")]
    InvalidNumber(String),

    #[error("Unterminated string")]
    UnterminatedString,

    #[error("Invalid escape sequence: \\{0}")]
    InvalidEscape(char),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SExpr {
    Sym(String),
    Str(String),
    Int(i64),
    F64(f64),
    List(Vec<SExpr>),
}

pub fn sym<S: Into<String>>(s: S) -> SExpr {
    SExpr::Sym(s.into())
}

pub fn str_lit<S: Into<String>>(s: S) -> SExpr {
    SExpr::Str(s.into())
}

pub fn kv<K: Into<String>, V: Into<SExpr>>(k: K, v: V) -> SExpr {
    SExpr::List(vec![sym(k), v.into()])
}

pub struct ListBuilder {
    items: Vec<SExpr>,
}

impl Default for ListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ListBuilder {
    pub fn node<N: Into<SExpr>>(name: N) -> Self {
        Self {
            items: vec![name.into()],
        }
    }

    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn push<V: Into<SExpr>>(&mut self, v: V) -> &mut Self {
        self.items.push(v.into());
        self
    }

    pub fn push_if<V: Into<SExpr>>(&mut self, cond: bool, v: V) -> &mut Self {
        if cond {
            self.items.push(v.into());
        }
        self
    }

    pub fn extend<I, V>(&mut self, iter: I) -> &mut Self
    where
        I: IntoIterator<Item = V>,
        V: Into<SExpr>,
    {
        self.items.extend(iter.into_iter().map(Into::into));
        self
    }

    pub fn build(self) -> SExpr {
        SExpr::List(self.items)
    }
}

// Convenient From implementations
impl From<&str> for SExpr {
    fn from(s: &str) -> Self {
        sym(s)
    }
}
impl From<String> for SExpr {
    fn from(s: String) -> Self {
        sym(s)
    }
}
impl From<i64> for SExpr {
    fn from(n: i64) -> Self {
        SExpr::Int(n)
    }
}
impl From<i32> for SExpr {
    fn from(n: i32) -> Self {
        SExpr::Int(n as i64)
    }
}
impl From<u32> for SExpr {
    fn from(n: u32) -> Self {
        SExpr::Int(n as i64)
    }
}
impl From<usize> for SExpr {
    fn from(n: usize) -> Self {
        SExpr::Int(n as i64)
    }
}
impl From<f64> for SExpr {
    fn from(n: f64) -> Self {
        SExpr::F64(n)
    }
}
impl From<f32> for SExpr {
    fn from(n: f32) -> Self {
        SExpr::F64(n as f64)
    }
}
impl From<bool> for SExpr {
    fn from(b: bool) -> Self {
        sym(if b { "yes" } else { "no" })
    }
} // KiCad-friendly

// S-expression parser
pub struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<SExpr, ParseError> {
        self.skip_whitespace();
        if self.pos >= self.input.len() {
            return Err(ParseError::UnexpectedEof);
        }

        match self.current_char() {
            '(' => self.parse_list(),
            '"' => self.parse_string(),
            c if c.is_ascii_digit() || c == '-' || c == '+' => self.parse_number(),
            _ => self.parse_symbol(),
        }
    }

    fn current_char(&self) -> char {
        self.input.chars().nth(self.pos).unwrap_or('\0')
    }

    fn advance(&mut self) {
        if self.pos < self.input.len() {
            self.pos += 1;
        }
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len() && self.current_char().is_whitespace() {
            self.advance();
        }
    }

    fn parse_list(&mut self) -> Result<SExpr, ParseError> {
        // Skip opening '('
        self.advance();
        let mut items = Vec::new();

        loop {
            self.skip_whitespace();
            if self.pos >= self.input.len() {
                return Err(ParseError::UnexpectedEofInList);
            }

            if self.current_char() == ')' {
                self.advance();
                break;
            }

            items.push(self.parse()?);
        }

        Ok(SExpr::List(items))
    }

    fn parse_string(&mut self) -> Result<SExpr, ParseError> {
        // Skip opening quote
        self.advance();
        let mut result = String::new();

        while self.pos < self.input.len() {
            match self.current_char() {
                '"' => {
                    self.advance();
                    return Ok(SExpr::Str(result));
                }
                '\\' => {
                    self.advance();
                    if self.pos >= self.input.len() {
                        return Err(ParseError::UnterminatedString);
                    }
                    match self.current_char() {
                        '"' => result.push('"'),
                        '\\' => result.push('\\'),
                        'n' => result.push('\n'),
                        'r' => result.push('\r'),
                        't' => result.push('\t'),
                        c => return Err(ParseError::InvalidEscape(c)),
                    }
                    self.advance();
                }
                c => {
                    result.push(c);
                    self.advance();
                }
            }
        }

        Err(ParseError::UnterminatedString)
    }

    fn parse_number(&mut self) -> Result<SExpr, ParseError> {
        let start = self.pos;

        // Handle sign
        if self.current_char() == '-' || self.current_char() == '+' {
            self.advance();
        }

        // Parse digits
        while self.pos < self.input.len()
            && (self.current_char().is_ascii_digit() || self.current_char() == '.')
        {
            self.advance();
        }

        let number_str = &self.input[start..self.pos];

        // Try to parse as int first, then float
        if let Ok(int_val) = number_str.parse::<i64>() {
            Ok(SExpr::Int(int_val))
        } else if let Ok(float_val) = number_str.parse::<f64>() {
            Ok(SExpr::F64(float_val))
        } else {
            Err(ParseError::InvalidNumber(number_str.to_string()))
        }
    }

    fn parse_symbol(&mut self) -> Result<SExpr, ParseError> {
        let start = self.pos;

        while self.pos < self.input.len() {
            let c = self.current_char();
            if c.is_whitespace() || c == '(' || c == ')' || c == '"' {
                break;
            }
            self.advance();
        }

        let symbol = self.input[start..self.pos].to_string();
        Ok(SExpr::Sym(symbol))
    }
}

pub fn parse(input: &str) -> Result<SExpr, ParseError> {
    let mut parser = Parser::new(input);
    parser.parse()
}

impl SExpr {
    /// Get the symbol name if this is a symbol
    pub fn as_sym(&self) -> Option<&str> {
        match self {
            SExpr::Sym(s) => Some(s),
            _ => None,
        }
    }

    /// Get the string content if this is a string literal
    pub fn as_str(&self) -> Option<&str> {
        match self {
            SExpr::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Get the integer value if this is an integer
    pub fn as_int(&self) -> Option<i64> {
        match self {
            SExpr::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Get the float value if this is a float
    pub fn as_float(&self) -> Option<f64> {
        match self {
            SExpr::F64(f) => Some(*f),
            _ => None,
        }
    }

    /// Get the list items if this is a list
    pub fn as_list(&self) -> Option<&[SExpr]> {
        match self {
            SExpr::List(items) => Some(items),
            _ => None,
        }
    }

    /// Find a child list with the given name (first element)
    pub fn find_list(&self, name: &str) -> Option<&[SExpr]> {
        if let Some(items) = self.as_list() {
            for item in items {
                if let Some(list_items) = item.as_list() {
                    if let Some(first) = list_items.first() {
                        if first.as_sym() == Some(name) {
                            return Some(list_items);
                        }
                    }
                }
            }
        }
        None
    }

    /// Find all child lists with the given name
    pub fn find_all_lists(&self, name: &str) -> Vec<&[SExpr]> {
        let mut result = Vec::new();
        if let Some(items) = self.as_list() {
            for item in items {
                if let Some(list_items) = item.as_list() {
                    if let Some(first) = list_items.first() {
                        if first.as_sym() == Some(name) {
                            result.push(list_items);
                        }
                    }
                }
            }
        }
        result
    }
}

// Compact format by default
impl fmt::Display for SExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_compact(self, f)
    }
}

pub struct Pretty<'a>(pub &'a SExpr);

impl<'a> fmt::Display for Pretty<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_pretty(self.0, f, 0, "\t") // tabs by default
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn trim_float(mut s: String) -> String {
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s.is_empty() {
        "0".to_string()
    } else {
        s
    }
}

fn write_atom(a: &SExpr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match a {
        SExpr::Sym(s) => write!(f, "{}", s),
        SExpr::Str(s) => write!(f, "\"{}\"", escape_str(s)),
        SExpr::Int(n) => write!(f, "{}", n),
        SExpr::F64(x) => write!(f, "{}", trim_float(format!("{}", x))),
        SExpr::List(_) => unreachable!(),
    }
}

fn write_compact(sexpr: &SExpr, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match sexpr {
        SExpr::List(items) => {
            write!(f, "(")?;
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    write!(f, " ")?;
                }
                match it {
                    SExpr::List(_) => write_compact(it, f)?,
                    _ => write_atom(it, f)?,
                }
            }
            write!(f, ")")
        }
        _ => write_atom(sexpr, f),
    }
}

fn write_pretty(
    sexpr: &SExpr,
    f: &mut fmt::Formatter<'_>,
    depth: usize,
    indent: &str,
) -> fmt::Result {
    match sexpr {
        SExpr::List(items) => {
            write!(f, "(")?;
            if items.is_empty() {
                return write!(f, ")");
            }
            // Heuristic: if any child is a list, split lines; otherwise single-line
            let multiline = items.iter().any(|x| matches!(x, SExpr::List(_)));
            if !multiline {
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write_compact(it, f)?;
                }
                return write!(f, ")");
            }
            // First element inline, rest on new lines
            write_compact(&items[0], f)?;
            for it in &items[1..] {
                writeln!(f)?;
                for _ in 0..=depth {
                    write!(f, "{}", indent)?;
                }
                write_pretty(it, f, depth + 1, indent)?;
            }
            write!(f, ")")
        }
        _ => write_atom(sexpr, f),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_list() {
        let input = r#"(0 "F.Cu" signal)"#;
        let result = parse(input).unwrap();

        let items = result.as_list().unwrap();
        assert_eq!(items[0].as_int(), Some(0));
        assert_eq!(items[1].as_str(), Some("F.Cu"));
        assert_eq!(items[2].as_sym(), Some("signal"));
    }

    #[test]
    fn test_round_trip() {
        let mut builder = ListBuilder::node(sym("test"));
        builder.push(str_lit("hello")).push(42).push(3.14);
        let original = builder.build();

        let generated = format!("{}", original);
        let parsed = parse(&generated).unwrap();

        assert_eq!(original, parsed);
    }
}
