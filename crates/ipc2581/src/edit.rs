//! Surgical editing of IPC-2581 source text.
//!
//! Instead of parsing and re-serializing the whole document (which reformats
//! every byte and loses the original text), edits are expressed as byte-range
//! splices against the original source. A [`Doc`] indexes the source with the
//! same arena-backed DOM used by [`crate::Ipc2581::parse`], each node carrying
//! its exact byte range; navigation locates the elements to change and the
//! `Edit` constructors turn them into splices. [`apply`] then rebuilds the
//! document in a single pass, leaving everything outside the edited ranges
//! byte-for-byte intact.

use std::ops::Range;

use crate::{Ipc2581Error, Result};

/// A parsed view over IPC-2581 source text that maps elements back to their
/// byte ranges in the source.
pub struct Doc<'a> {
    source: &'a str,
    dom: uppsala::Document<'a>,
}

/// Handle to an element in a [`Doc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node(uppsala::NodeId);

/// A single splice: delete `delete` bytes at `at`, then insert `insert` there.
#[derive(Debug, Clone)]
pub struct Edit {
    at: usize,
    delete: usize,
    insert: String,
    /// Set when the splice replaces the `/>` of a self-closing element:
    /// the end tag owed after everything appended inside it.
    end_tag: Option<String>,
}

impl Edit {
    fn splice(at: usize, delete: usize, insert: String) -> Self {
        Self {
            at,
            delete,
            insert,
            end_tag: None,
        }
    }
}

impl<'a> Doc<'a> {
    pub fn parse(source: &'a str) -> Result<Self> {
        let dom = crate::checksum::parse_document(source)?;
        Ok(Self { source, dom })
    }

    /// The document (root) element.
    pub fn root(&self) -> Result<Node> {
        self.dom
            .document_element()
            .map(Node)
            .ok_or(Ipc2581Error::MissingElement("document root"))
    }

    /// Local name of an element.
    pub fn name(&self, node: Node) -> &str {
        self.dom
            .element(node.0)
            .map(|element| element.name.local_name.as_ref())
            .unwrap_or_default()
    }

    /// Attribute value by local name.
    pub fn attr(&self, node: Node, name: &str) -> Option<&str> {
        self.dom.element(node.0)?.get_attribute(name)
    }

    /// All attributes of an element as (name, value) pairs, in source order.
    pub fn attrs(&self, node: Node) -> impl Iterator<Item = (&str, &str)> {
        self.dom
            .element(node.0)
            .into_iter()
            .flat_map(|element| element.attributes.iter())
            .map(|attr| (attr.name.local_name.as_ref(), attr.value.as_ref()))
    }

    fn child_elements(&self, node: Node) -> impl Iterator<Item = Node> + '_ {
        self.dom
            .children_iter(node.0)
            .filter(|&id| self.dom.element(id).is_some())
            .map(Node)
    }

    /// Child elements, in source order.
    pub fn children(&self, node: Node) -> Vec<Node> {
        self.child_elements(node).collect()
    }

    /// First child element with the given local name.
    pub fn child(&self, node: Node, name: &str) -> Option<Node> {
        self.child_elements(node)
            .find(|&child| self.name(child) == name)
    }

    /// Raw source text of an element, including its tags.
    pub fn source(&self, node: Node) -> &'a str {
        &self.source[self.span(node)]
    }

    /// Insert `xml` immediately before an element's opening tag.
    pub fn insert_before(&self, node: Node, xml: impl Into<String>) -> Edit {
        Edit::splice(self.span(node).start, 0, xml.into())
    }

    /// Insert `xml` immediately after an element's closing tag.
    pub fn insert_after(&self, node: Node, xml: impl Into<String>) -> Edit {
        Edit::splice(self.span(node).end, 0, xml.into())
    }

    /// Insert `xml` as the last content of an element, just before its closing
    /// tag. A self-closing element is expanded to an open/close pair, once for
    /// all the edits that append inside it.
    pub fn append_inside(&self, node: Node, xml: impl Into<String>) -> Edit {
        let span = self.span(node);
        let slice = self.source(node);
        let Some(start_tag) = slice.strip_suffix("/>") else {
            return Edit::splice(span.start + self.end_tag_offset(node), 0, xml.into());
        };
        // The name as written, so a prefixed element closes with its prefix.
        let name = &slice[1..];
        let name = &name[..name
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(name.len())];
        let at = span.start + start_tag.trim_end().len();
        Edit {
            at,
            delete: span.end - at,
            insert: xml.into(),
            end_tag: Some(format!("</{name}>")),
        }
    }

    /// Delete an element (tags and content).
    pub fn delete(&self, node: Node) -> Edit {
        let span = self.span(node);
        Edit::splice(span.start, span.len(), String::new())
    }

    /// Replace an element (tags and content) with `xml`.
    pub fn replace(&self, node: Node, xml: impl Into<String>) -> Edit {
        let span = self.span(node);
        Edit::splice(span.start, span.len(), xml.into())
    }

    /// Replace just an element's opening tag (or the whole element when
    /// self-closing) with `xml`. Use to rewrite attributes in place.
    pub fn replace_start_tag(&self, node: Node, xml: impl Into<String>) -> Edit {
        let start_tag = start_tag_len(self.source(node));
        Edit::splice(self.span(node).start, start_tag, xml.into())
    }

    /// All elements with the given local name, anywhere in the document,
    /// in document order.
    pub fn find_all(&self, name: &str) -> Vec<Node> {
        // Pre-order walk over the sibling links; nothing is allocated per node.
        let mut next = self.dom.document_element();
        std::iter::from_fn(|| {
            let current = next?;
            next = self.dom.first_child(current).or_else(|| {
                std::iter::successors(Some(current), |&node| self.dom.parent(node))
                    .find_map(|node| self.dom.next_sibling(node))
            });
            Some(Node(current))
        })
        .filter(|&node| self.dom.element(node.0).is_some() && self.name(node) == name)
        .collect()
    }

    /// Byte range of an element in the source, including its tags.
    pub fn span(&self, node: Node) -> Range<usize> {
        self.dom
            .node_range(node.0)
            .expect("nodes come from parsed source")
    }

    /// Byte offset of the closing tag within a non-self-closing element.
    fn end_tag_offset(&self, node: Node) -> usize {
        let span = self.span(node);
        match self.dom.children(node.0).last() {
            Some(&last) => {
                let child_end = self
                    .dom
                    .node_range(last)
                    .expect("nodes come from parsed source")
                    .end;
                child_end - span.start
            }
            // No child nodes at all: content is empty, so the closing tag
            // starts right after the opening tag.
            None => start_tag_len(self.source(node)),
        }
    }
}

/// Apply a set of non-overlapping edits to `source` in one pass.
///
/// Edits are ordered by position; insertions at the same position keep the
/// order in which they were created and land before any deletion starting
/// there (so inserting at an element and replacing it compose). Appends into
/// the same self-closing element share its expansion. A checksum trailer is
/// dropped because it describes the unedited text.
pub fn apply(source: &str, mut edits: Vec<Edit>) -> Result<String> {
    let (source, _) = crate::checksum::split_trailer(source);
    edits.sort_by_key(|edit| (edit.at, edit.delete > 0));

    let grows: usize = edits
        .iter()
        .map(|edit| edit.insert.len() + edit.end_tag.as_ref().map_or(0, |tag| tag.len() + 1))
        .sum();
    let mut out = String::with_capacity(source.len() + grows);
    let mut cursor = 0usize;
    // The self-closing element being appended into, and the end tag it is owed.
    let mut expanded: Option<(usize, &str)> = None;
    for edit in &edits {
        let same_element = edit.end_tag.is_some() && expanded.is_some_and(|(at, _)| at == edit.at);
        if !same_element {
            if let Some((_, end_tag)) = expanded.take() {
                out.push_str(end_tag);
            }
            if edit.at < cursor {
                return Err(Ipc2581Error::InvalidStructure(format!(
                    "overlapping edits at byte {}",
                    edit.at
                )));
            }
            out.push_str(&source[cursor..edit.at]);
            cursor = edit.at + edit.delete;
            if let Some(end_tag) = &edit.end_tag {
                out.push('>');
                expanded = Some((edit.at, end_tag));
            }
        }
        out.push_str(&edit.insert);
    }
    if let Some((_, end_tag)) = expanded {
        out.push_str(end_tag);
    }
    out.push_str(&source[cursor..]);
    Ok(out)
}

/// Length of the opening tag: everything through the first `>` that is not
/// inside a quoted attribute value.
pub fn start_tag_len(element_source: &str) -> usize {
    let mut quote = 0u8;
    for (index, byte) in element_source.bytes().enumerate() {
        match (quote, byte) {
            (0, b'"') | (0, b'\'') => quote = byte,
            (0, b'>') => return index + 1,
            (q, b) if q == b => quote = 0,
            _ => {}
        }
    }
    element_source.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<IPC-2581 revision="C">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL"/>
      <Step name="board"><Datum x="0" y="0"/></Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    #[test]
    fn navigation_finds_elements_by_name() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        assert_eq!(doc.name(root), "IPC-2581");
        assert_eq!(doc.attr(root, "revision"), Some("C"));

        let ecad = doc.child(root, "Ecad").unwrap();
        let cad_data = doc.child(ecad, "CadData").unwrap();
        let names: Vec<_> = doc
            .children(cad_data)
            .iter()
            .map(|&child| doc.name(child))
            .collect();
        assert_eq!(names, ["Layer", "Step"]);
    }

    #[test]
    fn edits_splice_without_touching_surroundings() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        let content = doc.child(root, "Content").unwrap();
        let function_mode = doc.child(content, "FunctionMode").unwrap();
        let step_ref = doc.child(content, "StepRef").unwrap();

        let edits = vec![
            doc.insert_after(function_mode, "<BomRef name=\"bom\"/>"),
            doc.delete(step_ref),
        ];
        let out = apply(XML, edits).unwrap();

        assert!(out.contains("<FunctionMode mode=\"FABRICATION\"/><BomRef name=\"bom\"/>"));
        assert!(!out.contains("StepRef"));
        // untouched regions are byte-identical
        assert!(out.contains("<Layer name=\"TOP\" layerFunction=\"SIGNAL\"/>"));
        assert!(out.starts_with("<?xml version=\"1.0\"?>"));
    }

    #[test]
    fn append_inside_expands_self_closing_elements() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        let ecad = doc.child(root, "Ecad").unwrap();
        let cad_header = doc.child(ecad, "CadHeader").unwrap();

        let edit = doc.append_inside(cad_header, "<Spec name=\"vcut\"/>");
        let out = apply(XML, vec![edit]).unwrap();

        assert!(out.contains("<CadHeader units=\"MILLIMETER\"><Spec name=\"vcut\"/></CadHeader>"));
    }

    #[test]
    fn appends_into_one_self_closing_element_compose() {
        let xml = r#"<ipc:IPC-2581 xmlns:ipc="urn:x"><ipc:Step name="a" /><Characteristics/><Tail/></ipc:IPC-2581>"#;
        let doc = Doc::parse(xml).unwrap();
        let root = doc.root().unwrap();
        let step = doc.child(root, "Step").unwrap();
        let characteristics = doc.child(root, "Characteristics").unwrap();
        let tail = doc.child(root, "Tail").unwrap();

        let edits = vec![
            doc.append_inside(characteristics, "<Textual name=\"distributor\"/>"),
            doc.insert_after(characteristics, "<After/>"),
            doc.append_inside(step, "<Datum/>"),
            doc.append_inside(characteristics, "<Textual name=\"alias\"/>"),
            doc.delete(tail),
        ];
        let out = apply(xml, edits).unwrap();

        assert_eq!(
            out,
            r#"<ipc:IPC-2581 xmlns:ipc="urn:x"><ipc:Step name="a"><Datum/></ipc:Step><Characteristics><Textual name="distributor"/><Textual name="alias"/></Characteristics><After/></ipc:IPC-2581>"#
        );
        assert!(Doc::parse(&out).is_ok());
        // Replacing the element still conflicts with appending inside it.
        let conflict = vec![
            doc.append_inside(step, "<Datum/>"),
            doc.replace(step, "<Step/>"),
        ];
        assert!(apply(xml, conflict).is_err());
    }

    #[test]
    fn find_all_walks_nested_elements_in_document_order() {
        let xml = r#"<R><Set id="1"><Set id="2"/><Pad/></Set><!-- c --><Other><Set id="3"/></Other>text<Set id="4"/></R>"#;
        let doc = Doc::parse(xml).unwrap();

        let ids: Vec<_> = doc
            .find_all("Set")
            .into_iter()
            .map(|node| doc.attr(node, "id").unwrap())
            .collect();

        assert_eq!(ids, ["1", "2", "3", "4"]);
        assert_eq!(doc.find_all("R").len(), 1);
        assert!(doc.find_all("Missing").is_empty());
    }

    #[test]
    fn append_inside_lands_before_the_closing_tag() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        let ecad = doc.child(root, "Ecad").unwrap();
        let cad_data = doc.child(ecad, "CadData").unwrap();

        let edit = doc.append_inside(cad_data, "<Step name=\"panel\"/>");
        let out = apply(XML, vec![edit]).unwrap();

        assert!(out.contains("</Step>\n    <Step name=\"panel\"/></CadData>"));
    }

    #[test]
    fn same_position_inserts_keep_creation_order() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        let ecad = doc.child(root, "Ecad").unwrap();
        let cad_data = doc.child(ecad, "CadData").unwrap();

        let edits = vec![
            doc.append_inside(cad_data, "<A/>"),
            doc.append_inside(cad_data, "<B/>"),
        ];
        let out = apply(XML, edits).unwrap();

        assert!(out.contains("<A/><B/>"));
    }

    #[test]
    fn overlapping_edits_are_rejected() {
        let doc = Doc::parse(XML).unwrap();
        let root = doc.root().unwrap();
        let content = doc.child(root, "Content").unwrap();

        let edits = vec![
            doc.delete(content),
            doc.delete(doc.child(content, "StepRef").unwrap()),
        ];
        assert!(apply(XML, edits).is_err());
    }

    #[test]
    fn checksum_trailer_is_accepted_and_dropped() {
        use base64::Engine as _;
        use md5::Digest as _;

        let root = &XML[XML.find("<IPC-2581").unwrap()..];
        let digest = base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(root));
        let source = format!("{XML}\n{digest}\n");
        let doc = Doc::parse(&source).unwrap();
        let content = doc.child(doc.root().unwrap(), "Content").unwrap();

        let out = apply(&source, vec![doc.delete(content)]).unwrap();

        assert!(out.ends_with("</Ecad>\n</IPC-2581>"));
        assert!(matches!(
            Doc::parse(&format!("{XML}\nAAAAAAAAAAAAAAAAAAAAAA==\n")),
            Err(Ipc2581Error::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn replace_start_tag_rewrites_attributes_only() {
        let xml = r#"<IPC-2581><HistoryRecord number="1" note="a &gt; b"><FileRevision fileRevisionId="1"/></HistoryRecord></IPC-2581>"#;
        let doc = Doc::parse(xml).unwrap();
        let root = doc.root().unwrap();
        let record = doc.child(root, "HistoryRecord").unwrap();
        assert_eq!(doc.attr(record, "note"), Some("a > b"));

        let edit = doc.replace_start_tag(record, "<HistoryRecord number=\"2\">");
        let out = apply(xml, vec![edit]).unwrap();

        assert!(out.contains("<HistoryRecord number=\"2\"><FileRevision fileRevisionId=\"1\"/>"));
    }
}
