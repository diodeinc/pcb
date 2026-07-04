use anyhow::{Context, Result};
use ipc2581::edit::start_tag_len;
use uppsala::{Document, NodeId, NodeKind};

/// Reformat XML with proper 2-space indentation.
///
/// Regenerates only the whitespace between tags: every tag keeps its exact
/// source bytes (attribute order, escaping, quoting), whitespace-only text is
/// dropped, and other text content stays inline within its element.
pub fn reformat_xml(xml: &str) -> Result<String> {
    let doc = uppsala::parse(xml).map_err(|err| anyhow::anyhow!("XML parse error: {err}"))?;
    doc.document_element()
        .context("XML document has no root element")?;

    let mut out = String::with_capacity(xml.len());
    if let Some(decl) = &doc.xml_declaration {
        out.push_str("<?xml version=\"");
        out.push_str(&decl.version);
        out.push('"');
        if let Some(encoding) = &decl.encoding {
            out.push_str(" encoding=\"");
            out.push_str(encoding);
            out.push('"');
        }
        if let Some(standalone) = decl.standalone {
            out.push_str(" standalone=\"");
            out.push_str(if standalone { "yes" } else { "no" });
            out.push('"');
        }
        out.push_str("?>");
    }
    if let Some(doctype) = &doc.doctype {
        line_start(&mut out, 0);
        out.push_str(doctype);
    }
    for child in doc.children(doc.root()) {
        write_node(&doc, xml, child, 0, &mut out);
    }
    Ok(out)
}

fn write_node(doc: &Document, src: &str, id: NodeId, depth: usize, out: &mut String) {
    match doc.node_kind(id) {
        Some(NodeKind::Element(_)) => write_element(doc, src, id, depth, out),
        Some(NodeKind::Comment(_) | NodeKind::ProcessingInstruction(_) | NodeKind::CData(_)) => {
            line_start(out, depth);
            out.push_str(&src[span(doc, id)]);
        }
        _ => {}
    }
}

fn write_element(doc: &Document, src: &str, id: NodeId, depth: usize, out: &mut String) {
    line_start(out, depth);
    let range = span(doc, id);
    let slice = &src[range.clone()];
    if slice.ends_with("/>") {
        out.push_str(slice);
        return;
    }

    let start_len = start_tag_len(slice);
    out.push_str(&slice[..start_len]);

    let children = doc.children(id);
    // The closing tag begins right after the last child node; with no
    // children it follows the start tag.
    let end_offset = children
        .last()
        .map(|&child| span(doc, child).end - range.start)
        .unwrap_or(start_len);

    let mut nodes = Vec::new();
    let mut texts = Vec::new();
    for &child in &children {
        match doc.node_kind(child) {
            Some(NodeKind::Text(_)) => {
                let text = src[span(doc, child)].trim();
                if !text.is_empty() {
                    texts.push(text);
                }
            }
            Some(NodeKind::Document) | None => {}
            _ => nodes.push(child),
        }
    }

    if nodes.is_empty() {
        // Empty or text-only content stays inline: <Name>text</Name>
        for text in texts {
            out.push_str(text);
        }
        out.push_str(&slice[end_offset..]);
        return;
    }
    if !texts.is_empty() {
        // Mixed content: whitespace is significant, keep it verbatim.
        out.push_str(&slice[start_len..]);
        return;
    }

    for &child in &nodes {
        write_node(doc, src, child, depth + 1, out);
    }
    line_start(out, depth);
    out.push_str(&slice[end_offset..]);
}

fn span(doc: &Document, id: NodeId) -> std::ops::Range<usize> {
    doc.node_range(id).expect("nodes come from parsed source")
}

fn line_start(out: &mut String, depth: usize) {
    if !out.is_empty() {
        out.push('\n');
    }
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// Format a numeric value with up to six decimals, trimming trailing zeros.
pub use ipc2581::write::fmt_num;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindents_and_preserves_tag_bytes() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Root revision=\"C\" xmlns=\"urn:x\"><A>\n\n   <B  attr=\"a &gt; b\" />\n</A><Empty></Empty><Text> padded </Text></Root>";

        let out = reformat_xml(xml).unwrap();

        let expected = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Root revision=\"C\" xmlns=\"urn:x\">\n  <A>\n    <B  attr=\"a &gt; b\" />\n  </A>\n  <Empty></Empty>\n  <Text>padded</Text>\n</Root>";
        assert_eq!(out, expected);
    }
}
