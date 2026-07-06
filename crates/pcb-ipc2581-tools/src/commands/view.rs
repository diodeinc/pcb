use std::path::Path;

use anyhow::Result;
use ipc2581::edit::{self, Doc};
use ipc2581::{Mode, XmlWriter};

use crate::ViewMode;
use crate::utils::file as file_utils;

/// Defines which sections to exclude for each mode
/// Based on IPC-2581C Function Mode Table (Table 4)
fn excluded_sections(mode: Mode) -> &'static [&'static str] {
    match mode {
        Mode::UserDef => &[],
        Mode::Bom => &[
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
        Mode::Assembly => &[
            // Assembly needs most data except stackup details
            "Stackup",
        ],
        Mode::Fabrication => &[
            // Fabrication doesn't need component placement
            "Component",
        ],
        Mode::Stackup => &[
            // Stackup only needs layer definitions and stackup info
            "PadstackDef",
            "Package",
            "Component",
            "PhyNetGroup",
            "LayerFeature",
        ],
        Mode::Test => &[
            // Test needs placement and nets but not fabrication details
            "PadstackDef",
            "Stackup",
            "LayerFeature",
        ],
        Mode::Stencil => &[
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
        Mode::Dfx => &[
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
    let excluded = excluded_sections(mode.as_ipc_mode());
    let doc = Doc::parse(xml)?;
    let mut edits = Vec::new();

    // Delete excluded sections wherever they appear, skipping any nested
    // inside an element that is already being deleted.
    let mut spans: Vec<_> = excluded
        .iter()
        .flat_map(|name| doc.find_all(name))
        .map(|node| (doc.span(node), node))
        .collect();
    spans.sort_by_key(|(span, _)| span.start);
    let mut deleted_end = 0usize;
    let mut deleted_spans = Vec::new();
    for (span, node) in spans {
        if span.start < deleted_end {
            continue;
        }
        deleted_end = span.end;
        deleted_spans.push(span.clone());
        edits.push(doc.delete(node));
    }

    // Rewrite FunctionMode's mode attribute, preserving other attributes.
    for function_mode in doc.find_all("FunctionMode") {
        let span = doc.span(function_mode);
        if deleted_spans
            .iter()
            .any(|deleted| deleted.start <= span.start && span.end <= deleted.end)
        {
            continue;
        }
        let mut attrs = vec![("mode".to_string(), mode.as_str().to_string())];
        attrs.extend(
            doc.attrs(function_mode)
                .filter(|(key, _)| *key != "mode")
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        let mut writer = XmlWriter::new();
        writer.empty_element_with("FunctionMode", attrs);
        edits.push(doc.replace(function_mode, writer.into_string()));
    }

    Ok(edit::apply(xml, edits)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bom_excludes_components() {
        let excluded = excluded_sections(Mode::Bom);
        assert!(excluded.contains(&"Component"));
        assert!(excluded.contains(&"Package"));
        assert!(excluded.contains(&"Layer"));
    }

    #[test]
    fn test_assembly_minimal_exclusions() {
        let excluded = excluded_sections(Mode::Assembly);
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
}
