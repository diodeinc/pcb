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

use std::fmt::Write as _;
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
}

impl<'a> Doc<'a> {
    pub fn parse(source: &'a str) -> Result<Self> {
        let dom = uppsala::parse(source).map_err(|err| Ipc2581Error::XmlParse(err.to_string()))?;
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

    /// Child elements, in source order.
    pub fn children(&self, node: Node) -> Vec<Node> {
        self.dom
            .children(node.0)
            .into_iter()
            .filter(|&id| matches!(self.dom.node_kind(id), Some(uppsala::NodeKind::Element(_))))
            .map(Node)
            .collect()
    }

    /// First child element with the given local name.
    pub fn child(&self, node: Node, name: &str) -> Option<Node> {
        self.children(node)
            .into_iter()
            .find(|&child| self.name(child) == name)
    }

    /// Raw source text of an element, including its tags.
    pub fn source(&self, node: Node) -> &'a str {
        &self.source[self.span(node)]
    }

    /// Insert `xml` immediately before an element's opening tag.
    pub fn insert_before(&self, node: Node, xml: impl Into<String>) -> Edit {
        Edit {
            at: self.span(node).start,
            delete: 0,
            insert: xml.into(),
        }
    }

    /// Insert `xml` immediately after an element's closing tag.
    pub fn insert_after(&self, node: Node, xml: impl Into<String>) -> Edit {
        Edit {
            at: self.span(node).end,
            delete: 0,
            insert: xml.into(),
        }
    }

    /// Insert `xml` as the last content of an element, just before its closing
    /// tag. A self-closing element is expanded to an open/close pair.
    pub fn append_inside(&self, node: Node, xml: impl Into<String>) -> Edit {
        let span = self.span(node);
        let slice = self.source(node);
        if let Some(start_tag) = slice.strip_suffix("/>") {
            let mut insert = String::with_capacity(slice.len() + 16);
            let _ = write!(
                insert,
                "{}>{}</{}>",
                start_tag.trim_end(),
                xml.into(),
                self.name(node)
            );
            return Edit {
                at: span.start,
                delete: span.len(),
                insert,
            };
        }
        Edit {
            at: span.start + self.end_tag_offset(node),
            delete: 0,
            insert: xml.into(),
        }
    }

    /// Delete an element (tags and content).
    pub fn delete(&self, node: Node) -> Edit {
        let span = self.span(node);
        Edit {
            at: span.start,
            delete: span.len(),
            insert: String::new(),
        }
    }

    /// Replace an element (tags and content) with `xml`.
    pub fn replace(&self, node: Node, xml: impl Into<String>) -> Edit {
        let span = self.span(node);
        Edit {
            at: span.start,
            delete: span.len(),
            insert: xml.into(),
        }
    }

    /// Replace just an element's opening tag (or the whole element when
    /// self-closing) with `xml`. Use to rewrite attributes in place.
    pub fn replace_start_tag(&self, node: Node, xml: impl Into<String>) -> Edit {
        let span = self.span(node);
        Edit {
            at: span.start,
            delete: start_tag_len(self.source(node)),
            insert: xml.into(),
        }
    }

    /// All elements with the given local name, anywhere in the document,
    /// in document order.
    pub fn find_all(&self, name: &str) -> Vec<Node> {
        self.dom
            .get_elements_by_tag_name(name)
            .into_iter()
            .map(Node)
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
/// there (so inserting at an element and replacing it compose).
pub fn apply(source: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by_key(|edit| (edit.at, edit.delete > 0));

    let grows: usize = edits.iter().map(|edit| edit.insert.len()).sum();
    let mut out = String::with_capacity(source.len() + grows);
    let mut cursor = 0usize;
    for edit in &edits {
        if edit.at < cursor {
            return Err(Ipc2581Error::InvalidStructure(format!(
                "overlapping edits at byte {}",
                edit.at
            )));
        }
        out.push_str(&source[cursor..edit.at]);
        out.push_str(&edit.insert);
        cursor = edit.at + edit.delete;
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
