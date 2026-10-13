//! Parse S-expressions into nodes in a bump arena.
//!
//! [`Arena::parse`] reads the source in one pass. Each list's children are one
//! slice in the arena, so [`Node::as_list`] returns `&[Node]`. Symbols, plain
//! strings and number text borrow from the source, so only lists and strings
//! with escapes take arena space, and dropping the arena frees everything at
//! once. Use [`Arena`] to read a file; use [`Sexpr`] to build or edit a tree
//! ([`Node::to_sexpr`] converts).

use std::borrow::Cow;
use std::fmt;

use bumpalo::Bump;

use crate::parser::{self, Number, Sink};
use crate::{ParseError, Sexpr, SexprKind, Span, formatter};

/// Memory for parsed nodes. Nodes borrow it and the source.
#[derive(Default)]
pub struct Arena(Bump);

impl Arena {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the first top-level node of `source`, like [`crate::parse`].
    pub fn parse<'a>(&'a self, source: &'a str) -> Result<&'a Node<'a>, ParseError> {
        let root = parser::scan(source, &mut Builder(&self.0), true)?
            .pop()
            .ok_or(ParseError::UnexpectedEof)?;
        Ok(self.0.alloc(root))
    }
}

/// One parsed node, borrowed from an [`Arena`] and the source.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    start: u32,
    end: u32,
    kind: Kind<'a>,
}

#[derive(Clone, Copy)]
enum Kind<'a> {
    List(&'a [Node<'a>]),
    Symbol(&'a str),
    String(&'a str),
    Int(i64),
    F64(f64),
    /// Numbers that formatting would not reproduce: the source text, from
    /// which the value is parsed on access.
    RawInt(&'a str),
    RawF64(&'a str),
}

/// A view of a [`Node`], for `match`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeKind<'a> {
    Symbol(&'a str),
    String(&'a str),
    Int(i64),
    F64(f64),
    List(&'a [Node<'a>]),
}

impl<'a> Node<'a> {
    /// Byte span of the node in the source.
    pub fn span(&self) -> Span {
        Span::new(self.start as usize, self.end as usize)
    }

    pub fn kind(&self) -> NodeKind<'a> {
        match self.kind {
            Kind::List(items) => NodeKind::List(items),
            Kind::Symbol(s) => NodeKind::Symbol(s),
            Kind::String(s) => NodeKind::String(s),
            Kind::Int(n) => NodeKind::Int(n),
            Kind::F64(f) => NodeKind::F64(f),
            Kind::RawInt(raw) => NodeKind::Int(raw.parse().unwrap_or_default()),
            Kind::RawF64(raw) => NodeKind::F64(raw.parse().unwrap_or_default()),
        }
    }

    /// The symbol or string text.
    pub fn as_atom(&self) -> Option<&'a str> {
        match self.kind {
            Kind::Symbol(s) | Kind::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_sym(&self) -> Option<&'a str> {
        match self.kind {
            Kind::Symbol(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&'a str> {
        match self.kind {
            Kind::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self.kind() {
            NodeKind::Int(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self.kind() {
            NodeKind::F64(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&'a [Node<'a>]> {
        match self.kind {
            Kind::List(items) => Some(items),
            _ => None,
        }
    }

    /// The atom as written; see [`Sexpr::atom_text`].
    pub fn atom_text(&self) -> Option<Cow<'a, str>> {
        match self.kind {
            Kind::List(_) => None,
            Kind::Symbol(s) | Kind::String(s) | Kind::RawInt(s) | Kind::RawF64(s) => {
                Some(Cow::Borrowed(s))
            }
            Kind::Int(n) => Some(Cow::Owned(n.to_string())),
            Kind::F64(f) => Some(Cow::Owned(formatter::format_float(f))),
        }
    }

    /// The first child list `(name ...)`.
    pub fn find_list(&self, name: &str) -> Option<&'a [Node<'a>]> {
        self.as_list()?.iter().find_map(|item| {
            let items = item.as_list()?;
            (items.first()?.as_sym() == Some(name)).then_some(items)
        })
    }

    /// Copy this node into an owned [`Sexpr`] tree.
    pub fn to_sexpr(&self) -> Sexpr {
        let kind = match self.kind() {
            NodeKind::List(items) => SexprKind::List(items.iter().map(Node::to_sexpr).collect()),
            NodeKind::Symbol(s) => SexprKind::Symbol(s.to_owned()),
            NodeKind::String(s) => SexprKind::String(s.to_owned()),
            NodeKind::Int(n) => SexprKind::Int(n),
            NodeKind::F64(f) => SexprKind::F64(f),
        };
        let raw = match self.kind {
            Kind::RawInt(raw) | Kind::RawF64(raw) => Some(raw.into()),
            _ => None,
        };
        Sexpr {
            kind,
            span: self.span(),
            raw_atom: raw,
        }
    }
}

impl fmt::Debug for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("kind", &self.kind())
            .field("span", &self.span())
            .finish()
    }
}

impl PartialEq for Node<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
}

/// Makes [`Node`]s for [`parser::scan`].
struct Builder<'a>(&'a Bump);

impl<'a> Sink<'a> for Builder<'a> {
    type Node = Node<'a>;

    fn symbol(&mut self, start: u32, text: &'a str) -> Node<'a> {
        atom(start, text, Kind::Symbol(text))
    }

    fn number(&mut self, start: u32, text: &'a str, number: Number, canonical: bool) -> Node<'a> {
        let kind = match number {
            // Keep the source text of numbers that formatting would not reproduce.
            Number::Int(_) if !canonical => Kind::RawInt(text),
            Number::F64(_) if !canonical => Kind::RawF64(text),
            Number::Int(n) => Kind::Int(n),
            Number::F64(f) => Kind::F64(f),
        };
        atom(start, text, kind)
    }

    fn string(&mut self, start: u32, end: u32, text: &'a str, unescaped: Option<&str>) -> Node<'a> {
        let text = unescaped.map_or(text, |text| self.0.alloc_str(text));
        Node {
            start,
            end,
            kind: Kind::String(text),
        }
    }

    fn list(
        &mut self,
        start: u32,
        end: u32,
        pending: &mut Vec<Node<'a>>,
        first: usize,
    ) -> Node<'a> {
        Node {
            start,
            end,
            kind: Kind::List(self.0.alloc_slice_fill_iter(pending.drain(first..))),
        }
    }
}

fn atom<'a>(start: u32, text: &str, kind: Kind<'a>) -> Node<'a> {
    Node {
        start,
        end: start + text.len() as u32,
        kind,
    }
}
