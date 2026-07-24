use anyhow::{Result, bail};
use ipc2581::XmlWriter;
use ipc2581::edit::{self, Doc, Edit, Node};

/// PCB tool version from Cargo.toml
const PCB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Append a schema-valid history entry to HistoryRecord per IPC-2581C spec.
///
/// Per IPC-2581C Section 6.1 & 6.2:
/// - HistoryRecord number must be incremented on every modification
/// - lastChange must be updated to current timestamp
/// - HistoryRecord contains one FileRevision followed by ChangeRec entries
/// - ChangeRec entries preserve the audit trail for subsequent modifications
///
/// This function:
/// - Increments HistoryRecord/@number
/// - Updates HistoryRecord/@lastChange to current timestamp
/// - Updates HistoryRecord/@software to "pcb"
/// - Preserves HistoryRecord/@origination
/// - Creates a HistoryRecord if the file does not already have one
/// - Creates the initial FileRevision when the record is empty
/// - Appends a ChangeRec when the record already contains its FileRevision
pub fn append_file_revision(original_xml: &str, comment: &str) -> Result<String> {
    let doc = Doc::parse(original_xml)?;
    let edits = file_revision_edits(&doc, comment)?;
    Ok(edit::apply(original_xml, edits)?)
}

/// The edits behind [`append_file_revision`], for composing with other edits
/// against the same parsed document in a single splice pass.
pub fn file_revision_edits(doc: &Doc, comment: &str) -> Result<Vec<Edit>> {
    let now = jiff::Timestamp::now().to_string();
    let root = doc.root()?;

    let edits = match doc.child(root, "HistoryRecord") {
        // A childless record is expanded in place, keeping its attributes.
        Some(record) if doc.source(record).ends_with("/>") => {
            let mut writer = XmlWriter::new();
            writer.start_element_with("HistoryRecord", initial_history_attrs(doc, record, &now));
            write_file_revision(&mut writer, 1, comment);
            writer.end_element("HistoryRecord");
            vec![doc.replace(record, writer.into_string())]
        }
        Some(record) => {
            let file_revision_count = doc
                .children(record)
                .into_iter()
                .filter(|&child| doc.name(child) == "FileRevision")
                .count();
            match file_revision_count {
                0 => bail!("HistoryRecord has no FileRevision"),
                1 => {}
                _ => bail!(
                    "HistoryRecord contains multiple FileRevision elements; IPC-2581C allows exactly one"
                ),
            }

            let mut start_tag = XmlWriter::new();
            start_tag.start_element_with("HistoryRecord", updated_history_attrs(doc, record, &now));
            let mut change = XmlWriter::new();
            write_change_record(&mut change, &now, &history_person_ref(doc, root), comment);
            vec![
                doc.replace_start_tag(record, start_tag.into_string()),
                doc.append_inside(record, change.into_string()),
            ]
        }
        None => {
            let mut writer = XmlWriter::new();
            writer.start_element(
                "HistoryRecord",
                &[
                    ("number", "1"),
                    ("origination", now.as_str()),
                    ("software", "pcb"),
                    ("lastChange", now.as_str()),
                ],
            );
            write_file_revision(&mut writer, 1, comment);
            writer.end_element("HistoryRecord");
            // Per the schema, HistoryRecord follows Content and LogisticHeader.
            let anchor = doc
                .children(root)
                .into_iter()
                .find(|&child| !matches!(doc.name(child), "Content" | "LogisticHeader"));
            match anchor {
                Some(node) => vec![doc.insert_before(node, writer.into_string())],
                None => vec![doc.append_inside(root, writer.into_string())],
            }
        }
    };

    Ok(edits)
}

/// Attributes for expanding a childless HistoryRecord: number stays "1",
/// lastChange/software are updated, everything else is preserved.
fn initial_history_attrs(doc: &Doc, record: Node, now: &str) -> Vec<(String, String)> {
    let mut attrs = vec![("number".to_string(), "1".to_string())];
    attrs.extend(doc.attrs(record).filter(|(key, _)| *key != "number").map(
        |(key, value)| match key {
            "lastChange" => (key.to_string(), now.to_string()),
            "software" => (key.to_string(), "pcb".to_string()),
            _ => (key.to_string(), value.to_string()),
        },
    ));
    attrs
}

/// Attributes for an existing HistoryRecord: number is incremented,
/// lastChange/software are updated, everything else is preserved.
fn updated_history_attrs(doc: &Doc, record: Node, now: &str) -> Vec<(String, String)> {
    doc.attrs(record)
        .map(|(key, value)| match key {
            "number" => {
                let incremented = value
                    .parse::<u32>()
                    .map(|n| (n + 1).to_string())
                    .unwrap_or_else(|_| format!("{value}.1"));
                (key.to_string(), incremented)
            }
            "lastChange" => (key.to_string(), now.to_string()),
            "software" => (key.to_string(), "pcb".to_string()),
            _ => (key.to_string(), value.to_string()),
        })
        .collect()
}

fn write_file_revision(writer: &mut XmlWriter, revision_id: u32, comment: &str) {
    writer.start_element(
        "FileRevision",
        &[
            ("fileRevisionId", revision_id.to_string().as_str()),
            ("comment", comment),
            ("label", ""),
        ],
    );
    writer.start_element(
        "SoftwarePackage",
        &[
            ("name", "pcb"),
            ("revision", PCB_VERSION),
            ("vendor", "Diode"),
        ],
    );
    writer.empty_element("Certification", &[("certificationStatus", "SELFTEST")]);
    writer.end_element("SoftwarePackage");
    writer.end_element("FileRevision");
}

fn write_change_record(writer: &mut XmlWriter, datetime: &str, person_ref: &str, comment: &str) {
    writer.empty_element(
        "ChangeRec",
        &[
            ("datetime", datetime),
            ("personRef", person_ref),
            ("application", "pcb"),
            ("change", comment),
        ],
    );
}

fn history_person_ref(doc: &Doc, root: Node) -> String {
    doc.child(root, "LogisticHeader")
        .and_then(|header| {
            doc.children(header)
                .into_iter()
                .find(|&child| doc.name(child) == "Person")
        })
        .and_then(|person| doc.attr(person, "name"))
        .unwrap_or("pcb")
        .to_string()
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

        // A subsequent edit is recorded as ChangeRec; HistoryRecord allows
        // only one FileRevision.
        assert_eq!(result.matches("<FileRevision").count(), 1);
        assert!(result.contains("<ChangeRec"));
        assert!(result.contains("application=\"pcb\""));
        assert!(result.contains("change=\"BOM alternatives added\""));
    }

    #[test]
    fn test_existing_change_records_preserved() {
        let original = r#"<?xml version="1.0"?>
<IPC-2581>
  <HistoryRecord number="3" origination="2025-10-23T16:30:12" software="pcb" lastChange="2025-11-17T20:00:00">
    <FileRevision fileRevisionId="1" comment="Initial" label="">
      <SoftwarePackage name="KiCad" revision="9.0.5" vendor="KiCad EDA"/>
    </FileRevision>
    <ChangeRec datetime="2025-11-01T20:00:00Z" personRef="pcb" application="pcb" change="First edit"/>
    <ChangeRec datetime="2025-11-10T20:00:00Z" personRef="pcb" application="pcb" change="Second edit"/>
  </HistoryRecord>
</IPC-2581>"#;

        let result = append_file_revision(original, "Third edit").unwrap();

        // Number incremented from 3 to 4
        assert!(result.contains("number=\"4\""));

        // The original revision and both existing changes are preserved.
        assert_eq!(result.matches("<FileRevision").count(), 1);
        assert!(result.contains("fileRevisionId=\"1\""));
        assert!(result.contains("Initial"));
        assert!(result.contains("First edit"));
        assert!(result.contains("Second edit"));

        // New edit appended as a third ChangeRec.
        assert_eq!(result.matches("<ChangeRec").count(), 3);
        assert!(result.contains("change=\"Third edit\""));
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
