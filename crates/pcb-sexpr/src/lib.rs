//! A simple S-expression parser that preserves the exact format of atoms

use std::fmt;

/// An S-expression value
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Sexpr {
    /// A symbol - unquoted identifier
    Symbol(String),
    /// A string - quoted text
    String(String),
    /// A list of S-expressions
    List(Vec<Sexpr>),
}

impl Sexpr {
    /// Create a symbol (unquoted atom)
    pub fn symbol(s: impl Into<String>) -> Self {
        Sexpr::Symbol(s.into())
    }

    /// Create a string (quoted atom)
    pub fn string(s: impl Into<String>) -> Self {
        Sexpr::String(s.into())
    }

    /// Create an atom - for backwards compatibility
    /// This creates a symbol by default
    pub fn atom(s: impl Into<String>) -> Self {
        Sexpr::Symbol(s.into())
    }

    /// Create a list from a vector of S-expressions
    pub fn list(items: Vec<Sexpr>) -> Self {
        Sexpr::List(items)
    }

    /// Check if this is an atom (symbol or string)
    pub fn is_atom(&self) -> bool {
        matches!(self, Sexpr::Symbol(_) | Sexpr::String(_))
    }

    /// Check if this is a list
    pub fn is_list(&self) -> bool {
        matches!(self, Sexpr::List(_))
    }

    /// Get the atom value if this is an atom (symbol or string)
    pub fn as_atom(&self) -> Option<&str> {
        match self {
            Sexpr::Symbol(s) | Sexpr::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get the list items if this is a list
    pub fn as_list(&self) -> Option<&[Sexpr]> {
        match self {
            Sexpr::List(items) => Some(items),
            _ => None,
        }
    }

    /// Get mutable access to list items if this is a list
    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Sexpr>> {
        match self {
            Sexpr::List(items) => Some(items),
            _ => None,
        }
    }
}

/// Parser for S-expressions
pub struct Parser<'a> {
    input: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    current_pos: usize,
}

impl<'a> Parser<'a> {
    /// Create a new parser for the given input
    pub fn new(input: &'a str) -> Self {
        Parser {
            input,
            chars: input.char_indices().peekable(),
            current_pos: 0,
        }
    }

    /// Parse the input and return the S-expression
    pub fn parse(&mut self) -> Result<Sexpr, ParseError> {
        self.skip_whitespace();
        if self.is_at_end() {
            return Err(ParseError::UnexpectedEof);
        }

        if self.peek_char() == Some('(') {
            self.parse_list()
        } else {
            self.parse_atom()
        }
    }

    /// Parse multiple S-expressions from the input
    pub fn parse_all(&mut self) -> Result<Vec<Sexpr>, ParseError> {
        let mut results = Vec::new();

        loop {
            self.skip_whitespace();
            if self.is_at_end() {
                break;
            }
            results.push(self.parse()?);
        }

        Ok(results)
    }

    fn parse_list(&mut self) -> Result<Sexpr, ParseError> {
        let start_pos = self.current_pos;
        self.expect('(')?;
        let mut items = Vec::new();
        let mut item_count = 0;

        loop {
            self.skip_whitespace();

            if self.is_at_end() {
                return Err(ParseError::UnclosedList);
            }

            if self.peek_char() == Some(')') {
                self.advance();
                break;
            }

            items.push(self.parse()?);
            item_count += 1;

            // Log progress for large lists
            if item_count % 1000 == 0 {
                log::trace!("Parsed {item_count} items in list at position {start_pos}");
            }
        }

        Ok(Sexpr::List(items))
    }

    fn parse_atom(&mut self) -> Result<Sexpr, ParseError> {
        self.skip_whitespace();

        if self.peek_char() == Some('"') {
            // Parse quoted string
            self.parse_string()
        } else {
            // Parse unquoted atom
            let start = self.current_pos;
            while let Some(ch) = self.peek_char() {
                if ch.is_whitespace() || ch == '(' || ch == ')' {
                    break;
                }
                self.advance();
            }

            if self.current_pos == start {
                return Err(ParseError::EmptyAtom);
            }

            Ok(Sexpr::Symbol(
                self.input[start..self.current_pos].to_string(),
            ))
        }
    }

    fn parse_string(&mut self) -> Result<Sexpr, ParseError> {
        self.expect('"')?;
        let mut result = String::new();

        loop {
            match self.peek_char() {
                None => return Err(ParseError::UnterminatedString),
                Some('"') => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    self.advance();
                    match self.peek_char() {
                        Some('n') => {
                            result.push('\n');
                            self.advance();
                        }
                        Some('r') => {
                            result.push('\r');
                            self.advance();
                        }
                        Some('t') => {
                            result.push('\t');
                            self.advance();
                        }
                        Some('\\') => {
                            result.push('\\');
                            self.advance();
                        }
                        Some('"') => {
                            result.push('"');
                            self.advance();
                        }
                        Some(ch) => {
                            result.push(ch);
                            self.advance();
                        }
                        None => return Err(ParseError::UnterminatedString),
                    }
                }
                Some(ch) => {
                    result.push(ch);
                    self.advance();
                }
            }
        }

        Ok(Sexpr::String(result))
    }

    fn skip_whitespace(&mut self) {
        let start_pos = self.current_pos;
        let mut skipped = 0;

        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                self.advance();
                skipped += 1;
            } else if ch == ';' {
                // Skip comment until end of line
                self.advance();
                while let Some(ch) = self.peek_char() {
                    self.advance();
                    if ch == '\n' {
                        break;
                    }
                }
                skipped += 1;
            } else {
                break;
            }

            // Log progress for large whitespace sections
            if skipped % 10000 == 0 && skipped > 0 {
                log::trace!(
                    "Skipped {skipped} whitespace/comment chars starting at position {start_pos}"
                );
            }
        }
    }

    fn peek_char(&mut self) -> Option<char> {
        self.chars.peek().map(|(_, ch)| *ch)
    }

    fn advance(&mut self) {
        if let Some((pos, _)) = self.chars.next() {
            self.current_pos = pos + 1; // pos is the start of the char, we want the position after it
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), ParseError> {
        match self.peek_char() {
            Some(ch) if ch == expected => {
                self.advance();
                Ok(())
            }
            Some(ch) => Err(ParseError::UnexpectedChar(ch, expected)),
            None => Err(ParseError::UnexpectedEof),
        }
    }

    fn is_at_end(&mut self) -> bool {
        self.chars.peek().is_none()
    }
}

/// Parse a string into an S-expression
pub fn parse(input: &str) -> Result<Sexpr, ParseError> {
    log::trace!("Parsing S-expression from {} bytes of input", input.len());
    let result = Parser::new(input).parse();
    match &result {
        Ok(_) => log::trace!("Successfully parsed S-expression"),
        Err(e) => log::trace!("Failed to parse S-expression: {e:?}"),
    }
    result
}

/// Parse a string into multiple S-expressions
pub fn parse_all(input: &str) -> Result<Vec<Sexpr>, ParseError> {
    log::trace!(
        "Parsing multiple S-expressions from {} bytes of input",
        input.len()
    );
    let result = Parser::new(input).parse_all();
    match &result {
        Ok(exprs) => log::trace!("Successfully parsed {} S-expressions", exprs.len()),
        Err(e) => log::trace!("Failed to parse S-expressions: {e:?}"),
    }
    result
}

/// Errors that can occur during parsing
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    UnexpectedEof,
    UnexpectedChar(char, char),
    UnclosedList,
    UnterminatedString,
    EmptyAtom,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::UnexpectedEof => write!(f, "Unexpected end of input"),
            ParseError::UnexpectedChar(found, expected) => {
                write!(f, "Expected '{expected}', found '{found}'")
            }
            ParseError::UnclosedList => write!(f, "Unclosed list"),
            ParseError::UnterminatedString => write!(f, "Unterminated string"),
            ParseError::EmptyAtom => write!(f, "Empty atom"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Format an S-expression with proper indentation
pub fn format_sexpr(sexpr: &Sexpr, indent_level: usize) -> String {
    format_sexpr_inner(sexpr, indent_level, true)
}

/// Internal formatting function with control over whether to add initial indent
fn format_sexpr_inner(sexpr: &Sexpr, indent_level: usize, add_indent: bool) -> String {
    let indent = if add_indent {
        "  ".repeat(indent_level)
    } else {
        String::new()
    };

    match sexpr {
        Sexpr::Symbol(s) => {
            // Symbols are never quoted
            format!("{indent}{s}")
        }
        Sexpr::String(s) => {
            // Strings are always quoted
            format!("{}\"{}\"", indent, escape_string(s))
        }
        Sexpr::List(items) => {
            if items.is_empty() {
                return format!("{indent}()");
            }

            // Check if this is a simple list that should be on one line
            let is_simple = is_simple_list(items);

            if is_simple {
                let mut result = format!("{indent}(");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        result.push(' ');
                    }
                    result.push_str(&format_sexpr_inner(item, 0, false));
                }
                result.push(')');
                result
            } else {
                let mut result = format!("{indent}(");

                // First item on the same line
                if let Some(first) = items.first() {
                    result.push_str(&format_sexpr_inner(first, 0, false));
                }

                // Rest of items on new lines
                for item in items.iter().skip(1) {
                    result.push('\n');
                    result.push_str(&format_sexpr_inner(item, indent_level + 1, true));
                }

                result.push('\n');
                result.push_str(&indent);
                result.push(')');
                result
            }
        }
    }
}

fn escape_string(s: &str) -> String {
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

fn is_simple_list(items: &[Sexpr]) -> bool {
    // Check if this is a known simple form
    if let Some(Sexpr::Symbol(first)) = items.first() {
        match first.as_str() {
            // These forms should always be on one line
            "at" | "xy" | "size" | "diameter" | "width" | "type" | "shape"
            | "fields_autoplaced" => return true,
            // Color with exactly 5 items (color r g b a)
            "color" if items.len() == 5 => return true,
            // Font with exactly 2 items (font (size ...))
            "font" if items.len() == 2 => return true,
            // Justify with 2-3 items (justify left) or (justify left top)
            "justify" if items.len() <= 3 => return true,
            // lib_id, uuid, reference, unit, page with 2 items
            "lib_id" | "uuid" | "reference" | "unit" | "page" | "path" | "title" | "date"
            | "paper"
                if items.len() == 2 =>
            {
                return true
            }
            // Boolean flags
            "in_bom" | "on_board" | "dnp" | "hide" if items.len() <= 2 => return true,
            _ => {}
        }
    }

    // Otherwise, simple if very short and all atoms
    items.len() <= 2
        && items
            .iter()
            .all(|item| matches!(item, Sexpr::Symbol(_) | Sexpr::String(_)))
}

impl fmt::Display for Sexpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_sexpr(self, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_atom() {
        assert_eq!(parse("hello").unwrap(), Sexpr::Symbol("hello".to_string()));
        assert_eq!(parse("123").unwrap(), Sexpr::Symbol("123".to_string()));
        assert_eq!(parse("3.14").unwrap(), Sexpr::Symbol("3.14".to_string()));
        assert_eq!(
            parse("symbol-with-dashes").unwrap(),
            Sexpr::Symbol("symbol-with-dashes".to_string())
        );
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(
            parse("\"hello world\"").unwrap(),
            Sexpr::String("hello world".to_string())
        );
        assert_eq!(
            parse("\"with\\\"quotes\\\"\"").unwrap(),
            Sexpr::String("with\"quotes\"".to_string())
        );
        assert_eq!(
            parse("\"line\\nbreak\"").unwrap(),
            Sexpr::String("line\nbreak".to_string())
        );
    }

    #[test]
    fn test_parse_list() {
        assert_eq!(parse("()").unwrap(), Sexpr::List(vec![]));
        assert_eq!(
            parse("(a b c)").unwrap(),
            Sexpr::List(vec![
                Sexpr::Symbol("a".to_string()),
                Sexpr::Symbol("b".to_string()),
                Sexpr::Symbol("c".to_string()),
            ])
        );
    }

    #[test]
    fn test_parse_nested() {
        let input = "(define (square x) (* x x))";
        let expected = Sexpr::List(vec![
            Sexpr::Symbol("define".to_string()),
            Sexpr::List(vec![
                Sexpr::Symbol("square".to_string()),
                Sexpr::Symbol("x".to_string()),
            ]),
            Sexpr::List(vec![
                Sexpr::Symbol("*".to_string()),
                Sexpr::Symbol("x".to_string()),
                Sexpr::Symbol("x".to_string()),
            ]),
        ]);
        assert_eq!(parse(input).unwrap(), expected);
    }

    #[test]
    fn test_parse_kicad_pin() {
        let input = r#"(pin passive line (at 0 0 0) (length 2.54) (name "1") (number "1"))"#;
        let result = parse(input).unwrap();

        // Verify that pin numbers remain as strings
        if let Sexpr::List(items) = result {
            assert_eq!(items[0], Sexpr::Symbol("pin".to_string()));

            // Find the number field
            for item in &items {
                if let Sexpr::List(sub_items) = item {
                    if sub_items.len() >= 2 && sub_items[0] == Sexpr::Symbol("number".to_string()) {
                        assert_eq!(sub_items[1], Sexpr::String("1".to_string()));
                    }
                }
            }
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_format_simple() {
        let sexpr = Sexpr::List(vec![
            Sexpr::Symbol("at".to_string()),
            Sexpr::Symbol("10".to_string()),
            Sexpr::Symbol("20".to_string()),
        ]);
        assert_eq!(format_sexpr(&sexpr, 0), "(at 10 20)");
    }

    #[test]
    fn test_format_nested() {
        let sexpr = Sexpr::List(vec![
            Sexpr::Symbol("symbol".to_string()),
            Sexpr::List(vec![
                Sexpr::Symbol("lib_id".to_string()),
                Sexpr::Symbol("Device:R".to_string()),
            ]),
            Sexpr::List(vec![
                Sexpr::Symbol("at".to_string()),
                Sexpr::Symbol("50".to_string()),
                Sexpr::Symbol("50".to_string()),
                Sexpr::Symbol("0".to_string()),
            ]),
        ]);

        let formatted = format_sexpr(&sexpr, 0);
        assert!(formatted.contains("(symbol"));
        assert!(formatted.contains("(lib_id Device:R)"));
        assert!(formatted.contains("(at 50 50 0)"));
    }

    #[test]
    fn test_parse_with_comments() {
        let input = r#"
        ; This is a comment
        (test ; inline comment
          value)
        "#;
        let result = parse(input).unwrap();
        assert_eq!(
            result,
            Sexpr::List(vec![
                Sexpr::Symbol("test".to_string()),
                Sexpr::Symbol("value".to_string()),
            ])
        );
    }

    #[test]
    fn test_roundtrip() {
        let inputs = vec![
            "(simple list)",
            "(nested (list with) (multiple levels))",
            r#"(with "quoted string" and atoms)"#,
            "(pin passive line (at 0 0 0) (length 2.54) (name \"1\") (number \"1\"))",
        ];

        for input in inputs {
            let parsed = parse(input).unwrap();
            let formatted = format_sexpr(&parsed, 0);
            let reparsed = parse(&formatted).unwrap();
            assert_eq!(parsed, reparsed, "Roundtrip failed for: {input}");
        }
    }
}
