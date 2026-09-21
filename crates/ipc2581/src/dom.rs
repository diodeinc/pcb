//! The element tree the typed parser and [`crate::edit`] read, in flat arenas.
//!
//! IPC-2581 carries all of its data in attributes, so text, comments and
//! processing instructions are not kept. A general DOM spends about 200 bytes
//! per node and makes a node of every whitespace run between elements, which
//! is a gigabyte for a 60 MB file; this keeps 40 bytes per element and 48 per
//! attribute, and 12 more per element for the source offsets an editor asks
//! for. Well-formedness, namespaces, entities and the depth and expansion
//! limits are the pull parser's, exactly as for `uppsala::parse`.

use std::borrow::Cow;
use std::ops::Range;

use uppsala::{PullEvent, PullParser};

use crate::{Ipc2581Error, Result};

const NONE: u32 = u32::MAX;

/// Handle to an element of a [`Dom`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Node(u32);

/// Which tables [`Dom::parse`] fills beyond the element tree.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    Tree,
    /// Also where each element lies in the source.
    SourceSpans,
}

/// Byte offsets of an element: its `<`, the `<` of its end tag (its own `<`
/// when self-closing) and the end of that tag.
#[derive(Clone, Copy)]
struct SourceSpan {
    start: u32,
    end_tag: u32,
    end: u32,
}

struct Element<'a> {
    name: Cow<'a, str>,
    /// Start and length of the element's run in `Dom::attributes`.
    attributes: (u32, u32),
    first_child: u32,
    next_sibling: u32,
}

pub(crate) struct Dom<'a> {
    /// In document order; the root element is first.
    elements: Vec<Element<'a>>,
    /// Local name and value, grouped by element.
    attributes: Vec<(Cow<'a, str>, Cow<'a, str>)>,
    /// Parallel to `elements` under [`Keep::SourceSpans`], else empty.
    spans: Vec<SourceSpan>,
    root_namespace: Option<Cow<'a, str>>,
    root_range: Range<usize>,
}

impl<'a> Dom<'a> {
    pub(crate) fn parse(xml: &'a str, keep: Keep) -> Result<Self> {
        let invalid = |err: uppsala::XmlError| Ipc2581Error::XmlParse(err.to_string());
        let index = |len: usize| {
            u32::try_from(len)
                .ok()
                .filter(|index| *index != NONE)
                .ok_or_else(|| Ipc2581Error::XmlParse("document is too large".to_string()))
        };

        // Counting tag and attribute openers sizes the arenas once; growing
        // them by doubling would hold up to three copies at the peak.
        let pairs = |first: u8, second: fn(u8) -> bool| {
            xml.as_bytes()
                .windows(2)
                .filter(|pair| pair[0] == first && second(pair[1]))
                .count()
        };
        let elements = pairs(b'<', |next| !matches!(next, b'/' | b'!' | b'?'));
        let spans = match keep {
            Keep::Tree => 0,
            Keep::SourceSpans => {
                // Offsets are kept in 32 bits.
                index(xml.len())?;
                elements
            }
        };
        let mut dom = Self {
            elements: Vec::with_capacity(elements),
            attributes: Vec::with_capacity(pairs(b'=', |next| matches!(next, b'"' | b'\''))),
            spans: Vec::with_capacity(spans),
            root_namespace: None,
            root_range: 0..0,
        };
        // Open elements, each with its most recently closed child.
        let mut open: Vec<(u32, u32)> = Vec::new();
        let mut parser = PullParser::new(xml);
        while let Some(event) = parser.next_event().map_err(invalid)? {
            match event {
                PullEvent::StartElement {
                    name,
                    attributes,
                    byte_start,
                    ..
                } => {
                    let element = index(dom.elements.len())?;
                    match open.last() {
                        Some(&(parent, NONE)) => {
                            dom.elements[parent as usize].first_child = element
                        }
                        Some(&(_, sibling)) => {
                            dom.elements[sibling as usize].next_sibling = element
                        }
                        None => {
                            dom.root_namespace = name.namespace_uri;
                            dom.root_range.start = byte_start;
                        }
                    }
                    let first_attribute = index(dom.attributes.len())?;
                    let count = index(attributes.len())?;
                    dom.attributes.extend(
                        attributes
                            .into_iter()
                            .map(|attribute| (attribute.name.local_name, attribute.value)),
                    );
                    dom.elements.push(Element {
                        name: name.local_name,
                        attributes: (first_attribute, count),
                        first_child: NONE,
                        next_sibling: NONE,
                    });
                    if keep == Keep::SourceSpans {
                        let start = byte_start as u32;
                        dom.spans.push(SourceSpan {
                            start,
                            end_tag: start,
                            end: start,
                        });
                    }
                    open.push((element, NONE));
                }
                PullEvent::EndElement {
                    byte_start,
                    byte_end,
                    ..
                } => {
                    let (element, _) = open.pop().expect("the pull parser balances tags");
                    if let Some(span) = dom.spans.get_mut(element as usize) {
                        span.end_tag = byte_start as u32;
                        span.end = byte_end as u32;
                    }
                    match open.last_mut() {
                        Some((_, last_child)) => *last_child = element,
                        None => dom.root_range.end = byte_end,
                    }
                }
                _ => {}
            }
        }
        if !open.is_empty() || dom.elements.is_empty() {
            return Err(Ipc2581Error::XmlParse(
                "document must have one complete root element".to_string(),
            ));
        }
        Ok(dom)
    }

    pub(crate) fn root(&self) -> Node {
        Node(0)
    }

    pub(crate) fn root_namespace(&self) -> Option<&str> {
        self.root_namespace.as_deref()
    }

    /// Source bytes of the root element, tags included.
    pub(crate) fn root_range(&self) -> Range<usize> {
        self.root_range.clone()
    }

    /// Local name of an element.
    pub(crate) fn name(&self, node: Node) -> &str {
        &self.elements[node.0 as usize].name
    }

    /// Attribute value by local name.
    pub(crate) fn attr(&self, node: Node, name: &str) -> Option<&str> {
        self.attrs(node)
            .find_map(|(candidate, value)| (candidate == name).then_some(value))
    }

    /// Attributes of an element as (local name, value) pairs, in source order.
    pub(crate) fn attrs(&self, node: Node) -> impl Iterator<Item = (&str, &str)> {
        let (start, count) = self.elements[node.0 as usize].attributes;
        self.attributes[start as usize..(start + count) as usize]
            .iter()
            .map(|(name, value)| (&**name, &**value))
    }

    /// Every element, in document order.
    pub(crate) fn elements(&self) -> impl Iterator<Item = Node> {
        (0..self.elements.len() as u32).map(Node)
    }

    /// Source bytes of an element, tags included. Needs [`Keep::SourceSpans`].
    pub(crate) fn range(&self, node: Node) -> Range<usize> {
        let span = self.spans[node.0 as usize];
        span.start as usize..span.end as usize
    }

    /// Source offset of an element's end tag, which for a self-closing
    /// element is its own start. Needs [`Keep::SourceSpans`].
    pub(crate) fn end_tag(&self, node: Node) -> usize {
        self.spans[node.0 as usize].end_tag as usize
    }

    /// Child elements in document order.
    pub(crate) fn children(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
        let link = |index: u32| (index != NONE).then_some(Node(index));
        std::iter::successors(
            link(self.elements[node.0 as usize].first_child),
            move |child| link(self.elements[child.0 as usize].next_sibling),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_elements_and_attributes_and_drops_the_rest() {
        let xml = "<?xml version=\"1.0\"?>\r\n<!-- c --><r xmlns=\"urn:x\" a=\"1\">\r\n  text<x:b xmlns:x=\"urn:y\" x:k=\"a &amp; b\"/><![CDATA[<no/>]]>\r\n  <c><d/></c><?pi?>\r\n</r>\n";
        let dom = Dom::parse(xml, Keep::SourceSpans).unwrap();
        let root = dom.root();

        assert_eq!(dom.name(root), "r");
        assert_eq!(dom.root_namespace(), Some("urn:x"));
        assert_eq!(
            &xml[dom.root_range()],
            xml.trim_end().split_once("-->").unwrap().1
        );
        assert_eq!(
            (dom.attr(root, "a"), dom.attr(root, "xmlns")),
            (Some("1"), None)
        );
        let children = dom.children(root).collect::<Vec<_>>();
        let names = children
            .iter()
            .map(|child| dom.name(*child))
            .collect::<Vec<_>>();
        assert_eq!(names, ["b", "c"]);
        assert_eq!(dom.attr(children[0], "k"), Some("a & b"));
        assert_eq!(dom.children(children[0]).count(), 0);
        let grandchildren = dom.children(children[1]).map(|node| dom.name(node));
        assert_eq!(grandchildren.collect::<Vec<_>>(), ["d"]);

        let names = dom.elements().map(|node| dom.name(node));
        assert_eq!(names.collect::<Vec<_>>(), ["r", "b", "c", "d"]);
        assert_eq!(dom.range(root), dom.root_range());
        assert_eq!(&xml[dom.range(children[1])], "<c><d/></c>");
        assert_eq!(&xml[dom.end_tag(children[1])..], "</c><?pi?>\r\n</r>\n");
        assert_eq!(dom.end_tag(children[0]), dom.range(children[0]).start);
        assert!(Dom::parse(xml, Keep::Tree).unwrap().spans.is_empty());
    }

    #[test]
    fn reports_malformed_xml() {
        for xml in ["", "<a><b></a>", "<a/><b/>", "<a/>trailing"] {
            assert!(
                matches!(Dom::parse(xml, Keep::Tree), Err(Ipc2581Error::XmlParse(_))),
                "{xml}"
            );
        }
    }
}
