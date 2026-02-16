//! A simple S-expression parser that preserves the exact format of atoms
//! and tracks source spans for each node.
//!
//! # Tree Traversal and Patching
//!
//! This crate provides generic APIs for walking S-expression trees and applying
//! in-place patches to source text:
//!
//! - [`Sexpr::walk`] - Depth-first traversal with ancestor context
//! - [`Sexpr::walk_strings`] - Walk only string literals
//! - [`PatchSet`] - Collect patches and write directly to any `std::io::Write`

pub mod board;
pub mod formatter;
pub mod kicad;

use std::fmt;

/// Find a direct child list `(name ...)` within a list of [`Sexpr`] nodes.
pub fn find_child_list<'a>(items: &'a [Sexpr], name: &str) -> Option<&'a [Sexpr]> {
    for item in items {
        if let Some(list_items) = item.as_list() {
            if list_items.first().and_then(Sexpr::as_sym) == Some(name) {
                return Some(list_items);
            }
        }
    }
    None
}

/// Find all direct child lists `(name ...)` within a list of [`Sexpr`] nodes.
pub fn find_all_child_lists<'a>(items: &'a [Sexpr], name: &str) -> Vec<&'a [Sexpr]> {
    let mut result = Vec::new();
    for item in items {
        if let Some(list_items) = item.as_list() {
            if list_items.first().and_then(Sexpr::as_sym) == Some(name) {
                result.push(list_items);
            }
        }
    }
    result
}

/// Coerce a number atom into f64.
///
/// KiCad S-exprs sometimes encode whole numbers as ints and sometimes as floats.
pub(crate) fn number_as_f64(node: &Sexpr) -> Option<f64> {
    node.as_float().or_else(|| node.as_int().map(|v| v as f64))
}

/// Context provided while walking the S-expression tree.
#[derive(Debug, Clone)]
pub struct WalkCtx<'a> {
    /// Ancestors from root to parent of the current node (root first).
    pub ancestors: &'a [&'a Sexpr],
    /// Index of this node in its parent list, if it has a parent.
    pub index_in_parent: Option<usize>,
}

impl<'a> WalkCtx<'a> {
    /// Get the parent node (last ancestor).
    pub fn parent(&self) -> Option<&'a Sexpr> {
        self.ancestors.last().copied()
    }

    /// Get the grandparent node (second-to-last ancestor).
    pub fn grandparent(&self) -> Option<&'a Sexpr> {
        if self.ancestors.len() >= 2 {
            Some(self.ancestors[self.ancestors.len() - 2])
        } else {
            None
        }
    }

    /// Check if parent list has the given tag (first element symbol).
    pub fn parent_tag(&self) -> Option<&'a str> {
        self.parent()?.as_list()?.first()?.as_sym()
    }

    /// Check if grandparent list has the given tag.
    pub fn grandparent_tag(&self) -> Option<&'a str> {
        self.grandparent()?.as_list()?.first()?.as_sym()
    }
}

/// Byte span in source text
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    /// Start byte offset (inclusive)
    pub start: usize,
    /// End byte offset (exclusive)
    pub end: usize,
}

impl Span {
    /// Create a new span
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Create an empty/synthetic span (for constructed nodes)
    pub fn synthetic() -> Self {
        Self { start: 0, end: 0 }
    }

    /// Check if this is a synthetic (non-parsed) span
    pub fn is_synthetic(&self) -> bool {
        self.start == 0 && self.end == 0
    }

    /// Get the length of the span
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Check if span is empty
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }
}

/// The kind of S-expression value
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SexprKind {
    /// A symbol - unquoted identifier
    Symbol(String),
    /// A string - quoted text
    String(String),
    /// An integer value
    Int(i64),
    /// A floating-point value
    F64(f64),
    /// A list of S-expressions
    List(Vec<Sexpr>),
}

/// An S-expression value with source span
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Sexpr {
    /// The kind of S-expression
    pub kind: SexprKind,
    /// Source span (byte offsets)
    pub span: Span,
}

impl PartialEq for Sexpr {
    fn eq(&self, other: &Self) -> bool {
        // Compare only the kind, not the span
        self.kind == other.kind
    }
}

impl Sexpr {
    /// Create a new Sexpr with a span
    pub fn with_span(kind: SexprKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// Create a symbol (unquoted atom) with synthetic span
    pub fn symbol(s: impl Into<String>) -> Self {
        Self {
            kind: SexprKind::Symbol(s.into()),
            span: Span::synthetic(),
        }
    }

    /// Create a string (quoted atom) with synthetic span
    pub fn string(s: impl Into<String>) -> Self {
        Self {
            kind: SexprKind::String(s.into()),
            span: Span::synthetic(),
        }
    }

    /// Create an integer with synthetic span
    pub fn int(n: i64) -> Self {
        Self {
            kind: SexprKind::Int(n),
            span: Span::synthetic(),
        }
    }

    /// Create a float with synthetic span
    pub fn float(f: f64) -> Self {
        Self {
            kind: SexprKind::F64(f),
            span: Span::synthetic(),
        }
    }

    /// Create a list from a vector of S-expressions with synthetic span
    pub fn list(items: Vec<Sexpr>) -> Self {
        Self {
            kind: SexprKind::List(items),
            span: Span::synthetic(),
        }
    }

    /// Check if this is a list
    pub fn is_list(&self) -> bool {
        matches!(self.kind, SexprKind::List(_))
    }

    /// Get the atom value if this is an atom (symbol or string) - for compatibility
    pub fn as_atom(&self) -> Option<&str> {
        match &self.kind {
            SexprKind::Symbol(s) | SexprKind::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get the symbol name if this is a symbol
    pub fn as_sym(&self) -> Option<&str> {
        match &self.kind {
            SexprKind::Symbol(s) => Some(s),
            _ => None,
        }
    }

    /// Get the string content if this is a string literal
    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            SexprKind::String(s) => Some(s),
            _ => None,
        }
    }

    /// Get the integer value if this is an integer
    pub fn as_int(&self) -> Option<i64> {
        match &self.kind {
            SexprKind::Int(n) => Some(*n),
            _ => None,
        }
    }

    /// Get the float value if this is a float
    pub fn as_float(&self) -> Option<f64> {
        match &self.kind {
            SexprKind::F64(f) => Some(*f),
            _ => None,
        }
    }

    /// Get the list items if this is a list
    pub fn as_list(&self) -> Option<&[Sexpr]> {
        match &self.kind {
            SexprKind::List(items) => Some(items),
            _ => None,
        }
    }

    /// Get mutable access to list items if this is a list
    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Sexpr>> {
        match &mut self.kind {
            SexprKind::List(items) => Some(items),
            _ => None,
        }
    }

    /// Find a child list with the given name (first element)
    pub fn find_list(&self, name: &str) -> Option<&[Sexpr]> {
        find_child_list(self.as_list()?, name)
    }

    /// Find all child lists with the given name
    pub fn find_all_lists(&self, name: &str) -> Vec<&[Sexpr]> {
        self.as_list()
            .map(|items| find_all_child_lists(items, name))
            .unwrap_or_default()
    }

    /// Depth-first traversal of the tree, visiting every node once.
    ///
    /// The callback receives each node along with a [`WalkCtx`] containing
    /// the ancestor stack and index within its parent list.
    ///
    /// # Example
    ///
    /// ```
    /// use pcb_sexpr::{parse, SexprKind};
    ///
    /// let sexpr = parse("(a (b c) d)").unwrap();
    /// let mut symbols = Vec::new();
    /// sexpr.walk(|node, _ctx| {
    ///     if let SexprKind::Symbol(s) = &node.kind {
    ///         symbols.push(s.clone());
    ///     }
    /// });
    /// assert_eq!(symbols, vec!["a", "b", "c", "d"]);
    /// ```
    pub fn walk<F>(&self, mut f: F)
    where
        F: FnMut(&Sexpr, WalkCtx<'_>),
    {
        fn walk_recursive<'a, F>(
            node: &'a Sexpr,
            stack: &mut Vec<&'a Sexpr>,
            f: &mut F,
            index_in_parent: Option<usize>,
        ) where
            F: FnMut(&Sexpr, WalkCtx<'_>),
        {
            let ctx = WalkCtx {
                ancestors: stack,
                index_in_parent,
            };
            f(node, ctx);

            if let Some(children) = node.as_list() {
                stack.push(node);
                for (i, child) in children.iter().enumerate() {
                    walk_recursive(child, stack, f, Some(i));
                }
                stack.pop();
            }
        }

        let mut stack = Vec::new();
        walk_recursive(self, &mut stack, &mut f, None);
    }

    /// Walk all string literals in the tree.
    ///
    /// Convenience method that visits only [`SexprKind::String`] nodes,
    /// providing the string value, source span, and walk context.
    ///
    /// # Example
    ///
    /// ```
    /// use pcb_sexpr::parse;
    ///
    /// let sexpr = parse(r#"(net 1 "VCC")"#).unwrap();
    /// sexpr.walk_strings(|value, span, ctx| {
    ///     assert_eq!(value, "VCC");
    ///     assert_eq!(ctx.index_in_parent, Some(2));
    /// });
    /// ```
    pub fn walk_strings<F>(&self, mut f: F)
    where
        F: FnMut(&str, Span, WalkCtx<'_>),
    {
        self.walk(|node, ctx| {
            if let SexprKind::String(ref s) = node.kind {
                f(s, node.span, ctx);
            }
        });
    }
}

/// Create a key-value pair list
pub fn kv<K: Into<String>, V: Into<Sexpr>>(k: K, v: V) -> Sexpr {
    Sexpr::list(vec![Sexpr::symbol(k), v.into()])
}

/// A builder for constructing lists incrementally
#[derive(Debug)]
pub struct ListBuilder {
    items: Vec<Sexpr>,
}

impl Default for ListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ListBuilder {
    /// Create a new builder with a node name
    pub fn node<N: Into<Sexpr>>(name: N) -> Self {
        Self {
            items: vec![name.into()],
        }
    }

    /// Create an empty builder
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Push a value to the list
    pub fn push<V: Into<Sexpr>>(&mut self, v: V) -> &mut Self {
        self.items.push(v.into());
        self
    }

    /// Conditionally push a value to the list
    pub fn push_if<V: Into<Sexpr>>(&mut self, cond: bool, v: V) -> &mut Self {
        if cond {
            self.items.push(v.into());
        }
        self
    }

    /// Extend the list with an iterator of values
    pub fn extend<I, V>(&mut self, iter: I) -> &mut Self
    where
        I: IntoIterator<Item = V>,
        V: Into<Sexpr>,
    {
        self.items.extend(iter.into_iter().map(Into::into));
        self
    }

    /// Build the final list
    pub fn build(self) -> Sexpr {
        Sexpr::list(self.items)
    }
}

/// From implementations for automatic conversion
impl From<&str> for Sexpr {
    fn from(s: &str) -> Self {
        Self::symbol(s)
    }
}

impl From<String> for Sexpr {
    fn from(s: String) -> Self {
        Self::symbol(s)
    }
}

impl From<i64> for Sexpr {
    fn from(n: i64) -> Self {
        Sexpr::int(n)
    }
}

impl From<u32> for Sexpr {
    fn from(n: u32) -> Self {
        Sexpr::int(n as i64)
    }
}

impl From<f64> for Sexpr {
    fn from(n: f64) -> Self {
        Sexpr::float(n)
    }
}

impl From<bool> for Sexpr {
    fn from(b: bool) -> Self {
        Self::symbol(if b { "yes" } else { "no" })
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

        let end_pos = self.current_pos;
        Ok(Sexpr::with_span(
            SexprKind::List(items),
            Span::new(start_pos, end_pos),
        ))
    }

    fn parse_atom(&mut self) -> Result<Sexpr, ParseError> {
        self.skip_whitespace();

        if self.peek_char() == Some('"') {
            // Parse quoted string
            self.parse_string()
        } else {
            // Parse unquoted atom - could be number or symbol
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

            let end = self.current_pos;
            let atom_str = self.input[start..end].to_string();
            let span = Span::new(start, end);

            // Try to parse as number first
            if let Ok(int_val) = atom_str.parse::<i64>() {
                Ok(Sexpr::with_span(SexprKind::Int(int_val), span))
            } else if let Ok(float_val) = atom_str.parse::<f64>() {
                Ok(Sexpr::with_span(SexprKind::F64(float_val), span))
            } else {
                // Otherwise treat as symbol
                Ok(Sexpr::with_span(SexprKind::Symbol(atom_str), span))
            }
        }
    }

    fn parse_string(&mut self) -> Result<Sexpr, ParseError> {
        let start_pos = self.current_pos;
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

        let end_pos = self.current_pos;
        Ok(Sexpr::with_span(
            SexprKind::String(result),
            Span::new(start_pos, end_pos),
        ))
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
        if let Some((pos, ch)) = self.chars.next() {
            self.current_pos = pos + ch.len_utf8(); // pos is the start of the char, we want the position after it
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

/// A single patch to apply to source text.
#[derive(Debug, Clone)]
pub struct Patch {
    /// Byte span to replace
    pub span: Span,
    /// New text to insert
    pub new_text: String,
}

/// A collection of patches to apply to source text.
///
/// Patches are sorted by span start and applied in a single forward pass,
/// writing directly to any `std::io::Write` implementation.
#[derive(Debug, Default)]
pub struct PatchSet {
    patches: Vec<Patch>,
}

impl PatchSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, mut other: PatchSet) {
        self.patches.append(&mut other.patches);
    }

    /// Add a patch to replace a string value.
    /// The new_value should NOT include quotes - they will be added.
    pub fn replace_string(&mut self, span: Span, new_value: &str) {
        self.patches.push(Patch {
            span,
            new_text: formatter::quote_string(new_value),
        });
    }

    /// Add a raw patch (caller provides exact replacement text).
    pub fn replace_raw(&mut self, span: Span, new_text: String) {
        self.patches.push(Patch { span, new_text });
    }

    pub fn is_empty(&self) -> bool {
        self.patches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.patches.len()
    }

    /// Write the patched source to a writer.
    ///
    /// This is the most efficient method - it streams patches in a single forward
    /// pass without intermediate allocations. Use this when writing to a file
    /// or any other `Write` destination.
    pub fn write_to<W: std::io::Write>(&self, source: &str, mut writer: W) -> std::io::Result<()> {
        if self.patches.is_empty() {
            return writer.write_all(source.as_bytes());
        }

        // Sort patches by start offset (ascending), using references to avoid cloning
        let mut sorted: Vec<&Patch> = self.patches.iter().collect();
        sorted.sort_by_key(|p| p.span.start);

        // Validate patches are non-overlapping and in bounds
        debug_assert!(sorted
            .windows(2)
            .all(|w| { w[0].span.end <= w[1].span.start && w[1].span.end <= source.len() }));

        let mut cursor = 0;
        for patch in sorted {
            // Write text before this patch
            if patch.span.start > cursor {
                writer.write_all(&source.as_bytes()[cursor..patch.span.start])?;
            }
            // Write replacement
            writer.write_all(patch.new_text.as_bytes())?;
            cursor = patch.span.end;
        }

        // Write remaining tail
        if cursor < source.len() {
            writer.write_all(&source.as_bytes()[cursor..])?;
        }

        Ok(())
    }
}

impl fmt::Display for Sexpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let formatted = formatter::format_tree(self, formatter::FormatMode::Normal);
        write!(f, "{}", formatted.trim_end_matches('\n'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_atom() {
        assert_eq!(
            parse("hello").unwrap().kind,
            SexprKind::Symbol("hello".to_string())
        );
        assert_eq!(parse("123").unwrap().kind, SexprKind::Int(123));
        assert_eq!(parse("3.15").unwrap().kind, SexprKind::F64(3.15));
        assert_eq!(
            parse("symbol-with-dashes").unwrap().kind,
            SexprKind::Symbol("symbol-with-dashes".to_string())
        );
    }

    #[test]
    fn test_parse_string() {
        assert_eq!(
            parse("\"hello world\"").unwrap().kind,
            SexprKind::String("hello world".to_string())
        );
        assert_eq!(
            parse("\"with\\\"quotes\\\"\"").unwrap().kind,
            SexprKind::String("with\"quotes\"".to_string())
        );
        assert_eq!(
            parse("\"line\\nbreak\"").unwrap().kind,
            SexprKind::String("line\nbreak".to_string())
        );
    }

    #[test]
    fn test_parse_list() {
        assert_eq!(parse("()").unwrap().kind, SexprKind::List(vec![]));
        let parsed = parse("(a b c)").unwrap();
        if let SexprKind::List(items) = &parsed.kind {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].kind, SexprKind::Symbol("a".to_string()));
            assert_eq!(items[1].kind, SexprKind::Symbol("b".to_string()));
            assert_eq!(items[2].kind, SexprKind::Symbol("c".to_string()));
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_parse_nested() {
        let input = "(define (square x) (* x x))";
        let result = parse(input).unwrap();
        if let SexprKind::List(items) = &result.kind {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].kind, SexprKind::Symbol("define".to_string()));
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_parse_kicad_pin() {
        let input = r#"(pin passive line (at 0 0 0) (length 2.54) (name "1") (number "1"))"#;
        let result = parse(input).unwrap();

        // Verify that pin numbers remain as strings
        if let SexprKind::List(items) = &result.kind {
            assert_eq!(items[0].kind, SexprKind::Symbol("pin".to_string()));

            // Find the number field
            for item in items {
                if let SexprKind::List(sub_items) = &item.kind {
                    if sub_items.len() >= 2
                        && sub_items[0].kind == SexprKind::Symbol("number".to_string())
                    {
                        assert_eq!(sub_items[1].kind, SexprKind::String("1".to_string()));
                    }
                }
            }
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_format_simple() {
        let sexpr = Sexpr::list(vec![
            Sexpr::symbol("at"),
            Sexpr::symbol("10"),
            Sexpr::symbol("20"),
        ]);
        assert_eq!(
            formatter::format_tree(&sexpr, formatter::FormatMode::Normal),
            "(at 10 20)\n"
        );
    }

    #[test]
    fn test_format_nested() {
        let sexpr = Sexpr::list(vec![
            Sexpr::symbol("symbol"),
            Sexpr::list(vec![Sexpr::symbol("lib_id"), Sexpr::symbol("Device:R")]),
            Sexpr::list(vec![
                Sexpr::symbol("at"),
                Sexpr::symbol("50"),
                Sexpr::symbol("50"),
                Sexpr::symbol("0"),
            ]),
        ]);

        let formatted = formatter::format_tree(&sexpr, formatter::FormatMode::Normal);
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
        if let SexprKind::List(items) = &result.kind {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].kind, SexprKind::Symbol("test".to_string()));
            assert_eq!(items[1].kind, SexprKind::Symbol("value".to_string()));
        } else {
            panic!("Expected a list");
        }
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
            let formatted = formatter::format_tree(&parsed, formatter::FormatMode::Normal);
            let reparsed = parse(&formatted).unwrap();
            assert_eq!(parsed, reparsed, "Roundtrip failed for: {input}");
        }
    }

    #[test]
    fn test_utf8_handling() {
        // Test with multi-byte UTF-8 characters
        let input = r#"(symbol "résistance" "日本語" "🔥")"#;
        let parsed = parse(input).unwrap();

        if let SexprKind::List(items) = &parsed.kind {
            assert_eq!(items.len(), 4);
            assert_eq!(items[0].kind, SexprKind::Symbol("symbol".to_string()));
            assert_eq!(items[1].kind, SexprKind::String("résistance".to_string()));
            assert_eq!(items[2].kind, SexprKind::String("日本語".to_string()));
            assert_eq!(items[3].kind, SexprKind::String("🔥".to_string()));
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_span_tracking() {
        let input = r#"(property "Path" "S1.R1.R")"#;
        let parsed = parse(input).unwrap();

        // The outer list should span the entire input
        assert_eq!(parsed.span.start, 0);
        assert_eq!(parsed.span.end, input.len());

        if let SexprKind::List(items) = &parsed.kind {
            // "property" symbol
            assert_eq!(items[0].span.start, 1);
            assert_eq!(items[0].span.end, 9);
            assert_eq!(&input[items[0].span.start..items[0].span.end], "property");

            // "Path" string
            assert_eq!(&input[items[1].span.start..items[1].span.end], "\"Path\"");

            // "S1.R1.R" string - this is what we'd patch for moved()
            assert_eq!(
                &input[items[2].span.start..items[2].span.end],
                "\"S1.R1.R\""
            );
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_span_tracking_net() {
        let input = r#"(net 5 "VCC_3V3")"#;
        let parsed = parse(input).unwrap();

        if let SexprKind::List(items) = &parsed.kind {
            // The net name string
            assert_eq!(
                &input[items[2].span.start..items[2].span.end],
                "\"VCC_3V3\""
            );
        } else {
            panic!("Expected a list");
        }
    }

    #[test]
    fn test_span_tracking_group() {
        let input = r#"(group "PowerSupply")"#;
        let parsed = parse(input).unwrap();

        if let SexprKind::List(items) = &parsed.kind {
            // The group name string
            assert_eq!(
                &input[items[1].span.start..items[1].span.end],
                "\"PowerSupply\""
            );
        } else {
            panic!("Expected a list");
        }
    }
}
