use anyhow::{Context, Result, bail};
use ipc2581::edit::start_tag_len;

/// Reformat XML with proper 2-space indentation.
///
/// Regenerates only the whitespace between tags: every tag keeps its exact
/// source bytes (attribute order, escaping, quoting), whitespace-only text is
/// dropped, and other text content stays inline within its element.
///
/// Two streaming passes over the source: the first classifies each element's
/// content, the second writes it. Memory beyond the output is one byte per
/// element, so reformatting stays cheap on documents of any size.
pub fn reformat_xml(xml: &str) -> Result<String> {
    let contents = element_contents(xml)?;
    let mut contents = contents.into_iter();
    let mut out = String::with_capacity(xml.len());
    // Content kinds of the open elements; a mixed element is copied
    // verbatim, so nothing inside it is pushed.
    let mut open: Vec<Content> = Vec::new();
    let mut verbatim: Option<(usize, usize)> = None;
    for token in Tokens::new(xml) {
        let token = token?;
        if let Some((from, depth)) = &mut verbatim {
            match token.kind {
                TokenKind::Start => *depth += 1,
                TokenKind::End if *depth == 0 => {
                    out.push_str(&xml[*from..token.range.end]);
                    verbatim = None;
                }
                TokenKind::End => *depth -= 1,
                _ => {}
            }
            continue;
        }
        let depth = open.len();
        let source = &xml[token.range.clone()];
        match token.kind {
            TokenKind::Declaration => write_declaration(source, &mut out),
            TokenKind::Text => {
                if open.last() == Some(&Content::Inline) {
                    out.push_str(source.trim());
                }
            }
            TokenKind::Node | TokenKind::EmptyElement => {
                line_start(&mut out, depth);
                out.push_str(source);
            }
            TokenKind::Start => {
                line_start(&mut out, depth);
                out.push_str(source);
                let content = contents.next().expect("first pass saw every element");
                if content == Content::Mixed {
                    verbatim = Some((token.range.end, 0));
                } else {
                    open.push(content);
                }
            }
            TokenKind::End => {
                if open.pop() == Some(Content::Elements) {
                    line_start(&mut out, depth - 1);
                }
                out.push_str(source);
            }
        }
    }
    Ok(out)
}

/// What an element directly contains, decided before it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// Nothing, or text only: written on one line.
    Inline,
    /// Elements and other markup, with only whitespace between: indented.
    Elements,
    /// Markup and text together: whitespace is significant, kept verbatim.
    Mixed,
}

/// The content of every non-empty element, in document order; also checks
/// that start and end tags balance under one root element.
fn element_contents(xml: &str) -> Result<Vec<Content>> {
    let mut contents = Vec::new();
    // Per open element: its index in `contents`, its name, and whether it
    // directly holds text and markup.
    let mut open: Vec<(usize, &str, bool, bool)> = Vec::new();
    let mut roots = 0;
    for token in Tokens::new(xml) {
        let token = token?;
        let source = &xml[token.range.clone()];
        let is_markup = matches!(
            token.kind,
            TokenKind::Node | TokenKind::EmptyElement | TokenKind::Start
        );
        match open.last_mut() {
            Some((_, _, text, markup)) => {
                *text |= token.kind == TokenKind::Text && !source.trim().is_empty();
                *markup |= is_markup;
            }
            None if token.kind == TokenKind::Text && !source.trim().is_empty() => {
                bail!("XML parse error: text outside the root element");
            }
            None => {
                roots += usize::from(matches!(
                    token.kind,
                    TokenKind::EmptyElement | TokenKind::Start
                ));
            }
        }
        match token.kind {
            TokenKind::Start => {
                open.push((contents.len(), element_name(&source[1..]), false, false));
                contents.push(Content::Inline);
            }
            TokenKind::End => {
                let name = element_name(&source[2..]);
                let Some((index, open_name, text, markup)) = open.pop() else {
                    bail!("XML parse error: unexpected end tag </{name}>");
                };
                if name != open_name {
                    bail!("XML parse error: end tag </{name}> does not match <{open_name}>");
                }
                contents[index] = match (text, markup) {
                    (true, true) => Content::Mixed,
                    (false, true) => Content::Elements,
                    (_, false) => Content::Inline,
                };
            }
            _ => {}
        }
    }
    if let Some((_, name, ..)) = open.last() {
        bail!("XML parse error: <{name}> is never closed");
    }
    match roots {
        0 => bail!("XML document has no root element"),
        1 => Ok(contents),
        _ => bail!("XML parse error: more than one root element"),
    }
}

/// The qualified name at the start of a tag's body.
fn element_name(body: &str) -> &str {
    let end = body
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .unwrap_or(body.len());
    &body[..end]
}

/// The XML declaration, rewritten in its canonical form.
fn write_declaration(source: &str, out: &mut String) {
    out.push_str("<?xml");
    for name in ["version", "encoding", "standalone"] {
        if let Some(value) = pseudo_attribute(source, name) {
            out.push_str(&format!(" {name}=\"{value}\""));
        }
    }
    out.push_str("?>");
}

/// The value of the pseudo-attribute `name` in an XML declaration.
fn pseudo_attribute<'a>(declaration: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = declaration;
    while let Some(at) = rest.find(name) {
        let preceded = rest[..at].ends_with(|c: char| c.is_ascii_whitespace());
        rest = &rest[at + name.len()..];
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        if preceded {
            return value[1..].split(quote).next();
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    /// `<?xml ...?>` at the start of the document.
    Declaration,
    /// A comment, processing instruction, CDATA section, or DOCTYPE.
    Node,
    EmptyElement,
    Start,
    End,
    Text,
}

struct Token {
    kind: TokenKind,
    range: std::ops::Range<usize>,
}

/// The markup and text runs of an XML document, in order.
struct Tokens<'a> {
    xml: &'a str,
    at: usize,
}

impl<'a> Tokens<'a> {
    fn new(xml: &'a str) -> Self {
        Self { xml, at: 0 }
    }

    fn token(&self) -> Result<(TokenKind, usize)> {
        let rest = &self.xml[self.at..];
        let through = |terminator: &str, what: &str| {
            rest.find(terminator)
                .map(|end| end + terminator.len())
                .with_context(|| format!("XML parse error: unterminated {what}"))
        };
        if !rest.starts_with('<') {
            return Ok((TokenKind::Text, rest.find('<').unwrap_or(rest.len())));
        }
        if rest.starts_with("<?") {
            let len = through("?>", "processing instruction")?;
            let declaration = self.xml[..self.at]
                .trim_start_matches('\u{feff}')
                .is_empty()
                && rest.strip_prefix("<?xml").is_some_and(|rest| {
                    rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '?')
                });
            let kind = if declaration {
                TokenKind::Declaration
            } else {
                TokenKind::Node
            };
            return Ok((kind, len));
        }
        if rest.starts_with("<!--") {
            return Ok((TokenKind::Node, through("-->", "comment")?));
        }
        if rest.starts_with("<![CDATA[") {
            return Ok((TokenKind::Node, through("]]>", "CDATA section")?));
        }
        if rest.starts_with("<!") {
            return Ok((TokenKind::Node, doctype_len(rest)?));
        }
        if rest.starts_with("</") {
            return Ok((TokenKind::End, through(">", "end tag")?));
        }
        let len = start_tag_len(rest);
        if !rest[..len].ends_with('>') {
            bail!("XML parse error: unterminated start tag");
        }
        let kind = if rest[..len].ends_with("/>") {
            TokenKind::EmptyElement
        } else {
            TokenKind::Start
        };
        Ok((kind, len))
    }
}

impl Iterator for Tokens<'_> {
    type Item = Result<Token>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.at == self.xml.len() {
            return None;
        }
        Some(self.token().map(|(kind, len)| {
            let range = self.at..self.at + len;
            self.at = range.end;
            Token { kind, range }
        }))
    }
}

/// Length of a `<!DOCTYPE ...>`, whose internal subset may hold `>`.
fn doctype_len(source: &str) -> Result<usize> {
    let mut quote = None;
    let mut subset = false;
    for (index, byte) in source.bytes().enumerate() {
        match (quote, byte) {
            (Some(open), _) if byte == open => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'[') => subset = true,
            (None, b']') => subset = false,
            (None, b'>') if !subset => return Ok(index + 1),
            _ => {}
        }
    }
    bail!("XML parse error: unterminated DOCTYPE")
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
    fn rejects_malformed_documents() {
        for (xml, error) in [
            (
                "<r><a></b></r>",
                "XML parse error: end tag </b> does not match <a>",
            ),
            ("<r><a>", "XML parse error: <a> is never closed"),
            ("<r/><s/>", "XML parse error: more than one root element"),
            ("<r/>junk", "XML parse error: text outside the root element"),
            ("<r a=\"1\"", "XML parse error: unterminated start tag"),
            ("<!-- only -->", "XML document has no root element"),
        ] {
            assert_eq!(reformat_xml(xml).unwrap_err().to_string(), error, "{xml}");
        }
    }
}
