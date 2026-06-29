use anyhow::Result;
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use std::io::Cursor;

/// PCB tool version from Cargo.toml
const PCB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Append a FileRevision entry to HistoryRecord per IPC-2581C spec
///
/// Per IPC-2581C Section 6.1 & 6.2:
/// - HistoryRecord number must be incremented on every modification
/// - lastChange must be updated to current timestamp
/// - FileRevision elements track the sequence of changes and tools used
/// - ALL previous FileRevision elements must be preserved (audit trail)
///
/// This function:
/// - Increments HistoryRecord/@number
/// - Updates HistoryRecord/@lastChange to current timestamp
/// - Updates HistoryRecord/@software to "pcb"
/// - Preserves HistoryRecord/@origination
/// - Preserves ALL existing FileRevision elements
/// - Creates a HistoryRecord if the file does not already have one
/// - Appends NEW FileRevision element with:
///   - Incremented fileRevisionId
///   - Descriptive comment about what changed
///   - SoftwarePackage element with pcb version info
pub fn append_file_revision(original_xml: &str, comment: &str) -> Result<String> {
    let now = jiff::Timestamp::now().to_string();

    if has_history_record(original_xml)? {
        append_to_history_record(original_xml, comment, &now)
    } else {
        insert_history_record(original_xml, comment, &now)
    }
}

fn append_to_history_record(original_xml: &str, comment: &str, now: &str) -> Result<String> {
    let mut reader = Reader::from_str(original_xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    let mut in_history_record = false;
    let mut next_revision_id = 1u32;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,

            // Update HistoryRecord attributes
            Event::Start(ref e) if e.name().as_ref() == b"HistoryRecord" => {
                in_history_record = true;
                writer.write_event(Event::Start(update_history_attributes(e, now)?))?;
            }

            Event::Empty(ref e) if e.name().as_ref() == b"HistoryRecord" => {
                writer.write_event(Event::Start(initial_history_attributes(e, now)?))?;
                write_file_revision(&mut writer, next_revision_id, comment)?;
                writer.write_event(Event::End(BytesStart::new("HistoryRecord").to_end()))?;
            }

            // Track FileRevision IDs as we encounter them
            Event::Start(ref e) if e.name().as_ref() == b"FileRevision" && in_history_record => {
                next_revision_id = track_revision_id(e, next_revision_id)?;
                writer.write_event(Event::Start(e.to_owned()))?;
            }

            // Before closing HistoryRecord, append our new FileRevision
            Event::End(ref e) if e.name().as_ref() == b"HistoryRecord" => {
                in_history_record = false;
                write_file_revision(&mut writer, next_revision_id, comment)?;
                writer.write_event(Event::End(e.to_owned()))?;
            }

            e => writer.write_event(e)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn insert_history_record(original_xml: &str, comment: &str, now: &str) -> Result<String> {
    let mut reader = Reader::from_str(original_xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut inserted = false;
    let mut depth = 0usize;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(ref e) => {
                if should_insert_history_before(inserted, depth, e.name().as_ref()) {
                    write_history_record(&mut writer, now, comment)?;
                    inserted = true;
                }
                writer.write_event(Event::Start(e.to_owned()))?;
                depth += 1;
            }
            Event::Empty(ref e) => {
                if should_insert_history_before(inserted, depth, e.name().as_ref()) {
                    write_history_record(&mut writer, now, comment)?;
                    inserted = true;
                }
                writer.write_event(Event::Empty(e.to_owned()))?;
            }
            Event::End(ref e) => {
                if !inserted && depth == 1 && e.name().as_ref() == b"IPC-2581" {
                    write_history_record(&mut writer, now, comment)?;
                    inserted = true;
                }
                writer.write_event(Event::End(e.to_owned()))?;
                depth = depth.saturating_sub(1);
            }
            e => writer.write_event(e)?,
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn has_history_record(xml: &str) -> Result<bool> {
    let mut reader = Reader::from_str(xml);
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => return Ok(false),
            Event::Start(ref e) | Event::Empty(ref e) if e.name().as_ref() == b"HistoryRecord" => {
                return Ok(true);
            }
            _ => {}
        }
        buf.clear();
    }
}

fn should_insert_history_before(inserted: bool, depth: usize, element_name: &[u8]) -> bool {
    !inserted && depth == 1 && !matches!(element_name, b"Content" | b"LogisticHeader")
}

fn write_history_record(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    now: &str,
    comment: &str,
) -> Result<()> {
    let mut history = BytesStart::new("HistoryRecord");
    history.push_attribute(("number", "1"));
    history.push_attribute(("origination", now));
    history.push_attribute(("software", "pcb"));
    history.push_attribute(("lastChange", now));
    writer.write_event(Event::Start(history))?;

    write_file_revision(writer, 1, comment)?;

    writer.write_event(Event::End(BytesStart::new("HistoryRecord").to_end()))?;
    Ok(())
}

fn initial_history_attributes<'a>(e: &BytesStart, now: &'a str) -> Result<BytesStart<'a>> {
    let mut elem = BytesStart::new("HistoryRecord");
    elem.push_attribute(("number", "1"));
    for attr in e.attributes() {
        let attr = attr?;
        let key = attr.key.as_ref();
        match key {
            b"number" => {}
            b"lastChange" => elem.push_attribute(("lastChange", now)),
            b"software" => elem.push_attribute(("software", "pcb")),
            _ => elem.push_attribute(attr),
        }
    }
    Ok(elem)
}

fn update_history_attributes<'a>(e: &BytesStart, now: &'a str) -> Result<BytesStart<'a>> {
    let mut elem = BytesStart::new("HistoryRecord");
    for attr in e.attributes() {
        let attr = attr?;
        let key = attr.key.as_ref();
        match key {
            b"number" => {
                let incremented = attr
                    .unescape_value()?
                    .parse::<u32>()
                    .map(|n| (n + 1).to_string())
                    .unwrap_or_else(|_| format!("{}.1", attr.unescape_value().unwrap()));
                elem.push_attribute(("number", incremented.as_str()));
            }
            b"lastChange" => elem.push_attribute(("lastChange", now)),
            b"software" => elem.push_attribute(("software", "pcb")),
            _ => elem.push_attribute(attr),
        }
    }
    Ok(elem)
}

fn track_revision_id(e: &BytesStart, current_max: u32) -> Result<u32> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"fileRevisionId"
            && let Ok(id) = attr.unescape_value()?.parse::<u32>()
        {
            return Ok(current_max.max(id + 1));
        }
    }
    Ok(current_max)
}

fn write_file_revision(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    revision_id: u32,
    comment: &str,
) -> Result<()> {
    let mut file_revision = BytesStart::new("FileRevision");
    file_revision.push_attribute(("fileRevisionId", revision_id.to_string().as_str()));
    file_revision.push_attribute(("comment", comment));
    file_revision.push_attribute(("label", ""));
    writer.write_event(Event::Start(file_revision))?;

    let mut software = BytesStart::new("SoftwarePackage");
    software.push_attribute(("name", "pcb"));
    software.push_attribute(("revision", PCB_VERSION));
    software.push_attribute(("vendor", "Diode"));
    writer.write_event(Event::Start(software))?;

    let mut cert = BytesStart::new("Certification");
    cert.push_attribute(("certificationStatus", "NONE"));
    writer.write_event(Event::Empty(cert))?;

    writer.write_event(Event::End(BytesStart::new("SoftwarePackage").to_end()))?;
    writer.write_event(Event::End(BytesStart::new("FileRevision").to_end()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_append_file_revision() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <HistoryRecord number="1" origination="2025-10-23T16:30:12" software="KiCad EDA" lastChange="2025-10-23T16:30:12">
    <FileRevision fileRevisionId="1" comment="Initial export" label="">
      <SoftwarePackage name="KiCad" revision="9.0.5" vendor="KiCad EDA">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
</IPC-2581>"#;

        let result = append_file_revision(original, "BOM alternatives added").unwrap();

        // HistoryRecord number incremented
        assert!(result.contains("number=\"2\""));
        // Software updated
        assert!(result.contains("software=\"pcb\""));
        // Origination preserved
        assert!(result.contains("origination=\"2025-10-23T16:30:12\""));

        // Original FileRevision preserved
        assert!(result.contains("fileRevisionId=\"1\""));
        assert!(result.contains("Initial export"));
        assert!(result.contains("KiCad"));

        // New FileRevision appended
        assert!(result.contains("fileRevisionId=\"2\""));
        assert!(result.contains("BOM alternatives added"));
        assert!(result.contains("name=\"pcb\""));
        assert!(result.contains("vendor=\"Diode\""));
    }

    #[test]
    fn test_multiple_revisions_preserved() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <HistoryRecord number="3" origination="2025-10-23T16:30:12" software="pcb" lastChange="2025-11-17T20:00:00">
    <FileRevision fileRevisionId="1" comment="Initial" label="">
      <SoftwarePackage name="KiCad" revision="9.0.5" vendor="KiCad EDA"/>
    </FileRevision>
    <FileRevision fileRevisionId="2" comment="First edit" label="">
      <SoftwarePackage name="pcb" revision="0.2.25" vendor="Diode"/>
    </FileRevision>
    <FileRevision fileRevisionId="3" comment="Second edit" label="">
      <SoftwarePackage name="pcb" revision="0.2.26" vendor="Diode"/>
    </FileRevision>
  </HistoryRecord>
</IPC-2581>"#;

        let result = append_file_revision(original, "Third edit").unwrap();

        // Number incremented from 3 to 4
        assert!(result.contains("number=\"4\""));

        // All three previous revisions preserved
        assert!(result.contains("fileRevisionId=\"1\""));
        assert!(result.contains("Initial"));
        assert!(result.contains("fileRevisionId=\"2\""));
        assert!(result.contains("First edit"));
        assert!(result.contains("fileRevisionId=\"3\""));
        assert!(result.contains("Second edit"));

        // New revision appended as ID 4
        assert!(result.contains("fileRevisionId=\"4\""));
        assert!(result.contains("Third edit"));
    }

    #[test]
    fn test_creates_history_record_when_missing() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content/>
  <Ecad/>
</IPC-2581>"#;

        let result = append_file_revision(original, "Created board array").unwrap();

        assert!(result.contains("<HistoryRecord number=\"1\""));
        assert!(result.contains("software=\"pcb\""));
        assert!(result.contains("fileRevisionId=\"1\""));
        assert!(result.contains("Created board array"));
        assert!(result.contains("vendor=\"Diode\""));

        let content_idx = result.find("<Content").unwrap();
        let history_idx = result.find("<HistoryRecord").unwrap();
        let ecad_idx = result.find("<Ecad").unwrap();
        assert!(content_idx < history_idx);
        assert!(history_idx < ecad_idx);
    }

    #[test]
    fn test_expands_empty_history_record() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content/>
  <HistoryRecord number="1" origination="2025-10-23T16:30:12" software="KiCad EDA" lastChange="2025-10-23T16:30:12"/>
  <Ecad/>
</IPC-2581>"#;

        let result = append_file_revision(original, "Created board array").unwrap();

        assert!(result.contains("<HistoryRecord number=\"1\""));
        assert!(result.contains("fileRevisionId=\"1\""));
        assert!(result.contains("Created board array"));
    }
}
