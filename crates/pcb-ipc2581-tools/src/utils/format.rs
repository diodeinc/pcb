use anyhow::{Result, anyhow};
use uppsala::{PullEvent, PullParser};

/// Reformat XML with proper 2-space indentation.
///
/// Regenerates only the whitespace between tags: every tag keeps its exact
/// source bytes (attribute order, escaping, quoting), whitespace-only text is
/// dropped, and other text content stays inline within its element. An
/// element mixing text and markup is kept verbatim. A leading byte-order mark
/// is dropped.
///
/// Streams the document in one pass, so memory beyond the output stays small
/// on documents of any size.
pub fn reformat_xml(xml: &str) -> Result<String> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    let mut out = String::with_capacity(xml.len());
    let mut open: Vec<OpenElement> = Vec::new();
    let mut parser = PullParser::new(xml);
    while let Some(event) = parser
        .next_event()
        .map_err(|err| anyhow!("XML parse error: {err}"))?
    {
        match event {
            PullEvent::XmlDeclaration(declaration) => {
                out.push_str(&format!("<?xml version=\"{}\"", declaration.version));
                if let Some(encoding) = &declaration.encoding {
                    out.push_str(&format!(" encoding=\"{encoding}\""));
                }
                if let Some(standalone) = declaration.standalone {
                    let standalone = if standalone { "yes" } else { "no" };
                    out.push_str(&format!(" standalone=\"{standalone}\""));
                }
                out.push_str("?>");
            }
            PullEvent::Doctype(doctype) => write_markup(&mut out, &mut open, &doctype),
            PullEvent::StartElement {
                byte_start,
                byte_end,
                ..
            } => {
                write_markup(&mut out, &mut open, &xml[byte_start..byte_end]);
                open.push(OpenElement {
                    start: byte_start,
                    content: byte_end,
                    written: out.len(),
                    text: false,
                    markup: false,
                });
            }
            PullEvent::EndElement {
                byte_start,
                byte_end,
                ..
            } => {
                let element = open.pop().expect("the pull parser balances tags");
                match (element.text, element.markup) {
                    // `/>`: the start tag was the whole element.
                    _ if byte_start == element.start => {}
                    // Whitespace is significant in mixed content.
                    (true, true) => {
                        out.truncate(element.written);
                        out.push_str(&xml[element.content..byte_end]);
                    }
                    (false, true) => {
                        line_start(&mut out, open.len());
                        out.push_str(&xml[byte_start..byte_end]);
                    }
                    (_, false) => out.push_str(&xml[byte_start..byte_end]),
                }
            }
            PullEvent::Text {
                byte_start,
                byte_end,
                ..
            } => {
                let text = xml[byte_start..byte_end].trim();
                if let Some(element) = open.last_mut().filter(|_| !text.is_empty()) {
                    element.text = true;
                    out.push_str(text);
                }
            }
            PullEvent::CData {
                byte_start,
                byte_end,
                ..
            }
            | PullEvent::Comment {
                byte_start,
                byte_end,
                ..
            }
            | PullEvent::ProcessingInstruction {
                byte_start,
                byte_end,
                ..
            } => write_markup(&mut out, &mut open, &xml[byte_start..byte_end]),
            PullEvent::StartNamespace { .. } | PullEvent::EndNamespace => {}
        }
    }
    Ok(out)
}

/// An element whose end tag is still to come.
struct OpenElement {
    /// Source offset of its start tag.
    start: usize,
    /// Source offset of its content, just past the start tag.
    content: usize,
    /// Output length once its start tag was written.
    written: usize,
    text: bool,
    markup: bool,
}

/// Write markup on its own line inside the innermost open element.
fn write_markup(out: &mut String, open: &mut [OpenElement], source: &str) {
    if let Some(parent) = open.last_mut() {
        parent.markup = true;
    }
    line_start(out, open.len());
    out.push_str(source);
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

    #[test]
    fn keeps_markup_and_mixed_content() {
        let xml = "<?xml version='1.0' encoding='utf-8' standalone='yes'?><!DOCTYPE r [<!ENTITY e \"x>y\">]><!-- top --><r a=\"1>2\"><!-- c --><a>  t &amp; u  </a><m>x<b/> y</m><?pi d?><![CDATA[<z>]]><g>\n\n</g></r>\n<!-- tail -->";

        let out = reformat_xml(xml).unwrap();

        let expected = "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"yes\"?>\n<!DOCTYPE r [<!ENTITY e \"x>y\">]>\n<!-- top -->\n<r a=\"1>2\">\n  <!-- c -->\n  <a>t &amp; u</a>\n  <m>x<b/> y</m>\n  <?pi d?>\n  <![CDATA[<z>]]>\n  <g></g>\n</r>\n<!-- tail -->";
        assert_eq!(out, expected);
    }

    #[test]
    fn elements_after_mixed_content_keep_their_text() {
        let xml = "<r><m>x<b><c/></b>y</m><t> text </t><e><f/></e></r>";

        let out = reformat_xml(xml).unwrap();

        let expected = "<r>\n  <m>x<b><c/></b>y</m>\n  <t>text</t>\n  <e>\n    <f/>\n  </e>\n</r>";
        assert_eq!(out, expected);
    }

    #[test]
    fn drops_a_byte_order_mark() {
        let out = reformat_xml("\u{feff}<?xml version = '1.0'?>\r\n<r>\r\n<a/></r>").unwrap();

        assert_eq!(out, "<?xml version=\"1.0\"?>\n<r>\n  <a/>\n</r>");
    }

    #[test]
    fn rejects_malformed_documents() {
        for xml in [
            "<r><a></b></r>",
            "<r><a>",
            "<r/><s/>",
            "<r/>junk",
            "<r a=\"1\"",
            "<!-- only -->",
        ] {
            assert!(reformat_xml(xml).is_err(), "{xml}");
        }
    }
}
