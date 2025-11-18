use anyhow::Result;
use quick_xml::{events::Event, Reader, Writer};
use std::io::Cursor;

/// Reformat XML with proper 2-space indentation
/// Strips all whitespace-only text nodes and regenerates consistent indentation
pub fn reformat_xml(xml: &str) -> Result<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true); // Strip whitespace-only text nodes
    let mut writer = Writer::new_with_indent(Cursor::new(Vec::new()), b' ', 2);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            e => writer.write_event(e)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}
