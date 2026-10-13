//! A parsed S-expression document stored as one flat array of nodes.
//!
//! [`Doc::parse`] reads the source in one pass and allocates only a few large
//! buffers: the nodes, plus the text of strings that contain escapes. The
//! children of a list are adjacent in the node array, so [`Node::as_list`]
//! returns a plain slice. Symbols, plain strings and number text point into
//! the source. Use [`Doc`] to read a file; use [`Sexpr`] to build or edit a
//! tree ([`Node::to_sexpr`] converts).

use std::borrow::Cow;
use std::fmt;

use crate::parser::{self, Number, Sink};
use crate::{ParseError, Sexpr, SexprKind, Span, formatter};

/// A parsed document: the first top-level node of a source and its children.
pub struct Doc<'s> {
    source: Cow<'s, str>,
    /// All nodes; the root is last.
    nodes: Vec<Node>,
    /// Text of strings with escapes. Nodes point into it, so it must outlive them.
    _unescaped: Vec<String>,
}

impl<'s> Doc<'s> {
    /// Parse the first top-level node of `source`, like [`crate::parse`].
    /// Pass a `&str` to borrow the source, or a `String` to move it into the
    /// document.
    pub fn parse(source: impl Into<Cow<'s, str>>) -> Result<Self, ParseError> {
        let source = source.into();
        if u32::try_from(source.len()).is_err() {
            return Err(ParseError::InputTooLarge);
        }
        // KiCad files have about four nodes per list. Count lists, not bytes:
        // one embedded model can be megabytes of a single atom.
        let lists = memchr::memchr_iter(b'(', source.as_bytes()).count();
        let mut arena = Arena {
            nodes: Vec::with_capacity(lists * 9 / 2),
            unescaped: Vec::new(),
        };
        let root = parser::scan(&source, &mut arena, true)?
            .pop()
            .ok_or(ParseError::UnexpectedEof)?;
        arena.nodes.push(root);
        arena.fix_pointers();
        Ok(Self {
            source,
            nodes: arena.nodes,
            _unescaped: arena.unescaped,
        })
    }

    /// The source text. Every node's [`Node::span`] indexes into it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The top-level node.
    pub fn root(&self) -> &Node {
        &self.nodes[self.nodes.len() - 1]
    }
}

impl fmt::Debug for Doc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.root().fmt(f)
    }
}

/// One node of a [`Doc`]. Nodes exist only inside a document and are always
/// borrowed from it.
pub struct Node {
    start: u32,
    end: u32,
    kind: Kind,
}

// A node only points into the immutable buffers of the `Doc` it is borrowed from.
unsafe impl Send for Node {}
unsafe impl Sync for Node {}

/// Each pointer is to the given number of children or bytes of UTF-8 text in
/// the `Doc` that owns the node.
#[derive(Clone, Copy)]
enum Kind {
    List(*const Node, u32),
    Symbol(*const u8, u32),
    String(*const u8, u32),
    Int(i64),
    F64(f64),
    /// Numbers that formatting would not reproduce: the source text, from
    /// which the value is parsed on access.
    RawInt(*const u8, u32),
    RawF64(*const u8, u32),
}

/// A borrowed view of a [`Node`], for `match`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeKind<'a> {
    Symbol(&'a str),
    String(&'a str),
    Int(i64),
    F64(f64),
    List(&'a [Node]),
}

impl Node {
    /// Byte span of the node in [`Doc::source`].
    pub fn span(&self) -> Span {
        Span::new(self.start as usize, self.end as usize)
    }

    pub fn kind(&self) -> NodeKind<'_> {
        self.parts().0
    }

    /// The node's kind, and the source text of a raw number.
    fn parts(&self) -> (NodeKind<'_>, Option<&str>) {
        // SAFETY: the parser points each pointer at that many children or
        // bytes of UTF-8 in the `Doc` that `self` is borrowed from.
        let text = |ptr, len| unsafe {
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(ptr, len as usize))
        };
        match self.kind {
            Kind::List(ptr, len) => (
                NodeKind::List(unsafe { std::slice::from_raw_parts(ptr, len as usize) }),
                None,
            ),
            Kind::Symbol(ptr, len) => (NodeKind::Symbol(text(ptr, len)), None),
            Kind::String(ptr, len) => (NodeKind::String(text(ptr, len)), None),
            Kind::Int(n) => (NodeKind::Int(n), None),
            Kind::F64(f) => (NodeKind::F64(f), None),
            Kind::RawInt(ptr, len) => {
                let raw = text(ptr, len);
                (NodeKind::Int(raw.parse().unwrap_or_default()), Some(raw))
            }
            Kind::RawF64(ptr, len) => {
                let raw = text(ptr, len);
                (NodeKind::F64(raw.parse().unwrap_or_default()), Some(raw))
            }
        }
    }

    /// The symbol or string text.
    pub fn as_atom(&self) -> Option<&str> {
        match self.kind() {
            NodeKind::Symbol(s) | NodeKind::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_sym(&self) -> Option<&str> {
        match self.kind() {
            NodeKind::Symbol(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self.kind() {
            NodeKind::String(s) => Some(s),
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

    pub fn as_list(&self) -> Option<&[Node]> {
        match self.kind() {
            NodeKind::List(items) => Some(items),
            _ => None,
        }
    }

    /// The atom as written; see [`Sexpr::atom_text`].
    pub fn atom_text(&self) -> Option<Cow<'_, str>> {
        match self.parts() {
            (_, Some(raw)) => Some(Cow::Borrowed(raw)),
            (NodeKind::Symbol(s) | NodeKind::String(s), _) => Some(Cow::Borrowed(s)),
            (NodeKind::Int(n), _) => Some(Cow::Owned(n.to_string())),
            (NodeKind::F64(f), _) => Some(Cow::Owned(formatter::format_float(f))),
            (NodeKind::List(_), _) => None,
        }
    }

    /// The first child list `(name ...)`.
    pub fn find_list(&self, name: &str) -> Option<&[Node]> {
        self.as_list()?.iter().find_map(|item| {
            let items = item.as_list()?;
            (items.first()?.as_sym() == Some(name)).then_some(items)
        })
    }

    /// Copy this node into an owned [`Sexpr`] tree.
    pub fn to_sexpr(&self) -> Sexpr {
        let (kind, raw) = self.parts();
        let kind = match kind {
            NodeKind::List(items) => SexprKind::List(items.iter().map(Node::to_sexpr).collect()),
            NodeKind::Symbol(s) => SexprKind::Symbol(s.to_owned()),
            NodeKind::String(s) => SexprKind::String(s.to_owned()),
            NodeKind::Int(n) => SexprKind::Int(n),
            NodeKind::F64(f) => SexprKind::F64(f),
        };
        Sexpr {
            kind,
            span: self.span(),
            raw_atom: raw.map(Into::into),
        }
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Node")
            .field("kind", &self.kind())
            .field("span", &self.span())
            .finish()
    }
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
}

/// Builds the node array of a [`Doc`].
struct Arena {
    /// Finished nodes; each list's children are adjacent.
    nodes: Vec<Node>,
    unescaped: Vec<String>,
}

impl Arena {
    fn text(start: u32, text: &str, kind: fn(*const u8, u32) -> Kind) -> Node {
        Node {
            start,
            end: start + text.len() as u32,
            kind: kind(text.as_ptr(), text.len() as u32),
        }
    }

    /// Lists hold the index of their first child until `nodes` stops growing;
    /// turn the indexes into pointers.
    fn fix_pointers(&mut self) {
        let base = self.nodes.as_ptr();
        for node in &mut self.nodes {
            if let Kind::List(first, _) = &mut node.kind {
                *first = base.wrapping_add(first.addr());
            }
        }
    }
}

impl<'s> Sink<'s> for Arena {
    type Node = Node;

    fn symbol(&mut self, start: u32, text: &'s str) -> Node {
        Self::text(start, text, Kind::Symbol)
    }

    fn number(&mut self, start: u32, text: &'s str, number: Number, canonical: bool) -> Node {
        let kind = match number {
            // Point at the source text of numbers that formatting would not reproduce.
            Number::Int(_) if !canonical => return Self::text(start, text, Kind::RawInt),
            Number::F64(_) if !canonical => return Self::text(start, text, Kind::RawF64),
            Number::Int(n) => Kind::Int(n),
            Number::F64(f) => Kind::F64(f),
        };
        Node {
            start,
            end: start + text.len() as u32,
            kind,
        }
    }

    fn string(&mut self, start: u32, end: u32, text: &str, escaped: bool) -> Node {
        let text = if escaped {
            // A `String` keeps its heap text in place when the `Vec` grows.
            self.unescaped.push(text.to_owned());
            self.unescaped.last().unwrap()
        } else {
            text
        };
        Node {
            end,
            ..Self::text(start, text, Kind::String)
        }
    }

    fn list(&mut self, start: u32, end: u32, pending: &mut Vec<Node>, first: usize) -> Node {
        let index = self.nodes.len();
        self.nodes.extend(pending.drain(first..));
        Node {
            start,
            end,
            kind: Kind::List(
                std::ptr::without_provenance(index),
                (self.nodes.len() - index) as u32,
            ),
        }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Node>() == 24);
