use std::path::Path;

use anyhow::Result;
use quick_xml::{
    events::{BytesStart, Event},
    Reader, Writer,
};
use std::io::Cursor;

use crate::utils::file as file_utils;
use crate::ViewMode;

/// Defines which sections to exclude for each mode
/// Based on IPC-2581C Function Mode Table (Table 4)
fn excluded_sections(mode: ViewMode) -> &'static [&'static str] {
    match mode {
        ViewMode::Bom => &[
            // ECAD data
            "PadstackDef",
            "Package",
            "Component",
            "Stackup",
            "Profile",
            "LayerFeature",
            "LogicalNet",
            "PhyNetGroup",
            // Layers - BOM doesn't need any layer data
            "Layer",
        ],
        ViewMode::Assembly => &[
            // Assembly needs most data except stackup details
            "Stackup",
        ],
        ViewMode::Fabrication => &[
            // Fabrication doesn't need component placement
            "Component",
        ],
        ViewMode::Stackup => &[
            // Stackup only needs layer definitions and stackup info
            "PadstackDef",
            "Package",
            "Component",
            "PhyNetGroup",
            "LayerFeature",
        ],
        ViewMode::Test => &[
            // Test needs placement and nets but not fabrication details
            "PadstackDef",
            "Stackup",
            "LayerFeature",
        ],
        ViewMode::Stencil => &[
            // Stencil only needs paste layers
            "PadstackDef",
            "Package",
            "Component",
            "Bom",
            "Avl",
            "Stackup",
            "LogicalNet",
            "PhyNetGroup",
        ],
        ViewMode::Dfx => &[
            // DFX only needs measurement data
            "PadstackDef",
            "Package",
            "Component",
            "Stackup",
            "Profile",
            "LogicalNet",
            "PhyNetGroup",
            "LayerFeature",
        ],
    }
}

pub fn execute(input: &Path, mode: ViewMode, output: &Path) -> Result<()> {
    let content = file_utils::load_ipc_file(input)?;
    let mut filtered_xml = filter_by_mode(&content, mode)?;

    // Append FileRevision to HistoryRecord per IPC-2581C spec
    let comment = format!("Filtered to {} view", mode.as_str());
    filtered_xml = crate::utils::history::append_file_revision(&filtered_xml, &comment)?;

    // Reformat XML with proper indentation
    filtered_xml = crate::utils::format::reformat_xml(&filtered_xml)?;

    file_utils::save_ipc_file(output, &filtered_xml)?;

    eprintln!("✓ Exported {} mode view to {:?}", mode.as_str(), output);
    Ok(())
}

fn filter_by_mode(xml: &str, mode: ViewMode) -> Result<String> {
    let excluded = excluded_sections(mode);
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();
    let mut skip_depth = 0;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Empty(ref e) | Event::Start(ref e) if e.name().as_ref() == b"FunctionMode" => {
                write_function_mode_element(&mut writer, mode, e)?;
            }
            Event::End(ref e) if e.name().as_ref() == b"FunctionMode" => {
                writer.write_event(Event::End(e.to_owned()))?;
            }
            Event::Start(ref e) if skip_depth == 0 => {
                if should_exclude(e.name().as_ref(), excluded) {
                    skip_depth = 1;
                } else {
                    writer.write_event(Event::Start(e.to_owned()))?;
                }
            }
            Event::Empty(ref e) if skip_depth == 0 => {
                if !should_exclude(e.name().as_ref(), excluded) {
                    writer.write_event(Event::Empty(e.to_owned()))?;
                }
            }
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::End(_) if skip_depth > 0 => skip_depth -= 1,
            event if skip_depth == 0 => writer.write_event(event)?,
            _ => {}
        }
        buf.clear();
    }

    Ok(String::from_utf8(writer.into_inner().into_inner())?)
}

fn should_exclude(name: &[u8], excluded: &[&str]) -> bool {
    excluded.iter().any(|&ex| ex.as_bytes() == name)
}

fn write_function_mode_element(
    writer: &mut Writer<Cursor<Vec<u8>>>,
    mode: ViewMode,
    event: &BytesStart,
) -> Result<()> {
    let mut elem = BytesStart::new("FunctionMode");
    elem.push_attribute(("mode", mode.as_str()));
    for attr in event.attributes().flatten() {
        if attr.key.as_ref() != b"mode" {
            elem.push_attribute(attr);
        }
    }
    writer.write_event(Event::Empty(elem))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bom_excludes_components() {
        let excluded = excluded_sections(ViewMode::Bom);
        assert!(excluded.contains(&"Component"));
        assert!(excluded.contains(&"Package"));
        assert!(excluded.contains(&"Layer"));
    }

    #[test]
    fn test_assembly_minimal_exclusions() {
        let excluded = excluded_sections(ViewMode::Assembly);
        assert!(excluded.contains(&"Stackup"));
        assert!(!excluded.contains(&"Component"));
    }

    #[test]
    fn test_filter_updates_function_mode() {
        let xml = r#"<?xml version="1.0"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY" level="1"/>
  </Content>
</IPC-2581>"#;

        let result = filter_by_mode(xml, ViewMode::Bom).unwrap();
        assert!(result.contains("mode=\"BOM\""));
        assert!(!result.contains("mode=\"ASSEMBLY\""));
    }

    #[test]
    fn test_filter_removes_excluded_sections() {
        let xml = r#"<?xml version="1.0"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
  </Content>
  <Ecad>
    <CadData>
      <Step>
        <Component refDes="R1"/>
        <Package name="PKG1"/>
      </Step>
    </CadData>
  </Ecad>
  <Bom name="BOM1"/>
</IPC-2581>"#;

        let result = filter_by_mode(xml, ViewMode::Bom).unwrap();

        // Should exclude components and packages
        assert!(!result.contains("Component"));
        assert!(!result.contains("Package"));
        assert!(!result.contains("R1"));
        assert!(!result.contains("PKG1"));

        // Should keep BOM
        assert!(result.contains("<Bom"));

        // Should keep structure
        assert!(result.contains("<Ecad"));
        assert!(result.contains("<Step"));
    }

    #[test]
    fn test_should_exclude_byte_comparison() {
        let excluded = &["Component", "Package"];
        assert!(should_exclude(b"Component", excluded));
        assert!(should_exclude(b"Package", excluded));
        assert!(!should_exclude(b"Bom", excluded));
    }
}
