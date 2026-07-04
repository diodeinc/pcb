use anyhow::Result;
use ipc2581::edit::{self, Doc, Node};
use quick_xml::{
    Writer,
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
/// - Preserves ALL existing FileRevision elements (untouched bytes)
/// - Creates a HistoryRecord if the file does not already have one
/// - Appends NEW FileRevision element with:
///   - Incremented fileRevisionId
///   - Descriptive comment about what changed
///   - SoftwarePackage element with pcb version info
pub fn append_file_revision(original_xml: &str, comment: &str) -> Result<String> {
    let now = jiff::Timestamp::now().to_string();
    let doc = Doc::parse(original_xml)?;
    let root = doc.root()?;

    let edits = match doc.child(root, "HistoryRecord") {
        // A childless record is expanded in place, keeping its attributes.
        Some(record) if doc.source(record).ends_with("/>") => {
            let xml = render(|writer| {
                writer.write_event(Event::Start(initial_history_attributes(&doc, record, &now)))?;
                write_file_revision(writer, 1, comment)?;
                writer.write_event(Event::End(BytesStart::new("HistoryRecord").to_end()))
            })?;
            vec![doc.replace(record, xml)]
        }
        Some(record) => {
            let next_id = doc
                .children(record)
                .into_iter()
                .filter(|&child| doc.name(child) == "FileRevision")
                .filter_map(|child| doc.attr(child, "fileRevisionId")?.parse::<u32>().ok())
                .map(|id| id + 1)
                .max()
                .unwrap_or(1);
            let start_tag = render(|writer| {
                writer.write_event(Event::Start(update_history_attributes(&doc, record, &now)))
            })?;
            let revision = render(|writer| write_file_revision(writer, next_id, comment))?;
            vec![
                doc.replace_start_tag(record, start_tag),
                doc.append_inside(record, revision),
            ]
        }
        None => {
            let xml = render(|writer| {
                let mut history = BytesStart::new("HistoryRecord");
                history.push_attribute(("number", "1"));
                history.push_attribute(("origination", now.as_str()));
                history.push_attribute(("software", "pcb"));
                history.push_attribute(("lastChange", now.as_str()));
                writer.write_event(Event::Start(history))?;
                write_file_revision(writer, 1, comment)?;
                writer.write_event(Event::End(BytesStart::new("HistoryRecord").to_end()))
            })?;
            // Per the schema, HistoryRecord follows Content and LogisticHeader.
            let anchor = doc
                .children(root)
                .into_iter()
                .find(|&child| !matches!(doc.name(child), "Content" | "LogisticHeader"));
            match anchor {
                Some(node) => vec![doc.insert_before(node, xml)],
                None => vec![doc.append_inside(root, xml)],
            }
        }
    };

    Ok(edit::apply(original_xml, edits)?)
}

fn render(f: impl FnOnce(&mut Writer<Cursor<Vec<u8>>>) -> std::io::Result<()>) -> Result<String> {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    f(&mut writer)?;
    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

/// Start tag for expanding a childless HistoryRecord: number stays "1",
/// lastChange/software are updated, everything else is preserved.
fn initial_history_attributes(doc: &Doc, record: Node, now: &str) -> BytesStart<'static> {
    let mut elem = BytesStart::new("HistoryRecord");
    elem.push_attribute(("number", "1"));
    for (key, value) in doc.attrs(record) {
        match key {
            "number" => {}
            "lastChange" => elem.push_attribute(("lastChange", now)),
            "software" => elem.push_attribute(("software", "pcb")),
            _ => elem.push_attribute((key, value)),
        }
    }
    elem
}

/// Start tag for an existing HistoryRecord: number is incremented,
/// lastChange/software are updated, everything else is preserved.
fn update_history_attributes(doc: &Doc, record: Node, now: &str) -> BytesStart<'static> {
    let mut elem = BytesStart::new("HistoryRecord");
    for (key, value) in doc.attrs(record) {
        match key {
            "number" => {
                let incremented = value
                    .parse::<u32>()
                    .map(|n| (n + 1).to_string())
                    .unwrap_or_else(|_| format!("{value}.1"));
                elem.push_attribute(("number", incremented.as_str()));
            }
            "lastChange" => elem.push_attribute(("lastChange", now)),
            "software" => elem.push_attribute(("software", "pcb")),
            _ => elem.push_attribute((key, value)),
        }
    }
    elem
}

fn write_file_revision(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    revision_id: u32,
    comment: &str,
) -> std::io::Result<()> {
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

    #[test]
    fn untouched_sections_stay_byte_identical() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <Content roleRef="Owner">
    <FunctionMode  mode="FABRICATION" />
  </Content>
  <HistoryRecord number="1" origination="2025-10-23T16:30:12">
    <FileRevision fileRevisionId="1" comment="Initial" label=""/>
  </HistoryRecord>
  <Ecad  units="MILLIMETER"/>
</IPC-2581>"#;

        let result = append_file_revision(original, "edit").unwrap();

        // Quirky-but-valid formatting outside the HistoryRecord is preserved.
        assert!(result.contains("<FunctionMode  mode=\"FABRICATION\" />"));
        assert!(result.contains("<Ecad  units=\"MILLIMETER\"/>"));
        assert!(
            result.contains("<FileRevision fileRevisionId=\"1\" comment=\"Initial\" label=\"\"/>")
        );
        assert!(result.contains("number=\"2\""));
    }
}
