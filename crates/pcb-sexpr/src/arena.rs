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

/// A parsed document: one or more top-level nodes and the source they span.
pub struct Doc<'s> {
    source: Cow<'s, str>,
    nodes: Vec<Node>,
    /// The top-level nodes are `nodes[roots..]`.
    roots: usize,
    /// Text of strings with escapes. Nodes point into it, so it must outlive them.
    _unescaped: String,
}

impl<'s> Doc<'s> {
    /// Parse the first top-level node of `source`, like [`crate::parse`].
    /// Pass a `&str` to borrow the source, or a `String` to move it into the
    /// document.
    pub fn parse(source: impl Into<Cow<'s, str>>) -> Result<Self, ParseError> {
        Self::build(source.into(), true)
    }

    /// Parse every top-level node of `source`, like [`crate::parse_all`], but
    /// fail on input with no node.
    pub fn parse_all(source: impl Into<Cow<'s, str>>) -> Result<Self, ParseError> {
        Self::build(source.into(), false)
    }

    fn build(source: Cow<'s, str>, first_only: bool) -> Result<Self, ParseError> {
        let mut arena = Arena {
            // KiCad files have about one node per 14 bytes.
            nodes: Vec::with_capacity(source.len() / 12),
            unescaped: String::new(),
        };
        let mut roots = parser::scan(&source, &mut arena, first_only)?;
        if roots.is_empty() {
            return Err(ParseError::UnexpectedEof);
        }
        let first_root = arena.nodes.len();
        arena.nodes.append(&mut roots);
        arena.fix_pointers();
        Ok(Self {
            source,
            nodes: arena.nodes,
            roots: first_root,
            _unescaped: arena.unescaped,
        })
    }

    /// The source text. Every node's [`Node::span`] indexes into it.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The top-level nodes, in order.
    pub fn roots(&self) -> &[Node] {
        &self.nodes[self.roots..]
    }

    /// The first top-level node. A parsed document has at least one.
    pub fn root(&self) -> &Node {
        &self.roots()[0]
    }
}

impl fmt::Debug for Doc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.roots()).finish()
    }
}

/// One node of a [`Doc`]. Nodes exist only inside a document and are always
/// borrowed from it.
pub struct Node {
    start: u32,
    end: u32,
    /// List length, or text length for nodes that keep text.
    len: u32,
    tag: Tag,
    data: Data,
}

// A node only points into the immutable buffers of the `Doc` it is borrowed from.
unsafe impl Send for Node {}
unsafe impl Sync for Node {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tag {
    /// `data.ptr` points to the first child.
    List,
    /// `data.ptr` points to the text.
    Symbol,
    String,
    /// `data` holds the value.
    Int,
    F64,
    /// Numbers that formatting would not reproduce: `data.ptr` points to the
    /// source text, and the value is parsed from it on access.
    RawInt,
    RawF64,
    /// During the parse: a string whose text is at `data.index` in the unescaped buffer.
    Unescaped,
}

#[derive(Clone, Copy)]
union Data {
    ptr: *const u8,
    /// During the parse, before pointers are fixed: the first child of a list,
    /// or the offset of a string's unescaped text.
    index: usize,
    int: i64,
    float: f64,
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
        match self.tag {
            Tag::List => NodeKind::List(self.children()),
            Tag::Symbol => NodeKind::Symbol(self.text()),
            Tag::String => NodeKind::String(self.text()),
            Tag::Int => NodeKind::Int(unsafe { self.data.int }),
            Tag::F64 => NodeKind::F64(unsafe { self.data.float }),
            Tag::RawInt => NodeKind::Int(self.text().parse().unwrap_or_default()),
            Tag::RawF64 => NodeKind::F64(self.text().parse().unwrap_or_default()),
            Tag::Unescaped => unreachable!(),
        }
    }

    /// Text of a symbol, string or raw number node.
    fn text(&self) -> &str {
        debug_assert!(!matches!(self.tag, Tag::List | Tag::Int | Tag::F64));
        // SAFETY: the parser sets `ptr` and `len` to UTF-8 text in the
        // document's source or unescaped buffer, which outlive `&self`.
        unsafe {
            let bytes = std::slice::from_raw_parts(self.data.ptr, self.len as usize);
            std::str::from_utf8_unchecked(bytes)
        }
    }

    fn children(&self) -> &[Node] {
        debug_assert!(self.tag == Tag::List);
        // SAFETY: the parser points `ptr` at `len` adjacent nodes in the
        // document's node array, which outlives `&self`.
        unsafe { std::slice::from_raw_parts(self.data.ptr.cast::<Node>(), self.len as usize) }
    }

    /// The symbol or string text.
    pub fn as_atom(&self) -> Option<&str> {
        match self.tag {
            Tag::Symbol | Tag::String => Some(self.text()),
            _ => None,
        }
    }

    pub fn as_sym(&self) -> Option<&str> {
        (self.tag == Tag::Symbol).then(|| self.text())
    }

    pub fn as_str(&self) -> Option<&str> {
        (self.tag == Tag::String).then(|| self.text())
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
        (self.tag == Tag::List).then(|| self.children())
    }

    /// The atom as written; see [`Sexpr::atom_text`].
    pub fn atom_text(&self) -> Option<Cow<'_, str>> {
        match self.tag {
            Tag::List => None,
            Tag::Symbol | Tag::String | Tag::RawInt | Tag::RawF64 => {
                Some(Cow::Borrowed(self.text()))
            }
            Tag::Int => Some(Cow::Owned(unsafe { self.data.int }.to_string())),
            Tag::F64 => Some(Cow::Owned(formatter::format_float(unsafe {
                self.data.float
            }))),
            Tag::Unescaped => unreachable!(),
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
        let kind = match self.tag {
            Tag::List => SexprKind::List(self.children().iter().map(Node::to_sexpr).collect()),
            Tag::Symbol => SexprKind::Symbol(self.text().to_owned()),
            Tag::String => SexprKind::String(self.text().to_owned()),
            Tag::Int => SexprKind::Int(unsafe { self.data.int }),
            Tag::F64 => SexprKind::F64(unsafe { self.data.float }),
            Tag::RawInt | Tag::RawF64 => {
                let kind = match self.kind() {
                    NodeKind::Int(n) => SexprKind::Int(n),
                    NodeKind::F64(f) => SexprKind::F64(f),
                    _ => unreachable!(),
                };
                return Sexpr {
                    kind,
                    span: self.span(),
                    raw_atom: Some(self.text().into()),
                };
            }
            Tag::Unescaped => unreachable!(),
        };
        Sexpr::with_span(kind, self.span())
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
    unescaped: String,
}

impl Arena {
    fn text(start: u32, text: &str, tag: Tag) -> Node {
        Node {
            start,
            end: start + text.len() as u32,
            len: text.len() as u32,
            tag,
            data: Data { ptr: text.as_ptr() },
        }
    }

    /// Turn list child indexes and unescaped text offsets into pointers, now
    /// that `nodes` and `unescaped` no longer grow.
    fn fix_pointers(&mut self) {
        let base = self.nodes.as_mut_ptr();
        let unescaped = self.unescaped.as_ptr();
        for i in 0..self.nodes.len() {
            // SAFETY: `i` is in bounds, list indexes are at most `nodes.len()`,
            // and string offsets are within `unescaped`.
            unsafe {
                let node = base.add(i);
                match (*node).tag {
                    Tag::List => {
                        (*node).data.ptr = base.add((*node).data.index).cast::<u8>().cast_const()
                    }
                    Tag::Unescaped => {
                        (*node).tag = Tag::String;
                        (*node).data.ptr = unescaped.add((*node).data.index);
                    }
                    _ => {}
                }
            }
        }
    }
}

impl<'s> Sink<'s> for Arena {
    type Node = Node;

    fn symbol(&mut self, start: u32, text: &'s str) -> Node {
        Self::text(start, text, Tag::Symbol)
    }

    fn number(&mut self, start: u32, text: &'s str, number: Number, canonical: bool) -> Node {
        let (tag, data) = match number {
            // Point at the source text of numbers that formatting would not reproduce.
            Number::Int(_) if !canonical => return Self::text(start, text, Tag::RawInt),
            Number::F64(_) if !canonical => return Self::text(start, text, Tag::RawF64),
            Number::Int(int) => (Tag::Int, Data { int }),
            Number::F64(float) => (Tag::F64, Data { float }),
        };
        Node {
            start,
            end: start + text.len() as u32,
            len: 0,
            tag,
            data,
        }
    }

    fn string(&mut self, start: u32, end: u32, text: &str, escaped: bool) -> Node {
        let mut node = Self::text(start, text, Tag::String);
        node.end = end;
        if escaped {
            node.tag = Tag::Unescaped;
            node.data.index = self.unescaped.len();
            self.unescaped.push_str(text);
        }
        node
    }

    fn list(&mut self, start: u32, end: u32, pending: &mut Vec<Node>, first: usize) -> Node {
        let index = self.nodes.len();
        let len = pending.len() - first;
        self.nodes.reserve(len);
        // SAFETY: moves the children, which have no `Drop`, from the end of
        // `pending` to the end of `nodes`.
        unsafe {
            std::ptr::copy_nonoverlapping(
                pending.as_ptr().add(first),
                self.nodes.as_mut_ptr().add(index),
                len,
            );
            self.nodes.set_len(index + len);
            pending.set_len(first);
        }
        Node {
            start,
            end,
            len: len as u32,
            tag: Tag::List,
            data: Data { index },
        }
    }
}
